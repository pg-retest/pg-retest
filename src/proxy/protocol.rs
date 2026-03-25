use anyhow::{bail, Result};
use bytes::{BufMut, BytesMut};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// A parsed PG protocol message frame.
#[derive(Debug, Clone)]
pub struct PgMessage {
    /// Message type byte. 0 for startup messages (no type byte).
    pub msg_type: u8,
    /// Complete message bytes (including length but NOT type byte).
    /// For startup messages, includes length + payload (no type byte).
    pub payload: BytesMut,
}

impl PgMessage {
    /// Total wire size of this message.
    pub fn wire_len(&self) -> usize {
        self.payload.len() + if self.msg_type != 0 { 1 } else { 0 }
    }

    /// Get the payload bytes (after the 4-byte length field).
    pub fn body(&self) -> &[u8] {
        &self.payload[4..]
    }
}

/// SSLRequest magic: length=8, code=80877103
const SSL_REQUEST_CODE: i32 = 80877103;
/// CancelRequest magic: length=16, code=80877102
const CANCEL_REQUEST_CODE: i32 = 80877102;
/// Protocol version 3.0: 196608
const PROTOCOL_VERSION_3: i32 = 196608;

/// Identifies what kind of startup-phase message this is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartupType {
    SslRequest,
    CancelRequest,
    StartupMessage,
    Unknown,
}

/// Read a single PG message from a stream (post-startup phase).
/// Returns None if the stream is closed (EOF).
pub async fn read_message<R: AsyncRead + Unpin>(stream: &mut R) -> Result<Option<PgMessage>> {
    // Read type byte
    let msg_type = match read_byte(stream).await? {
        Some(b) => b,
        None => return Ok(None),
    };

    // Read 4-byte length
    let mut len_buf = [0u8; 4];
    stream.read_exact(&mut len_buf).await?;
    let len = i32::from_be_bytes(len_buf) as usize;

    if len < 4 {
        bail!("Invalid message length: {len}");
    }

    // Read remaining payload (length includes the 4 length bytes)
    let body_len = len - 4;
    let mut payload = BytesMut::with_capacity(len);
    payload.put_slice(&len_buf);
    if body_len > 0 {
        payload.resize(len, 0);
        stream.read_exact(&mut payload[4..]).await?;
    }

    Ok(Some(PgMessage { msg_type, payload }))
}

/// Read a startup-phase message (no type byte — just length + payload).
/// Used for the first message from a client (StartupMessage, SSLRequest, CancelRequest).
pub async fn read_startup_message<R: AsyncRead + Unpin>(
    stream: &mut R,
) -> Result<Option<PgMessage>> {
    // Read 4-byte length
    let mut len_buf = [0u8; 4];
    match stream.read_exact(&mut len_buf).await {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e.into()),
    }
    let len = i32::from_be_bytes(len_buf) as usize;

    if len < 4 {
        bail!("Invalid startup message length: {len}");
    }

    let body_len = len - 4;
    let mut payload = BytesMut::with_capacity(len);
    payload.put_slice(&len_buf);
    if body_len > 0 {
        payload.resize(len, 0);
        stream.read_exact(&mut payload[4..]).await?;
    }

    Ok(Some(PgMessage {
        msg_type: 0,
        payload,
    }))
}

/// Write a PgMessage to a stream.
pub async fn write_message<W: AsyncWrite + Unpin>(stream: &mut W, msg: &PgMessage) -> Result<()> {
    if msg.msg_type != 0 {
        stream.write_all(&[msg.msg_type]).await?;
    }
    stream.write_all(&msg.payload).await?;
    Ok(())
}

/// Classify a startup-phase message by its protocol code.
pub fn classify_startup(msg: &PgMessage) -> StartupType {
    if msg.payload.len() < 8 {
        return StartupType::Unknown;
    }
    let code = i32::from_be_bytes([
        msg.payload[4],
        msg.payload[5],
        msg.payload[6],
        msg.payload[7],
    ]);
    match code {
        SSL_REQUEST_CODE => StartupType::SslRequest,
        CANCEL_REQUEST_CODE => StartupType::CancelRequest,
        PROTOCOL_VERSION_3 => StartupType::StartupMessage,
        _ => StartupType::Unknown,
    }
}

/// Extract user and database from a StartupMessage.
/// The body after the version field is a sequence of null-terminated key-value pairs.
pub fn parse_startup_params(msg: &PgMessage) -> (Option<String>, Option<String>) {
    let body = msg.body();
    if body.len() < 4 {
        return (None, None);
    }
    // Skip 4-byte version field
    let params = &body[4..];

    let mut user = None;
    let mut database = None;
    let mut iter = params.split(|&b| b == 0);

    loop {
        let key = match iter.next() {
            Some(k) if !k.is_empty() => k,
            _ => break,
        };
        let value = match iter.next() {
            Some(v) => v,
            None => break,
        };
        match key {
            b"user" => user = Some(String::from_utf8_lossy(value).into_owned()),
            b"database" => database = Some(String::from_utf8_lossy(value).into_owned()),
            _ => {}
        }
    }

    (user, database)
}

/// Read a single byte, returning None on EOF.
async fn read_byte<R: AsyncRead + Unpin>(stream: &mut R) -> Result<Option<u8>> {
    let mut buf = [0u8; 1];
    match stream.read_exact(&mut buf).await {
        Ok(_) => Ok(Some(buf[0])),
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => Ok(None),
        Err(e) => Err(e.into()),
    }
}

// ── Content extraction from specific message types ──────────────────

/// Extract SQL text from a Query ('Q') message.
/// Body is: SQL string followed by null terminator.
/// Build a Query ('Q') message from a SQL string.
pub fn build_query_message(sql: &str) -> PgMessage {
    let sql_bytes = sql.as_bytes();
    // Length = 4 (length field) + sql bytes + 1 (null terminator)
    let len = 4 + sql_bytes.len() + 1;
    let mut payload = BytesMut::with_capacity(len);
    payload.put_i32(len as i32);
    payload.put_slice(sql_bytes);
    payload.put_u8(0); // null terminator
    PgMessage {
        msg_type: b'Q',
        payload,
    }
}

pub fn extract_query_sql(msg: &PgMessage) -> Option<String> {
    if msg.msg_type != b'Q' {
        return None;
    }
    let body = msg.body();
    // Strip trailing null terminator
    let sql = if body.last() == Some(&0) {
        &body[..body.len() - 1]
    } else {
        body
    };
    Some(String::from_utf8_lossy(sql).into_owned())
}

/// Parsed Parse ('P') message: statement name + SQL text.
pub struct ParseMessage {
    pub statement_name: String,
    pub sql: String,
}

/// Extract statement name and SQL from a Parse ('P') message.
/// Body: name (null-terminated) + query (null-terminated) + param count (i16) + param OIDs.
pub fn extract_parse(msg: &PgMessage) -> Option<ParseMessage> {
    if msg.msg_type != b'P' {
        return None;
    }
    let body = msg.body();
    let name_end = body.iter().position(|&b| b == 0)?;
    let name = String::from_utf8_lossy(&body[..name_end]).into_owned();
    let rest = &body[name_end + 1..];
    let sql_end = rest.iter().position(|&b| b == 0)?;
    let sql = String::from_utf8_lossy(&rest[..sql_end]).into_owned();
    Some(ParseMessage {
        statement_name: name,
        sql,
    })
}

/// Parsed Bind ('B') message: portal name, statement name, parameter values.
pub struct BindMessage {
    pub portal_name: String,
    pub statement_name: String,
    pub parameters: Vec<Option<Vec<u8>>>,
}

/// Extract portal name, statement name, and parameters from a Bind ('B') message.
/// Body: portal (null-term) + stmt (null-term) + format_count (i16) + formats
///       + param_count (i16) + params (len + data each, -1 for NULL).
pub fn extract_bind(msg: &PgMessage) -> Option<BindMessage> {
    if msg.msg_type != b'B' {
        return None;
    }
    let body = msg.body();
    let mut pos = 0;

    // Portal name (null-terminated)
    let portal_end = body[pos..].iter().position(|&b| b == 0)?;
    let portal_name = String::from_utf8_lossy(&body[pos..pos + portal_end]).into_owned();
    pos += portal_end + 1;

    // Statement name (null-terminated)
    let stmt_end = body[pos..].iter().position(|&b| b == 0)?;
    let statement_name = String::from_utf8_lossy(&body[pos..pos + stmt_end]).into_owned();
    pos += stmt_end + 1;

    // Format codes count (i16) + skip format codes
    if pos + 2 > body.len() {
        return None;
    }
    let format_count = i16::from_be_bytes([body[pos], body[pos + 1]]) as usize;
    pos += 2 + format_count * 2;

    // Parameter count (i16)
    if pos + 2 > body.len() {
        return None;
    }
    let param_count = i16::from_be_bytes([body[pos], body[pos + 1]]) as usize;
    pos += 2;

    let mut parameters = Vec::with_capacity(param_count);
    for _ in 0..param_count {
        if pos + 4 > body.len() {
            break;
        }
        let param_len =
            i32::from_be_bytes([body[pos], body[pos + 1], body[pos + 2], body[pos + 3]]);
        pos += 4;
        if param_len == -1 {
            parameters.push(None); // NULL
        } else {
            let len = param_len as usize;
            if pos + len > body.len() {
                break;
            }
            parameters.push(Some(body[pos..pos + len].to_vec()));
            pos += len;
        }
    }

    Some(BindMessage {
        portal_name,
        statement_name,
        parameters,
    })
}

/// Extract the command tag from a CommandComplete ('C') message.
/// Body: tag string (null-terminated), e.g. "SELECT 5", "INSERT 0 1".
pub fn extract_command_complete(msg: &PgMessage) -> Option<String> {
    if msg.msg_type != b'C' {
        return None;
    }
    let body = msg.body();
    let end = body.iter().position(|&b| b == 0).unwrap_or(body.len());
    Some(String::from_utf8_lossy(&body[..end]).into_owned())
}

/// Extract the transaction state from a ReadyForQuery ('Z') message.
/// Body: single byte — 'I' (idle), 'T' (in transaction), 'E' (failed transaction).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TxnState {
    Idle,
    InTransaction,
    Failed,
}

pub fn extract_ready_for_query(msg: &PgMessage) -> Option<TxnState> {
    if msg.msg_type != b'Z' {
        return None;
    }
    let body = msg.body();
    if body.is_empty() {
        return None;
    }
    match body[0] {
        b'I' => Some(TxnState::Idle),
        b'T' => Some(TxnState::InTransaction),
        b'E' => Some(TxnState::Failed),
        _ => None,
    }
}

/// Extract error message from an ErrorResponse ('E') message.
/// Body: sequence of (type byte + null-terminated string) pairs, terminated by 0.
/// We extract the 'M' (message) field.
pub fn extract_error_message(msg: &PgMessage) -> Option<String> {
    if msg.msg_type != b'E' {
        return None;
    }
    let body = msg.body();
    let mut pos = 0;
    while pos < body.len() {
        let field_type = body[pos];
        pos += 1;
        if field_type == 0 {
            break;
        }
        let end = body[pos..].iter().position(|&b| b == 0)?;
        let value = &body[pos..pos + end];
        pos += end + 1;
        if field_type == b'M' {
            return Some(String::from_utf8_lossy(value).into_owned());
        }
    }
    None
}

/// Extract the PID and secret key from a BackendKeyData ('K') message.
pub fn extract_backend_key_data(msg: &PgMessage) -> Option<(i32, i32)> {
    if msg.msg_type != b'K' {
        return None;
    }
    let body = msg.body();
    if body.len() < 8 {
        return None;
    }
    let pid = i32::from_be_bytes([body[0], body[1], body[2], body[3]]);
    let secret = i32::from_be_bytes([body[4], body[5], body[6], body[7]]);
    Some((pid, secret))
}

/// Parse RowDescription ('T') message — returns column names.
/// Body: Int16 (num columns), then per column:
///   String (name\0), Int32 (table OID), Int16 (col num), Int32 (type OID),
///   Int16 (type size), Int32 (type mod), Int16 (format code) = 18 bytes after name
pub fn extract_row_description(msg: &PgMessage) -> Option<Vec<String>> {
    if msg.msg_type != b'T' {
        return None;
    }
    let body = msg.body();
    if body.len() < 2 {
        return None;
    }
    let num_cols = u16::from_be_bytes([body[0], body[1]]) as usize;
    let mut columns = Vec::with_capacity(num_cols);
    let mut pos = 2;
    for _ in 0..num_cols {
        let name_end = body[pos..].iter().position(|&b| b == 0)?;
        let name = String::from_utf8_lossy(&body[pos..pos + name_end]).into_owned();
        pos += name_end + 1;
        pos += 18; // skip table_oid(4) + col_num(2) + type_oid(4) + type_size(2) + type_mod(4) + format(2)
        if pos > body.len() {
            return None;
        }
        columns.push(name);
    }
    Some(columns)
}

/// Parse DataRow ('D') message — returns column values as text strings.
/// Body: Int16 (num columns), then per column: Int32 (length, -1=NULL), bytes
pub fn extract_data_row(msg: &PgMessage, num_columns: usize) -> Option<Vec<Option<String>>> {
    if msg.msg_type != b'D' {
        return None;
    }
    let body = msg.body();
    if body.len() < 2 {
        return None;
    }
    let num_cols = u16::from_be_bytes([body[0], body[1]]) as usize;
    if num_cols != num_columns {
        return None;
    }
    let mut values = Vec::with_capacity(num_cols);
    let mut pos = 2;
    for _ in 0..num_cols {
        if pos + 4 > body.len() {
            return None;
        }
        let len = i32::from_be_bytes([body[pos], body[pos + 1], body[pos + 2], body[pos + 3]]);
        pos += 4;
        if len == -1 {
            values.push(None);
        } else {
            let len = len as usize;
            if pos + len > body.len() {
                return None;
            }
            values.push(Some(
                String::from_utf8_lossy(&body[pos..pos + len]).into_owned(),
            ));
            pos += len;
        }
    }
    Some(values)
}

/// Format parameter values from a Bind message as strings for capture.
/// Text parameters are converted to strings; binary params shown as description.
/// NULL parameters become the string "NULL".
pub fn format_bind_params(params: &[Option<Vec<u8>>]) -> Vec<String> {
    params
        .iter()
        .map(|p| match p {
            None => "NULL".to_string(),
            Some(bytes) => match std::str::from_utf8(bytes) {
                Ok(s) => format!("'{}'", s.replace('\'', "''")),
                Err(_) => format!("'<binary {} bytes>'", bytes.len()),
            },
        })
        .collect()
}

/// Inline bind parameters into a SQL template, replacing $1, $2, etc.
pub fn inline_bind_params(sql: &str, params: &[String]) -> String {
    let mut result = sql.to_string();
    // Replace in reverse order ($10 before $1)
    for (i, value) in params.iter().enumerate().rev() {
        let placeholder = format!("${}", i + 1);
        result = result.replace(&placeholder, value);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn make_message(msg_type: u8, body: &[u8]) -> Vec<u8> {
        let len = (body.len() + 4) as i32;
        let mut buf = Vec::new();
        buf.push(msg_type);
        buf.extend_from_slice(&len.to_be_bytes());
        buf.extend_from_slice(body);
        buf
    }

    #[tokio::test]
    async fn test_read_message_query() {
        let sql = b"SELECT 1\0";
        let wire = make_message(b'Q', sql);
        let mut cursor = Cursor::new(wire);
        let msg = read_message(&mut cursor).await.unwrap().unwrap();
        assert_eq!(msg.msg_type, b'Q');
        assert_eq!(msg.body(), sql);
    }

    #[tokio::test]
    async fn test_read_message_eof() {
        let mut cursor = Cursor::new(Vec::<u8>::new());
        let msg = read_message(&mut cursor).await.unwrap();
        assert!(msg.is_none());
    }

    #[tokio::test]
    async fn test_read_startup_message_ssl_request() {
        let mut buf = Vec::new();
        buf.extend_from_slice(&8i32.to_be_bytes()); // length = 8
        buf.extend_from_slice(&SSL_REQUEST_CODE.to_be_bytes());
        let mut cursor = Cursor::new(buf);
        let msg = read_startup_message(&mut cursor).await.unwrap().unwrap();
        assert_eq!(msg.msg_type, 0);
        assert_eq!(classify_startup(&msg), StartupType::SslRequest);
    }

    #[tokio::test]
    async fn test_read_startup_message_v3() {
        let mut buf = Vec::new();
        let params = b"user\0app\0database\0mydb\0\0";
        let total_len = (4 + 4 + params.len()) as i32;
        buf.extend_from_slice(&total_len.to_be_bytes());
        buf.extend_from_slice(&PROTOCOL_VERSION_3.to_be_bytes());
        buf.extend_from_slice(params);
        let mut cursor = Cursor::new(buf);
        let msg = read_startup_message(&mut cursor).await.unwrap().unwrap();
        assert_eq!(classify_startup(&msg), StartupType::StartupMessage);
        let (user, db) = parse_startup_params(&msg);
        assert_eq!(user.as_deref(), Some("app"));
        assert_eq!(db.as_deref(), Some("mydb"));
    }

    #[tokio::test]
    async fn test_write_message_roundtrip() {
        let sql = b"SELECT 1\0";
        let wire = make_message(b'Q', sql);
        let mut cursor = Cursor::new(wire);
        let msg = read_message(&mut cursor).await.unwrap().unwrap();

        let mut output = Vec::new();
        write_message(&mut output, &msg).await.unwrap();

        // Re-read from output
        let mut cursor2 = Cursor::new(output);
        let msg2 = read_message(&mut cursor2).await.unwrap().unwrap();
        assert_eq!(msg2.msg_type, b'Q');
        assert_eq!(msg2.body(), sql);
    }

    #[tokio::test]
    async fn test_read_startup_eof() {
        let mut cursor = Cursor::new(Vec::<u8>::new());
        let msg = read_startup_message(&mut cursor).await.unwrap();
        assert!(msg.is_none());
    }

    #[test]
    fn test_extract_query_sql() {
        let body = b"SELECT * FROM users\0";
        let msg = PgMessage {
            msg_type: b'Q',
            payload: {
                let len = (body.len() + 4) as i32;
                let mut p = BytesMut::new();
                p.put_slice(&len.to_be_bytes());
                p.put_slice(body);
                p
            },
        };
        assert_eq!(extract_query_sql(&msg).unwrap(), "SELECT * FROM users");
    }

    #[test]
    fn test_extract_parse() {
        let mut body = Vec::new();
        body.extend_from_slice(b"stmt1\0");
        body.extend_from_slice(b"SELECT * FROM t WHERE id = $1\0");
        body.extend_from_slice(&0i16.to_be_bytes());
        let msg = PgMessage {
            msg_type: b'P',
            payload: {
                let len = (body.len() + 4) as i32;
                let mut p = BytesMut::new();
                p.put_slice(&len.to_be_bytes());
                p.put_slice(&body);
                p
            },
        };
        let parsed = extract_parse(&msg).unwrap();
        assert_eq!(parsed.statement_name, "stmt1");
        assert_eq!(parsed.sql, "SELECT * FROM t WHERE id = $1");
    }

    #[test]
    fn test_extract_bind() {
        let mut body = Vec::new();
        body.extend_from_slice(b"\0"); // portal name (empty)
        body.extend_from_slice(b"stmt1\0"); // statement name
        body.extend_from_slice(&0i16.to_be_bytes()); // 0 format codes
        body.extend_from_slice(&2i16.to_be_bytes()); // 2 parameters
        body.extend_from_slice(&2i32.to_be_bytes()); // Param 1: "42" (len=2)
        body.extend_from_slice(b"42");
        body.extend_from_slice(&(-1i32).to_be_bytes()); // Param 2: NULL
        let msg = PgMessage {
            msg_type: b'B',
            payload: {
                let len = (body.len() + 4) as i32;
                let mut p = BytesMut::new();
                p.put_slice(&len.to_be_bytes());
                p.put_slice(&body);
                p
            },
        };
        let bind = extract_bind(&msg).unwrap();
        assert_eq!(bind.statement_name, "stmt1");
        assert_eq!(bind.parameters.len(), 2);
        assert_eq!(bind.parameters[0].as_deref(), Some(b"42".as_slice()));
        assert!(bind.parameters[1].is_none());
    }

    #[test]
    fn test_extract_command_complete() {
        let body = b"SELECT 5\0";
        let msg = PgMessage {
            msg_type: b'C',
            payload: {
                let len = (body.len() + 4) as i32;
                let mut p = BytesMut::new();
                p.put_slice(&len.to_be_bytes());
                p.put_slice(body);
                p
            },
        };
        assert_eq!(extract_command_complete(&msg).unwrap(), "SELECT 5");
    }

    #[test]
    fn test_extract_ready_for_query() {
        let msg = PgMessage {
            msg_type: b'Z',
            payload: {
                let mut p = BytesMut::new();
                p.put_slice(&5i32.to_be_bytes());
                p.put_u8(b'I');
                p
            },
        };
        assert_eq!(extract_ready_for_query(&msg).unwrap(), TxnState::Idle);
    }

    #[test]
    fn test_extract_error_message() {
        let mut body = Vec::new();
        body.push(b'S');
        body.extend_from_slice(b"ERROR\0");
        body.push(b'M');
        body.extend_from_slice(b"relation \"foo\" does not exist\0");
        body.push(0);
        let msg = PgMessage {
            msg_type: b'E',
            payload: {
                let len = (body.len() + 4) as i32;
                let mut p = BytesMut::new();
                p.put_slice(&len.to_be_bytes());
                p.put_slice(&body);
                p
            },
        };
        assert_eq!(
            extract_error_message(&msg).unwrap(),
            "relation \"foo\" does not exist"
        );
    }

    #[test]
    fn test_inline_bind_params() {
        let sql = "SELECT * FROM users WHERE id = $1 AND name = $2";
        let params = vec!["42".to_string(), "'alice'".to_string()];
        let result = inline_bind_params(sql, &params);
        assert_eq!(
            result,
            "SELECT * FROM users WHERE id = 42 AND name = 'alice'"
        );
    }

    #[test]
    fn test_format_bind_params() {
        let params = vec![Some(b"hello".to_vec()), None, Some(b"42".to_vec())];
        let formatted = format_bind_params(&params);
        assert_eq!(formatted[0], "'hello'");
        assert_eq!(formatted[1], "NULL");
        assert_eq!(formatted[2], "'42'");
    }

    /// Helper to build a RowDescription column entry (name + 18 bytes of field metadata).
    fn row_desc_column(name: &str) -> Vec<u8> {
        let mut col = Vec::new();
        col.extend_from_slice(name.as_bytes());
        col.push(0); // null terminator
        col.extend_from_slice(&[0u8; 18]); // table_oid(4) + col_num(2) + type_oid(4) + type_size(2) + type_mod(4) + format(2)
        col
    }

    /// Helper to build a PgMessage from type byte and body bytes.
    fn make_pg_message(msg_type: u8, body: &[u8]) -> PgMessage {
        let len = (body.len() + 4) as i32;
        let mut p = BytesMut::new();
        p.put_slice(&len.to_be_bytes());
        p.put_slice(body);
        PgMessage {
            msg_type,
            payload: p,
        }
    }

    #[test]
    fn test_extract_row_description_single_column() {
        let mut body = Vec::new();
        body.extend_from_slice(&1u16.to_be_bytes()); // 1 column
        body.extend_from_slice(&row_desc_column("id"));
        let msg = make_pg_message(b'T', &body);
        let cols = extract_row_description(&msg).unwrap();
        assert_eq!(cols, vec!["id"]);
    }

    #[test]
    fn test_extract_row_description_two_columns() {
        let mut body = Vec::new();
        body.extend_from_slice(&2u16.to_be_bytes()); // 2 columns
        body.extend_from_slice(&row_desc_column("id"));
        body.extend_from_slice(&row_desc_column("name"));
        let msg = make_pg_message(b'T', &body);
        let cols = extract_row_description(&msg).unwrap();
        assert_eq!(cols, vec!["id", "name"]);
    }

    #[test]
    fn test_extract_data_row_single_value() {
        let mut body = Vec::new();
        body.extend_from_slice(&1u16.to_be_bytes()); // 1 column
        body.extend_from_slice(&2i32.to_be_bytes()); // length 2
        body.extend_from_slice(b"42");
        let msg = make_pg_message(b'D', &body);
        let values = extract_data_row(&msg, 1).unwrap();
        assert_eq!(values, vec![Some("42".to_string())]);
    }

    #[test]
    fn test_extract_data_row_null_value() {
        let mut body = Vec::new();
        body.extend_from_slice(&1u16.to_be_bytes()); // 1 column
        body.extend_from_slice(&(-1i32).to_be_bytes()); // NULL
        let msg = make_pg_message(b'D', &body);
        let values = extract_data_row(&msg, 1).unwrap();
        assert_eq!(values, vec![None]);
    }

    #[test]
    fn test_extract_data_row_wrong_type() {
        let msg = make_pg_message(b'Q', b"SELECT 1\0");
        assert!(extract_data_row(&msg, 1).is_none());
    }
}
