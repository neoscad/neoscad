//! The link's framing: one JSON message per line (`\n`), both ways, as
//! MCP's stdio binding frames its messages. `serde_json` never writes a
//! raw newline inside a message (it escapes them in strings), so a line
//! is always one whole message.

use std::io::{self, BufRead, Write};

use serde_json::Value;

/// The longest message either side accepts: a capture's PNG in base64 with
/// room to spare, as the web bridge's bound. A peer never needs more, and
/// a bound keeps a broken one from growing the other process.
pub const MAX_MESSAGE: usize = 32 << 20;

/// The next message, `None` at the end of the stream. A line longer than
/// [`MAX_MESSAGE`] or one that is not JSON is an error (the connection is
/// then dropped: the peer is broken or not ours); blank lines are skipped.
pub fn read_message(r: &mut impl BufRead) -> io::Result<Option<Value>> {
    let mut line = Vec::new();
    loop {
        let (found, used) = {
            let available = match r.fill_buf() {
                Ok(a) => a,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e),
            };
            if available.is_empty() {
                return if line.iter().all(u8::is_ascii_whitespace) {
                    Ok(None)
                } else {
                    Err(io::ErrorKind::UnexpectedEof.into())
                };
            }
            match available.iter().position(|&b| b == b'\n') {
                Some(i) => {
                    line.extend_from_slice(&available[..i]);
                    (true, i + 1)
                }
                None => {
                    line.extend_from_slice(available);
                    (false, available.len())
                }
            }
        };
        r.consume(used);
        if line.len() > MAX_MESSAGE {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "a message is longer than 32 MiB",
            ));
        }
        if found {
            if line.iter().all(u8::is_ascii_whitespace) {
                line.clear();
                continue;
            }
            return serde_json::from_slice(&line)
                .map(Some)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e));
        }
    }
}

/// Write `msg` as one line and flush it.
pub fn write_message(w: &mut (impl Write + ?Sized), msg: &Value) -> io::Result<()> {
    let mut bytes = serde_json::to_vec(msg).map_err(io::Error::other)?;
    bytes.push(b'\n');
    w.write_all(&bytes)?;
    w.flush()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn messages_round_trip_one_per_line() {
        let mut buf = Vec::new();
        write_message(&mut buf, &json!({"text": "a\nb"})).unwrap();
        write_message(&mut buf, &json!({"id": 2})).unwrap();
        assert_eq!(buf.iter().filter(|&&b| b == b'\n').count(), 2);
        buf.extend_from_slice(b"\n  \n");
        let mut r = io::BufReader::with_capacity(4, &buf[..]);
        assert_eq!(read_message(&mut r).unwrap().unwrap()["text"], "a\nb");
        assert_eq!(read_message(&mut r).unwrap().unwrap()["id"], 2);
        assert!(read_message(&mut r).unwrap().is_none());
    }

    #[test]
    fn broken_input_is_an_error() {
        let mut r = io::BufReader::new(&b"{not json}\n"[..]);
        assert!(read_message(&mut r).is_err());
        let mut r = io::BufReader::new(&b"{\"cut\": "[..]);
        assert!(read_message(&mut r).is_err());
        // Over the bound, without a newline in sight.
        let long = vec![b'x'; MAX_MESSAGE + 2];
        let mut r = io::BufReader::new(&long[..]);
        assert_eq!(
            read_message(&mut r).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }
}
