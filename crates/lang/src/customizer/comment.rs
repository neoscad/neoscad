//! The customizer's parameter-comment language: `// [0:100]`,
//! `// [a:Label, b:Other]`, `// 23`, `// free text`.
//!
//! A port of src/core/customizer/comment_lexer.l and comment_parser.y. The
//! Bison grammar is ambiguous and relies on its default conflict
//! resolution (shift wins), which gives these rules:
//!
//! - `[n : m]` and `[n : s : m]` with numbers are ranges; `[n : m, ...]`
//!   is a list of labelled pairs;
//! - a run of words and numbers containing at least one word is one text
//!   value, its parts joined by single spaces (numbers formatted with
//!   C++'s default `%g`); two numbers with no word are an error;
//! - parentheses are ignored, and anything unparsable yields no annotation
//!   (the caller then uses `""`).

use crate::ast::{Ast, ExprId, ExprKind};
use crate::number::fmt_g;
use crate::source::Span;

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Num(f64),
    Word(Vec<u8>),
    LBrack,
    RBrack,
    Comma,
    Colon,
}

fn is_word_byte(b: u8) -> bool {
    // `[^(\[ \] \, \" \:)]`: everything but these, and note that tabs and
    // newlines are word characters.
    !matches!(b, b'(' | b'[' | b' ' | b']' | b',' | b'"' | b':' | b')')
}

/// Length of the longest NUM match at `i`, or 0.
fn num_len(s: &[u8], i: usize) -> usize {
    let digits = |from: usize| s.get(from..).map_or(0, |t| t.iter().take_while(|b| b.is_ascii_digit()).count());
    let exp = |from: usize| -> usize {
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
    let mut p = i;
    if matches!(s.get(p), Some(b'+' | b'-')) {
        p += 1;
    }
    let d1 = digits(p);
    let mut best = 0;
    if d1 > 0 {
        best = p + d1 + exp(p + d1) - i;
    }
    if s.get(p + d1) == Some(&b'.') {
        let d2 = digits(p + d1 + 1);
        if d2 > 0 || d1 > 0 {
            let base = p + d1 + 1 + d2;
            best = best.max(base + exp(base) - i);
        }
    }
    best
}

fn lex(s: &[u8]) -> Vec<Tok> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < s.len() {
        let nl = num_len(s, i);
        let wl = s[i..].iter().take_while(|&&b| is_word_byte(b)).count();
        if nl > 0 && nl >= wl {
            // lexical_cast<double> failures (overflow) make the rule not
            // return, so the text is skipped.
            if let Ok(v) = std::str::from_utf8(&s[i..i + nl]).unwrap_or("x").parse::<f64>()
                && v.is_finite()
            {
                out.push(Tok::Num(v));
            }
            i += nl;
            continue;
        }
        if wl > 1 {
            out.push(Tok::Word(s[i..i + wl].to_vec()));
            i += wl;
            continue;
        }
        match s[i] {
            b'[' => out.push(Tok::LBrack),
            b']' => out.push(Tok::RBrack),
            b',' => out.push(Tok::Comma),
            b':' => out.push(Tok::Colon),
            b' ' | b'\t' => {}
            b'"' => {
                let (word, end) = string(s, i + 1);
                out.push(Tok::Word(word));
                i = end;
                continue;
            }
            _ if wl == 1 => out.push(Tok::Word(s[i..i + 1].to_vec())),
            _ => {} // `(` and `)`: matched by `.` and ignored
        }
        i += 1;
    }
    out
}

/// A quoted string starting after the quote; ends at a quote or the end.
fn string(s: &[u8], mut i: usize) -> (Vec<u8>, usize) {
    let mut out = Vec::new();
    while i < s.len() {
        match s[i] {
            b'"' => return (out, i + 1),
            b'\\' => {
                let e = match s.get(i + 1) {
                    Some(b'n') => Some(b'\n'),
                    Some(b't') => Some(b'\t'),
                    Some(b'r') => Some(b'\r'),
                    Some(b'\\') => Some(b'\\'),
                    Some(b'"') => Some(b'"'),
                    _ => None,
                };
                match e {
                    Some(c) => {
                        out.push(c);
                        i += 2;
                    }
                    // No rule matches: flex echoes the backslash and drops it.
                    None => i += 1,
                }
            }
            b'\n' => i += 1,
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    (out, i)
}

#[derive(Debug)]
enum Item {
    Num(f64),
    Word(Vec<u8>),
}

struct P<'a> {
    toks: &'a [Tok],
    pos: usize,
}

impl P<'_> {
    fn peek(&self, n: usize) -> Option<&Tok> {
        self.toks.get(self.pos + n)
    }

    /// A maximal run of numbers and words: one number, or text if it holds
    /// at least one word.
    fn item(&mut self) -> Option<Item> {
        let start = self.pos;
        while matches!(self.peek(0), Some(Tok::Num(_) | Tok::Word(_))) {
            self.pos += 1;
        }
        let run = &self.toks[start..self.pos];
        if run.is_empty() {
            return None;
        }
        if !run.iter().any(|t| matches!(t, Tok::Word(_))) {
            return match run {
                [Tok::Num(v)] => Some(Item::Num(*v)),
                _ => None,
            };
        }
        let mut s = Vec::new();
        for (k, t) in run.iter().enumerate() {
            if k > 0 {
                s.push(b' ');
            }
            match t {
                Tok::Num(v) => s.extend_from_slice(fmt_g(*v).as_bytes()),
                Tok::Word(w) => s.extend_from_slice(w),
                _ => {}
            }
        }
        Some(Item::Word(s))
    }
}

fn lit(ast: &mut Ast, item: Item) -> ExprId {
    match item {
        Item::Num(v) => ast.add(ExprKind::Number(v), Span::default()),
        Item::Word(w) => ast.add(ExprKind::String(w.into()), Span::default()),
    }
}

/// Parse a parameter comment (the text after `//`) into an expression in
/// `ast`, or `None` when the grammar rejects it.
pub fn parse(comment: &[u8], ast: &mut Ast) -> Option<ExprId> {
    let toks = lex(comment);
    let mut p = P { toks: &toks, pos: 0 };
    let e = match p.peek(0)? {
        Tok::LBrack => {
            p.pos += 1;
            // `[n : m]` / `[n : s : m]`: the numbers must be bare (a number
            // followed by a word starts a text value instead).
            let is_num = |t: Option<&Tok>| matches!(t, Some(Tok::Num(_)));
            if is_num(p.peek(0))
                && p.peek(1) == Some(&Tok::Colon)
                && is_num(p.peek(2))
                && matches!(p.peek(3), Some(Tok::RBrack | Tok::Colon))
            {
                let num = |t: Option<&Tok>| if let Some(Tok::Num(v)) = t { *v } else { 0.0 };
                let a = num(p.peek(0));
                let b = num(p.peek(2));
                let begin = ast.add(ExprKind::Number(a), Span::default());
                if p.peek(3) == Some(&Tok::RBrack) {
                    p.pos += 4;
                    let end = ast.add(ExprKind::Number(b), Span::default());
                    ast.add(ExprKind::Range { begin, step: None, end }, Span::default())
                } else {
                    if !(is_num(p.peek(4)) && p.peek(5) == Some(&Tok::RBrack)) {
                        return None;
                    }
                    let c = num(p.peek(4));
                    p.pos += 6;
                    let step = ast.add(ExprKind::Number(b), Span::default());
                    let end = ast.add(ExprKind::Number(c), Span::default());
                    ast.add(ExprKind::Range { begin, step: Some(step), end }, Span::default())
                }
            } else {
                let mut values = Vec::new();
                loop {
                    let first = p.item()?;
                    let value = if p.peek(0) == Some(&Tok::Colon) {
                        p.pos += 1;
                        let second = p.item()?;
                        let a = lit(ast, first);
                        let b = lit(ast, second);
                        ast.add(ExprKind::Vector(vec![a, b]), Span::default())
                    } else {
                        lit(ast, first)
                    };
                    values.push(value);
                    match p.peek(0) {
                        Some(Tok::Comma) => p.pos += 1,
                        Some(Tok::RBrack) => {
                            p.pos += 1;
                            break;
                        }
                        _ => return None,
                    }
                }
                ast.add(ExprKind::Vector(values), Span::default())
            }
        }
        _ => {
            let item = p.item()?;
            lit(ast, item)
        }
    };
    (p.pos == toks.len()).then_some(e)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dump::expr_to_string;

    fn show(c: &str) -> Option<String> {
        let mut ast = Ast::default();
        let e = parse(c.as_bytes(), &mut ast)?;
        Some(expr_to_string(&ast, e))
    }

    #[test]
    fn parameter_comments() {
        assert_eq!(show(" [0, 1, 2, 3]").as_deref(), Some("[0, 1, 2, 3]"));
        assert_eq!(show(" [10:L, 20:M, 30:L]").as_deref(), Some("[[10, \"L\"], [20, \"M\"], [30, \"L\"]]"));
        assert_eq!(show(" [S:Small, M:Medium]").as_deref(), Some("[[\"S\", \"Small\"], [\"M\", \"Medium\"]]"));
        assert_eq!(show(" [10:100]").as_deref(), Some("[10 : 100]"));
        assert_eq!(show("[0:5:100]").as_deref(), Some("[0 : 5 : 100]"));
        assert_eq!(show("[1:2, 3]").as_deref(), Some("[[1, 2], 3]"));
        assert_eq!(show("23").as_deref(), Some("23"));
        assert_eq!(show("comment").as_deref(), Some("\"comment\""));
        assert_eq!(show("any thing").as_deref(), Some("\"any thing\""));
        assert_eq!(show("5 mm 1.5").as_deref(), Some("\"5 mm 1.5\""));
        assert_eq!(show("(max)").as_deref(), Some("\"max\""));
        assert_eq!(show("\"a b\"").as_deref(), Some("\"a b\""));
        assert_eq!(show("1e6 x").as_deref(), Some("\"1e+06 x\""));
        assert_eq!(show("5 6"), None);
        assert_eq!(show("a, b"), None);
        assert_eq!(show("[1:2:3, 4]"), None);
        assert_eq!(show("[a:b:c]"), None);
        assert_eq!(show("   "), None);
    }
}
