//! `GoldenOracle`: truth = the result of an author-verified reference PG query, run on
//! the same seeded PostgreSQL. A candidate is `Equivalent` iff it executes and returns
//! the same canonical rows as the reference. This proves *behavior*, not just syntax —
//! the live counterpart to the transformer's `pg_query` parse gate.

use tokio_postgres::{Client, NoTls};

use super::normalize::{is_ordered, rows_equivalent};
use super::{Oracle, Verdict};
use async_trait::async_trait;

/// Default connection target; override with `PG_RETEST_ORACLE_URL`.
pub const DEFAULT_CONN: &str = "host=localhost port=5441 user=oracle password=oracle dbname=oracle";

/// The connection string the oracle should use (`PG_RETEST_ORACLE_URL` or the default).
pub fn conn_str() -> String {
    std::env::var("PG_RETEST_ORACLE_URL").unwrap_or_else(|_| DEFAULT_CONN.to_string())
}

pub struct GoldenOracle {
    client: Client,
}

impl GoldenOracle {
    /// Connect and spawn the connection driver. Returns `Err` if PG is unreachable
    /// (callers skip the test/benchmark cleanly in that case).
    pub async fn connect(conn: &str) -> Result<Self, tokio_postgres::Error> {
        let (client, connection) = tokio_postgres::connect(conn, NoTls).await?;
        tokio::spawn(async move {
            let _ = connection.await;
        });
        Ok(Self { client })
    }

    /// Run arbitrary SQL (e.g. the seed) ignoring results.
    pub async fn batch(&self, sql: &str) -> Result<(), tokio_postgres::Error> {
        self.client.batch_execute(sql).await
    }

    /// Execute `sql` and return each row as PostgreSQL's own record-text (positional,
    /// type- and column-name-agnostic). `Err` on any execution failure.
    async fn canonical_rows(&self, sql: &str) -> Result<Vec<String>, String> {
        // Wrap so PG renders the whole row to text: SELECT _s::text FROM ( <sql> ) _s.
        let wrapped = format!("SELECT _s::text AS r FROM ( {sql} ) AS _s");
        let rows = self
            .client
            .query(&wrapped, &[])
            .await
            .map_err(|e| e.to_string())?;
        rows.iter()
            .map(|row| {
                row.try_get::<_, Option<String>>("r")
                    .map(|opt| opt.unwrap_or_else(|| "<NULL ROW>".into()))
                    .map_err(|e| e.to_string())
            })
            .collect()
    }
}

#[async_trait]
impl Oracle for GoldenOracle {
    async fn verify(&self, candidate_sql: &str, reference_sql: &str) -> Verdict {
        let reference = match self.canonical_rows(reference_sql).await {
            Ok(r) => r,
            // A broken reference is a corpus bug — surface it, don't hide it.
            Err(e) => {
                return Verdict::Error {
                    detail: format!("reference failed: {e}"),
                }
            }
        };
        let candidate = match self.canonical_rows(candidate_sql).await {
            Ok(r) => r,
            Err(e) => return Verdict::Error { detail: e },
        };
        let ordered = is_ordered(candidate_sql) && is_ordered(reference_sql);
        if rows_equivalent(&candidate, &reference, ordered) {
            Verdict::Equivalent
        } else {
            Verdict::Divergent {
                detail: format!(
                    "{} candidate rows vs {} reference rows",
                    candidate.len(),
                    reference.len()
                ),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn oracle() -> Option<GoldenOracle> {
        match GoldenOracle::connect(&conn_str()).await {
            Ok(o) => Some(o),
            Err(_) => {
                eprintln!("SKIP: PostgreSQL not available at {}", conn_str());
                None
            }
        }
    }

    #[tokio::test]
    async fn test_equivalent_when_results_match() {
        let Some(o) = oracle().await else { return };
        // Syntactically different, result-identical.
        assert_eq!(
            o.verify("SELECT 1 AS x", "SELECT 1 AS y").await,
            Verdict::Equivalent
        );
    }

    #[tokio::test]
    async fn test_divergent_when_results_differ() {
        let Some(o) = oracle().await else { return };
        assert!(matches!(
            o.verify("SELECT 1", "SELECT 2").await,
            Verdict::Divergent { .. }
        ));
    }

    #[tokio::test]
    async fn test_error_when_candidate_fails_to_execute() {
        let Some(o) = oracle().await else { return };
        assert!(matches!(
            o.verify("SELECT bogus_fn_xyz()", "SELECT 1").await,
            Verdict::Error { .. }
        ));
    }

    #[tokio::test]
    async fn test_string_literal_corruption_is_divergent() {
        let Some(o) = oracle().await else { return };
        // The headline behaviorally: regex would turn 'IF(x,1,0)' into 'CASE WHEN...'.
        assert!(matches!(
            o.verify(
                "SELECT 'CASE WHEN x THEN 1 ELSE 0 END' AS lit",
                "SELECT 'IF(x,1,0)' AS lit",
            )
            .await,
            Verdict::Divergent { .. }
        ));
    }
}
