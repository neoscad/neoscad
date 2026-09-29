//! From the lossless syntax tree to a layout [`Doc`].
//!
//! Every significant token is printed exactly once, in order, by
//! [`Builder::tok`], which also places the comments around it: comments
//! on the same line after a token stay after it ("trailing"), the others
//! go on their own lines before the next token ("leading"), with at most
//! one blank line kept before each. Whitespace is the only thing the
//! formatter changes; `lib.rs` checks that afterwards.

use lang::source::SourceFile;
use lang::syntax::{Cst, Element, Node, SyntaxKind, TokenRef};

use crate::doc::Doc;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CommentKind {
    Line,
    Block,
    /// `include <...>`, which the parser sees as trivia.
    Directive,
}

#[derive(Debug, Clone)]
struct Comment {
    text: String,
    kind: CommentKind,
    /// Line breaks between the previous comment or token and this one.
    nl_before: u32,
    /// The comment's line in the source.
    line: u32,
    /// The blanks before it on its line, when it starts the line.
    indent: String,
}

/// The trivia around one significant token.
#[derive(Debug, Clone, Default)]
struct Gap {
    /// Comments on their own lines (or at the start of the file) before
    /// the token.
    leading: Vec<Comment>,
    /// Line breaks between the last leading comment (or the previous
    /// token) and the token.
    nl_before: u32,
    /// Comments after the token on the same line.
    trailing: Vec<Comment>,
}

/// Why a file cannot be laid out.
#[derive(Debug)]
pub struct Unsupported(pub String);

pub struct Builder<'a> {
    file: &'a SourceFile,
    /// Indexed by raw token index; meaningful for significant tokens.
    gaps: Vec<Gap>,
    /// Comments after the last token.
    eof: Gap,
    /// Set before an item (a statement, an argument, a list element): its
    /// first token honours a blank line before it.
    item: bool,
    /// Set before a top-level statement (see [`Builder::comments`]).
    top: bool,
    emitted: usize,
    significant: usize,
    /// First line at which OpenSCAD stops collecting customizer
    /// parameters (the first `{`).
    region_end: u32,
    failed: Option<String>,
}

fn text_of(file: &SourceFile, start: u32, end: u32) -> String {
    String::from_utf8_lossy(file.slice(start, end)).into_owned()
}

impl<'a> Builder<'a> {
    pub fn new(cst: &Cst, file: &'a SourceFile, region_end: u32) -> Result<Self, Unsupported> {
        let toks = cst.tokens();
        let mut gaps = vec![Gap::default(); toks.len()];
        let mut prev: Option<usize> = None;
        let mut cur = Gap::default();
        let mut nl = 0u32;
        let mut same_line = true;
        let mut significant = 0usize;
        for (i, t) in toks.iter().enumerate() {
            use SyntaxKind::*;
            match t.kind {
                Whitespace => {
                    let n = file
                        .slice(t.start, t.end())
                        .iter()
                        .filter(|&&b| b == b'\n')
                        .count() as u32;
                    nl += n;
                    if n > 0 {
                        same_line = false;
                    }
                }
                LineComment | BlockComment | IncludeDirective => {
                    let mut text = text_of(file, t.start, t.end());
                    let line = file.line_of(t.start);
                    let kind = match t.kind {
                        LineComment => {
                            // The line break is ours to write (a CRLF file
                            // gets its `\r` back with it), and so are
                            // trailing blanks, except at the top of the
                            // file: there OpenSCAD takes a comment's text
                            // as a customizer description or parameter,
                            // blanks included.
                            while text.ends_with('\r') {
                                text.pop();
                            }
                            if line >= region_end {
                                let n = text.trim_end_matches([' ', '\t', '\r']).len();
                                text.truncate(n);
                            }
                            CommentKind::Line
                        }
                        BlockComment => CommentKind::Block,
                        _ => CommentKind::Directive,
                    };
                    let start = file.slice(0, t.start);
                    let bol = start.iter().rposition(|&b| b == b'\n').map_or(0, |i| i + 1);
                    let before = &start[bol..];
                    let indent = if before.iter().all(|&b| b == b' ' || b == b'\t') {
                        text_of(file, bol as u32, t.start)
                    } else {
                        std::string::String::new()
                    };
                    let c = Comment {
                        text,
                        kind,
                        nl_before: nl,
                        line,
                        indent,
                    };
                    match prev {
                        Some(p) if same_line => gaps[p].trailing.push(c),
                        _ => cur.leading.push(c),
                    }
                    nl = 0;
                }
                DroppedNumber => {
                    return Err(Unsupported(
                        "a number literal out of range (OpenSCAD drops it from the program)".into(),
                    ));
                }
                Ignored => {
                    return Err(Unsupported(
                        "a NUL byte (OpenSCAD ignores the rest of the file)".into(),
                    ));
                }
                Eot => return Err(Unsupported("an end-of-text byte (0x03)".into())),
                Error => return Err(Unsupported("a lexical error".into())),
                _ => {
                    cur.nl_before = nl;
                    gaps[i].leading = std::mem::take(&mut cur.leading);
                    gaps[i].nl_before = nl;
                    prev = Some(i);
                    nl = 0;
                    same_line = true;
                    significant += 1;
                }
            }
        }
        cur.nl_before = nl;
        Ok(Builder {
            file,
            gaps,
            eof: cur,
            item: false,
            top: false,
            emitted: 0,
            significant,
            region_end,
            failed: None,
        })
    }

    /// The finished document, or why the tree was not what the builder
    /// expects.
    pub fn file(mut self, root: Node<'_>) -> Result<Doc, String> {
        let stmts: Vec<Node<'_>> = root.children().collect();
        let body = self.stmts(&stmts, true);
        let eof = std::mem::take(&mut self.eof);
        let mut v = vec![body];
        let has_stmts = !stmts.is_empty();
        self.comments(&eof.leading, eof.nl_before, has_stmts, true, &mut v);
        if let Some(f) = self.failed {
            return Err(f);
        }
        if self.emitted != self.significant {
            return Err(format!(
                "printed {} of {} tokens",
                self.emitted, self.significant
            ));
        }
        Ok(Doc::Concat(v))
    }

    fn fail(&mut self, what: &str) -> Doc {
        if self.failed.is_none() {
            self.failed = Some(format!("unexpected syntax tree shape: {what}"));
        }
        Doc::Concat(Vec::new())
    }

    fn line_of(&self, offset: u32) -> u32 {
        self.file.line_of(offset)
    }

    fn gap(&self, t: TokenRef<'_>) -> &Gap {
        &self.gaps[t.index() as usize]
    }

    /// Own-line comments, each after a line break (a blank line kept when
    /// there was one), followed by a line break when `then_nl` (the thing
    /// after them starts a line).
    ///
    /// `top`: comments between top-level statements. In the customizer's
    /// region there an indented `//` comment keeps its indent: OpenSCAD
    /// takes a comment in column 1 on the line before an assignment as
    /// its description, so moving it to column 1 would add one.
    fn comments(&self, cs: &[Comment], after_nl: u32, blank_ok: bool, top: bool, v: &mut Vec<Doc>) {
        for (i, c) in cs.iter().enumerate() {
            if c.nl_before >= 2 && (blank_ok || i > 0) {
                v.push(Doc::Blank);
            } else if c.nl_before >= 1 {
                v.push(Doc::EnsureLine);
            } else {
                v.push(Doc::Space);
            }
            let keep = top
                && c.kind == CommentKind::Line
                && c.line < self.region_end
                && !c.indent.is_empty()
                && c.nl_before >= 1;
            if keep {
                v.push(Doc::Text(format!("{}{}", c.indent, c.text)));
            } else {
                v.push(Doc::Text(c.text.clone()));
            }
            let next_nl = cs.get(i + 1).map_or(after_nl, |n| n.nl_before);
            if c.kind != CommentKind::Block || next_nl >= 1 {
                v.push(Doc::EnsureLine);
            } else {
                v.push(Doc::Space);
            }
        }
    }

    /// The comments before `t` and whether a blank line goes before it,
    /// without the token.
    fn lead(&mut self, t: TokenRef<'_>, v: &mut Vec<Doc>) {
        let item = std::mem::take(&mut self.item);
        let top = std::mem::take(&mut self.top);
        let g = self.gap(t).clone();
        self.comments(&g.leading, g.nl_before, item, top, v);
        if item && g.nl_before >= 2 {
            v.push(Doc::Blank);
        }
    }

    /// The token itself and the comments after it on its line.
    fn tail(&mut self, t: TokenRef<'_>, v: &mut Vec<Doc>) {
        self.bare(t, v);
        self.trailing(t, v);
    }

    /// The token's text alone; [`Self::trailing`] gives its comments.
    fn bare(&mut self, t: TokenRef<'_>, v: &mut Vec<Doc>) {
        self.emitted += 1;
        v.push(Doc::Text(text_of(
            self.file,
            t.token().start,
            t.token().end(),
        )));
    }

    /// The comments after `t` on its line.
    fn trailing(&mut self, t: TokenRef<'_>, v: &mut Vec<Doc>) {
        let trailing = self.gap(t).trailing.clone();
        for c in trailing {
            match c.kind {
                // The line ends after a `//` comment, wherever the layout
                // would have gone on: a comment stays after the token it
                // followed.
                CommentKind::Line => {
                    v.push(Doc::Space);
                    v.push(Doc::Text(c.text));
                    v.push(Doc::EnsureLine);
                }
                _ => {
                    v.push(Doc::Space);
                    v.push(Doc::Text(c.text));
                    v.push(Doc::Space);
                }
            }
        }
    }

    fn tok(&mut self, t: TokenRef<'_>) -> Doc {
        let mut v = Vec::new();
        self.lead(t, &mut v);
        self.tail(t, &mut v);
        Doc::Concat(v)
    }

    fn has_comments(&self, t: TokenRef<'_>) -> bool {
        let g = self.gap(t);
        !g.leading.is_empty() || !g.trailing.is_empty()
    }

    /// Whether the original had a line break (or an own-line comment)
    /// before the first token of `n`.
    fn newline_before(&self, n: Node<'_>) -> bool {
        match first_tok(n) {
            Some(t) => {
                let g = self.gap(t);
                g.nl_before > 0 || !g.leading.is_empty()
            }
            None => false,
        }
    }

    // --- statements --------------------------------------------------------

    fn stmts(&mut self, nodes: &[Node<'_>], top: bool) -> Doc {
        let mut v = Vec::new();
        let mut prev_last_line = 0u32;
        for (i, &n) in nodes.iter().enumerate() {
            let span = n.span().unwrap_or_default();
            let (first, last) = (self.line_of(span.start), self.line_of(span.end));
            // OpenSCAD reads customizer annotations from the raw lines of
            // the top of the main file: a trailing `//` comment belongs to
            // the assignment only when it is alone on its line, the line
            // before holds its description, and an assignment on the line
            // of the first `{` is no parameter. So there, an assignment
            // that shared a line with the statement before keeps sharing
            // it.
            let in_region = top && first < self.region_end;
            if i > 0 {
                if in_region && first == prev_last_line && n.kind() == SyntaxKind::Assignment {
                    v.push(Doc::Space);
                } else {
                    v.push(Doc::Hard);
                }
            }
            self.item = true;
            self.top = top;
            let mut d = self.stmt(n);
            if in_region
                && n.kind() == SyntaxKind::Assignment
                && first == last
                && self.ends_in_line_comment(n)
            {
                // Wrapping would move the annotation comment off the
                // assignment's first line.
                d = flatten(d);
            }
            v.push(d);
            prev_last_line = last;
        }
        Doc::Concat(v)
    }

    /// Whether the last token of `n` is followed by a `//` comment.
    fn ends_in_line_comment(&self, n: Node<'_>) -> bool {
        let last = significant(n).into_iter().rev().find_map(|e| match e {
            Element::Token(t) => Some(t),
            Element::Node(_) => None,
        });
        last.is_some_and(|t| {
            self.gap(t)
                .trailing
                .iter()
                .any(|c| c.kind == CommentKind::Line)
        })
    }

    fn stmt(&mut self, n: Node<'_>) -> Doc {
        use SyntaxKind::*;
        match n.kind() {
            UseStmt | EmptyStmt => self.tokens_plain(n),
            BlockStmt => self.block(n),
            Assignment => self.assignment(n),
            ModuleDef => self.module_def(n),
            FunctionDef => self.function_def(n),
            ModuleInst | ModifierInst | IfInst => self.inst(n),
            ChildBlock => self.block(n),
            k => self.fail(&format!("statement {k:?}")),
        }
    }

    /// A node of tokens only, printed without spaces between them.
    fn tokens_plain(&mut self, n: Node<'_>) -> Doc {
        let toks: Vec<TokenRef<'_>> = n.tokens().collect();
        Doc::Concat(toks.into_iter().map(|t| self.tok(t)).collect())
    }

    /// `{ statements }`.
    fn block(&mut self, n: Node<'_>) -> Doc {
        let (Some(open), Some(close)) = (n.token(SyntaxKind::LBrace), n.token(SyntaxKind::RBrace))
        else {
            return self.fail("block without braces");
        };
        let inner: Vec<Node<'_>> = n.children().collect();
        let open_doc = self.tok(open);
        let body = self.stmts(&inner, false);
        let mut close_lead = Vec::new();
        let g = self.gap(close).clone();
        self.comments(&g.leading, g.nl_before, true, false, &mut close_lead);
        let mut close_doc = Vec::new();
        self.tail(close, &mut close_doc);
        if inner.is_empty() && close_lead.is_empty() && self.gap(open).trailing.is_empty() {
            return Doc::Concat(vec![open_doc, Doc::Concat(close_doc)]);
        }
        Doc::Concat(vec![
            open_doc,
            Doc::NoBlank,
            Doc::indent(Doc::Concat(vec![Doc::Hard, body, Doc::Concat(close_lead)])),
            Doc::Hard,
            Doc::Concat(close_doc),
        ])
    }

    fn assignment(&mut self, n: Node<'_>) -> Doc {
        let mut c = Cursor::new(n);
        let (Some(name), Some(eq), Some(e), Some(semi)) = (c.tok(), c.tok(), c.node(), c.tok())
        else {
            return self.fail("assignment");
        };
        let name = self.tok(name);
        let eq = self.tok(eq);
        let rhs = self.rhs(e);
        let semi = self.tok(semi);
        Doc::Concat(vec![name, Doc::Space, eq, rhs, semi])
    }

    /// The right-hand side of `=`: bracketed values start on the same line
    /// and break inside; others move to the next line when too long.
    fn rhs(&mut self, e: Node<'_>) -> Doc {
        use SyntaxKind::*;
        let hug = matches!(e.kind(), VectorExpr | CallExpr | FunctionExpr | RangeExpr)
            && !self.newline_before(e);
        let d = self.expr(e);
        if hug {
            Doc::Concat(vec![Doc::Space, d])
        } else {
            Doc::group(Doc::indent(Doc::Concat(vec![Doc::Line, d])))
        }
    }

    fn module_def(&mut self, n: Node<'_>) -> Doc {
        let mut c = Cursor::new(n);
        let (Some(kw), Some(name), Some(lp), Some(params), Some(rp), Some(body)) =
            (c.tok(), c.tok(), c.tok(), c.node(), c.tok(), c.node())
        else {
            return self.fail("module definition");
        };
        let kw = self.tok(kw);
        let name = self.tok(name);
        let params = self.params(lp, params, rp);
        let body = match body.kind() {
            SyntaxKind::BlockStmt => Doc::Concat(vec![Doc::Space, self.block(body)]),
            SyntaxKind::EmptyStmt => self.stmt(body),
            _ => self.child_on_line(body),
        };
        Doc::Concat(vec![kw, Doc::Space, name, params, body])
    }

    fn function_def(&mut self, n: Node<'_>) -> Doc {
        let mut c = Cursor::new(n);
        let (Some(kw), Some(name), Some(lp), Some(params), Some(rp), Some(eq), Some(e), Some(semi)) = (
            c.tok(),
            c.tok(),
            c.tok(),
            c.node(),
            c.tok(),
            c.tok(),
            c.node(),
            c.tok(),
        ) else {
            return self.fail("function definition");
        };
        let kw = self.tok(kw);
        let name = self.tok(name);
        let params = self.params(lp, params, rp);
        let eq = self.tok(eq);
        let rhs = self.rhs(e);
        let semi = self.tok(semi);
        Doc::Concat(vec![
            kw,
            Doc::Space,
            name,
            params,
            Doc::Space,
            eq,
            rhs,
            semi,
        ])
    }

    /// A child statement after an instantiation's `)` (or `else`).
    fn child(&mut self, n: Node<'_>) -> Doc {
        match n.kind() {
            SyntaxKind::EmptyStmt => self.stmt(n),
            SyntaxKind::ChildBlock | SyntaxKind::BlockStmt => {
                Doc::Concat(vec![Doc::Space, self.block(n)])
            }
            _ => self.child_on_line(n),
        }
    }

    /// A single child instantiation: on the next line, indented, when it
    /// was there in the source (a chain of transforms written one per
    /// line stays that way); otherwise on the same line if it fits.
    fn child_on_line(&mut self, n: Node<'_>) -> Doc {
        let nl = self.newline_before(n);
        let d = self.stmt(n);
        if nl {
            Doc::indent(Doc::Concat(vec![Doc::Hard, d]))
        } else {
            Doc::group(Doc::indent(Doc::Concat(vec![Doc::Line, d])))
        }
    }

    fn inst(&mut self, n: Node<'_>) -> Doc {
        use SyntaxKind::*;
        match n.kind() {
            ModifierInst => {
                let mut c = Cursor::new(n);
                let (Some(op), Some(inner)) = (c.tok(), c.node()) else {
                    return self.fail("modifier");
                };
                let op = self.tok(op);
                Doc::Concat(vec![op, self.inst(inner)])
            }
            IfInst => {
                let mut c = Cursor::new(n);
                let (Some(kw), Some(lp), Some(cond), Some(rp), Some(then)) =
                    (c.tok(), c.tok(), c.node(), c.tok(), c.node())
                else {
                    return self.fail("if");
                };
                let mut v = vec![self.tok(kw), Doc::Space, self.tok(lp)];
                v.push(self.expr(cond));
                v.push(self.tok(rp));
                let then_block = matches!(then.kind(), ChildBlock | BlockStmt);
                let then_nl = !then_block && self.newline_before(then);
                v.push(self.child(then));
                if let Some(e) = c.node() {
                    let mut ec = Cursor::new(e);
                    let (Some(kw), Some(body)) = (ec.tok(), ec.node()) else {
                        return self.fail("else");
                    };
                    let nl = self.gap(kw).nl_before > 0 || !self.gap(kw).leading.is_empty();
                    v.push(if !then_block && (nl || then_nl) {
                        Doc::Hard
                    } else {
                        Doc::Space
                    });
                    v.push(self.tok(kw));
                    if body.kind() == IfInst && !self.newline_before(body) {
                        v.push(Doc::Space);
                        v.push(self.inst(body));
                    } else {
                        v.push(self.child(body));
                    }
                }
                Doc::Concat(v)
            }
            ModuleInst => {
                let mut c = Cursor::new(n);
                let (Some(name), Some(lp), Some(args), Some(rp), Some(child)) =
                    (c.tok(), c.tok(), c.node(), c.tok(), c.node())
                else {
                    return self.fail("module instantiation");
                };
                let bind = binds(name, self.file);
                let mut v = vec![self.tok(name)];
                if bind {
                    v.push(Doc::Space);
                }
                v.push(self.args(lp, args, rp, bind));
                v.push(self.child(child));
                Doc::Concat(v)
            }
            _ => self.stmt(n),
        }
    }

    // --- lists ---------------------------------------------------------------

    fn params(&mut self, lp: TokenRef<'_>, list: Node<'_>, rp: TokenRef<'_>) -> Doc {
        let items = self.items(list, |b, p| b.param(p));
        self.list(lp, items, rp, false)
    }

    fn param(&mut self, p: Node<'_>) -> Doc {
        let mut v = Vec::new();
        for e in significant(p) {
            match e {
                Element::Token(t) => v.push(self.tok(t)),
                Element::Node(n) => v.push(self.expr(n)),
            }
        }
        Doc::Concat(v)
    }

    /// `(args)`. `bind`: `for`/`let` bindings, spaced as assignments
    /// (`i = [0:3]`); named arguments are not (`center=true`).
    fn args(&mut self, lp: TokenRef<'_>, list: Node<'_>, rp: TokenRef<'_>, bind: bool) -> Doc {
        let items = self.items(list, |b, a| b.arg(a, bind));
        // A lone vector argument hugs the parentheses:
        // `polygon([` ... `])` rather than a vector on its own lines.
        let args: Vec<Node<'_>> = list.children().collect();
        let hug = args.len() == 1
            && list.tokens().next().is_none()
            && args[0].children().last().map(|e| e.kind()) == Some(SyntaxKind::VectorExpr)
            && args[0].tokens().next().is_none()
            && !self.has_comments(lp)
            // A comment after the `)` stays after it (see `list_with`).
            && self.gap(rp).leading.is_empty();
        self.list(lp, items, rp, hug)
    }

    fn arg(&mut self, a: Node<'_>, bind: bool) -> Doc {
        let mut v = Vec::new();
        for e in significant(a) {
            match e {
                Element::Token(t) if t.kind() == SyntaxKind::Eq && bind => {
                    v.push(Doc::Space);
                    v.push(self.tok(t));
                    v.push(Doc::Space);
                }
                Element::Token(t) => v.push(self.tok(t)),
                Element::Node(n) => v.push(self.expr(n)),
            }
        }
        Doc::Concat(v)
    }

    /// The items of a list node (its child nodes), each with the comma
    /// after it.
    fn items(&mut self, list: Node<'_>, mut f: impl FnMut(&mut Self, Node<'_>) -> Doc) -> Vec<Doc> {
        let mut out: Vec<Doc> = Vec::new();
        for e in significant(list) {
            match e {
                Element::Node(n) => {
                    self.item = true;
                    out.push(f(self, n));
                }
                Element::Token(t) if t.kind() == SyntaxKind::Comma => {
                    let d = self.tok(t);
                    match out.last_mut() {
                        Some(last) => *last = Doc::Concat(vec![std::mem::take(last), d]),
                        None => out.push(d),
                    }
                }
                Element::Token(t) => {
                    let d = self.tok(t);
                    out.push(d);
                }
            }
        }
        out
    }

    /// `open item, item, ... close`: on one line when it fits, otherwise
    /// one item per line (`fill`: as many per line as fit).
    fn list_with(
        &mut self,
        open: TokenRef<'_>,
        items: Vec<Doc>,
        close: TokenRef<'_>,
        hug: bool,
        fill: bool,
    ) -> Doc {
        let open = self.tok(open);
        let mut close_lead = Vec::new();
        let g = self.gap(close).clone();
        self.comments(&g.leading, g.nl_before, true, false, &mut close_lead);
        let mut close_doc = Vec::new();
        self.bare(close, &mut close_doc);
        let close_doc = Doc::Concat(close_doc);
        // A `//` comment after the closing bracket ends the line after the
        // list, not inside it: in the group, its line break would break the
        // list too, and `linear_extrude(5) // c` became
        // `linear_extrude(\n    5\n) // c` (the T2 transcript audit).
        let mut after = Vec::new();
        self.trailing(close, &mut after);
        let after = Doc::Concat(after);
        if items.is_empty() && close_lead.is_empty() {
            return Doc::Concat(vec![open, close_doc, after]);
        }
        if hug && close_lead.is_empty() {
            return Doc::Concat(
                std::iter::once(open)
                    .chain(items)
                    .chain([close_doc, after])
                    .collect(),
            );
        }
        let mut body = Vec::new();
        for (i, it) in items.into_iter().enumerate() {
            if i > 0 {
                body.push(Doc::Line);
            }
            body.push(it);
        }
        let body = if fill {
            Doc::Fill(body)
        } else {
            Doc::Concat(body)
        };
        let group = Doc::group(Doc::Concat(vec![
            open,
            Doc::NoBlank,
            Doc::indent(Doc::Concat(vec![
                Doc::SoftLine,
                body,
                Doc::Concat(close_lead),
            ])),
            Doc::SoftLine,
            close_doc,
        ]));
        Doc::Concat(vec![group, after])
    }

    fn list(&mut self, open: TokenRef<'_>, items: Vec<Doc>, close: TokenRef<'_>, hug: bool) -> Doc {
        self.list_with(open, items, close, hug, false)
    }

    // --- expressions ---------------------------------------------------------

    fn expr(&mut self, n: Node<'_>) -> Doc {
        use SyntaxKind::*;
        match n.kind() {
            Literal | NameRef => self.tokens_plain(n),
            ParenExpr | LcParen => {
                let mut v = Vec::new();
                for e in significant(n) {
                    v.push(match e {
                        Element::Token(t) => self.tok(t),
                        Element::Node(c) => self.expr(c),
                    });
                }
                Doc::Concat(v)
            }
            UnaryExpr => {
                let mut c = Cursor::new(n);
                let (Some(op), Some(e)) = (c.tok(), c.node()) else {
                    return self.fail("unary");
                };
                let op = self.tok(op);
                Doc::Concat(vec![op, self.expr(e)])
            }
            BinaryExpr => self.binary(n),
            TernaryExpr => self.ternary(n),
            CallExpr => {
                let mut c = Cursor::new(n);
                let (Some(f), Some(lp), Some(args), Some(rp)) =
                    (c.node(), c.tok(), c.node(), c.tok())
                else {
                    return self.fail("call");
                };
                let f = self.expr(f);
                Doc::Concat(vec![f, self.args(lp, args, rp, false)])
            }
            IndexExpr | MemberExpr => {
                let mut v = Vec::new();
                for e in significant(n) {
                    v.push(match e {
                        Element::Token(t) => self.tok(t),
                        Element::Node(c) => self.expr(c),
                    });
                }
                Doc::Concat(v)
            }
            RangeExpr => {
                let spaced = n.children().any(|c| !atomic(c));
                let mut v = Vec::new();
                for e in significant(n) {
                    match e {
                        Element::Token(t) if t.kind() == Colon && spaced => {
                            v.push(Doc::Space);
                            v.push(self.tok(t));
                            v.push(Doc::Space);
                        }
                        Element::Token(t) => v.push(self.tok(t)),
                        Element::Node(c) => v.push(self.expr(c)),
                    }
                }
                Doc::Concat(v)
            }
            VectorExpr => {
                let (Some(open), Some(close)) = (n.token(LBrack), n.token(RBrack)) else {
                    return self.fail("vector");
                };
                let fill = n.children().count() > 1 && n.children().all(numeric);
                let items = self.items_between(n, open, close);
                self.list_with(open, items, close, false, fill)
            }
            FunctionExpr => {
                let mut c = Cursor::new(n);
                let (Some(kw), Some(lp), Some(params), Some(rp), Some(body)) =
                    (c.tok(), c.tok(), c.node(), c.tok(), c.node())
                else {
                    return self.fail("function literal");
                };
                let kw = self.tok(kw);
                let params = self.params(lp, params, rp);
                let body = self.expr(body);
                Doc::group(Doc::Concat(vec![
                    kw,
                    params,
                    Doc::indent(Doc::Concat(vec![Doc::Line, body])),
                ]))
            }
            LetExpr | AssertExpr | EchoExpr | LcLet => {
                let mut c = Cursor::new(n);
                let (Some(kw), Some(lp), Some(args), Some(rp)) =
                    (c.tok(), c.tok(), c.node(), c.tok())
                else {
                    return self.fail("let");
                };
                let bind = kw.kind() == KwLet;
                let mut v = vec![self.tok(kw)];
                if bind {
                    v.push(Doc::Space);
                }
                v.push(self.args(lp, args, rp, bind));
                if let Some(body) = c.node() {
                    v.push(Doc::Line);
                    v.push(self.expr(body));
                }
                Doc::group(Doc::Concat(v))
            }
            LcEach => {
                let mut c = Cursor::new(n);
                let (Some(kw), Some(body)) = (c.tok(), c.node()) else {
                    return self.fail("each");
                };
                let kw = self.tok(kw);
                Doc::Concat(vec![kw, Doc::Space, self.expr(body)])
            }
            LcFor | LcForC => self.lc_for(n),
            LcIf => self.lc_if(n),
            k => self.fail(&format!("expression {k:?}")),
        }
    }

    /// The elements of a vector between its brackets, each with its comma.
    fn items_between(&mut self, n: Node<'_>, open: TokenRef<'_>, close: TokenRef<'_>) -> Vec<Doc> {
        let mut out: Vec<Doc> = Vec::new();
        for e in significant(n) {
            match e {
                Element::Token(t) if t == open || t == close => {}
                Element::Token(t) => {
                    let d = self.tok(t);
                    match out.last_mut() {
                        Some(last) => *last = Doc::Concat(vec![std::mem::take(last), d]),
                        None => out.push(d),
                    }
                }
                Element::Node(c) => {
                    self.item = true;
                    out.push(self.expr(c));
                }
            }
        }
        out
    }

    fn binary(&mut self, n: Node<'_>) -> Doc {
        let Some(op) = op_token(n) else {
            return self.fail("binary operator");
        };
        if op.kind() == SyntaxKind::Caret {
            let mut c = Cursor::new(n);
            let (Some(l), Some(op), Some(r)) = (c.node(), c.tok(), c.node()) else {
                return self.fail("power");
            };
            let l = self.expr(l);
            let op = self.tok(op);
            let r = self.expr(r);
            return Doc::Concat(vec![l, Doc::Space, op, Doc::Space, r]);
        }
        // Flatten a left-leaning chain of one precedence level
        // (`a + b - c`), so it breaks as one list of operands.
        let lvl = level(op.kind());
        let mut chain = vec![n];
        let mut cur = n;
        while let Some(l) = cur.children().next() {
            match op_token(l) {
                Some(o) if l.kind() == SyntaxKind::BinaryExpr && level(o.kind()) == lvl => {
                    chain.push(l);
                    cur = l;
                }
                _ => break,
            }
        }
        let first = cur.children().next();
        let Some(first) = first else {
            return self.fail("binary operand");
        };
        let head = self.expr(first);
        let mut rest = Vec::new();
        for b in chain.iter().rev() {
            let mut c = Cursor::new(*b);
            let (Some(_), Some(op), Some(r)) = (c.node(), c.tok(), c.node()) else {
                return self.fail("binary");
            };
            rest.push(Doc::Space);
            rest.push(self.tok(op));
            rest.push(Doc::Line);
            rest.push(self.expr(r));
        }
        Doc::group(Doc::Concat(vec![head, Doc::indent(Doc::Concat(rest))]))
    }

    fn ternary(&mut self, n: Node<'_>) -> Doc {
        let mut v = Vec::new();
        let mut cur = n;
        loop {
            let mut c = Cursor::new(cur);
            let (Some(cond), Some(q), Some(a), Some(colon), Some(b)) =
                (c.node(), c.tok(), c.node(), c.tok(), c.node())
            else {
                return self.fail("ternary");
            };
            // `cond ? value` on one line if it fits, else the value on
            // the next, indented under its condition.
            let cond = self.expr(cond);
            let q = self.tok(q);
            let a = self.expr(a);
            v.push(Doc::group(Doc::Concat(vec![
                cond,
                Doc::indent(Doc::Concat(vec![
                    Doc::Line,
                    q,
                    Doc::Space,
                    Doc::Align(2, Box::new(a)),
                ])),
            ])));
            v.push(Doc::Line);
            v.push(self.tok(colon));
            v.push(Doc::Space);
            if b.kind() == SyntaxKind::TernaryExpr {
                // A chain (`a ? x : b ? y : z`) breaks as one: each
                // condition after a `:` at the same column.
                cur = b;
                continue;
            }
            let b = self.expr(b);
            v.push(Doc::Align(2, Box::new(b)));
            break;
        }
        Doc::group(Doc::Concat(v))
    }

    fn lc_for(&mut self, n: Node<'_>) -> Doc {
        let mut v = Vec::new();
        let mut body = None;
        let els: Vec<Element<'_>> = significant(n);
        let last = els.len().saturating_sub(1);
        for (i, e) in els.into_iter().enumerate() {
            match e {
                Element::Token(t) if t.kind() == SyntaxKind::KwFor => {
                    v.push(self.tok(t));
                    v.push(Doc::Space);
                }
                Element::Token(t) if t.kind() == SyntaxKind::Semi => {
                    v.push(self.tok(t));
                    v.push(Doc::Space);
                }
                Element::Token(t) => v.push(self.tok(t)),
                Element::Node(c) if i == last => body = Some(c),
                Element::Node(c) if c.kind() == SyntaxKind::ArgList => {
                    // `for (i = [0:3])`, or the init and update lists of
                    // a C-style `for (i = 0; i < 3; i = i + 1)`.
                    let items = self.items(c, |b, a| b.arg(a, true));
                    let mut w = Vec::new();
                    for (k, it) in items.into_iter().enumerate() {
                        if k > 0 {
                            w.push(Doc::Space);
                        }
                        w.push(it);
                    }
                    v.push(Doc::group(Doc::Concat(w)));
                }
                Element::Node(c) => v.push(self.expr(c)),
            }
        }
        let Some(body) = body else {
            return self.fail("for body");
        };
        let body = self.expr(body);
        v.push(Doc::indent(Doc::Concat(vec![Doc::Line, body])));
        Doc::group(Doc::Concat(v))
    }

    fn lc_if(&mut self, n: Node<'_>) -> Doc {
        let mut c = Cursor::new(n);
        let (Some(kw), Some(lp), Some(cond), Some(rp), Some(then)) =
            (c.tok(), c.tok(), c.node(), c.tok(), c.node())
        else {
            return self.fail("comprehension if");
        };
        let mut v = vec![self.tok(kw), Doc::Space, self.tok(lp)];
        v.push(self.expr(cond));
        v.push(self.tok(rp));
        let then = self.expr(then);
        v.push(Doc::indent(Doc::Concat(vec![Doc::Line, then])));
        if let Some(kw) = c.tok() {
            let Some(e) = c.node() else {
                return self.fail("comprehension else");
            };
            v.push(Doc::Line);
            v.push(self.tok(kw));
            if e.kind() == SyntaxKind::LcIf {
                v.push(Doc::Space);
                v.push(self.expr(e));
            } else {
                let e = self.expr(e);
                v.push(Doc::indent(Doc::Concat(vec![Doc::Line, e])));
            }
        }
        Doc::group(Doc::Concat(v))
    }
}

/// Print `d` on one line: every group flat.
fn flatten(d: Doc) -> Doc {
    match d {
        Doc::Line => Doc::Space,
        Doc::SoftLine => Doc::Concat(Vec::new()),
        Doc::Group(inner, _) => flatten(*inner),
        Doc::Indent(inner) => Doc::Indent(Box::new(flatten(*inner))),
        Doc::Align(n, inner) => Doc::Align(n, Box::new(flatten(*inner))),
        Doc::Concat(v) => Doc::Concat(v.into_iter().map(flatten).collect()),
        Doc::Fill(v) => Doc::Concat(v.into_iter().map(flatten).collect()),
        other => other,
    }
}

/// The significant children of a node, in order.
fn significant(n: Node<'_>) -> Vec<Element<'_>> {
    n.children_with_tokens()
        .filter(|e| match e {
            Element::Token(t) => !t.kind().is_trivia(),
            Element::Node(_) => true,
        })
        .collect()
}

struct Cursor<'t> {
    it: std::vec::IntoIter<Element<'t>>,
    peeked: Option<Element<'t>>,
}

impl<'t> Cursor<'t> {
    fn new(n: Node<'t>) -> Self {
        Cursor {
            it: significant(n).into_iter(),
            peeked: None,
        }
    }

    fn next(&mut self) -> Option<Element<'t>> {
        self.peeked.take().or_else(|| self.it.next())
    }

    fn tok(&mut self) -> Option<TokenRef<'t>> {
        match self.next()? {
            Element::Token(t) => Some(t),
            e => {
                self.peeked = Some(e);
                None
            }
        }
    }

    fn node(&mut self) -> Option<Node<'t>> {
        match self.next()? {
            Element::Node(n) => Some(n),
            e => {
                self.peeked = Some(e);
                None
            }
        }
    }
}

fn first_tok(n: Node<'_>) -> Option<TokenRef<'_>> {
    for e in significant(n) {
        match e {
            Element::Token(t) => return Some(t),
            Element::Node(c) => {
                if let Some(t) = first_tok(c) {
                    return Some(t);
                }
            }
        }
    }
    None
}

fn op_token(n: Node<'_>) -> Option<TokenRef<'_>> {
    (n.kind() == SyntaxKind::BinaryExpr)
        .then(|| n.tokens().next())
        .flatten()
}

fn level(k: SyntaxKind) -> u8 {
    use SyntaxKind::*;
    match k {
        OrOr => 0,
        AndAnd => 1,
        EqEq | Ne => 2,
        Gt | Ge | Lt | Le => 3,
        Pipe => 4,
        Amp => 5,
        Shl | Shr => 6,
        Plus | Minus => 7,
        Star | Slash | Percent => 8,
        _ => 9,
    }
}

/// `for`, `let` and `intersection_for` take bindings, spaced as
/// assignments.
fn binds(name: TokenRef<'_>, file: &SourceFile) -> bool {
    match name.kind() {
        SyntaxKind::KwFor | SyntaxKind::KwLet => true,
        SyntaxKind::Ident => {
            let t = name.token();
            file.slice(t.start, t.end()) == b"intersection_for"
        }
        _ => false,
    }
}

/// A literal, a name, or a negated one: range bounds printed without
/// spaces (`[0:n]`).
fn atomic(n: Node<'_>) -> bool {
    match n.kind() {
        SyntaxKind::Literal | SyntaxKind::NameRef => true,
        SyntaxKind::UnaryExpr => n.children().next().is_some_and(atomic),
        _ => false,
    }
}

/// A number literal, possibly negated: vectors of them fill lines.
fn numeric(n: Node<'_>) -> bool {
    match n.kind() {
        SyntaxKind::Literal => n.tokens().next().map(|t| t.kind()) == Some(SyntaxKind::Number),
        SyntaxKind::UnaryExpr => n.children().next().is_some_and(numeric),
        _ => false,
    }
}
