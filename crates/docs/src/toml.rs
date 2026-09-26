//! The subset of TOML `builtins.toml` is written in: `#` comments,
//! `[[table]]` array headers, and `key = value` where a value is a basic
//! string (`"..."` with `\"`, `\\`, `\n`, `\t` escapes) or an array of
//! them, which may span lines. A full TOML parser would be a dependency
//! for one embedded file; this is enough, and errors name the line.

/// One `[[kind]]` table: its keys in order.
#[derive(Debug, Clone, Default)]
pub struct Table {
    pub kind: String,
    pub line: usize,
    pub values: Vec<(String, Value)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    Str(String),
    List(Vec<String>),
}

impl Table {
    pub fn str(&self, key: &str) -> Option<&str> {
        self.values.iter().find_map(|(k, v)| match v {
            Value::Str(s) if k == key => Some(s.as_str()),
            _ => None,
        })
    }

    pub fn list(&self, key: &str) -> Option<&[String]> {
        self.values.iter().find_map(|(k, v)| match v {
            Value::List(l) if k == key => Some(l.as_slice()),
            _ => None,
        })
    }
}

struct Scanner<'a> {
    s: &'a [u8],
    i: usize,
    line: usize,
}

impl Scanner<'_> {
    fn err<T>(&self, m: &str) -> Result<T, String> {
        Err(format!("line {}: {m}", self.line))
    }

    fn peek(&self) -> Option<u8> {
        self.s.get(self.i).copied()
    }

    /// Skip blanks, and with `lines` also line breaks and comments.
    fn skip(&mut self, lines: bool) {
        while let Some(c) = self.peek() {
            match c {
                b' ' | b'\t' | b'\r' => self.i += 1,
                b'\n' if lines => {
                    self.line += 1;
                    self.i += 1;
                }
                b'#' if lines => {
                    while self.peek().is_some_and(|c| c != b'\n') {
                        self.i += 1;
                    }
                }
                _ => break,
            }
        }
    }

    fn string(&mut self) -> Result<String, String> {
        if self.peek() != Some(b'"') {
            return self.err("expected a string");
        }
        self.i += 1;
        let mut out = Vec::new();
        loop {
            match self.peek() {
                None | Some(b'\n') => return self.err("unterminated string"),
                Some(b'"') => {
                    self.i += 1;
                    break;
                }
                Some(b'\\') => {
                    let e = self.s.get(self.i + 1).copied();
                    out.push(match e {
                        Some(b'n') => b'\n',
                        Some(b't') => b'\t',
                        Some(b'"') => b'"',
                        Some(b'\\') => b'\\',
                        _ => return self.err("unknown escape"),
                    });
                    self.i += 2;
                }
                Some(c) => {
                    out.push(c);
                    self.i += 1;
                }
            }
        }
        String::from_utf8(out).or_else(|_| self.err("string is not UTF-8"))
    }

    fn value(&mut self) -> Result<Value, String> {
        if self.peek() != Some(b'[') {
            return self.string().map(Value::Str);
        }
        self.i += 1;
        let mut items = Vec::new();
        loop {
            self.skip(true);
            match self.peek() {
                Some(b']') => {
                    self.i += 1;
                    return Ok(Value::List(items));
                }
                None => return self.err("unterminated array"),
                _ => {}
            }
            items.push(self.string()?);
            self.skip(true);
            match self.peek() {
                Some(b',') => self.i += 1,
                Some(b']') => {}
                _ => return self.err("expected ',' or ']'"),
            }
        }
    }
}

/// Parse the file into its `[[kind]]` tables, in order.
pub fn parse(text: &str) -> Result<Vec<Table>, String> {
    let mut sc = Scanner {
        s: text.as_bytes(),
        i: 0,
        line: 1,
    };
    let mut tables: Vec<Table> = Vec::new();
    loop {
        sc.skip(true);
        let Some(c) = sc.peek() else {
            return Ok(tables);
        };
        if c == b'[' {
            let rest = &sc.s[sc.i..];
            let end = rest.iter().position(|&b| b == b'\n').unwrap_or(rest.len());
            let head = std::str::from_utf8(&rest[..end]).unwrap_or("").trim();
            let Some(kind) = head.strip_prefix("[[").and_then(|h| h.strip_suffix("]]")) else {
                return sc.err("expected a [[table]] header");
            };
            tables.push(Table {
                kind: kind.trim().to_string(),
                line: sc.line,
                values: Vec::new(),
            });
            sc.i += end;
            continue;
        }
        let start = sc.i;
        while sc
            .peek()
            .is_some_and(|c| c.is_ascii_alphanumeric() || c == b'_')
        {
            sc.i += 1;
        }
        if start == sc.i {
            return sc.err("expected a key");
        }
        let key = String::from_utf8_lossy(&sc.s[start..sc.i]).into_owned();
        sc.skip(false);
        if sc.peek() != Some(b'=') {
            return sc.err("expected '='");
        }
        sc.i += 1;
        sc.skip(false);
        let v = sc.value()?;
        let Some(t) = tables.last_mut() else {
            return sc.err("a key before the first [[table]]");
        };
        if t.values.iter().any(|(k, _)| *k == key) {
            return sc.err(&format!("duplicate key '{key}'"));
        }
        t.values.push((key, v));
        sc.skip(false);
        if sc.peek().is_some_and(|c| c != b'\n' && c != b'#') {
            return sc.err("expected the end of the line");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tables_strings_and_arrays() {
        let t =
            parse("# c\n[[module]]\nname = \"cube\" # x\nparams = [\n  \"a\\\"b\",\n  \"c\",\n]\n")
                .unwrap();
        assert_eq!(t.len(), 1);
        assert_eq!(t[0].kind, "module");
        assert_eq!(t[0].str("name"), Some("cube"));
        assert_eq!(
            t[0].list("params"),
            Some(&["a\"b".to_string(), "c".to_string()][..])
        );
        assert!(parse("[[m]]\nname = \"x\nq\"").is_err());
        assert!(parse("name = \"x\"").is_err());
    }
}
