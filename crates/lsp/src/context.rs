//! What the cursor is in, read from the tokens around it rather than the
//! tree: while a call is being typed its tree is an error node, but its
//! tokens (`cube(size=10, ce`) still say which call and which argument.

use lang::syntax::SyntaxKind as K;
use lang::syntax::lexer::Token;

use crate::world::Analyzed;

/// The significant tokens of a file (trivia dropped).
fn significant(f: &Analyzed) -> impl DoubleEndedIterator<Item = &Token> {
    f.program
        .cst
        .tokens()
        .iter()
        .filter(|t| !t.kind.is_trivia())
}

fn is_word(k: K) -> bool {
    k == K::Ident
        || matches!(
            k,
            K::KwFor | K::KwLet | K::KwEach | K::KwIf | K::KwEcho | K::KwAssert | K::KwElse
        )
        || matches!(
            k,
            K::KwTrue | K::KwFalse | K::KwUndef | K::KwModule | K::KwFunction
        )
}

/// The identifier or keyword at `offset`, or ending there (the cursor
/// just after a name is on it).
pub fn word_at(f: &Analyzed, offset: u32) -> Option<(u32, u32)> {
    let toks = f.program.cst.tokens();
    let i = toks.partition_point(|t| t.end() < offset);
    toks[i..]
        .iter()
        .take(2)
        .find(|t| t.start <= offset && offset <= t.end() && is_word(t.kind))
        .map(|t| (t.start, t.end()))
}

/// Whether `offset` is inside a comment or a string, where there is
/// nothing to complete.
pub fn in_comment_or_string(f: &Analyzed, offset: u32) -> bool {
    let toks = f.program.cst.tokens();
    let i = toks.partition_point(|t| t.end() < offset);
    toks[i..].iter().take(2).any(|t| {
        let inside = t.start < offset && offset < t.end();
        match t.kind {
            K::String | K::BlockComment => inside,
            // A line comment runs to the end of its line, cursor included.
            K::LineComment => t.start < offset && offset <= t.end(),
            // An unterminated string or comment.
            K::Error => {
                t.start < offset
                    && matches!(f.text().get(t.start as usize), Some(b'"') | Some(b'/'))
            }
            _ => false,
        }
    })
}

/// The index of the directive whose text contains `offset`.
pub fn directive_at(f: &Analyzed, offset: u32) -> Option<usize> {
    f.index
        .directives
        .iter()
        .position(|d| d.span.0 <= offset && offset <= d.span.1)
}

/// The start of the name being typed at `offset` (letters, digits, `_`
/// and a leading `$`).
pub fn prefix_start(f: &Analyzed, offset: u32) -> u32 {
    let t = f.text();
    let mut i = (offset as usize).min(t.len());
    while i > 0 && (t[i - 1].is_ascii_alphanumeric() || t[i - 1] == b'_' || t[i - 1] == b'$') {
        i -= 1;
    }
    i as u32
}

/// The last significant token ending at or before `offset`.
pub fn previous(f: &Analyzed, offset: u32) -> Option<&Token> {
    significant(f).rev().find(|t| t.end() <= offset)
}

/// Whether a statement (a module instantiation, an assignment, a
/// definition) starts at `offset`, rather than an expression.
pub fn statement_position(f: &Analyzed, offset: u32) -> bool {
    let mut at = offset;
    loop {
        let Some(t) = previous(f, at) else {
            return true;
        };
        match t.kind {
            // After `)` only a statement can follow a name: the child of
            // `translate(...)`, `if (...)`, `for (...)`, a module's body.
            K::Semi | K::LBrace | K::RBrace | K::RParen | K::KwElse | K::Eot => return true,
            // A modifier (`#cube`), unless it is an operator (`a * b`).
            K::Bang | K::Hash | K::Percent | K::Star => at = t.start,
            _ => return false,
        }
    }
}

/// The call around the cursor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Call {
    pub name: String,
    pub name_span: (u32, u32),
    /// Offset of its `(`.
    pub open: u32,
    /// The argument the cursor is in, counting from 0.
    pub arg: usize,
    /// The argument's name, when it is `name = ...`.
    pub named: Option<String>,
    /// Nothing but the name being typed stands before the cursor in its
    /// argument (where a parameter name may be written).
    pub arg_start: bool,
    /// A module instantiation (a statement), not a function call.
    pub statement: bool,
}

/// The innermost call whose argument list holds `offset`.
pub fn enclosing_call(f: &Analyzed, offset: u32) -> Option<Call> {
    let text = f.text();
    let word =
        |t: &Token| String::from_utf8_lossy(&text[t.start as usize..t.end() as usize]).into_owned();
    // The name being typed at the cursor is not part of what stands
    // before it.
    let typed = prefix_start(f, offset);
    let toks: Vec<&Token> = significant(f)
        .filter(|t| t.end() <= offset && !(t.end() == offset && t.start >= typed && typed < offset))
        .collect();
    let mut depth = 0u32;
    let mut commas = 0usize;
    let mut named: Option<String> = None;
    // Tokens of the current argument seen so far, and whether its start
    // (the call's `(` or a comma) has been reached.
    let mut content = 0usize;
    let mut settled: Option<bool> = None;
    let mut i = toks.len();
    while i > 0 {
        i -= 1;
        let t = toks[i];
        let top = depth == 0;
        match t.kind {
            K::RParen | K::RBrack | K::RBrace if !(top && t.kind == K::RBrace) => depth += 1,
            K::RBrace | K::LBrace | K::Semi if top => return None,
            K::LBrace => depth -= 1,
            K::LBrack | K::LParen if !top => depth -= 1,
            K::LParen if is_callee(toks.get(i.wrapping_sub(1))) => {
                let c = toks[i - 1];
                if c.kind == K::KwIf
                    || i.checked_sub(2)
                        .is_some_and(|j| matches!(toks[j].kind, K::KwModule | K::KwFunction))
                {
                    // `if (`, and the parameter lists of `module m(` and
                    // `function f(`.
                    return None;
                }
                return Some(Call {
                    name: word(c),
                    name_span: (c.start, c.end()),
                    open: t.start,
                    arg: commas,
                    named,
                    arg_start: settled.unwrap_or(content == 0),
                    statement: statement_position(f, c.start),
                });
            }
            // A vector or a parenthesised expression inside an argument:
            // what came after it belongs to it, not to the call.
            K::LBrack | K::LParen => {
                commas = 0;
                named = None;
                settled = None;
                content += 1;
            }
            K::Comma if top => {
                commas += 1;
                if settled.is_none() {
                    settled = Some(content == 0);
                }
            }
            K::Eq if top && settled.is_none() => {
                if let Some(j) = i.checked_sub(1)
                    && toks[j].kind == K::Ident
                {
                    named = Some(word(toks[j]));
                }
                content += 1;
            }
            _ => content += 1,
        }
    }
    None
}

/// Whether a token can name a called module or function.
fn is_callee(t: Option<&&Token>) -> bool {
    t.is_some_and(|t| is_word(t.kind) && !matches!(t.kind, K::KwFunction | K::KwModule))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(src: &str) -> Analyzed {
        Analyzed::new("/t.scad".into(), src.as_bytes().to_vec())
    }

    fn call(src: &str) -> Option<Call> {
        let at = src.find('|').unwrap();
        let text = src.replace('|', "");
        enclosing_call(&file(&text), at as u32)
    }

    #[test]
    fn calls_and_arguments() {
        let c = call("cube(|").unwrap();
        assert_eq!(
            (c.name.as_str(), c.arg, c.arg_start, c.statement),
            ("cube", 0, true, true)
        );
        let c = call("cube([1, 2, 3], ce|").unwrap();
        assert_eq!((c.arg, c.arg_start, c.named), (1, true, None));
        let c = call("cube(size = [1, |").unwrap();
        assert_eq!(
            (c.arg, c.named.as_deref(), c.arg_start),
            (0, Some("size"), false)
        );
        let c = call("x = f(a, g(1), b + |").unwrap();
        assert_eq!(
            (c.name.as_str(), c.arg, c.statement, c.arg_start),
            ("f", 2, false, false)
        );
        let c = call("translate([1, 0, 0]) sphere(r = 2, $f|);").unwrap();
        assert_eq!((c.name.as_str(), c.arg, c.arg_start), ("sphere", 1, true));
        assert!(call("module m(a, |").is_none());
        assert!(call("cube(1); |").is_none());
        assert!(call("if (a|").is_none());
    }

    #[test]
    fn statement_positions() {
        let f = file("x = 1;\ntranslate([1, 0, 0]) cu;\n#sph\ny = a * b");
        let at = |s: &str| {
            f.text()
                .windows(s.len())
                .position(|w| w == s.as_bytes())
                .unwrap() as u32
        };
        assert!(statement_position(&f, 0));
        assert!(!statement_position(&f, at("1;")));
        assert!(statement_position(&f, at("cu")));
        assert!(statement_position(&f, at("sph")));
        assert!(!statement_position(&f, at("b")));
    }

    #[test]
    fn comments_and_strings() {
        let f = file("// note\nx = \"abc\";\n");
        assert!(in_comment_or_string(&f, 4));
        assert!(in_comment_or_string(&f, 13));
        assert!(in_comment_or_string(&f, 14));
        assert!(!in_comment_or_string(&f, 8));
        assert!(!in_comment_or_string(&f, 10));
    }
}
