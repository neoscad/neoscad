//! JSON for the experimental `import()` function (`--enable
//! import-function`), read as OpenSCAD reads it: with nlohmann/json 3.12
//! (`src/ext/json/json.hpp`), `std::istream >> json`, then `to_value` in
//! `src/io/import_json.cc`.
//!
//! What that combination shows, and this reproduces:
//!
//! - objects are nlohmann's default `std::map`, so their keys come out
//!   sorted bytewise, and a repeated key keeps its last value;
//! - numbers: an integer token that fits a 64-bit integer is read as one
//!   and converted to a double (so `-0` is `0`), anything else with
//!   `strtod`; a float token that overflows is an error ("number
//!   overflow");
//! - `null` is `undef`;
//! - `>>` parses non-strictly: whatever follows the first value is
//!   ignored;
//! - errors are nlohmann's exception texts, line, column and "last read"
//!   included, which OpenSCAD prints in its "Failed to parse file"
//!   warning. The lexer is ported state for state for that reason: the
//!   "last read" text is every byte read since the last string or number
//!   began, which only the same reading order gives.
//!
//! The parse is iterative (OpenSCAD's `to_value` recurses and would crash
//! on a deep enough file; here that is the printer's problem, which
//! reports it) and polls the memory limit, since a large file becomes
//! many values inside one builtin call.

use std::collections::BTreeMap;

use crate::value::{ObjectBuilder, Str, Value};

const EOF: i32 = -1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Token {
    Uninitialized,
    LiteralTrue,
    LiteralFalse,
    LiteralNull,
    ValueString,
    ValueUnsigned,
    ValueInteger,
    ValueFloat,
    BeginArray,
    BeginObject,
    EndArray,
    EndObject,
    NameSeparator,
    ValueSeparator,
    ParseError,
    EndOfInput,
    LiteralOrValue,
}

impl Token {
    /// `lexer::token_type_name`.
    fn name(self) -> &'static str {
        match self {
            Token::Uninitialized => "<uninitialized>",
            Token::LiteralTrue => "true literal",
            Token::LiteralFalse => "false literal",
            Token::LiteralNull => "null literal",
            Token::ValueString => "string literal",
            Token::ValueUnsigned | Token::ValueInteger | Token::ValueFloat => "number literal",
            Token::BeginArray => "'['",
            Token::BeginObject => "'{'",
            Token::EndArray => "']'",
            Token::EndObject => "'}'",
            Token::NameSeparator => "':'",
            Token::ValueSeparator => "','",
            Token::ParseError => "<parse error>",
            Token::EndOfInput => "end of input",
            Token::LiteralOrValue => "'[', '{', or a literal",
        }
    }
}

/// nlohmann's `lexer` over a byte buffer, as its `std::istream` adapter
/// feeds it.
struct Lexer<'a> {
    input: &'a [u8],
    at: usize,
    current: i32,
    next_unget: bool,
    chars_read_total: usize,
    chars_read_current_line: usize,
    lines_read: usize,
    /// The raw bytes read since the last `reset`, for error messages.
    token_string: Vec<u8>,
    /// A string's or number's decoded bytes.
    token_buffer: Vec<u8>,
    error_message: &'static str,
    number: f64,
}

impl<'a> Lexer<'a> {
    fn new(input: &'a [u8]) -> Lexer<'a> {
        Lexer {
            input,
            at: 0,
            current: EOF,
            next_unget: false,
            chars_read_total: 0,
            chars_read_current_line: 0,
            lines_read: 0,
            token_string: Vec::new(),
            token_buffer: Vec::new(),
            error_message: "",
            number: 0.0,
        }
    }

    fn get(&mut self) -> i32 {
        self.chars_read_total += 1;
        self.chars_read_current_line += 1;
        if self.next_unget {
            self.next_unget = false;
        } else {
            self.current = match self.input.get(self.at) {
                Some(&b) => {
                    self.at += 1;
                    i32::from(b)
                }
                None => EOF,
            };
        }
        if self.current != EOF {
            self.token_string.push(self.current as u8);
        }
        if self.current == i32::from(b'\n') {
            self.lines_read += 1;
            self.chars_read_current_line = 0;
        }
        self.current
    }

    fn unget(&mut self) {
        self.next_unget = true;
        self.chars_read_total -= 1;
        if self.chars_read_current_line == 0 {
            if self.lines_read > 0 {
                self.lines_read -= 1;
            }
        } else {
            self.chars_read_current_line -= 1;
        }
        if self.current != EOF {
            self.token_string.pop();
        }
    }

    fn add(&mut self, c: i32) {
        self.token_buffer.push(c as u8);
    }

    fn reset(&mut self) {
        self.token_buffer.clear();
        self.token_string.clear();
        self.token_string.push(self.current as u8);
    }

    /// `get_token_string`: control characters as `<U+XXXX>`, other bytes
    /// as they are (OpenSCAD prints an ill-formed byte raw).
    fn token_string(&self) -> Vec<u8> {
        let mut out = Vec::new();
        for &c in &self.token_string {
            if c <= 0x1F {
                out.extend_from_slice(format!("<U+{c:04X}>").as_bytes());
            } else {
                out.push(c);
            }
        }
        out
    }

    /// `parse_error::position_string`.
    fn position(&self) -> String {
        format!(
            " at line {}, column {}",
            self.lines_read + 1,
            self.chars_read_current_line
        )
    }

    fn skip_bom(&mut self) -> bool {
        if self.get() == 0xEF {
            return self.get() == 0xBB && self.get() == 0xBF;
        }
        self.unget();
        true
    }

    fn skip_whitespace(&mut self) {
        loop {
            self.get();
            if !matches!(self.current, 0x20 | 0x09 | 0x0A | 0x0D) {
                break;
            }
        }
    }

    fn scan(&mut self) -> Token {
        if self.chars_read_total == 0 && !self.skip_bom() {
            self.error_message = "invalid BOM; must be 0xEF 0xBB 0xBF if given";
            return Token::ParseError;
        }
        self.skip_whitespace();
        match self.current {
            0x5B => Token::BeginArray,
            0x5D => Token::EndArray,
            0x7B => Token::BeginObject,
            0x7D => Token::EndObject,
            0x3A => Token::NameSeparator,
            0x2C => Token::ValueSeparator,
            0x74 => self.scan_literal(b"true", Token::LiteralTrue),
            0x66 => self.scan_literal(b"false", Token::LiteralFalse),
            0x6E => self.scan_literal(b"null", Token::LiteralNull),
            0x22 => self.scan_string(),
            0x2D | 0x30..=0x39 => self.scan_number(),
            0 | EOF => Token::EndOfInput,
            _ => {
                self.error_message = "invalid literal";
                Token::ParseError
            }
        }
    }

    fn scan_literal(&mut self, text: &[u8], t: Token) -> Token {
        for &c in &text[1..] {
            if self.get() != i32::from(c) {
                self.error_message = "invalid literal";
                return Token::ParseError;
            }
        }
        t
    }

    fn get_codepoint(&mut self) -> Option<u32> {
        let mut cp = 0;
        for factor in [12, 8, 4, 0] {
            let c = self.get();
            let d = match c {
                0x30..=0x39 => c - 0x30,
                0x41..=0x46 => c - 0x37,
                0x61..=0x66 => c - 0x57,
                _ => return None,
            };
            cp += (d as u32) << factor;
        }
        Some(cp)
    }

    fn next_byte_in_range(&mut self, ranges: &[(i32, i32)]) -> bool {
        self.add(self.current);
        for &(lo, hi) in ranges {
            let c = self.get();
            if lo <= c && c <= hi {
                self.add(c);
            } else {
                self.error_message = "invalid string: ill-formed UTF-8 byte";
                return false;
            }
        }
        true
    }

    fn scan_string(&mut self) -> Token {
        self.reset();
        loop {
            let c = self.get();
            match c {
                EOF => {
                    self.error_message = "invalid string: missing closing quote";
                    return Token::ParseError;
                }
                0x22 => return Token::ValueString,
                0x5C => {
                    let e = self.get();
                    let b = match e {
                        0x22 => b'"',
                        0x5C => b'\\',
                        0x2F => b'/',
                        0x62 => 0x08,
                        0x66 => 0x0C,
                        0x6E => b'\n',
                        0x72 => b'\r',
                        0x74 => b'\t',
                        0x75 => {
                            let Some(cp1) = self.get_codepoint() else {
                                self.error_message =
                                    "invalid string: '\\u' must be followed by 4 hex digits";
                                return Token::ParseError;
                            };
                            let mut cp = cp1;
                            if (0xD800..=0xDBFF).contains(&cp1) {
                                if self.get() == 0x5C && self.get() == 0x75 {
                                    let Some(cp2) = self.get_codepoint() else {
                                        self.error_message = "invalid string: '\\u' must be followed by 4 hex digits";
                                        return Token::ParseError;
                                    };
                                    if (0xDC00..=0xDFFF).contains(&cp2) {
                                        cp = (cp1 << 10) + cp2 - 0x35F_DC00;
                                    } else {
                                        self.error_message = "invalid string: surrogate U+D800..U+DBFF must be followed by U+DC00..U+DFFF";
                                        return Token::ParseError;
                                    }
                                } else {
                                    self.error_message = "invalid string: surrogate U+D800..U+DBFF must be followed by U+DC00..U+DFFF";
                                    return Token::ParseError;
                                }
                            } else if (0xDC00..=0xDFFF).contains(&cp1) {
                                self.error_message = "invalid string: surrogate U+DC00..U+DFFF must follow U+D800..U+DBFF";
                                return Token::ParseError;
                            }
                            let ch = char::from_u32(cp).unwrap_or('\u{FFFD}');
                            let mut buf = [0u8; 4];
                            self.token_buffer
                                .extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
                            continue;
                        }
                        _ => {
                            self.error_message =
                                "invalid string: forbidden character after backslash";
                            return Token::ParseError;
                        }
                    };
                    self.token_buffer.push(b);
                }
                0x00..=0x1F => {
                    self.error_message = CONTROL[c as usize];
                    return Token::ParseError;
                }
                0x20..=0x7F => self.add(c),
                0xC2..=0xDF => {
                    if !self.next_byte_in_range(&[(0x80, 0xBF)]) {
                        return Token::ParseError;
                    }
                }
                0xE0 => {
                    if !self.next_byte_in_range(&[(0xA0, 0xBF), (0x80, 0xBF)]) {
                        return Token::ParseError;
                    }
                }
                0xE1..=0xEC | 0xEE | 0xEF => {
                    if !self.next_byte_in_range(&[(0x80, 0xBF), (0x80, 0xBF)]) {
                        return Token::ParseError;
                    }
                }
                0xED => {
                    if !self.next_byte_in_range(&[(0x80, 0x9F), (0x80, 0xBF)]) {
                        return Token::ParseError;
                    }
                }
                0xF0 => {
                    if !self.next_byte_in_range(&[(0x90, 0xBF), (0x80, 0xBF), (0x80, 0xBF)]) {
                        return Token::ParseError;
                    }
                }
                0xF1..=0xF3 => {
                    if !self.next_byte_in_range(&[(0x80, 0xBF), (0x80, 0xBF), (0x80, 0xBF)]) {
                        return Token::ParseError;
                    }
                }
                0xF4 => {
                    if !self.next_byte_in_range(&[(0x80, 0x8F), (0x80, 0xBF), (0x80, 0xBF)]) {
                        return Token::ParseError;
                    }
                }
                _ => {
                    self.error_message = "invalid string: ill-formed UTF-8 byte";
                    return Token::ParseError;
                }
            }
        }
    }

    fn scan_number(&mut self) -> Token {
        let digit = |c: i32| (0x30..=0x39).contains(&c);
        self.reset();
        let mut ty = Token::ValueUnsigned;
        // The states of nlohmann's `scan_number`, by their labels.
        #[derive(Clone, Copy)]
        enum S {
            Minus,
            Zero,
            Any1,
            Decimal1,
            Decimal2,
            Exponent,
            Sign,
            Any2,
            Done,
        }
        let mut s = match self.current {
            0x2D => S::Minus,
            0x30 => S::Zero,
            _ => S::Any1,
        };
        self.add(self.current);
        loop {
            s = match s {
                S::Minus => {
                    ty = Token::ValueInteger;
                    let c = self.get();
                    if c == 0x30 {
                        self.add(c);
                        S::Zero
                    } else if digit(c) {
                        self.add(c);
                        S::Any1
                    } else {
                        self.error_message = "invalid number; expected digit after '-'";
                        return Token::ParseError;
                    }
                }
                S::Zero => match self.get() {
                    0x2E => {
                        self.add(0x2E);
                        S::Decimal1
                    }
                    c @ (0x65 | 0x45) => {
                        self.add(c);
                        S::Exponent
                    }
                    _ => S::Done,
                },
                S::Any1 => match self.get() {
                    c if digit(c) => {
                        self.add(c);
                        S::Any1
                    }
                    0x2E => {
                        self.add(0x2E);
                        S::Decimal1
                    }
                    c @ (0x65 | 0x45) => {
                        self.add(c);
                        S::Exponent
                    }
                    _ => S::Done,
                },
                S::Decimal1 => {
                    ty = Token::ValueFloat;
                    let c = self.get();
                    if digit(c) {
                        self.add(c);
                        S::Decimal2
                    } else {
                        self.error_message = "invalid number; expected digit after '.'";
                        return Token::ParseError;
                    }
                }
                S::Decimal2 => match self.get() {
                    c if digit(c) => {
                        self.add(c);
                        S::Decimal2
                    }
                    c @ (0x65 | 0x45) => {
                        self.add(c);
                        S::Exponent
                    }
                    _ => S::Done,
                },
                S::Exponent => {
                    ty = Token::ValueFloat;
                    match self.get() {
                        c @ (0x2B | 0x2D) => {
                            self.add(c);
                            S::Sign
                        }
                        c if digit(c) => {
                            self.add(c);
                            S::Any2
                        }
                        _ => {
                            self.error_message =
                                "invalid number; expected '+', '-', or digit after exponent";
                            return Token::ParseError;
                        }
                    }
                }
                S::Sign => {
                    let c = self.get();
                    if digit(c) {
                        self.add(c);
                        S::Any2
                    } else {
                        self.error_message = "invalid number; expected digit after exponent sign";
                        return Token::ParseError;
                    }
                }
                S::Any2 => match self.get() {
                    c if digit(c) => {
                        self.add(c);
                        S::Any2
                    }
                    _ => S::Done,
                },
                S::Done => break,
            };
        }
        self.unget();
        // The buffer holds only ASCII digits, signs, `.` and `e`.
        let text = std::str::from_utf8(&self.token_buffer).unwrap_or("0");
        // `strtoull`/`strtoll`, falling back to `strtod` when the integer
        // does not fit; a JSON integer converts to a double as a C++
        // `static_cast` does, rounding to nearest.
        match ty {
            Token::ValueUnsigned => {
                if let Ok(u) = text.parse::<u64>() {
                    self.number = u as f64;
                    return Token::ValueUnsigned;
                }
            }
            Token::ValueInteger => {
                if let Ok(i) = text.parse::<i64>() {
                    self.number = i as f64;
                    return Token::ValueInteger;
                }
            }
            _ => {}
        }
        self.number = text.parse::<f64>().unwrap_or(f64::NAN);
        Token::ValueFloat
    }
}

/// The lexer's message for an unescaped control character in a string.
const CONTROL: [&str; 32] = [
    "invalid string: control character U+0000 (NUL) must be escaped to \\u0000",
    "invalid string: control character U+0001 (SOH) must be escaped to \\u0001",
    "invalid string: control character U+0002 (STX) must be escaped to \\u0002",
    "invalid string: control character U+0003 (ETX) must be escaped to \\u0003",
    "invalid string: control character U+0004 (EOT) must be escaped to \\u0004",
    "invalid string: control character U+0005 (ENQ) must be escaped to \\u0005",
    "invalid string: control character U+0006 (ACK) must be escaped to \\u0006",
    "invalid string: control character U+0007 (BEL) must be escaped to \\u0007",
    "invalid string: control character U+0008 (BS) must be escaped to \\u0008 or \\b",
    "invalid string: control character U+0009 (HT) must be escaped to \\u0009 or \\t",
    "invalid string: control character U+000A (LF) must be escaped to \\u000A or \\n",
    "invalid string: control character U+000B (VT) must be escaped to \\u000B",
    "invalid string: control character U+000C (FF) must be escaped to \\u000C or \\f",
    "invalid string: control character U+000D (CR) must be escaped to \\u000D or \\r",
    "invalid string: control character U+000E (SO) must be escaped to \\u000E",
    "invalid string: control character U+000F (SI) must be escaped to \\u000F",
    "invalid string: control character U+0010 (DLE) must be escaped to \\u0010",
    "invalid string: control character U+0011 (DC1) must be escaped to \\u0011",
    "invalid string: control character U+0012 (DC2) must be escaped to \\u0012",
    "invalid string: control character U+0013 (DC3) must be escaped to \\u0013",
    "invalid string: control character U+0014 (DC4) must be escaped to \\u0014",
    "invalid string: control character U+0015 (NAK) must be escaped to \\u0015",
    "invalid string: control character U+0016 (SYN) must be escaped to \\u0016",
    "invalid string: control character U+0017 (ETB) must be escaped to \\u0017",
    "invalid string: control character U+0018 (CAN) must be escaped to \\u0018",
    "invalid string: control character U+0019 (EM) must be escaped to \\u0019",
    "invalid string: control character U+001A (SUB) must be escaped to \\u001A",
    "invalid string: control character U+001B (ESC) must be escaped to \\u001B",
    "invalid string: control character U+001C (FS) must be escaped to \\u001C",
    "invalid string: control character U+001D (GS) must be escaped to \\u001D",
    "invalid string: control character U+001E (RS) must be escaped to \\u001E",
    "invalid string: control character U+001F (US) must be escaped to \\u001F",
];

/// Why a parse stopped: nlohmann's exception text, or the memory limit.
#[derive(Debug, PartialEq)]
pub(crate) enum Failed {
    /// `e.what()`, for OpenSCAD's "Failed to parse file" warning: bytes,
    /// as "last read" quotes the input's.
    Parse(Vec<u8>),
    /// The memory limit passed while building the values; the evaluator
    /// reports it.
    Memory,
}

/// A container being filled.
enum Open {
    Array(Vec<Value>),
    /// Entries so far, sorted as `std::map` sorts them, and the key of the
    /// value being read.
    Object(BTreeMap<Vec<u8>, Value>, Vec<u8>),
}

/// Tokens between looks at the memory estimate.
const POLL: u32 = 4096;

struct Parser<'a> {
    lx: Lexer<'a>,
    last: Token,
}

impl Parser<'_> {
    fn next(&mut self) -> Token {
        self.last = self.lx.scan();
        self.last
    }

    /// `parser::exception_message`, wrapped as `parse_error::create(101)`.
    fn error(&self, expected: Token, context: &str) -> Failed {
        let mut m = b"syntax error ".to_vec();
        if !context.is_empty() {
            m.extend_from_slice(format!("while parsing {context} ").as_bytes());
        }
        m.extend_from_slice(b"- ");
        if self.last == Token::ParseError {
            m.extend_from_slice(self.lx.error_message.as_bytes());
            m.extend_from_slice(b"; last read: '");
            m.extend_from_slice(&self.lx.token_string());
            m.push(b'\'');
        } else {
            m.extend_from_slice(format!("unexpected {}", self.last.name()).as_bytes());
        }
        if expected != Token::Uninitialized {
            m.extend_from_slice(format!("; expected {}", expected.name()).as_bytes());
        }
        self.fail(&m)
    }

    fn fail(&self, what: &[u8]) -> Failed {
        let mut m = format!(
            "[json.exception.parse_error.101] parse error{}: ",
            self.lx.position()
        )
        .into_bytes();
        m.extend_from_slice(what);
        Failed::Parse(m)
    }

    /// A value is complete: into its container, or the result.
    fn put(stack: &mut [Open], v: Value) -> Option<Value> {
        match stack.last_mut() {
            None => Some(v),
            Some(Open::Array(items)) => {
                items.push(v);
                None
            }
            Some(Open::Object(map, key)) => {
                map.insert(std::mem::take(key), v);
                None
            }
        }
    }

    /// `sax_parse_internal` with `json_sax_dom_parser` and `to_value`.
    fn parse(&mut self) -> Result<Value, Failed> {
        let mut stack: Vec<Open> = Vec::new();
        let mut ticks = 0u32;
        // Set when a container just closed: go straight to what follows
        // it (`skip_to_state_evaluation`).
        let mut done: Option<Value> = None;
        loop {
            ticks += 1;
            if ticks.is_multiple_of(POLL) && crate::limits::live::over() {
                return Err(Failed::Memory);
            }
            let v = match done.take() {
                Some(v) => v,
                None => match self.last {
                    Token::BeginObject => {
                        if self.next() == Token::EndObject {
                            object(BTreeMap::new())
                        } else {
                            if self.last != Token::ValueString {
                                return Err(self.error(Token::ValueString, "object key"));
                            }
                            let key = std::mem::take(&mut self.lx.token_buffer);
                            if self.next() != Token::NameSeparator {
                                return Err(self.error(Token::NameSeparator, "object separator"));
                            }
                            stack.push(Open::Object(BTreeMap::new(), key));
                            self.next();
                            continue;
                        }
                    }
                    Token::BeginArray => {
                        if self.next() == Token::EndArray {
                            Value::vector(Vec::new())
                        } else {
                            stack.push(Open::Array(Vec::new()));
                            continue;
                        }
                    }
                    Token::ValueFloat => {
                        if !self.lx.number.is_finite() {
                            let mut m =
                                b"[json.exception.out_of_range.406] number overflow parsing '"
                                    .to_vec();
                            m.extend_from_slice(&self.lx.token_string());
                            m.push(b'\'');
                            return Err(Failed::Parse(m));
                        }
                        Value::Number(self.lx.number)
                    }
                    Token::ValueUnsigned | Token::ValueInteger => Value::Number(self.lx.number),
                    Token::LiteralFalse => Value::Bool(false),
                    Token::LiteralTrue => Value::Bool(true),
                    Token::LiteralNull => Value::Undef,
                    Token::ValueString => Value::Str(Str::new(&self.lx.token_buffer)),
                    Token::ParseError => return Err(self.error(Token::Uninitialized, "value")),
                    Token::EndOfInput => {
                        if self.lx.chars_read_total == 1 {
                            return Err(self.fail(
                                b"attempting to parse an empty input; check that your input string or stream contains the expected JSON",
                            ));
                        }
                        return Err(self.error(Token::LiteralOrValue, "value"));
                    }
                    _ => return Err(self.error(Token::LiteralOrValue, "value")),
                },
            };
            if let Some(result) = Self::put(&mut stack, v) {
                return Ok(result);
            }
            match stack.last() {
                Some(Open::Array(_)) => {
                    if self.next() == Token::ValueSeparator {
                        self.next();
                        continue;
                    }
                    if self.last == Token::EndArray {
                        let Some(Open::Array(items)) = stack.pop() else {
                            unreachable!("an array is open");
                        };
                        done = Some(Value::vector(items));
                        continue;
                    }
                    return Err(self.error(Token::EndArray, "array"));
                }
                Some(Open::Object(..)) => {
                    if self.next() == Token::ValueSeparator {
                        if self.next() != Token::ValueString {
                            return Err(self.error(Token::ValueString, "object key"));
                        }
                        let key = std::mem::take(&mut self.lx.token_buffer);
                        if let Some(Open::Object(_, k)) = stack.last_mut() {
                            *k = key;
                        }
                        if self.next() != Token::NameSeparator {
                            return Err(self.error(Token::NameSeparator, "object separator"));
                        }
                        self.next();
                        continue;
                    }
                    if self.last == Token::EndObject {
                        let Some(Open::Object(map, _)) = stack.pop() else {
                            unreachable!("an object is open");
                        };
                        done = Some(object(map));
                        continue;
                    }
                    return Err(self.error(Token::EndObject, "object"));
                }
                None => unreachable!("a value with no container was the result"),
            }
        }
    }
}

fn object(map: BTreeMap<Vec<u8>, Value>) -> Value {
    let mut b = ObjectBuilder::new();
    for (k, v) in map {
        b.set(Str::from_vec(k), v);
    }
    Value::Object(b.finish(|_| false))
}

/// Parse a JSON file's bytes as `import()` does.
pub(crate) fn parse(bytes: &[u8]) -> Result<Value, Failed> {
    let mut p = Parser {
        lx: Lexer::new(bytes),
        last: Token::Uninitialized,
    };
    // The parser reads its first token when it is made.
    p.next();
    p.parse()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn err(s: &str) -> String {
        match parse(s.as_bytes()) {
            Err(Failed::Parse(m)) => String::from_utf8(m).unwrap(),
            other => panic!("{s:?} parsed: {other:?}"),
        }
    }

    /// The nightly's messages for these inputs (its `import()` warnings).
    #[test]
    fn errors_are_nlohmanns() {
        assert_eq!(
            err("{\"a\": tru}"),
            "[json.exception.parse_error.101] parse error at line 1, column 10: syntax error while parsing value - invalid literal; last read: '\"a\": tru}'"
        );
        assert_eq!(
            err("{\"a\": 1,}"),
            "[json.exception.parse_error.101] parse error at line 1, column 9: syntax error while parsing object key - unexpected '}'; expected string literal"
        );
        assert_eq!(
            err(""),
            "[json.exception.parse_error.101] parse error at line 1, column 1: attempting to parse an empty input; check that your input string or stream contains the expected JSON"
        );
        assert_eq!(
            err("// c\n[1]"),
            "[json.exception.parse_error.101] parse error at line 1, column 1: syntax error while parsing value - invalid literal; last read: '/'"
        );
        assert_eq!(
            err("[1e400]"),
            "[json.exception.out_of_range.406] number overflow parsing '1e400'"
        );
    }

    #[test]
    fn values() {
        let v =
            parse(b"\xef\xbb\xbf{\"b\": [1, -0, 2.5, null, true], \"a\": \"x\", \"a\": 7} junk")
                .unwrap();
        let Value::Object(o) = v else { panic!() };
        let keys: Vec<&[u8]> = o.keys().iter().map(|k| k.as_bytes()).collect();
        assert_eq!(keys, [&b"a"[..], b"b"]);
        assert_eq!(o.get(b"a").as_number(), Some(7.0));
        let b = o.get(b"b");
        let b = b.as_vector().unwrap();
        assert!(b[1].as_number().unwrap().is_sign_positive());
        assert!(b[3].is_undef());
    }
}
