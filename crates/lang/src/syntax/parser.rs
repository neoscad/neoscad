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

/// A syntax error at a token. The message is Bison's "syntax error", or
/// "memory exhausted" when the input nests deeper than the parser's limit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SyntaxError {
    /// Index into the parsed token list; `tokens.len()` means end of input.
    pub token: u32,
    /// The syntax tree would have been deeper than the nesting limit
    /// ([`NESTING_LIMIT`]) at this token. Parsing stopped there: the rest of
    /// the input is kept in the tree, unparsed, as trailing tokens of the
    /// root.
    pub too_deep: bool,
}

/// How deep the syntax tree may nest, counted as the sum of the
/// [`nesting_weight`]s of the nodes from the root down to the deepest
/// leaf, before parsing stops with OpenSCAD's "memory exhausted" error.
///
/// Every stage after the parser walks the source's nesting recursively:
/// the parser itself, the lowering to the AST, the AST's clone and drop,
/// the evaluator's scopes and expressions, and the formatter. Bounding
/// the tree here is what turns a nesting that would overflow one of them
/// into a parse error. The bound is a count, not a measured stack, so a
/// program parses or fails the same way on every thread, and on a given
/// target it is the same for every host: a limit that varied would make
/// an included file's cached parse ([`crate::fragment`]) depend on who
/// parsed it first. The weights are the same on every target; only this
/// limit differs.
///
/// The count is weighted because a level of nesting takes a different
/// amount of stack depending on what nests: a level of `[` passes through
/// eight parser functions and a level of `{` through one, and the
/// lowering and the evaluation differ as much. A plain node count low
/// enough for `[` in WebKit (about 60 levels) would have refused MCAD's
/// `bitmap.scad`, whose `else if` chain is 186 nodes deep and parses in
/// every browser.
///
/// OpenSCAD's Bison parser has a stack of 200,000 entries (`YYMAXDEPTH` in
/// `parser.y`) and stops with "memory exhausted" when a program fills it:
/// the nightly does at 99,997 levels of `translate()` or of `{`, and
/// parses 100,000 levels of `(`. Its evaluator crashed on far less: 10,000
/// levels of `translate()`, 50,000 of `[`. By weight, the deepest files in
/// BOSL2, MCAD and OpenSCAD's tests are BOSL2's `nurbs.scad` (2,289: 66
/// `assert`s chained in one expression) and MCAD's `bitmap.scad` (2,005;
/// `tests/deep_nesting.rs` checks both against the wasm32 limit).
///
/// Natively the limit is 50,000 in an optimised build: 5,000 levels of
/// the cheapest kinds (weight 10), as many as the plain node count it
/// replaced allowed, and fewer of the costlier ones (2,380 levels of
/// `translate()`, 1,111 of `[`). It is set by the stages that run on a
/// thread's own stack. Measured in a release build on macOS arm64, when
/// the limit was 5,000 nodes:
/// - the parser and lowering take up to 800 bytes a node (`translate()`),
///   and the formatter more: `neoscad fmt`, on the main thread's 8 MiB,
///   overflowed between 8,000 and 10,000 levels of `(`;
/// - with an evaluator's 80 MiB (`eval::DEFAULT_THREAD_STACK`), a program
///   overflowed at 26,000 levels of `translate()`, and at over 150,000 of
///   `{` or `(`; the evaluator's start (`Unit::add_scope`) took between 1
///   and 2 MiB at 5,000 levels of `translate()`;
/// - `neoscad fmt`'s output is quadratic in the depth (each line indented
///   by its level): 240 MB for 5,000 levels of `translate()`, formatted
///   in 860 MB.
#[cfg(all(not(target_arch = "wasm32"), not(debug_assertions)))]
pub const NESTING_LIMIT: u32 = 50_000;

/// [`NESTING_LIMIT`] in an unoptimised native build, whose frames are
/// several times larger: on 80 MiB its evaluation of nested `translate()`
/// overflowed between 3,600 and 3,800 levels, and its parser and lowering
/// took 17 MB for 5,000. Half the optimised build's.
#[cfg(all(not(target_arch = "wasm32"), debug_assertions))]
pub const NESTING_LIMIT: u32 = 25_000;

/// [`NESTING_LIMIT`] on wasm32, where the engine's own stack is what the
/// recursive stages overflow, and a WebKit worker's is the smallest of the
/// browsers'. It is 70% of the 3,700 that [`nesting_weight`]'s weights
/// are sized by, so nesting stops here with 30% of the depth that
/// overflowed WebKit in its worst case to spare (20% for `let`, `assert`
/// and `echo` expressions outside functions; see there). Not covered: an
/// unoptimised wasm32 build, which was not measured.
#[cfg(target_arch = "wasm32")]
pub const NESTING_LIMIT: u32 = WASM32_NESTING_LIMIT;

/// The wasm32 build's [`NESTING_LIMIT`], defined on every target so that
/// native tests can check what a browser will parse.
pub const WASM32_NESTING_LIMIT: u32 = 2_590;

/// What a node of `kind` adds to the tree's depth as [`NESTING_LIMIT`]
/// counts it: its share of the stack that one level of nesting through
/// it takes in the stages that recurse on the tree. The cheapest levels
/// weigh 10.
///
/// Measured October 2026 in Playwright's WebKit on macOS arm64, through
/// the web core's worker (`crates/web`) with the limit lifted: for each
/// kind, the fewest levels that overflowed, over fresh workers and over
/// workers taking one program deeper each run (from several starting
/// depths and steps, so that the engine's tiers warm up part-way), for
/// parsing alone (the customizer's `parameters` request) and for a
/// preview. The worst case is a worker part-way through tiering up, not
/// a fresh one: 84 levels of `[` where a fresh worker reached 147. A
/// level weighs at least 3,700 divided by those levels:
/// - 69: `max(` (`CallExpr`, `ArgList` and `Arg`: 54), also with a named
///   argument;
/// - 84: `[` (`VectorExpr`: 45), `[a : b]` nested in `a` (`RangeExpr`),
///   `1 + (` (`BinaryExpr` and `ParenExpr`);
/// - 102: `(` (`ParenExpr`: 37), and 103 of `a[` (`IndexExpr`);
/// - 112: a comprehension's `for` (`LcFor`: 34), `(for` (`LcParen` and
///   `LcFor`);
/// - 125: `{` after a module or an `if` (`ChildBlock` and its
///   `ModuleInst` or `IfInst`: 31); 135: `function (a =` (`FunctionExpr`,
///   `ParamList` and `Param`: 30);
/// - 172: `.x` chains (`MemberExpr`: 22), 180 of `()` chains;
/// - 181: `translate()`, `if ()` (`ModuleInst`, `IfInst`: 21); 191 or
///   more of `for ()`, `let ()` and a user module; 193 of `else if`
///   (`IfInst` and `ElseClause`: 21);
/// - 214: a comprehension's `if` (`LcIf`: 18); 222: `?:` (`TernaryExpr`:
///   17); 231: a comprehension's `each` (`LcEach`: 17);
/// - 338: modifiers (`ModifierInst`: 11); 371 or more: `{` (`BlockStmt`:
///   10), `module m()`, `1 + 1 + `, `1 ^ 1 ^ `, unary `-`, `function ()`.
///
/// Mixed kinds overflowed no sooner than their weights' sum says: a level
/// of `max((-[` weighs 146, so the limit allows 17 levels, and WebKit
/// overflowed at 28. The other engines have far more room: Chromium
/// overflowed at 5 times WebKit's depths or more (913 levels of
/// `translate()`, 406 of `max(`), Firefox at 10 times or more (2,053 of
/// `translate()`, 1,174 of `[`) and node 24, in fresh processes only, at
/// 14 times or more (2,585 of `translate()`, 1,550 of `[`).
///
/// The exception is `let`, `assert` and `echo` expressions: 123 levels of
/// them overflowed in a statement's arguments (`echo(assert(true) ...
/// 1)`), where the native evaluator (`Evaluator::eval` in
/// `crates/eval/src/eval.rs`) recurses into each one's body, and 30% to
/// spare would need a weight of 31. Parsed alone, or in a function's body
/// (`function f() = assert(true) ... 1;`, called once), they reached 371.
/// BOSL2's `nurbs.scad` chains 66 of them in a function's body, and at 31
/// it would weigh 2,649, over the limit; at 26 it weighs 2,289, and the
/// limit (99 levels) leaves 20% of the 123 to spare.
pub const fn nesting_weight(kind: SyntaxKind) -> u32 {
    match kind {
        // The `if` it belongs to carries an `else if` level: a level of
        // `else if` overflowed WebKit no sooner than one of `if`.
        ElseClause => 0,
        ModifierInst => 11,
        TernaryExpr | LcEach => 17,
        LcIf => 18,
        ModuleInst | IfInst => 21,
        MemberExpr => 22,
        LetExpr | LcLet | AssertExpr | EchoExpr => 26,
        CallExpr | LcFor | LcForC => 34,
        ParenExpr | LcParen | IndexExpr => 37,
        VectorExpr | RangeExpr => 45,
        _ => 10,
    }
}

#[derive(Debug)]
pub struct Parse {
    pub cst: Cst,
    pub errors: Vec<SyntaxError>,
}

/// Parse a token stream (trivia included, as produced by the lexer or the
/// include-splicing loader).
pub fn parse(tokens: Vec<Token>) -> Parse {
    parse_with_limit(tokens, NESTING_LIMIT)
}

/// [`parse`] with a nesting limit other than [`NESTING_LIMIT`]: at most
/// `limit` nodes from the root to the deepest leaf.
pub fn parse_with_limit(tokens: Vec<Token>, limit: u32) -> Parse {
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

    let mut p = Parser {
        kinds,
        raw,
        pos: 0,
        events: Vec::with_capacity(tokens.len()),
        open: Vec::new(),
        depth: 0,
        limit,
        exhausted: false,
        errors: Vec::new(),
    };
    p.source_file();
    let errors = p
        .errors
        .iter()
        .map(|&(sig, too_deep)| SyntaxError {
            token: p.raw[sig as usize],
            too_deep,
        })
        .collect();
    let cst = build(tokens, p.events);
    Parse { cst, errors }
}

#[derive(Debug, Clone, Copy)]
enum Event {
    Start {
        kind: SyntaxKind,
        forward_parent: u32,
    },
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
struct Done {
    /// The node's start event.
    start: u32,
    /// The summed [`nesting_weight`]s from this node to its deepest leaf,
    /// itself included.
    height: u32,
}

/// A node that has been started and not yet completed.
#[derive(Debug, Clone, Copy)]
struct Open {
    /// Its start event.
    start: u32,
    /// Its own [`nesting_weight`].
    weight: u32,
    /// The height of its tallest completed child.
    tallest: u32,
}

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
    /// Nodes not yet completed, innermost last.
    open: Vec<Open>,
    /// The open nodes' summed [`nesting_weight`]s: the weighted depth of
    /// the node being parsed.
    depth: u32,
    /// The weighted depth the tree may reach ([`NESTING_LIMIT`]).
    limit: u32,
    /// The tree reached `limit`, and the input now ends at the token where
    /// it did, so every open rule unwinds as it would at the end of input.
    exhausted: bool,
    /// Significant-token positions of recorded errors, and whether each is
    /// the nesting limit's.
    errors: Vec<(u32, bool)>,
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
        // The rule whose node went past the nesting limit had already
        // matched its first token, and bumps it after opening the node;
        // the input ended there (`exhaust`), so the node stays empty.
        if self.exhausted {
            return;
        }
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
        // Once the nesting limit has cut the input short, the rules that
        // unwind meet its end: that is not a second error.
        if !self.exhausted {
            self.errors.push((self.pos as u32, false));
        }
        Err(Stop)
    }

    /// The tree would be deeper than the limit: record the error and end
    /// the input at the current token. Every rule then sees the end of
    /// input, so the parse unwinds through the usual error paths without
    /// going any deeper, and `build` puts the unparsed tokens at the end
    /// of the root, as it does trailing trivia.
    fn exhaust(&mut self) {
        if !self.exhausted {
            self.exhausted = true;
            self.errors.push((self.pos as u32, true));
            self.kinds.truncate(self.pos);
            self.kinds.push(Eof);
        }
    }

    // --- markers --------------------------------------------------------

    // Every recursive rule opens a node before it recurses, so checking the
    // depth where nodes open bounds the parser's own recursion as well as
    // the tree. A node that encloses a completed one (`precede`, as in a
    // chain `a + b + c`, which the parser reads in a loop but every later
    // stage walks as nested nodes) makes that subtree deeper at once, so it
    // checks the subtree's height too.

    /// Open a node that will be a `kind`, or one weighed the same where
    /// the rule decides only later: a vector or a range, a parenthesised
    /// expression or comprehension, a `let` expression or comprehension
    /// `let`, a `for` or C-style `for` (`undecided_kinds_weigh_the_same`).
    fn start(&mut self, kind: SyntaxKind) -> Marker {
        let pos = self.events.len() as u32;
        self.events.push(Event::Start {
            kind: Tombstone,
            forward_parent: 0,
        });
        let weight = nesting_weight(kind);
        self.open.push(Open {
            start: pos,
            weight,
            tallest: 0,
        });
        self.depth += weight;
        if self.depth > self.limit {
            self.exhaust();
        }
        Marker(pos)
    }

    /// Close the top open node: its start event and height, which goes
    /// into its parent's tallest child.
    fn close(&mut self) -> (u32, u32) {
        let Some(o) = self.open.pop() else {
            return (0, 0);
        };
        self.depth -= o.weight;
        let height = o.tallest + o.weight;
        if let Some(parent) = self.open.last_mut() {
            parent.tallest = parent.tallest.max(height);
        }
        (o.start, height)
    }

    fn complete(&mut self, m: Marker, kind: SyntaxKind) -> Done {
        let (start, height) = self.close();
        debug_assert_eq!(start, m.0, "markers complete in LIFO order");
        if let Event::Start { kind: k, .. } = &mut self.events[m.0 as usize] {
            *k = kind;
        }
        self.events.push(Event::Finish);
        Done { start: m.0, height }
    }

    /// Start a node that will enclose the already completed `d`.
    fn precede(&mut self, d: Done, kind: SyntaxKind) -> Marker {
        let m = self.start(kind);
        if let Some(o) = self.open.last_mut() {
            o.tallest = d.height;
        }
        if self.depth.saturating_add(d.height) > self.limit {
            self.exhaust();
        }
        if let Event::Start { forward_parent, .. } = &mut self.events[d.start as usize] {
            *forward_parent = m.0 - d.start;
        }
        m
    }

    /// Close every node opened since `depth` and skip to a statement
    /// boundary: past the next `;` or balanced `{...}`, or up to the `}`
    /// that closes the enclosing block.
    fn recover(&mut self, depth: usize, in_block: bool) {
        while self.open.len() > depth {
            let pos = self.close().0 as usize;
            if let Event::Start { kind, .. } = &mut self.events[pos]
                && *kind == Tombstone
            {
                *kind = ErrorNode;
            }
            self.events.push(Event::Finish);
        }
        let m = self.start(ErrorNode);
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
        let m = self.start(SourceFile);
        while !self.at(Eof) {
            if self.at(UseDirective) {
                let u = self.start(UseStmt);
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
                let m = self.start(EmptyStmt);
                self.bump();
                self.complete(m, EmptyStmt);
            }
            LBrace => {
                let m = self.start(BlockStmt);
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
                let m = self.start(ModuleDef);
                self.bump();
                self.expect(Ident)?;
                self.expect(LParen)?;
                self.parameters()?;
                self.expect(RParen)?;
                self.statement()?;
                self.complete(m, ModuleDef);
            }
            KwFunction => {
                let m = self.start(FunctionDef);
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
                let m = self.start(EotStmt);
                self.bump();
                self.complete(m, EotStmt);
            }
            Ident if self.nth(1) == Eq => self.assignment()?,
            _ => self.module_instantiation()?,
        }
        Ok(())
    }

    fn assignment(&mut self) -> PResult {
        let m = self.start(Assignment);
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
                let m = self.start(ModifierInst);
                self.bump();
                self.module_instantiation()?;
                self.complete(m, ModifierInst);
            }
            KwIf => {
                let m = self.start(IfInst);
                self.bump();
                self.expect(LParen)?;
                self.expr()?;
                self.expect(RParen)?;
                self.child_statement()?;
                if self.at(KwElse) {
                    let e = self.start(ElseClause);
                    self.bump();
                    self.child_statement()?;
                    self.complete(e, ElseClause);
                }
                self.complete(m, IfInst);
            }
            // "for", "let", "assert", "echo" and "each" are module names too.
            Ident | KwFor | KwLet | KwAssert | KwEcho | KwEach => {
                let m = self.start(ModuleInst);
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
                let m = self.start(EmptyStmt);
                self.bump();
                self.complete(m, EmptyStmt);
            }
            LBrace => {
                let m = self.start(ChildBlock);
                self.bump();
                while !self.at(RBrace) {
                    if self.at(Eof) {
                        return self.error();
                    }
                    let depth = self.open.len();
                    let r = if self.at(Ident) && self.nth(1) == Eq {
                        self.assignment()
                    } else {
                        self.child_statement()
                    };
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
        let list = self.start(ParamList);
        while self.at(Ident) {
            let m = self.start(Param);
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
        let list = self.start(ArgList);
        while self.cur().starts_expr() {
            let m = self.start(Arg);
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
                let m = self.start(FunctionExpr);
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
        let m = self.start(kind);
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
        let m = self.precede(cond, TernaryExpr);
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
            let m = self.precede(left, BinaryExpr);
            self.bump();
            self.binary(level + 1, None)?;
            left = self.complete(m, BinaryExpr);
        }
        Ok(left)
    }

    fn unary(&mut self, lhs: Option<Done>) -> PResult<Done> {
        if lhs.is_none() && matches!(self.cur(), Plus | Minus | Bang | Tilde) {
            let m = self.start(UnaryExpr);
            self.bump();
            self.unary(None)?;
            return Ok(self.complete(m, UnaryExpr));
        }
        let base = self.call(lhs)?;
        if !self.at(Caret) {
            return Ok(base);
        }
        let m = self.precede(base, BinaryExpr);
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
                    let m = self.precede(left, CallExpr);
                    self.bump();
                    self.arguments()?;
                    self.expect(RParen)?;
                    (m, CallExpr)
                }
                LBrack => {
                    let m = self.precede(left, IndexExpr);
                    self.bump();
                    self.expr()?;
                    self.expect(RBrack)?;
                    (m, IndexExpr)
                }
                Dot => {
                    let m = self.precede(left, MemberExpr);
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
                let m = self.start(Literal);
                self.bump();
                Ok(self.complete(m, Literal))
            }
            Ident => {
                let m = self.start(NameRef);
                self.bump();
                Ok(self.complete(m, NameRef))
            }
            LParen => {
                let m = self.start(ParenExpr);
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
        let m = self.start(VectorExpr);
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
                let m = self.start(LcParen);
                self.bump();
                self.lc_clause()?;
                self.expect(RParen)?;
                self.complete(m, LcParen);
                Ok(Elem::Lc)
            }
            LParen if self.nth(1) == KwLet => {
                let m = self.start(ParenExpr);
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
        let m = self.start(LetExpr);
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
                let m = self.start(LcEach);
                self.bump();
                self.element()?;
                self.complete(m, LcEach);
            }
            KwFor => {
                let m = self.start(LcFor);
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
                let m = self.start(LcIf);
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
            Event::Start {
                kind,
                forward_parent,
            } => {
                if kind == Tombstone && forward_parent == 0 {
                    continue;
                }
                chain.clear();
                chain.push(kind);
                let (mut j, mut fp) = (i, forward_parent);
                while fp != 0 {
                    j += fp as usize;
                    match std::mem::replace(&mut events[j], Event::Taken) {
                        Event::Start {
                            kind,
                            forward_parent,
                        } => {
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
            Some(t) => std::string::String::from_utf8_lossy(sm.get(t.file).slice(t.start, t.end()))
                .into_owned(),
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
            "a", "=", "(", ")", "[", "]", "{", "}", ";", ",", ":", "?", "1", "\"s\"", "let", "for",
            "if", "else", "each", "module", "function", "+", "-", "*", "!", "#", "%", ".", "^",
            "use <x>", "/*c*/", " ", "\n",
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

    /// The errors of `src` parsed with nesting limit `limit`, as (token
    /// text, too deep).
    fn errors_with_limit(src: &str, limit: u32) -> Vec<(std::string::String, bool)> {
        let mut sm = SourceMap::new();
        sm.add("t.scad".into(), src.as_bytes().to_vec());
        let p = parse_with_limit(lex(src.as_bytes(), FileId(0)).tokens, limit);
        assert_eq!(p.cst.text(&sm), src.as_bytes(), "lossless: {src}");
        let toks = p.cst.tokens();
        p.errors
            .iter()
            .map(|e| {
                let at = match toks.get(e.token as usize) {
                    Some(t) => {
                        std::string::String::from_utf8_lossy(sm.get(t.file).slice(t.start, t.end()))
                            .into_owned()
                    }
                    None => "<eof>".into(),
                };
                (at, e.too_deep)
            })
            .collect()
    }

    /// The weighted depth of a path of nodes, root first.
    fn depth(path: &[SyntaxKind]) -> u32 {
        path.iter().map(|&k| nesting_weight(k)).sum()
    }

    #[test]
    fn nesting_past_the_limit_stops_the_parse() {
        // SourceFile > Assignment > ParenExpr ... > Literal: every `(` adds
        // a ParenExpr's weight. The error is at the token where the node
        // that goes past the limit starts.
        let one = depth(&[SourceFile, Assignment, ParenExpr, Literal]);
        assert_eq!(errors_with_limit("x = (1);", one), vec![]);
        assert_eq!(
            errors_with_limit("x = (1);", one - 1),
            vec![("1".into(), true)]
        );
        assert_eq!(
            errors_with_limit("x = ((1));", one + nesting_weight(ParenExpr) - 1),
            vec![("1".into(), true)]
        );
        assert_eq!(
            errors_with_limit("x = ((1));", one),
            vec![("(".into(), true)]
        );
        // One error however much input follows, broken or not: the parse
        // ends where the limit was reached.
        let three = depth(&[SourceFile, Assignment, ParenExpr, ParenExpr, ParenExpr]);
        assert_eq!(
            errors_with_limit("x = ((((1)))); y = ; z = (((2)));", three),
            vec![("(".into(), true)]
        );
        // An earlier syntax error is still the first.
        assert_eq!(
            errors_with_limit("a = ; x = ((((1))));", three),
            vec![(";".into(), false), ("(".into(), true)]
        );
        // Statements: SourceFile > ModuleInst > ModuleInst > ... > ArgList.
        let four = depth(&[SourceFile, ModuleInst, ModuleInst, ModuleInst, ModuleInst]);
        let args = nesting_weight(ArgList);
        assert_eq!(errors_with_limit("a() b() c();", four + args - 1), vec![]);
        assert_eq!(
            errors_with_limit("a() b() c() d();", four + args - 1),
            vec![(")".into(), true)]
        );
        let blocks = depth(&[SourceFile, BlockStmt, BlockStmt, BlockStmt]);
        assert_eq!(
            errors_with_limit("{{{a();}}}", blocks),
            vec![("a".into(), true)]
        );
    }

    /// A chain the parser reads in a loop (`1 + 1 + ...`, `f()()`,
    /// `a[0][0]`) still makes nested nodes, which later stages walk
    /// recursively, so it counts against the limit as it grows.
    #[test]
    fn chains_count_against_the_limit() {
        // SourceFile > Assignment > BinaryExpr x2 > Literal.
        let two = depth(&[SourceFile, Assignment, BinaryExpr, BinaryExpr, Literal]);
        assert_eq!(errors_with_limit("x = 1 + 2 + 3;", two), vec![]);
        assert_eq!(
            errors_with_limit("x = 1 + 2 + 3 + 4;", two),
            vec![("+".into(), true)]
        );
        let index = depth(&[SourceFile, Assignment, IndexExpr, IndexExpr, NameRef]);
        assert_eq!(errors_with_limit("x = a[0][1];", index), vec![]);
        assert_eq!(
            errors_with_limit("x = a[0][1][2][3];", index),
            vec![("[".into(), true)]
        );
        // A right-nested chain grows as the parser recurses.
        assert_eq!(errors_with_limit("x = 1 ^ 2 ^ 3;", two), vec![]);
        assert_eq!(
            errors_with_limit("x = 1 ^ 2 ^ 3 ^ 4;", two),
            vec![("^".into(), true)]
        );
    }

    /// Kinds whose rule opens a node before it knows which of two kinds
    /// it is weigh the same, so the depth the parser checks is the depth
    /// of the tree it builds.
    #[test]
    fn undecided_kinds_weigh_the_same() {
        for (a, b) in [
            (VectorExpr, RangeExpr),
            (LetExpr, LcLet),
            (ParenExpr, LcParen),
            (LcFor, LcForC),
        ] {
            assert_eq!(nesting_weight(a), nesting_weight(b), "{a:?} {b:?}");
        }
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
