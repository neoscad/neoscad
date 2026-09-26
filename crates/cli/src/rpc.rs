//! JSON-RPC 2.0 messages with LSP's framing (`docs/serve-protocol.md`):
//! each message is `Content-Length: N\r\n\r\n` and N bytes of UTF-8 JSON.
//! Other header fields are allowed and ignored, as LSP allows them.

use std::io::{self, BufRead, Write};

use serde_json::{Value, json};

/// The largest message accepted: a request is a few kilobytes, a file's
/// full text at most a few megabytes.
const MAX_MESSAGE: usize = 64 << 20;

/// Read one message; `None` at the end of the stream. A body that is not
/// JSON is `ErrorKind::InvalidData` (the stream is still in step: the
/// next message can be read); a broken header is `InvalidInput` (it is
/// not, and the connection should end).
pub fn read(r: &mut impl BufRead) -> io::Result<Option<Value>> {
    let mut len: Option<usize> = None;
    let mut line = String::new();
    loop {
        line.clear();
        if r.read_line(&mut line)? == 0 {
            return if len.is_none() {
                Ok(None)
            } else {
                Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "end of stream in a header",
                ))
            };
        }
        let l = line.trim_end_matches(['\r', '\n']);
        if l.is_empty() {
            if len.is_some() {
                break;
            }
            // Blank lines between messages are tolerated.
            continue;
        }
        if let Some((k, v)) = l.split_once(':')
            && k.trim().eq_ignore_ascii_case("content-length")
        {
            let n: usize = v.trim().parse().map_err(|_| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("bad Content-Length '{}'", v.trim()),
                )
            })?;
            if n > MAX_MESSAGE {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("message of {n} bytes is over the {MAX_MESSAGE}-byte limit"),
                ));
            }
            len = Some(n);
        }
    }
    let mut buf = vec![0; len.unwrap_or(0)];
    r.read_exact(&mut buf)?;
    serde_json::from_slice(&buf)
        .map(Some)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("invalid JSON: {e}")))
}

/// Write one message.
pub fn write(w: &mut impl Write, msg: &Value) -> io::Result<()> {
    let body = serde_json::to_vec(msg).expect("a JSON value serialises");
    write!(w, "Content-Length: {}\r\n\r\n", body.len())?;
    w.write_all(&body)?;
    w.flush()
}

pub fn request(id: u64, method: &str, params: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})
}

pub fn notification(method: &str, params: Value) -> Value {
    json!({"jsonrpc": "2.0", "method": method, "params": params})
}

pub fn response(id: &Value, result: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "result": result})
}

/// JSON-RPC's error codes, and the protocol's own (`docs/serve-protocol.md`).
pub mod code {
    pub const PARSE_ERROR: i64 = -32700;
    pub const INVALID_REQUEST: i64 = -32600;
    pub const METHOD_NOT_FOUND: i64 = -32601;
    pub const INVALID_PARAMS: i64 = -32602;
    pub const INTERNAL_ERROR: i64 = -32603;
    /// LSP's `RequestCancelled`: a newer request on the same document, or
    /// `cancel`, stopped this one.
    pub const CANCELLED: i64 = -32800;
    /// The operation ran and failed in a way that is not the model's (no
    /// GPU, an output that cannot be written).
    pub const FAILED: i64 = -32001;
}

pub fn error(id: &Value, code: i64, message: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_round_trip_through_the_framing() {
        let mut buf = Vec::new();
        let a = request(1, "render", json!({"input": "a.scad", "text": "é\r\n"}));
        let b = notification("progress", json!({"id": 1}));
        write(&mut buf, &a).unwrap();
        write(&mut buf, &b).unwrap();
        let mut r = std::io::Cursor::new(buf);
        assert_eq!(read(&mut r).unwrap(), Some(a));
        assert_eq!(read(&mut r).unwrap(), Some(b));
        assert_eq!(read(&mut r).unwrap(), None);
    }

    #[test]
    fn extra_headers_are_ignored_and_bad_lengths_rejected() {
        let body = br#"{"jsonrpc":"2.0","id":7,"method":"stats"}"#;
        let mut m = format!(
            "Content-Type: application/vscode-jsonrpc; charset=utf-8\r\ncontent-length: {}\r\n\r\n",
            body.len()
        )
        .into_bytes();
        m.extend_from_slice(body);
        let v = read(&mut std::io::Cursor::new(m)).unwrap().unwrap();
        assert_eq!(v["id"], 7);
        let bad = b"Content-Length: x\r\n\r\n{}".to_vec();
        assert_eq!(
            read(&mut std::io::Cursor::new(bad)).unwrap_err().kind(),
            std::io::ErrorKind::InvalidInput
        );
        // A body that is not JSON leaves the stream readable.
        let mut two = b"Content-Length: 1\r\n\r\n{".to_vec();
        write(&mut two, &json!(1)).unwrap();
        let mut r = std::io::Cursor::new(two);
        assert_eq!(
            read(&mut r).unwrap_err().kind(),
            std::io::ErrorKind::InvalidData
        );
        assert_eq!(read(&mut r).unwrap(), Some(json!(1)));
    }
}
