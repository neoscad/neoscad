//! The lexer: bytes to a lossless token stream.
//!
//! Behaviour follows OpenSCAD's `src/core/lexer.l`, which is a flex scanner
//! and therefore picks the *longest* match and, on a tie, the earliest rule.
//! Several surprising cases fall out of that and are reproduced here:
//!
//! - `2d` is an identifier (with a deprecation warning) but `2` and `1e5`
//!   are numbers, and `1e5x` is again an identifier;
//! - `0x1F` is hex but `0X1F` is an identifier (the rule is lower-case);
//! - `include <...>` and `use <...>` are recognised by the scanner anywhere
//!   outside strings and comments, even in the middle of an expression;
//! - a float that overflows or underflows a double (`1e400`) produces no
//!   token at all;
//! - inside a string a raw newline is dropped, an unknown escape drops the
//!   backslash with a warning, and `\x00` becomes a space;
//! - a NUL byte ends the program.
//!
//! Every byte ends up in exactly one token, so the syntax tree built on top
//! is lossless. Token values (numbers, string contents) are not stored:
//! [`number_value`] and [`string_value`] decode a token's text on demand.

use crate::diag::{DiagCode, Severity};
use crate::source::FileId;
use crate::syntax::SyntaxKind;

/// One token. 16 bytes; text is recovered from the source by span.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Token {
    pub kind: SyntaxKind,
    pub file: FileId,
    pub start: u32,
    pub len: u32,
}

impl Token {
    pub fn end(&self) -> u32 {
        self.start + self.len
    }
}

/// A message the scanner emits while producing token `token`.
#[derive(Debug, Clone, PartialEq)]
pub struct LexDiag {
    /// Index of the token (within this file's token list) being scanned.
    pub token: u32,
    pub code: DiagCode,
    pub severity: Severity,
    pub message: String,
    pub start: u32,
    pub end: u32,
    /// Byte offset whose line OpenSCAD reports.
    pub line_at: u32,
}

#[derive(Debug, Default)]
pub struct Lexed {
    pub tokens: Vec<Token>,
    pub diags: Vec<LexDiag>,
}

/// Tokenise one file.
pub fn lex(src: &[u8], file: FileId) -> Lexed {
    // OpenSCAD reads its text as a C string: a NUL byte ends the input.
    let end = src.iter().position(|&b| b == 0).unwrap_or(src.len());
    let mut lx = Lexer {
        src: &src[..end],
        file,
        pos: 0,
        out: Lexed::default(),
    };
    lx.out.tokens.reserve(src.len() / 3);
    lx.run();
    let mut out = lx.out;
    if end < src.len() {
        out.tokens.push(Token {
            kind: SyntaxKind::Ignored,
            file,
            start: end as u32,
            len: (src.len() - end) as u32,
        });
    }
    out
}

struct Lexer<'a> {
    src: &'a [u8],
    file: FileId,
    pos: usize,
    out: Lexed,
}

fn is_ident_rest(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

fn is_cont(b: Option<&u8>) -> bool {
    matches!(b, Some(0x80..=0xbf))
}

/// Length of a non-ASCII UTF-8 sequence that may appear in an identifier
/// (`UNICODEID` in lexer.l), or 0. U+00A0 and U+FEFF are excluded because
/// they are whitespace. Like the flex pattern this is only a coarse filter;
/// it does not reject overlong forms or surrogates.
pub(crate) fn unicode_id_len(s: &[u8], i: usize) -> usize {
    let b = |k: usize| s.get(i + k);
    match s.get(i) {
        Some(0xc2) => match b(1) {
            Some(0x80..=0x9f | 0xa1..=0xbf) => 2,
            _ => 0,
        },
        Some(0xc3..=0xdf) if is_cont(b(1)) => 2,
        Some(0xe0..=0xee) if is_cont(b(1)) && is_cont(b(2)) => 3,
        Some(0xef) => match (b(1), b(2)) {
            (Some(0x80..=0xba | 0xbc..=0xbf), c) if is_cont(c) => 3,
            (Some(0xbb), Some(0x80..=0xbe)) => 3,
            _ => 0,
        },
        Some(0xf0..=0xf4) if is_cont(b(1)) && is_cont(b(2)) && is_cont(b(3)) => 4,
        _ => 0,
    }
}

/// Length of whitespace starting at `i` (one unit), or 0.
fn ws_len(s: &[u8], i: usize) -> usize {
    match s.get(i) {
        Some(b' ' | b'\t' | b'\r' | b'\n' | 0xa0) => 1,
        Some(0xc2) if s.get(i + 1) == Some(&0xa0) => 2,
        Some(0xef) if s.get(i + 1) == Some(&0xbb) && s.get(i + 2) == Some(&0xbf) => 3,
        _ => 0,
    }
}

impl Lexer<'_> {
    fn push(&mut self, kind: SyntaxKind, start: usize, end: usize) {
        self.out.tokens.push(Token {
            kind,
            file: self.file,
            start: start as u32,
            len: (end - start) as u32,
        });
    }

    /// Record a diagnostic for the token about to be pushed.
    fn diag(
        &mut self,
        code: DiagCode,
        severity: Severity,
        message: String,
        start: usize,
        end: usize,
        line_at: usize,
    ) {
        self.out.diags.push(LexDiag {
            token: self.out.tokens.len() as u32,
            code,
            severity,
            message,
            start: start as u32,
            end: end as u32,
            line_at: line_at as u32,
        });
    }

    fn run(&mut self) {
        let s = self.src;
        while self.pos < s.len() {
            let i = self.pos;
            let b = s[i];
            let w = ws_len(s, i);
            if w > 0 {
                let mut j = i + w;
                loop {
                    let w = ws_len(s, j);
                    if w == 0 {
                        break;
                    }
                    j += w;
                }
                self.push(SyntaxKind::Whitespace, i, j);
                self.pos = j;
                continue;
            }
            match b {
                b'/' if s.get(i + 1) == Some(&b'/') => {
                    let j = s[i..]
                        .iter()
                        .position(|&c| c == b'\n')
                        .map_or(s.len(), |p| i + p);
                    self.push(SyntaxKind::LineComment, i, j);
                    self.pos = j;
                }
                b'/' if s.get(i + 1) == Some(&b'*') => match find(&s[i + 2..], b"*/") {
                    Some(p) => {
                        self.push(SyntaxKind::BlockComment, i, i + 2 + p + 2);
                        self.pos = i + 2 + p + 2;
                    }
                    None => {
                        let n = s.len();
                        self.diag(
                            DiagCode::UnterminatedComment,
                            Severity::Error,
                            "Parser error: Unterminated comment".into(),
                            n,
                            n,
                            n,
                        );
                        self.push(SyntaxKind::Error, i, n);
                        self.pos = n;
                    }
                },
                b'"' => self.string(i),
                b'0'..=b'9' => self.number(i),
                b'.' if s.get(i + 1).is_some_and(u8::is_ascii_digit) => self.number(i),
                b'a'..=b'z' | b'A'..=b'Z' | b'_' | b'$' => self.ident(i),
                0x80..=0xff => {
                    if unicode_id_len(s, i) > 0 {
                        self.ident(i);
                    } else {
                        // No rule matches a stray byte except `.`, which hands
                        // it to the parser as an unknown character token.
                        self.push(SyntaxKind::Error, i, i + 1);
                        self.pos = i + 1;
                    }
                }
                _ => self.punct(i),
            }
        }
    }

    fn punct(&mut self, i: usize) {
        use SyntaxKind::*;
        let s = self.src;
        let next = s.get(i + 1).copied();
        let (kind, len) = match (s[i], next) {
            (b'<', Some(b'=')) => (Le, 2),
            (b'>', Some(b'=')) => (Ge, 2),
            (b'=', Some(b'=')) => (EqEq, 2),
            (b'!', Some(b'=')) => (Ne, 2),
            (b'&', Some(b'&')) => (AndAnd, 2),
            (b'|', Some(b'|')) => (OrOr, 2),
            (b'<', Some(b'<')) => (Shl, 2),
            (b'>', Some(b'>')) => (Shr, 2),
            (b';', _) => (Semi, 1),
            (b'{', _) => (LBrace, 1),
            (b'}', _) => (RBrace, 1),
            (b'(', _) => (LParen, 1),
            (b')', _) => (RParen, 1),
            (b'[', _) => (LBrack, 1),
            (b']', _) => (RBrack, 1),
            (b',', _) => (Comma, 1),
            (b'=', _) => (Eq, 1),
            (b'!', _) => (Bang, 1),
            (b'#', _) => (Hash, 1),
            (b'%', _) => (Percent, 1),
            (b'*', _) => (Star, 1),
            (b'+', _) => (Plus, 1),
            (b'-', _) => (Minus, 1),
            (b'/', _) => (Slash, 1),
            (b'<', _) => (Lt, 1),
            (b'>', _) => (Gt, 1),
            (b'?', _) => (Question, 1),
            (b':', _) => (Colon, 1),
            (b'.', _) => (Dot, 1),
            (b'^', _) => (Caret, 1),
            (b'&', _) => (Amp, 1),
            (b'|', _) => (Pipe, 1),
            (b'~', _) => (Tilde, 1),
            (0x03, _) => (Eot, 1),
            _ => (Error, 1),
        };
        self.push(kind, i, i + len);
        self.pos = i + len;
    }

    fn ident(&mut self, i: usize) {
        let s = self.src;
        let mut j = i;
        let mut ascii = true;
        // The first unit is IDSTART or a UNICODEID sequence; the rest are
        // IDREST or UNICODEID (digits cannot start, the caller ensured it).
        loop {
            if j < s.len() && (is_ident_rest(s[j]) || (j == i && s[j] == b'$')) {
                j += 1;
                continue;
            }
            let u = unicode_id_len(s, j);
            if u > 0 {
                ascii = false;
                j += u;
                continue;
            }
            break;
        }
        if !ascii {
            // Non-ASCII identifiers are experimental in OpenSCAD and rejected
            // unless the feature is enabled; the rejection reads the line
            // after the whole token has been scanned.
            self.diag(
                DiagCode::NonAsciiIdentifier,
                Severity::Error,
                "Parser error: Non-ASCII identifiers are experimental, enable the unicode-identifiers feature".into(),
                i,
                j,
                j,
            );
            self.push(SyntaxKind::Error, i, j);
            self.pos = j;
            return;
        }
        let text = &s[i..j];
        if text == b"include" || text == b"use" {
            let mut k = j;
            while matches!(s.get(k), Some(b' ' | b'\t' | b'\r' | b'\n')) {
                k += 1;
            }
            if s.get(k) == Some(&b'<') {
                self.directive(i, k + 1, text == b"include");
                return;
            }
        }
        let kind = SyntaxKind::keyword(text).unwrap_or(SyntaxKind::Ident);
        self.push(kind, i, j);
        self.pos = j;
    }

    /// `include <...>` / `use <...>`; `body` is just past the `<`.
    fn directive(&mut self, i: usize, body: usize, include: bool) {
        let s = self.src;
        let mut j = body;
        while j < s.len() && s[j] != b'>' {
            if s[j] == b'\n' || s[j] == b'\r' {
                let (code, msg) = if include {
                    (
                        DiagCode::NewlineInInclude,
                        "new lines in 'include<>'-statement is not defined - behavior may change in the future",
                    )
                } else {
                    (
                        DiagCode::NewlineInUse,
                        "new lines 'use<>'-statement is not defined - behavior may change in the future",
                    )
                };
                self.diag(code, Severity::Warning, msg.into(), j, j + 1, j + 1);
            }
            j += 1;
        }
        if j >= s.len() {
            let n = s.len();
            let (code, msg) = if include {
                (
                    DiagCode::UnterminatedInclude,
                    "Parser error: Unterminated include statement",
                )
            } else {
                (
                    DiagCode::UnterminatedUse,
                    "Parser error: Unterminated use statement",
                )
            };
            self.diag(code, Severity::Error, msg.into(), n, n, n);
            self.push(SyntaxKind::Error, i, n);
            self.pos = n;
            return;
        }
        let kind = if include {
            SyntaxKind::IncludeDirective
        } else {
            SyntaxKind::UseDirective
        };
        self.push(kind, i, j + 1);
        self.pos = j + 1;
    }

    fn string(&mut self, i: usize) {
        let mut warnings = Vec::new();
        let (end, terminated) = scan_string(self.src, i, None, &mut warnings);
        for w in warnings {
            self.diag(
                DiagCode::UndefinedEscape,
                Severity::Warning,
                "Undefined escape sequence".into(),
                w,
                w + 1,
                w,
            );
        }
        if terminated {
            self.push(SyntaxKind::String, i, end);
        } else {
            let n = self.src.len();
            self.diag(
                DiagCode::UnterminatedString,
                Severity::Error,
                "Parser error: Unterminated string".into(),
                n,
                n,
                n,
            );
            self.push(SyntaxKind::Error, i, n);
        }
        self.pos = end;
    }

    fn number(&mut self, i: usize) {
        let s = self.src;
        let (rule, len) = number_rule(s, i);
        let text = &s[i..i + len];
        // Only needed for messages; number tokens are ASCII.
        let t = || String::from_utf8_lossy(text).into_owned();
        match rule {
            NumRule::Hex => match parse_u64(&text[2..], 16) {
                None => self.diag(
                    DiagCode::HexTooLarge,
                    Severity::Warning,
                    format!("Hexadecimal constant \"{}\" too large", t()),
                    i,
                    i + len,
                    i,
                ),
                Some(v) if (v as f64) as u64 != v => self.imprecise(&t(), i, len),
                Some(_) => {}
            },
            NumRule::Int => match parse_u64(text, 10) {
                Some(v) if (v as f64) as u64 == v => {}
                _ => self.imprecise(&t(), i, len),
            },
            NumRule::Float => {
                if float_value(text).is_none() {
                    self.push(SyntaxKind::DroppedNumber, i, i + len);
                    self.pos = i + len;
                    return;
                }
            }
            NumRule::DigitIdent => {
                self.diag(
                    DiagCode::DigitIdentifier,
                    Severity::Deprecated,
                    format!("Variable names starting with digits (\"{}\") will be removed in future releases.", t()),
                    i,
                    i + len,
                    i,
                );
                self.push(SyntaxKind::Ident, i, i + len);
                self.pos = i + len;
                return;
            }
        }
        self.push(SyntaxKind::Number, i, i + len);
        self.pos = i + len;
    }

    fn imprecise(&mut self, t: &str, i: usize, len: usize) {
        self.diag(
            DiagCode::ImpreciseInteger,
            Severity::Warning,
            format!("Integer \"{t}\" cannot be represented precisely"),
            i,
            i + len,
            i,
        );
    }
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NumRule {
    Hex,
    Float,
    Int,
    DigitIdent,
}

/// Which of lexer.l's number-ish rules wins at `i`, and its length.
fn number_rule(s: &[u8], i: usize) -> (NumRule, usize) {
    let digits = |from: usize| {
        s[from.min(s.len())..]
            .iter()
            .take_while(|b| b.is_ascii_digit())
            .count()
    };
    let exp = |from: usize| -> usize {
        // [Ee][+-]?{D}+
        if !matches!(s.get(from), Some(b'e' | b'E')) {
            return 0;
        }
        let mut k = from + 1;
        if matches!(s.get(k), Some(b'+' | b'-')) {
            k += 1;
        }
        let d = digits(k);
        if d == 0 { 0 } else { k + d - from }
    };
    let d1 = digits(i);
    let mut best = (NumRule::Int, 0usize);
    let mut offer = |rule: NumRule, len: usize| {
        if len > best.1 {
            best = (rule, len);
        }
    };
    // Rules in lexer.l order, so a later rule wins only when strictly longer.
    if d1 > 0 && s[i] == b'0' && s.get(i + 1) == Some(&b'x') {
        let h = s[i + 2..]
            .iter()
            .take_while(|b| b.is_ascii_hexdigit())
            .count();
        if h > 0 {
            offer(NumRule::Hex, 2 + h);
        }
    }
    let mut float = 0;
    if d1 > 0 {
        let e = exp(i + d1);
        if e > 0 {
            float = d1 + e;
        }
    }
    if s.get(i + d1) == Some(&b'.') {
        let d2 = digits(i + d1 + 1);
        if d2 > 0 || d1 > 0 {
            let base = d1 + 1 + d2;
            float = float.max(base + exp(i + base));
        }
    }
    if float > 0 {
        offer(NumRule::Float, float);
    }
    if d1 > 0 {
        offer(NumRule::Int, d1);
        let id = s[i..].iter().take_while(|&&b| is_ident_rest(b)).count();
        offer(NumRule::DigitIdent, id);
    }
    best
}

fn parse_u64(digits: &[u8], radix: u32) -> Option<u64> {
    let mut v: u64 = 0;
    for &d in digits {
        let x = (d as char).to_digit(radix)? as u64;
        v = v.checked_mul(radix as u64)?.checked_add(x)?;
    }
    Some(v)
}

/// `boost::lexical_cast<double>`, which fails (and OpenSCAD then drops the
/// token) when the value overflows or underflows: `1e400`, `1e-400` and even
/// subnormal results such as `1e-310` are rejected, `0e999` is fine.
fn float_value(text: &[u8]) -> Option<f64> {
    let t = std::str::from_utf8(text).ok()?;
    let v: f64 = t.parse().ok()?;
    if v.is_infinite() {
        return None;
    }
    if v == 0.0 || v.abs() < f64::MIN_POSITIVE {
        let mantissa = t.split(['e', 'E']).next().unwrap_or("");
        if mantissa.bytes().any(|b| (b'1'..=b'9').contains(&b)) {
            return None;
        }
    }
    Some(v)
}

/// The value of a `Number` token, as OpenSCAD's scanner computes it.
pub fn number_value(text: &[u8]) -> f64 {
    if text.len() > 2 && text[0] == b'0' && text[1] == b'x' {
        // strtoull saturates on overflow.
        return parse_u64(&text[2..], 16).unwrap_or(u64::MAX) as f64;
    }
    if text.iter().all(u8::is_ascii_digit) {
        return match parse_u64(text, 10) {
            Some(v) => v as f64,
            // strtoull overflowed; lexical_cast<double> is used instead, and
            // if that fails too the saturated integer stays.
            None => float_value(text).unwrap_or(u64::MAX as f64),
        };
    }
    float_value(text).unwrap_or(0.0)
}

fn hex_val(b: u8) -> Option<u32> {
    (b as char).to_digit(16)
}

/// Scan a string literal starting at the opening quote `i`. Returns the end
/// offset (past the closing quote, or the end of input) and whether it was
/// terminated. Appends decoded bytes to `out` when given and the offsets of
/// undefined escape sequences to `warnings`.
pub(crate) fn scan_string(
    s: &[u8],
    i: usize,
    mut out: Option<&mut Vec<u8>>,
    warnings: &mut Vec<usize>,
) -> (usize, bool) {
    let mut j = i + 1;
    let hex_run = |from: usize, n: usize| -> Option<u32> {
        let mut v = 0u32;
        for k in 0..n {
            v = v * 16 + hex_val(*s.get(from + k)?)?;
        }
        Some(v)
    };
    macro_rules! emit {
        ($($b:expr),*) => {
            if let Some(o) = out.as_deref_mut() {
                $(o.extend_from_slice($b);)*
            }
        };
    }
    while j < s.len() {
        match s[j] {
            b'"' => return (j + 1, true),
            b'\n' => j += 1, // counted as a line, not added to the value
            b'\\' => match s.get(j + 1) {
                Some(b'\n') => j += 2,
                Some(b'\r') if s.get(j + 2) == Some(&b'\n') => j += 3,
                Some(b'n') => {
                    emit!(b"\n");
                    j += 2;
                }
                Some(b't') => {
                    emit!(b"\t");
                    j += 2;
                }
                Some(b'r') => {
                    emit!(b"\r");
                    j += 2;
                }
                Some(b'\\') => {
                    emit!(b"\\");
                    j += 2;
                }
                Some(b'"') => {
                    emit!(b"\"");
                    j += 2;
                }
                Some(b'x')
                    if matches!(s.get(j + 2), Some(b'0'..=b'7'))
                        && s.get(j + 3).and_then(|&b| hex_val(b)).is_some() =>
                {
                    let v = hex_run(j + 2, 2).unwrap_or(0) as u8;
                    emit!(&[if v == 0 { b' ' } else { v }]);
                    j += 4;
                }
                Some(b'u') if hex_run(j + 2, 4).is_some() => {
                    emit!(codepoint_utf8(hex_run(j + 2, 4).unwrap_or(0)).as_bytes());
                    j += 6;
                }
                Some(b'U') if hex_run(j + 2, 6).is_some() => {
                    emit!(codepoint_utf8(hex_run(j + 2, 6).unwrap_or(0)).as_bytes());
                    j += 8;
                }
                _ => {
                    warnings.push(j);
                    j += 1;
                }
            },
            _ => {
                let k = j + 1;
                emit!(&s[j..k]);
                j = k;
            }
        }
    }
    (s.len(), false)
}

/// `str_utf8_wrapper(uint32_t)`: NUL, surrogates and values past U+10FFFF
/// become a single space.
fn codepoint_utf8(cp: u32) -> String {
    match char::from_u32(cp) {
        Some(c) if cp != 0 => c.to_string(),
        _ => " ".to_string(),
    }
}

/// The value of a `String` token (with quotes), decoded. OpenSCAD strings
/// are byte strings: bytes that are not UTF-8 (a Latin-1 file) are kept.
pub fn string_value(text: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(text.len());
    let mut w = Vec::new();
    scan_string(text, 0, Some(&mut out), &mut w);
    out
}

/// The path parts of a `use <...>` / `include <...>` directive, following
/// the `cond_include` / `cond_use` scanner states. Tabs are matched by no
/// rule there and vanish; each run of other characters replaces the
/// previous one.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DirectivePath {
    /// For `include`: the part up to and including the last `/`.
    pub dir: Option<String>,
    /// The file name. `None` when the brackets held no name, in which case
    /// OpenSCAD reuses the name left over from the previous directive.
    pub name: Option<String>,
}

pub fn directive_path(text: &[u8], include: bool) -> DirectivePath {
    let lt = text
        .iter()
        .position(|&b| b == b'<')
        .map_or(text.len(), |p| p + 1);
    let body = &text[lt..text.len().saturating_sub(1).max(lt)];
    let mut out = DirectivePath::default();
    let lossy = |b: &[u8]| String::from_utf8_lossy(b).into_owned();
    for run in body.split(|&b| matches!(b, b'\t' | b'\r' | b'\n')) {
        if run.is_empty() {
            continue;
        }
        if include {
            match run.iter().rposition(|&b| b == b'/') {
                Some(p) => {
                    out.dir = Some(lossy(&run[..=p]));
                    if p + 1 < run.len() {
                        out.name = Some(lossy(&run[p + 1..]));
                    }
                }
                None => out.name = Some(lossy(run)),
            }
        } else {
            out.name = Some(lossy(run));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use SyntaxKind::*;

    fn kinds(src: &str) -> Vec<(SyntaxKind, std::string::String)> {
        let l = lex(src.as_bytes(), FileId(0));
        l.tokens
            .iter()
            .filter(|t| t.kind != Whitespace)
            .map(|t| {
                (
                    t.kind,
                    std::string::String::from_utf8_lossy(
                        &src.as_bytes()[t.start as usize..t.end() as usize],
                    )
                    .into_owned(),
                )
            })
            .collect()
    }

    fn k(src: &str) -> Vec<SyntaxKind> {
        kinds(src).into_iter().map(|(k, _)| k).collect()
    }

    #[test]
    fn lossless() {
        let src = "a = 1; // c\n/* b */ include <x.scad>\n\"s\\n\" \u{a0}\u{feff}é@";
        let l = lex(src.as_bytes(), FileId(0));
        let mut pos = 0;
        for t in &l.tokens {
            assert_eq!(t.start, pos);
            pos = t.end();
        }
        assert_eq!(pos as usize, src.len());
    }

    #[test]
    fn number_rule_longest_match() {
        assert_eq!(kinds("2d"), [(Ident, "2d".into())]);
        assert_eq!(kinds("123"), [(Number, "123".into())]);
        assert_eq!(kinds("1e5"), [(Number, "1e5".into())]);
        assert_eq!(kinds("1e5x"), [(Ident, "1e5x".into())]);
        assert_eq!(kinds("0x1F"), [(Number, "0x1F".into())]);
        assert_eq!(kinds("0X1F"), [(Ident, "0X1F".into())]);
        assert_eq!(kinds("0x1G"), [(Ident, "0x1G".into())]);
        assert_eq!(
            kinds("1.5.3"),
            [(Number, "1.5".into()), (Number, ".3".into())]
        );
        assert_eq!(kinds("5."), [(Number, "5.".into())]);
        assert_eq!(kinds(".5e3"), [(Number, ".5e3".into())]);
        assert_eq!(kinds("1.5e"), [(Number, "1.5".into()), (Ident, "e".into())]);
        assert_eq!(
            kinds("a.b"),
            [(Ident, "a".into()), (Dot, ".".into()), (Ident, "b".into())]
        );
        assert_eq!(k("1e400"), [DroppedNumber]);
        assert_eq!(k("1e-310"), [DroppedNumber]);
        assert_eq!(k("0e999"), [Number]);
    }

    #[test]
    fn number_values() {
        assert_eq!(number_value(b"0x10"), 16.0);
        assert_eq!(number_value(b"01.5"), 1.5);
        assert_eq!(number_value(b"5."), 5.0);
        assert_eq!(number_value(b".5e3"), 500.0);
        assert_eq!(
            number_value(b"123456789012345678901234567890"),
            1.2345678901234568e29
        );
        assert_eq!(number_value(b"2.2250738585072014e-308"), f64::MIN_POSITIVE);
    }

    #[test]
    fn number_warnings() {
        let l = lex(
            b"x = 123456789012345678901234567890 + 9007199254740993 + 2d;",
            FileId(0),
        );
        let codes: Vec<_> = l.diags.iter().map(|d| d.code).collect();
        assert_eq!(
            codes,
            [
                DiagCode::ImpreciseInteger,
                DiagCode::ImpreciseInteger,
                DiagCode::DigitIdentifier
            ]
        );
        assert_eq!(
            l.diags[2].message,
            "Variable names starting with digits (\"2d\") will be removed in future releases."
        );
    }

    #[test]
    fn keywords_and_directives() {
        assert_eq!(
            k("module modules include<a> use <b> use=1 include x"),
            [
                KwModule,
                Ident,
                IncludeDirective,
                UseDirective,
                Ident,
                Eq,
                Number,
                Ident,
                Ident
            ]
        );
        assert_eq!(k("include\n <a/b>"), [IncludeDirective]);
        let l = lex(b"include <foo\n", FileId(0));
        assert_eq!(l.diags.len(), 2);
        assert_eq!(l.diags[1].code, DiagCode::UnterminatedInclude);
    }

    #[test]
    fn operators() {
        assert_eq!(
            k("<= >= == != && || << >> <<= ! ~ ^ \u{3}"),
            [
                Le, Ge, EqEq, Ne, AndAnd, OrOr, Shl, Shr, Shl, Eq, Bang, Tilde, Caret, Eot
            ]
        );
        assert_eq!(k("$fn a$b"), [Ident, Ident, Ident]);
        assert_eq!(k("@ ' \\"), [Error, Error, Error]);
    }

    #[test]
    fn whitespace_forms() {
        assert_eq!(k("a\u{a0}b\u{feff}c"), [Ident, Ident, Ident]);
        assert_eq!(lex(b"a\xa0b", FileId(0)).tokens.len(), 3);
        assert_eq!(k("\u{e9}"), [Error]);
        assert_eq!(k("a\u{e9}"), [Error]);
        let l = lex(b"x\xc3(", FileId(0));
        assert_eq!(
            l.tokens.iter().map(|t| t.kind).collect::<Vec<_>>(),
            [Ident, Error, LParen]
        );
    }

    #[test]
    fn strings() {
        let sv = |b: &[u8]| std::string::String::from_utf8(string_value(b)).unwrap();
        assert_eq!(sv(br#""a\tb\"c\\""#), "a\tb\"c\\");
        assert_eq!(sv(b"\"a\nb\""), "ab");
        assert_eq!(sv(b"\"a\\\nb\""), "ab");
        assert_eq!(sv(b"\"\\x41\\x00\\x80\""), "A x80");
        assert_eq!(sv(b"\"\\u00e9\\U01F600\\ud800\""), "\u{e9}\u{1F600} ");
        assert_eq!(sv(b"\"\\q\""), "q");
        assert_eq!(sv(b"\"a\rb\""), "a\rb");
        assert_eq!(string_value(b"\"a\xa0\""), b"a\xa0");
        let l = lex(b"\"\\q\"", FileId(0));
        assert_eq!(l.diags[0].code, DiagCode::UndefinedEscape);
        let l = lex(b"a = \"abc", FileId(0));
        assert_eq!(l.tokens.last().unwrap().kind, Error);
        assert_eq!(l.diags[0].code, DiagCode::UnterminatedString);
    }

    #[test]
    fn comments_and_nul() {
        assert_eq!(
            k("/*/ */ a // b\nc"),
            [BlockComment, Ident, LineComment, Ident]
        );
        assert_eq!(k("/* x"), [Error]);
        assert_eq!(k("a\0b c"), [Ident, Ignored]);
    }

    #[test]
    fn directive_paths() {
        let p = directive_path(b"include <a/b/c.scad>", true);
        assert_eq!(
            p,
            DirectivePath {
                dir: Some("a/b/".into()),
                name: Some("c.scad".into())
            }
        );
        assert_eq!(
            directive_path(b"include <test/>", true),
            DirectivePath {
                dir: Some("test/".into()),
                name: None
            }
        );
        assert_eq!(directive_path(b"use <>", false), DirectivePath::default());
        assert_eq!(
            directive_path(b"use <a/b c>", false).name.as_deref(),
            Some("a/b c")
        );
        assert_eq!(
            directive_path(b"include <x\ty>", true).name.as_deref(),
            Some("y")
        );
    }
}
