//! What a command prints and returns, as one value: the server runs a
//! command for a client and sends this back, and the client prints it
//! exactly as the command would have printed it itself.

use std::io::Write;

use serde_json::{Value, json};

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Outcome {
    pub exit_code: u8,
    pub stderr: Vec<u8>,
    pub stdout: Vec<u8>,
}

/// Bytes as JSON: a string when they are UTF-8 (nearly always), else an
/// array of numbers, so no byte a model echoed is lost on the way.
fn bytes_json(b: &[u8]) -> Value {
    match std::str::from_utf8(b) {
        Ok(s) => json!(s),
        Err(_) => json!(b),
    }
}

fn bytes_of(v: &Value) -> Vec<u8> {
    match v {
        Value::String(s) => s.clone().into_bytes(),
        Value::Array(a) => a
            .iter()
            .filter_map(|x| x.as_u64().map(|n| n as u8))
            .collect(),
        _ => Vec::new(),
    }
}

impl Outcome {
    pub fn fail(code: u8, line: impl std::fmt::Display) -> Outcome {
        Outcome {
            exit_code: code,
            stderr: format!("{line}\n").into_bytes(),
            stdout: Vec::new(),
        }
    }

    /// Print it and return the exit code.
    pub fn emit(self) -> u8 {
        let mut e = std::io::stderr().lock();
        let _ = e.write_all(&self.stderr);
        let _ = e.flush();
        let mut o = std::io::stdout().lock();
        let _ = o.write_all(&self.stdout);
        let _ = o.flush();
        self.exit_code
    }

    pub fn json(&self) -> Value {
        json!({
            "exit_code": self.exit_code,
            "stderr": bytes_json(&self.stderr),
            "stdout": bytes_json(&self.stdout),
        })
    }

    pub fn from_json(v: &Value) -> Option<Outcome> {
        Some(Outcome {
            exit_code: u8::try_from(v.get("exit_code")?.as_u64()?).ok()?,
            stderr: bytes_of(v.get("stderr")?),
            stdout: bytes_of(v.get("stdout")?),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outcomes_round_trip_with_any_bytes() {
        let o = Outcome {
            exit_code: 3,
            stderr: b"ECHO: \"\xff\"\n".to_vec(),
            stdout: b"{}\n".to_vec(),
        };
        assert_eq!(Outcome::from_json(&o.json()), Some(o));
    }
}
