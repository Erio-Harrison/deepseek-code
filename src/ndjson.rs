//! Newline-delimited JSON codec for the stdio protocol.

use serde::Serialize;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};

/// Reads one line (without the trailing `\n`/`\r\n`) from an async buffered reader.
/// Returns `Ok(None)` on EOF.
pub async fn read_line<R>(reader: &mut BufReader<R>) -> std::io::Result<Option<String>>
where
    R: AsyncRead + Unpin,
{
    let mut line = String::new();
    let n = reader.read_line(&mut line).await?;
    if n == 0 {
        return Ok(None);
    }
    if line.ends_with('\n') {
        line.pop();
        if line.ends_with('\r') {
            line.pop();
        }
    }
    Ok(Some(line))
}

/// Writes one JSON line. U+2028/U+2029 are escaped, since some line readers
/// split on them and `serde_json` leaves them raw.
pub async fn write_line<W, T>(writer: &mut W, value: &T) -> anyhow::Result<()>
where
    W: AsyncWrite + Unpin,
    T: Serialize,
{
    let json = serde_json::to_string(value)?;
    let mut line = escape_line_separators(&json).into_bytes();
    line.push(b'\n');
    // One write, so cancellation can't leave a line without its newline.
    writer.write_all(&line).await?;
    writer.flush().await?;
    Ok(())
}

fn escape_line_separators(s: &str) -> String {
    if !s.contains('\u{2028}') && !s.contains('\u{2029}') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\u{2028}' => out.push_str("\\u2028"),
            '\u{2029}' => out.push_str("\\u2029"),
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn read_line_strips_newline() {
        let data = b"hello\nworld\n".to_vec();
        let mut reader = BufReader::new(&data[..]);
        assert_eq!(
            read_line(&mut reader).await.unwrap().as_deref(),
            Some("hello")
        );
        assert_eq!(
            read_line(&mut reader).await.unwrap().as_deref(),
            Some("world")
        );
        assert_eq!(read_line(&mut reader).await.unwrap(), None);
    }

    #[tokio::test]
    async fn read_line_strips_crlf() {
        let data = b"hello\r\n".to_vec();
        let mut reader = BufReader::new(&data[..]);
        assert_eq!(
            read_line(&mut reader).await.unwrap().as_deref(),
            Some("hello")
        );
    }

    #[tokio::test]
    async fn write_line_round_trips_plain_json() {
        let mut buf: Vec<u8> = Vec::new();
        write_line(&mut buf, &json!({"a": 1})).await.unwrap();
        let s = String::from_utf8(buf).unwrap();
        assert_eq!(s, "{\"a\":1}\n");
    }

    #[tokio::test]
    async fn write_line_escapes_u2028_and_u2029() {
        let mut buf: Vec<u8> = Vec::new();
        let value = json!({"text": "line1\u{2028}line2\u{2029}line3"});
        write_line(&mut buf, &value).await.unwrap();
        let s = String::from_utf8(buf).unwrap();

        // The raw separators must not appear in the emitted line...
        assert!(!s.contains('\u{2028}'));
        assert!(!s.contains('\u{2029}'));
        // ...but their escaped forms must, and the line must still be valid JSON
        // whose content round-trips back to the original text.
        assert!(s.contains("\\u2028"));
        assert!(s.contains("\\u2029"));
        let reparsed: serde_json::Value = serde_json::from_str(s.trim_end()).unwrap();
        assert_eq!(reparsed["text"], "line1\u{2028}line2\u{2029}line3");
    }
}
