//! Recursive-descent parser producing the lossless [`Cst`].
//!
//! It accepts exactly the language of OpenSCAD's `src/core/parser.y` and
//! reports its first error at the same token as that Bison parser: the
//! first token at which the input stops being a prefix of a valid program.
//! That is what makes the first error's line match OpenSCAD's.
//!
//! Unlike Bison it does not stop there. After an error it closes the nodes
//! that were open, wraps the tokens up to the next `;` or the end of the
//! enclosing block in an [`SyntaxKind::ErrorNode`] and continues, so an
//! editor can show every broken statement at once. Only the first error is
//! OpenSCAD-compatible; later ones depend on the recovery.
//!
//! The grammar is written the way parser.y resolves its ambiguities:
//!
//! - `let`, `assert`, `echo` and `function` expressions sit at the lowest
//!   precedence (`1 + let(a=1) a` is an error) and extend as far right as
//!   possible;
//! - `^` is right-associative and binds tighter than unary minus (`-2^2`
//!   is `-(2^2)`);
//! - `else` attaches to the nearest `if`;
//! - inside `[...]`, `let(...)` followed by a list-comprehension element (or
//!   a parenthesised one) is a comprehension `let`, otherwise it is a `let`
//!   expression; see [`Parser::element`].

use crate::syntax::SyntaxKind::{self, *};
use crate::syntax::cst::{Builder, Cst};
use crate::syntax::lexer::Token;

/// A syntax error at a token. The message is always Bison's "syntax error".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SyntaxError {
    /// Index into the parsed token list; `tokens.len()` means end of input.
    pub token: u32,
}

#[derive(Debug)]
pub struct Parse {
    pub cst: Cst,
    pub errors: Vec<SyntaxError>,
}

/// Parse a token stream (trivia included, as produced by the lexer or the
/// include-splicing loader).
pub fn parse(tokens: Vec<Token>) -> Parse {
    let mut kinds = Vec::with_capacity(tokens.len() / 2 + 1);
    let mut raw = Vec::with_capacity(tokens.len() / 2 + 1);
    for (i, t) in tokens.iter().enumerate() {
        if !t.kind.is_trivia() {
            kinds.push(t.kind);
            raw.push(i as u32);
        }
    }
    kinds.push(Eof);
    raw.push(tokens.len() as u32);

    let mut p = Parser { kinds, raw, pos: 0, events: Vec::with_capacity(tokens.len()), open: Vec::new(), errors: Vec::new() };
    p.source_file();
    let errors = p.errors.iter().map(|&sig| SyntaxError { token: p.raw[sig as usize] }).collect();
    let cst = build(tokens, p.events);
    Parse { cst, errors }
}

#[derive(Debug, Clone, Copy)]
enum Event {
    Start { kind: SyntaxKind, forward_parent: u32 },
    Token,
    Finish,
    /// A start event already consumed through a forward-parent chain.
    Taken,
}

/// The error has been recorded; unwind to the nearest statement list.
#[derive(Debug)]
struct Stop;

type PResult<T = ()> = Result<T, Stop>;

#[must_use]
#[derive(Debug)]
struct Marker(u32);

#[derive(Debug, Clone, Copy)]
struct Done(u32);

/// Whether an element inside `[...]` turned out to be a list-comprehension
/// clause or a plain expression.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Elem {
    Lc,
    Expr,
}

struct Parser {
    /// Significant token kinds, ending with `Eof`.
    kinds: Vec<SyntaxKind>,
    /// Raw token index of each significant token.
    raw: Vec<u32>,
    pos: usize,
    events: Vec<Event>,
    /// Start events of nodes not yet completed.
    open: Vec<u32>,
    /// Significant-token positions of recorded errors.
    errors: Vec<u32>,
}

impl Parser {
    // --- token access ---------------------------------------------------

    fn nth(&self, n: usize) -> SyntaxKind {
        self.kinds.get(self.pos + n).copied().unwrap_or(Eof)
    }

    fn cur(&self) -> SyntaxKind {
        self.nth(0)
    }

    fn at(&self, k: SyntaxKind) -> bool {
        self.cur() == k
    }

    fn bump(&mut self) {
        debug_assert!(self.cur() != Eof);
        self.events.push(Event::Token);
        self.pos += 1;
    }

    fn eat(&mut self, k: SyntaxKind) -> bool {
        if self.at(k) {
            self.bump();
            true
        } else {
            false
        }
    }

    fn expect(&mut self, k: SyntaxKind) -> PResult {
        if self.eat(k) { Ok(()) } else { self.error() }
    }

    fn error<T>(&mut self) -> PResult<T> {
        self.errors.push(self.pos as u32);
        Err(Stop)
    }

    // --- markers --------------------------------------------------------

    fn start(&mut self) -> Marker {
        let pos = self.events.len() as u32;
        self.events.push(Event::Start { kind: Tombstone, forward_parent: 0 });
        self.open.push(pos);
        Marker(pos)
    }

    fn complete(&mut self, m: Marker, kind: SyntaxKind) -> Done {
        let top = self.open.pop();
        debug_assert_eq!(top, Some(m.0), "markers complete in LIFO order");
        if let Event::Start { kind: k, .. } = &mut self.events[m.0 as usize] {
            *k = kind;
        }
        self.events.push(Event::Finish);
        Done(m.0)
    }

    /// Start a node that will enclose the already completed `d`.
    fn precede(&mut self, d: Done) -> Marker {
        let m = self.start();
        if let Event::Start { forward_parent, .. } = &mut self.events[d.0 as usize] {
            *forward_parent = m.0 - d.0;
        }
        m
    }

    /// Close every node opened since `depth` and skip to a statement
    /// boundary: past the next `;` or balanced `{...}`, or up to the `}`
    /// that closes the enclosing block.
    fn recover(&mut self, depth: usize, in_block: bool) {
        while self.open.len() > depth {
            let pos = self.open.pop().unwrap_or(0) as usize;
            if let Event::Start { kind, .. } = &mut self.events[pos]
                && *kind == Tombstone
            {
                *kind = ErrorNode;
            }
            self.events.push(Event::Finish);
        }
        let m = self.start();
        let mut nest = 0u32;
        loop {
            match self.cur() {
                Eof => break,
                Semi if nest == 0 => {
                    self.bump();
                    break;
                }
                RBrace if nest == 0 => {
                    if !in_block {
                        self.bump();
                    }
                    break;
                }
                LBrace => {
                    nest += 1;
                    self.bump();
                }
                RBrace => {
                    nest -= 1;
                    self.bump();
                    if nest == 0 {
                        break;
                    }
                }
                _ => self.bump(),
            }
        }
        self.complete(m, ErrorNode);
    }

    // --- statements -----------------------------------------------------

    fn source_file(&mut self) {
        let m = self.start();
        while !self.at(Eof) {
            if self.at(UseDirective) {
                let u = self.start();
                self.bump();
                self.complete(u, UseStmt);
                continue;
            }
            let depth = self.open.len();
            if self.statement().is_err() {
                self.recover(depth, false);
            }
        }
        self.complete(m, SourceFile);
    }

    fn statement(&mut self) -> PResult {
        match self.cur() {
            Semi => {
                let m = self.start();
                self.bump();
                self.complete(m, EmptyStmt);
            }
            LBrace => {
                let m = self.start();
                self.bump();
                while !self.at(RBrace) {
                    if self.at(Eof) {
                        return self.error();
                    }
                    let depth = self.open.len();
                    if self.statement().is_err() {
                        self.recover(depth, true);
                    }
                }
                self.bump();
                self.complete(m, BlockStmt);
            }
            KwModule => {
                let m = self.start();
                self.bump();
                self.expect(Ident)?;
                self.expect(LParen)?;
                self.parameters()?;
                self.expect(RParen)?;
                self.statement()?;
                self.complete(m, ModuleDef);
            }
            KwFunction => {
                let m = self.start();
                self.bump();
                self.expect(Ident)?;
                self.expect(LParen)?;
                self.parameters()?;
                self.expect(RParen)?;
                self.expect(Eq)?;
                self.expr()?;
                self.expect(Semi)?;
                self.complete(m, FunctionDef);
            }
            Eot => {
                let m = self.start();
                self.bump();
                self.complete(m, EotStmt);
            }
            Ident if self.nth(1) == Eq => self.assignment()?,
            _ => self.module_instantiation()?,
        }
        Ok(())
    }

    fn assignment(&mut self) -> PResult {
        let m = self.start();
        self.bump();
        self.bump();
        self.expr()?;
        self.expect(Semi)?;
        self.complete(m, Assignment);
        Ok(())
    }

    fn module_instantiation(&mut self) -> PResult {
        match self.cur() {
            Bang | Hash | Percent | Star => {
                let m = self.start();
                self.bump();
                self.module_instantiation()?;
                self.complete(m, ModifierInst);
            }
            KwIf => {
                let m = self.start();
                self.bump();
                self.expect(LParen)?;
                self.expr()?;
                self.expect(RParen)?;
                self.child_statement()?;
                if self.at(KwElse) {
                    let e = self.start();
                    self.bump();
                    self.child_statement()?;
                    self.complete(e, ElseClause);
                }
                self.complete(m, IfInst);
            }
            // "for", "let", "assert", "echo" and "each" are module names too.
            Ident | KwFor | KwLet | KwAssert | KwEcho | KwEach => {
                let m = self.start();
                self.bump();
                self.expect(LParen)?;
                self.arguments()?;
                self.expect(RParen)?;
                self.child_statement()?;
                self.complete(m, ModuleInst);
            }
            _ => return self.error(),
        }
        Ok(())
    }

    fn child_statement(&mut self) -> PResult {
        match self.cur() {
            Semi => {
                let m = self.start();
                self.bump();
                self.complete(m, EmptyStmt);
            }
            LBrace => {
                let m = self.start();
                self.bump();
                while !self.at(RBrace) {
                    if self.at(Eof) {
                        return self.error();
                    }
                    let depth = self.open.len();
                    let r = if self.at(Ident) && self.nth(1) == Eq { self.assignment() } else { self.child_statement() };
                    if r.is_err() {
                        self.recover(depth, true);
                    }
                }
                self.bump();
                self.complete(m, ChildBlock);
            }
            _ => self.module_instantiation()?,
        }
        Ok(())
    }

    fn parameters(&mut self) -> PResult {
        let list = self.start();
        while self.at(Ident) {
            let m = self.start();
            self.bump();
            if self.eat(Eq) {
                self.expr()?;
            }
            self.complete(m, Param);
            if !self.eat(Comma) {
                break;
            }
        }
        self.complete(list, ParamList);
        Ok(())
    }

    fn arguments(&mut self) -> PResult {
        let list = self.start();
        while self.cur().starts_expr() {
            let m = self.start();
            if self.at(Ident) && self.nth(1) == Eq {
                self.bump();
                self.bump();
            }
            self.expr()?;
            self.complete(m, Arg);
            if !self.eat(Comma) {
                break;
            }
        }
        self.complete(list, ArgList);
        Ok(())
    }

    // --- expressions ----------------------------------------------------

    fn expr(&mut self) -> PResult<Done> {
        let kind = match self.cur() {
            KwFunction => {
                let m = self.start();
                self.bump();
                self.expect(LParen)?;
                self.parameters()?;
                self.expect(RParen)?;
                self.expr()?;
                return Ok(self.complete(m, FunctionExpr));
            }
            KwLet => LetExpr,
            KwAssert => AssertExpr,
            KwEcho => EchoExpr,
            _ => return self.expr_from(None),
        };
        let m = self.start();
        self.bump();
        self.expect(LParen)?;
        self.arguments()?;
        self.expect(RParen)?;
        // `assert(...)` and `echo(...)` may stand alone; `let(...)` may not.
        if kind == LetExpr || self.cur().starts_expr() {
            self.expr()?;
        }
        Ok(self.complete(m, kind))
    }

    /// `logic_or ['?' expr ':' expr]`, optionally continuing from an
    /// already parsed primary.
    fn expr_from(&mut self, lhs: Option<Done>) -> PResult<Done> {
        let cond = self.binary(0, lhs)?;
        if !self.at(Question) {
            return Ok(cond);
        }
        let m = self.precede(cond);
        self.bump();
        self.expr()?;
        self.expect(Colon)?;
        self.expr()?;
        Ok(self.complete(m, TernaryExpr))
    }

    fn binary_level(k: SyntaxKind) -> Option<u8> {
        Some(match k {
            OrOr => 0,
            AndAnd => 1,
            EqEq | Ne => 2,
            Gt | Ge | Lt | Le => 3,
            Pipe => 4,
            Amp => 5,
            Shl | Shr => 6,
            Plus | Minus => 7,
            Star | Slash | Percent => 8,
            _ => return None,
        })
    }

    /// Left-associative binary operators at `min` or above.
    fn binary(&mut self, min: u8, lhs: Option<Done>) -> PResult<Done> {
        let mut left = self.unary(lhs)?;
        while let Some(level) = Self::binary_level(self.cur()) {
            if level < min {
                break;
            }
            let m = self.precede(left);
            self.bump();
            self.binary(level + 1, None)?;
            left = self.complete(m, BinaryExpr);
        }
        Ok(left)
    }

    fn unary(&mut self, lhs: Option<Done>) -> PResult<Done> {
        if lhs.is_none() && matches!(self.cur(), Plus | Minus | Bang | Tilde) {
            let m = self.start();
            self.bump();
            self.unary(None)?;
            return Ok(self.complete(m, UnaryExpr));
        }
        let base = self.call(lhs)?;
        if !self.at(Caret) {
            return Ok(base);
        }
        let m = self.precede(base);
        self.bump();
        self.unary(None)?;
        Ok(self.complete(m, BinaryExpr))
    }

    fn call(&mut self, lhs: Option<Done>) -> PResult<Done> {
        let mut left = match lhs {
            Some(d) => d,
            None => self.primary()?,
        };
        loop {
            let kind = match self.cur() {
                LParen => {
                    let m = self.precede(left);
                    self.bump();
                    self.arguments()?;
                    self.expect(RParen)?;
                    (m, CallExpr)
                }
                LBrack => {
                    let m = self.precede(left);
                    self.bump();
                    self.expr()?;
                    self.expect(RBrack)?;
                    (m, IndexExpr)
                }
                Dot => {
                    let m = self.precede(left);
                    self.bump();
                    self.expect(Ident)?;
                    (m, MemberExpr)
                }
                _ => return Ok(left),
            };
            left = self.complete(kind.0, kind.1);
        }
    }

    fn primary(&mut self) -> PResult<Done> {
        match self.cur() {
            KwTrue | KwFalse | KwUndef | Number | String => {
                let m = self.start();
                self.bump();
                Ok(self.complete(m, Literal))
            }
            Ident => {
                let m = self.start();
                self.bump();
                Ok(self.complete(m, NameRef))
            }
            LParen => {
                let m = self.start();
                self.bump();
                self.expr()?;
                self.expect(RParen)?;
                Ok(self.complete(m, ParenExpr))
            }
            LBrack => self.vector(),
            _ => self.error(),
        }
    }

    /// `[]`, `[a : b]`, `[a : s : b]` or `[elements]`.
    fn vector(&mut self) -> PResult<Done> {
        let m = self.start();
        self.bump();
        if self.eat(RBrack) {
            return Ok(self.complete(m, VectorExpr));
        }
        let first = self.element()?;
        if first == Elem::Expr && self.eat(Colon) {
            self.expr()?;
            if self.eat(Colon) {
                self.expr()?;
            }
            self.expect(RBrack)?;
            return Ok(self.complete(m, RangeExpr));
        }
        while self.eat(Comma) {
            if self.at(RBrack) {
                break;
            }
            self.element()?;
        }
        self.expect(RBrack)?;
        Ok(self.complete(m, VectorExpr))
    }

    /// A vector element: a list-comprehension clause (optionally in
    /// parentheses) or an expression.
    ///
    /// The two overlap on `let(...)` and `(`: parser.y decides by what
    /// follows. `let(a=1) for(...) ...` is a comprehension let,
    /// `let(a=1) a+1` a let expression. `(for ...)` is a parenthesised
    /// clause, while `(let(a=1) a)` is only known to be an expression once
    /// the inner `let` resolves, after which parsing continues as an
    /// expression whose first operand is that parenthesised term.
    fn element(&mut self) -> PResult<Elem> {
        match self.cur() {
            KwEach | KwFor | KwIf => {
                self.lc_clause()?;
                Ok(Elem::Lc)
            }
            KwLet => self.let_chain(),
            LParen if matches!(self.nth(1), KwEach | KwFor | KwIf) => {
                let m = self.start();
                self.bump();
                self.lc_clause()?;
                self.expect(RParen)?;
                self.complete(m, LcParen);
                Ok(Elem::Lc)
            }
            LParen if self.nth(1) == KwLet => {
                let m = self.start();
                self.bump();
                let inner = self.let_chain()?;
                self.expect(RParen)?;
                if inner == Elem::Lc {
                    self.complete(m, LcParen);
                    return Ok(Elem::Lc);
                }
                let paren = self.complete(m, ParenExpr);
                self.expr_from(Some(paren))?;
                Ok(Elem::Expr)
            }
            _ => {
                self.expr()?;
                Ok(Elem::Expr)
            }
        }
    }

    /// `let(args) X` inside a vector: a comprehension let when `X` is a
    /// clause, a let expression otherwise.
    fn let_chain(&mut self) -> PResult<Elem> {
        let m = self.start();
        self.bump();
        self.expect(LParen)?;
        self.arguments()?;
        self.expect(RParen)?;
        let body = self.element()?;
        self.complete(m, if body == Elem::Lc { LcLet } else { LetExpr });
        Ok(body)
    }

    /// `each`, `for` or `if` clause (`list_comprehension_elements` minus
    /// `let`, which [`Parser::let_chain`] handles).
    fn lc_clause(&mut self) -> PResult {
        match self.cur() {
            KwEach => {
                let m = self.start();
                self.bump();
                self.element()?;
                self.complete(m, LcEach);
            }
            KwFor => {
                let m = self.start();
                self.bump();
                self.expect(LParen)?;
                self.arguments()?;
                let kind = if self.eat(Semi) {
                    self.expr()?;
                    self.expect(Semi)?;
                    self.arguments()?;
                    LcForC
                } else {
                    LcFor
                };
                self.expect(RParen)?;
                self.element()?;
                self.complete(m, kind);
            }
            KwIf => {
                let m = self.start();
                self.bump();
                self.expect(LParen)?;
                self.expr()?;
                self.expect(RParen)?;
                self.element()?;
                if self.eat(KwElse) {
                    self.element()?;
                }
                self.complete(m, LcIf);
            }
            // Inside `(...)` a comprehension may also be a let chain, but it
            // must end in a clause.
            KwLet => {
                if self.let_chain()? == Elem::Expr {
                    return self.error();
                }
            }
            _ => return self.error(),
        }
        Ok(())
    }
}

/// Replay the events into a tree, attaching trivia: leading trivia goes to
/// the enclosing node, and whatever trails the last token to the root.
fn build(tokens: Vec<Token>, mut events: Vec<Event>) -> Cst {
    let n = tokens.len();
    let mut b = Builder::new(tokens);
    let mut raw = 0usize;
    let mut chain = Vec::new();

    fn flush(b: &mut Builder, raw: &mut usize, n: usize) {
        while *raw < n && b.token_kind(*raw).is_some_and(SyntaxKind::is_trivia) {
            b.token(*raw as u32);
            *raw += 1;
        }
    }

    for i in 0..events.len() {
        match std::mem::replace(&mut events[i], Event::Taken) {
            Event::Start { kind, forward_parent } => {
                if kind == Tombstone && forward_parent == 0 {
                    continue;
                }
                chain.clear();
                chain.push(kind);
                let (mut j, mut fp) = (i, forward_parent);
                while fp != 0 {
                    j += fp as usize;
                    match std::mem::replace(&mut events[j], Event::Taken) {
                        Event::Start { kind, forward_parent } => {
                            chain.push(kind);
                            fp = forward_parent;
                        }
                        _ => unreachable!("forward parent must be a start event"),
                    }
                }
                for &k in chain.iter().rev() {
                    if b.depth() > 0 {
                        flush(&mut b, &mut raw, n);
                    }
                    b.start(k);
                }
            }
            Event::Token => {
                flush(&mut b, &mut raw, n);
                b.token(raw as u32);
                raw += 1;
            }
            Event::Finish => {
                if b.depth() == 1 {
                    while raw < n {
                        b.token(raw as u32);
                        raw += 1;
                    }
                }
                b.finish();
            }
            Event::Taken => {}
        }
    }
    b.finish_tree()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::{FileId, SourceMap};
    use crate::syntax::lexer::lex;

    fn parse_str(src: &str) -> (Parse, SourceMap) {
        let mut sm = SourceMap::new();
        sm.add("t.scad".into(), src.as_bytes().to_vec());
        let l = lex(src.as_bytes(), FileId(0));
        (parse(l.tokens), sm)
    }

    /// Offset of the token at which the first error was reported, or None.
    fn first_error(src: &str) -> Option<std::string::String> {
        let (p, sm) = parse_str(src);
        let e = p.errors.first()?;
        let toks = p.cst.tokens();
        Some(match toks.get(e.token as usize) {
            Some(t) => std::string::String::from_utf8_lossy(sm.get(t.file).slice(t.start, t.end())).into_owned(),
            None => "<eof>".into(),
        })
    }

    #[test]
    fn tree_is_lossless() {
        for src in [
            "// c\na = 1; /* x */ module m(a, b = 2) { cube(a); }\n",
            "x = [for (i = [0:2]) let(a = i) if (a) each a else a];",
            "a = ; b = (1; c = 3;",
            "}}} {{{ a",
        ] {
            let (p, sm) = parse_str(src);
            assert_eq!(p.cst.text(&sm), src.as_bytes(), "{src}");
        }
    }

    #[test]
    fn accepts_valid_programs() {
        for src in [
            "",
            "a = 1;",
            ";;{ a = 1; { b = 2; } }",
            "module m() cube();",
            "module m() module n() o();",
            "module m()\n\u{3}\n",
            "function f(a, b = 1,) = a + b;",
            "!#%*cube();",
            "if (a) b(); else if (c) d(); else { e(); f = 1; }",
            "for (i = [0:1]) let (a = 1) echo(a) assert(true) each() n();",
            "x = f(1)(2)[3].y;",
            "y = function(a) a + 1;",
            "z = -(-1) ^ 2 ^ -3;",
            "e = echo(1) assert(2);",
            "e = assert(1);",
            "v = [];",
            "v = [1, 2,];",
            "v = [1 : 2 : 3];",
            "v = [let(a = 1) a : 2];",
            "v = [let(a = 1) for (i = a) i, let(b = 2) b, let(c=1) (for (k = 1) k)];",
            "v = [(for (i = 1) i), (let (a = 1) a) + 1, (let (a = 1) each a)];",
            "v = [for (i = 0; i < 2; i = i + 1) i];",
            "v = [if (a) if (b) 1 else 2];",
            "v = [each [1, 2]];",
            "t = a ? b : c ? d : e;",
            "t = a || b && c == d < e | f & g << h + i * j;",
            "f(a = 1, b, 2,);",
            "\u{3}\na = 1;",
        ] {
            assert_eq!(first_error(src), None, "{src}");
        }
    }

    #[test]
    fn first_error_token_matches_bison() {
        let cases = [
            ("a = 1 $ 2;", "$"),
            ("a = (1;", ";"),
            ("a = 1", "<eof>"),
            ("x = 1 + let(a = 1) a;", "let"),
            ("x = -function(a) a;", "function"),
            ("x = [1, 2 : 3];", ":"),
            ("x = [((for (i = 1) i))];", "for"),
            ("x = [1 : 2 : 3 : 4];", ":"),
            ("f(,);", ","),
            ("module m(a,,) x();", ","),
            ("let = 1;", "="),
            ("a b;", "b"),
            ("a;", ";"),
            ("}", "}"),
            ("m() { module n() x(); }", "module"),
            ("{ use <x> }", "use <x>"),
            ("x = a.for;", "for"),
            ("x = let(a = 1);", ";"),
            ("x = [let(a = 1)];", "]"),
            ("x = [(let(a = 1) for (i = 1) i) + 1];", "+"),
            ("x = each [1];", "each"),
        ];
        for (src, at) in cases {
            assert_eq!(first_error(src).as_deref(), Some(at), "{src}");
        }
    }

    /// Random token soup: recovery must never panic, loop or lose bytes.
    #[test]
    fn survives_garbage() {
        let pieces = [
            "a", "=", "(", ")", "[", "]", "{", "}", ";", ",", ":", "?", "1", "\"s\"", "let", "for", "if", "else",
            "each", "module", "function", "+", "-", "*", "!", "#", "%", ".", "^", "use <x>", "/*c*/", " ", "\n",
        ];
        let mut seed = 0x2545_f491_4f6c_dd1du64;
        for _ in 0..2000 {
            let mut src = std::string::String::new();
            for _ in 0..(seed % 40) {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                src.push_str(pieces[(seed % pieces.len() as u64) as usize]);
                src.push(' ');
            }
            let (p, sm) = parse_str(&src);
            assert_eq!(p.cst.text(&sm), src.as_bytes(), "{src}");
        }
    }

    #[test]
    fn recovers_and_reports_several_errors() {
        let (p, _) = parse_str("a = ;\nb = 2;\nc = (;\nmodule m() { x = ; y(); }\nz();");
        assert_eq!(p.errors.len(), 3);
    }

    #[test]
    fn shapes_expressions() {
        let (p, sm) = parse_str("x = -2^2 + 1;");
        let dump = p.cst.debug_dump(&sm);
        let bin = dump.find("BinaryExpr").unwrap();
        let un = dump.find("UnaryExpr").unwrap();
        assert!(bin < un, "{dump}");
    }
}
