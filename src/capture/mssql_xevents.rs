//! SQL Server Extended Events (XEvents) capture — the modern replacement for SQL
//! Profiler (deprecated since SQL Server 2012). Accepts an XML export: either a
//! `<RingBufferTarget>` dump (`CAST(target_data AS XML)` from
//! `sys.dm_xe_session_targets`) or a bare sequence of `<event>` elements (e.g.
//! concatenated `event_data` rows from `sys.fn_xe_file_target_read_file`). The
//! surrounding wrapper is ignored — this parser scans for `<event>` elements
//! wherever they appear, so either shape works. pg-retest never connects to SQL
//! Server: the DBA exports this XML offline and uploads it.
//!
//! Only "completed" events carry a real SQL text + duration, so only these are kept
//! (other XEvents — logins, waits, attentions, etc. — are silently skipped):
//! - `sql_batch_completed` → SQL text in the `batch_text` data field
//! - `rpc_completed` → SQL text in the `statement` data field
//! - `sql_statement_completed` / `sp_statement_completed` → SQL text in `statement`
//!
//! `duration` is in **microseconds** (Extended Events durations are always
//! microseconds — no GUI-vs-table unit split like Profiler has). Session grouping
//! uses the `session_id` action if present; falls back to session 0 otherwise.

use std::collections::HashMap;
use std::io::BufRead;
use std::path::Path;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use quick_xml::events::Event as XmlEvent;
use quick_xml::reader::Reader;
use tracing::debug;

use crate::profile::{
    assign_transaction_ids, Metadata, Query, QueryKind, Session, SourceDialect, WorkloadProfile,
};

pub struct MssqlXEventsCapture;

const COMPLETED_EVENTS: &[&str] = &[
    "sql_batch_completed",
    "rpc_completed",
    "sql_statement_completed",
    "sp_statement_completed",
];

struct XEventRow {
    session_id: u64,
    timestamp: Option<DateTime<Utc>>,
    duration_us: u64,
    database: String,
    sql: String,
}

#[derive(Default)]
struct EventBuilder {
    name: String,
    timestamp: Option<DateTime<Utc>>,
    fields: HashMap<String, String>,
}

impl MssqlXEventsCapture {
    pub fn capture_from_file(&self, path: &Path, source_host: &str) -> Result<WorkloadProfile> {
        let file = std::fs::File::open(path)
            .with_context(|| format!("Failed to open Extended Events XML: {}", path.display()))?;
        self.capture_from_reader(std::io::BufReader::new(file), source_host)
    }

    pub fn capture_from_reader(
        &self,
        reader: impl BufRead,
        source_host: &str,
    ) -> Result<WorkloadProfile> {
        let rows = parse_events(reader)?;
        debug!(
            "SQL Server Extended Events: {} completed statements",
            rows.len()
        );

        let mut session_map: HashMap<u64, Vec<XEventRow>> = HashMap::new();
        for row in rows {
            session_map.entry(row.session_id).or_default().push(row);
        }

        let mut sessions = Vec::new();
        let mut total_queries: u64 = 0;
        let mut next_txn_id: u64 = 1;
        let mut global_min: Option<DateTime<Utc>> = None;
        let mut global_max: Option<DateTime<Utc>> = None;

        for (session_id, mut xrows) in session_map {
            if xrows.is_empty() {
                continue;
            }
            xrows.sort_by_key(|r| r.timestamp);

            let first_time = xrows[0].timestamp;
            let database = xrows[0].database.clone();

            for r in &xrows {
                if let Some(t) = r.timestamp {
                    global_min = Some(global_min.map_or(t, |m| m.min(t)));
                    global_max = Some(global_max.map_or(t, |m| m.max(t)));
                }
            }

            let mut queries: Vec<Query> = Vec::new();
            for row in xrows {
                let offset = match (row.timestamp, first_time) {
                    (Some(t), Some(first)) => (t - first).num_microseconds().unwrap_or(0) as u64,
                    _ => 0,
                };
                queries.push(Query {
                    kind: QueryKind::from_sql(&row.sql),
                    sql: row.sql,
                    start_offset_us: offset,
                    duration_us: row.duration_us,
                    transaction_id: None,
                    response_values: None,
                    original_sql: None,
                });
            }

            assign_transaction_ids(&mut queries, &mut next_txn_id);
            total_queries += queries.len() as u64;

            if !queries.is_empty() {
                sessions.push(Session {
                    id: session_id,
                    user: String::new(),
                    database,
                    queries,
                });
            }
        }

        sessions.sort_by_key(|s| s.id);

        let capture_duration_us = match (global_min, global_max) {
            (Some(min), Some(max)) => (max - min).num_microseconds().unwrap_or(0) as u64,
            _ => 0,
        };
        let total_sessions = sessions.len() as u64;

        Ok(WorkloadProfile {
            version: 2,
            captured_at: Utc::now(),
            source_host: source_host.to_string(),
            pg_version: "unknown".to_string(),
            capture_method: "mssql_xevents".to_string(),
            sessions,
            metadata: Metadata {
                total_queries,
                total_sessions,
                capture_duration_us,
                sequence_snapshot: None,
                pk_map: None,
            },
            source_dialect: SourceDialect::SqlServer,
        })
    }
}

/// Stream-parse `<event>` elements out of the XML, regardless of the surrounding
/// wrapper (`<RingBufferTarget>`, a synthetic root, or none at all).
// `Attribute::unescape_value` is deprecated in favor of `normalized_value`, which
// requires picking an XmlVersion the source data never actually declares — not worth
// the extra ceremony for attribute values that are always plain numbers/identifiers here.
#[allow(deprecated)]
fn parse_events(reader: impl BufRead) -> Result<Vec<XEventRow>> {
    let mut xml = Reader::from_reader(reader);
    xml.config_mut().trim_text(true);

    let mut buf = Vec::new();
    let mut rows = Vec::new();

    let mut current: Option<EventBuilder> = None;
    // Name of the <data>/<action> field we're currently inside, if any.
    let mut current_field: Option<String> = None;
    // Whether we're inside that field's <value> or <text> child (where the text lives).
    let mut in_field_value = false;

    loop {
        match xml.read_event_into(&mut buf).context("malformed XML")? {
            XmlEvent::Start(e) | XmlEvent::Empty(e) => {
                let name = String::from_utf8_lossy(e.local_name().as_ref()).to_string();
                match name.as_str() {
                    "event" => {
                        let mut builder = EventBuilder::default();
                        for attr in e.attributes().flatten() {
                            match attr.key.local_name().as_ref() {
                                b"name" => {
                                    builder.name = attr.unescape_value()?.to_string();
                                }
                                b"timestamp" => {
                                    let ts = attr.unescape_value()?;
                                    builder.timestamp = ts.parse::<DateTime<Utc>>().ok();
                                }
                                _ => {}
                            }
                        }
                        current = Some(builder);
                    }
                    "data" | "action" => {
                        for attr in e.attributes().flatten() {
                            if attr.key.local_name().as_ref() == b"name" {
                                current_field = Some(attr.unescape_value()?.to_string());
                            }
                        }
                    }
                    "value" | "text" => in_field_value = true,
                    _ => {}
                }
            }
            XmlEvent::Text(e) => {
                if in_field_value {
                    if let (Some(builder), Some(field)) = (current.as_mut(), current_field.as_ref())
                    {
                        let decoded = e.decode().context("malformed XML text")?;
                        let text = quick_xml::escape::unescape(&decoded)
                            .context("malformed XML entity")?;
                        builder
                            .fields
                            .insert(field.clone(), text.trim().to_string());
                    }
                }
            }
            XmlEvent::End(e) => match e.local_name().as_ref() {
                b"value" | b"text" => in_field_value = false,
                b"data" | b"action" => current_field = None,
                b"event" => {
                    if let Some(builder) = current.take() {
                        if let Some(row) = finish_event(builder) {
                            rows.push(row);
                        }
                    }
                }
                _ => {}
            },
            XmlEvent::Eof => break,
            _ => {}
        }
        buf.clear();
    }

    Ok(rows)
}

fn finish_event(builder: EventBuilder) -> Option<XEventRow> {
    if !COMPLETED_EVENTS.contains(&builder.name.as_str()) {
        return None;
    }
    let sql = builder
        .fields
        .get("batch_text")
        .or_else(|| builder.fields.get("statement"))
        .cloned()
        .unwrap_or_default();
    if sql.is_empty() {
        return None;
    }
    let duration_us = builder
        .fields
        .get("duration")
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(0);
    let session_id = builder
        .fields
        .get("session_id")
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(0);
    let database = builder
        .fields
        .get("database_name")
        .cloned()
        .unwrap_or_default();

    Some(XEventRow {
        session_id,
        timestamp: builder.timestamp,
        duration_us,
        database,
        sql,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    const XML: &str = r#"<RingBufferTarget>
<event name="sql_batch_completed" package="sqlserver" timestamp="2026-03-08T10:00:00.100Z">
    <data name="duration"><value>1500</value></data>
    <data name="batch_text"><value>SELECT * FROM products WHERE id = 1</value></data>
    <action name="session_id" package="sqlserver"><value>52</value></action>
    <action name="database_name" package="sqlserver"><value>app</value></action>
</event>
<event name="rpc_completed" package="sqlserver" timestamp="2026-03-08T10:00:00.900Z">
    <data name="duration"><value>800</value></data>
    <data name="statement"><value>EXEC dbo.get_order 42</value></data>
    <action name="session_id" package="sqlserver"><value>52</value></action>
    <action name="database_name" package="sqlserver"><value>app</value></action>
</event>
<event name="login" package="sqlserver" timestamp="2026-03-08T10:00:00.050Z">
    <action name="session_id" package="sqlserver"><value>52</value></action>
</event>
</RingBufferTarget>"#;

    #[test]
    fn test_parses_completed_events_and_skips_others() {
        let p = MssqlXEventsCapture
            .capture_from_reader(Cursor::new(XML.as_bytes()), "mssql01")
            .unwrap();
        assert_eq!(p.source_dialect, SourceDialect::SqlServer);
        assert_eq!(p.capture_method, "mssql_xevents");
        // The "login" event has no duration/text and isn't in COMPLETED_EVENTS — skipped.
        assert_eq!(p.metadata.total_queries, 2);
        assert_eq!(p.metadata.total_sessions, 1);

        let session = &p.sessions[0];
        assert_eq!(session.id, 52);
        assert_eq!(session.database, "app");
        assert!(session.queries[0].sql.contains("SELECT"));
        assert_eq!(session.queries[0].duration_us, 1500);
        assert!(session.queries[1].sql.contains("get_order"));
        assert_eq!(session.queries[1].start_offset_us, 800_000);
    }

    #[test]
    fn test_bare_event_sequence_without_wrapper() {
        let xml = r#"<event name="sql_batch_completed" timestamp="2026-01-01T00:00:00Z">
            <data name="duration"><value>100</value></data>
            <data name="batch_text"><value>SELECT 1</value></data>
        </event>"#;
        let p = MssqlXEventsCapture
            .capture_from_reader(Cursor::new(xml.as_bytes()), "mssql01")
            .unwrap();
        assert_eq!(p.metadata.total_queries, 1);
    }

    #[test]
    fn test_empty_xml_produces_empty_profile() {
        let p = MssqlXEventsCapture
            .capture_from_reader(
                Cursor::new(b"<RingBufferTarget></RingBufferTarget>".as_slice()),
                "mssql01",
            )
            .unwrap();
        assert_eq!(p.metadata.total_queries, 0);
        assert!(p.sessions.is_empty());
    }
}
