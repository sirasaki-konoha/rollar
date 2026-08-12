use std::io::{self, BufRead, Write};

use serde_json::Value;

/// Read one Content-Length framed JSON-RPC message.
pub fn read_message(reader: &mut impl BufRead) -> io::Result<Option<Value>> {
    let mut content_length = None;
    loop {
        let mut header = String::new();
        let bytes = reader.read_line(&mut header)?;
        if bytes == 0 {
            return Ok(None);
        }
        if header == "\r\n" || header == "\n" {
            break;
        }
        let Some((name, value)) = header.split_once(':') else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "malformed LSP header",
            ));
        };
        if name.eq_ignore_ascii_case("Content-Length") {
            content_length = Some(value.trim().parse::<usize>().map_err(|_| {
                io::Error::new(io::ErrorKind::InvalidData, "invalid Content-Length")
            })?);
        }
    }

    let length = content_length.ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidData, "missing Content-Length header")
    })?;
    let mut body = vec![0; length];
    reader.read_exact(&mut body)?;
    serde_json::from_slice(&body)
        .map(Some)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

/// Write one Content-Length framed JSON-RPC message.
pub fn write_message(writer: &mut impl Write, message: &Value) -> io::Result<()> {
    let body = serde_json::to_vec(message).map_err(io::Error::other)?;
    write!(writer, "Content-Length: {}\r\n\r\n", body.len())?;
    writer.write_all(&body)?;
    writer.flush()
}

#[cfg(test)]
mod tests {
    use std::io::{BufReader, Cursor};

    use serde_json::json;

    use super::*;

    #[test]
    fn framed_message_round_trip() {
        let expected = json!({"jsonrpc": "2.0", "id": 1, "method": "initialize"});
        let mut bytes = Vec::new();
        write_message(&mut bytes, &expected).unwrap();
        let mut reader = BufReader::new(Cursor::new(bytes));
        assert_eq!(read_message(&mut reader).unwrap(), Some(expected));
    }

    #[test]
    fn eof_before_a_header_is_clean_shutdown() {
        let mut reader = BufReader::new(Cursor::new(Vec::<u8>::new()));
        assert_eq!(read_message(&mut reader).unwrap(), None);
    }
}
