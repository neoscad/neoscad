//! A small JSON reader that yields a Boost `property_tree`-like structure,
//! which is what OpenSCAD reads parameter files into: every scalar keeps its
//! source text (`1.50` stays `1.50`, `true` stays `true`), objects and
//! arrays become ordered child lists (array children have empty keys), and
//! duplicate keys are kept. Parameter import then parses those strings the
//! way `ptree::get_value_optional` does, so a JSON number and a JSON string
//! holding the same digits behave the same.

use std::fmt;

#[derive(Debug, Clone, PartialEq, Default)]
pub struct JsonNode {
    pub data: String,
    pub children: Vec<(String, JsonNode)>,
}

impl JsonNode {
    pub fn child(&self, key: &str) -> Option<&JsonNode> {
        self.children.iter().find(|(k, _)| k == key).map(|(_, v)| v)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JsonError {
    pub line: usize,
    pub message: &'static str,
}

impl fmt::Display for JsonError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "<unspecified file>({}): {}", self.line, self.message)
    }
}

pub fn parse(text: &[u8]) -> Result<JsonNode, JsonError> {
    let mut p = Parser { s: text, i: 0 };
    p.ws();
    let v = p.value()?;
    p.ws();
    if p.i != p.s.len() {
        return Err(p.err("garbage after data"));
    }
    Ok(v)
}

struct Parser<'a> {
    s: &'a [u8],
    i: usize,
}

impl Parser<'_> {
    fn err(&self, message: &'static str) -> JsonError {
        let line = self.s[..self.i.min(self.s.len())].iter().filter(|&&b| b == b'\n').count() + 1;
        JsonError { line, message }
    }

    fn ws(&mut self) {
        while matches!(self.s.get(self.i), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.i += 1;
        }
    }

    fn eat(&mut self, b: u8) -> bool {
        if self.s.get(self.i) == Some(&b) {
            self.i += 1;
            true
        } else {
            false
        }
    }

    fn value(&mut self) -> Result<JsonNode, JsonError> {
        match self.s.get(self.i) {
            Some(b'{') => {
                self.i += 1;
                let mut node = JsonNode::default();
                self.ws();
                if self.eat(b'}') {
                    return Ok(node);
                }
                loop {
                    self.ws();
                    if self.s.get(self.i) != Some(&b'"') {
                        return Err(self.err("expected key string"));
                    }
                    let key = self.string()?;
                    self.ws();
                    if !self.eat(b':') {
                        return Err(self.err("expected ':'"));
                    }
                    self.ws();
                    let v = self.value()?;
                    node.children.push((key, v));
                    self.ws();
                    if self.eat(b',') {
                        continue;
                    }
                    if self.eat(b'}') {
                        return Ok(node);
                    }
                    return Err(self.err("expected '}' or ','"));
                }
            }
            Some(b'[') => {
                self.i += 1;
                let mut node = JsonNode::default();
                self.ws();
                if self.eat(b']') {
                    return Ok(node);
                }
                loop {
                    self.ws();
                    let v = self.value()?;
                    node.children.push((String::new(), v));
                    self.ws();
                    if self.eat(b',') {
                        continue;
                    }
                    if self.eat(b']') {
                        return Ok(node);
                    }
                    return Err(self.err("expected ']' or ','"));
                }
            }
            Some(b'"') => Ok(JsonNode { data: self.string()?, children: Vec::new() }),
            Some(_) => {
                for word in [&b"true"[..], b"false", b"null"] {
                    if self.s[self.i..].starts_with(word) {
                        self.i += word.len();
                        return Ok(JsonNode { data: String::from_utf8_lossy(word).into_owned(), children: Vec::new() });
                    }
                }
                self.number()
            }
            None => Err(self.err("expected value")),
        }
    }

    fn number(&mut self) -> Result<JsonNode, JsonError> {
        let start = self.i;
        self.eat(b'-');
        let digits = |p: &mut Self| {
            let s = p.i;
            while p.s.get(p.i).is_some_and(u8::is_ascii_digit) {
                p.i += 1;
            }
            p.i - s
        };
        if digits(self) == 0 {
            return Err(self.err("expected value"));
        }
        if self.eat(b'.') && digits(self) == 0 {
            return Err(self.err("need at least one digit after '.'"));
        }
        if matches!(self.s.get(self.i), Some(b'e' | b'E')) {
            self.i += 1;
            if !self.eat(b'+') {
                self.eat(b'-');
            }
            if digits(self) == 0 {
                return Err(self.err("need at least one digit in exponent"));
            }
        }
        Ok(JsonNode { data: String::from_utf8_lossy(&self.s[start..self.i]).into_owned(), children: Vec::new() })
    }

    fn string(&mut self) -> Result<String, JsonError> {
        self.i += 1;
        let mut out = Vec::new();
        loop {
            match self.s.get(self.i) {
                None => return Err(self.err("unterminated string")),
                Some(b'"') => {
                    self.i += 1;
                    return Ok(String::from_utf8_lossy(&out).into_owned());
                }
                Some(b'\\') => {
                    let c = self.s.get(self.i + 1).copied();
                    self.i += 2;
                    match c {
                        Some(b'"') => out.push(b'"'),
                        Some(b'\\') => out.push(b'\\'),
                        Some(b'/') => out.push(b'/'),
                        Some(b'b') => out.push(8),
                        Some(b'f') => out.push(12),
                        Some(b'n') => out.push(b'\n'),
                        Some(b'r') => out.push(b'\r'),
                        Some(b't') => out.push(b'\t'),
                        Some(b'u') => {
                            let hex = |p: &Self, at: usize| {
                                p.s.get(at..at + 4)
                                    .and_then(|h| std::str::from_utf8(h).ok())
                                    .and_then(|h| u32::from_str_radix(h, 16).ok())
                            };
                            let Some(mut cp) = hex(self, self.i) else { return Err(self.err("invalid escape sequence")) };
                            self.i += 4;
                            if (0xd800..0xdc00).contains(&cp)
                                && self.s.get(self.i..self.i + 2) == Some(b"\\u")
                                && let Some(lo) = hex(self, self.i + 2)
                                && (0xdc00..0xe000).contains(&lo)
                            {
                                cp = 0x10000 + ((cp - 0xd800) << 10) + (lo - 0xdc00);
                                self.i += 6;
                            }
                            let ch = char::from_u32(cp).unwrap_or('\u{fffd}');
                            let mut buf = [0; 4];
                            out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
                        }
                        _ => return Err(self.err("invalid escape sequence")),
                    }
                }
                Some(&b) => {
                    out.push(b);
                    self.i += 1;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_scalar_text_and_order() {
        let v = parse(br#"{"a": {"x": "1", "y": 1.50, "z": true, "x": [1, "s"]}, "b": null}"#).unwrap();
        let a = v.child("a").unwrap();
        assert_eq!(a.children.len(), 4);
        assert_eq!(a.children[1].1.data, "1.50");
        assert_eq!(a.children[2].1.data, "true");
        assert_eq!(a.children[3].1.children[1].1.data, "s");
        assert_eq!(v.child("b").unwrap().data, "null");
        assert!(parse(b"{\"a\" 1}").is_err());
        assert_eq!(parse(b"\"\\u00e9\\n\"").unwrap().data, "\u{e9}\n");
    }
}
