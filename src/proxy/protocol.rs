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
}
