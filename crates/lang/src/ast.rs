//! The typed AST the evaluator consumes, lowered from the [`Cst`].
//!
//! The AST follows OpenSCAD's own model rather than the source text:
//!
//! - statements are sorted into a [`Scope`]'s functions, modules,
//!   assignments and instantiations, which is also the order OpenSCAD
//!   evaluates and prints them in;
//! - assigning a name twice in one scope replaces the first assignment's
//!   value in place (the last value wins, at the first position), with
//!   OpenSCAD's warnings;
//! - `*inst` is dropped, `+x` is `x`, `-<number>` is a negative literal and
//!   parentheses disappear;
//! - `let`, `echo` and `assert` expressions are their own node kinds.
//!
//! Expressions live in one arena ([`Ast::exprs`]) addressed by [`ExprId`];
//! names are interned ([`Name`]). Every node keeps its [`Span`].

use std::collections::HashMap;
use std::path::Path;

use crate::diag::{DiagCode, Diagnostic, PathBase, Severity};
use crate::loader::seq_for_token;
use crate::source::{SourceMap, Span};
use crate::syntax::SyntaxKind as K;
use crate::syntax::cst::{Cst, Node, TokenRef};
use crate::syntax::lexer::{number_value, string_value};

/// An interned identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Name(pub u32);

/// FxHash (rustc's hasher): identifiers are short and trusted, so a fast
/// non-cryptographic hash beats SipHash here.
#[derive(Debug, Default, Clone, Copy)]
struct FxHasher(u64);

impl std::hash::Hasher for FxHasher {
    fn write(&mut self, bytes: &[u8]) {
        for chunk in bytes.chunks(8) {
            let mut b = [0u8; 8];
            b[..chunk.len()].copy_from_slice(chunk);
            self.0 = (self.0.rotate_left(5) ^ u64::from_le_bytes(b)).wrapping_mul(0x51_7c_c1_b7_27_22_0a_95);
        }
    }
    fn write_u8(&mut self, i: u8) {
        self.0 = (self.0.rotate_left(5) ^ u64::from(i)).wrapping_mul(0x51_7c_c1_b7_27_22_0a_95);
    }
    fn finish(&self) -> u64 {
        self.0
    }
}

type FxBuild = std::hash::BuildHasherDefault<FxHasher>;

#[derive(Debug, Default)]
pub struct Interner {
    map: HashMap<Box<str>, Name, FxBuild>,
    names: Vec<Box<str>>,
}

impl Interner {
    pub fn intern(&mut self, s: &str) -> Name {
        if let Some(&n) = self.map.get(s) {
            return n;
        }
        let n = Name(self.names.len() as u32);
        self.names.push(s.into());
        self.map.insert(s.into(), n);
        n
    }

    pub fn get(&self, s: &str) -> Option<Name> {
        self.map.get(s).copied()
    }

    pub fn resolve(&self, n: Name) -> &str {
        &self.names[n.0 as usize]
    }

    /// All names, in [`Name`] order (names are numbered densely from 0).
    pub fn iter(&self) -> impl Iterator<Item = &str> {
        self.names.iter().map(|n| &**n)
    }

    pub fn len(&self) -> usize {
        self.names.len()
    }

    pub fn is_empty(&self) -> bool {
        self.names.is_empty()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ExprId(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnaryOp {
    Not,
    Negate,
    BinaryNot,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinaryOp {
    LogicalOr,
    LogicalAnd,
    Equal,
    NotEqual,
    Greater,
    GreaterEqual,
    Less,
    LessEqual,
    BinaryOr,
    BinaryAnd,
    ShiftLeft,
    ShiftRight,
    Plus,
    Minus,
    Multiply,
    Divide,
    Modulo,
    Exponent,
}

impl BinaryOp {
    pub fn as_str(self) -> &'static str {
        match self {
            BinaryOp::LogicalOr => "||",
            BinaryOp::LogicalAnd => "&&",
            BinaryOp::Equal => "==",
            BinaryOp::NotEqual => "!=",
            BinaryOp::Greater => ">",
            BinaryOp::GreaterEqual => ">=",
            BinaryOp::Less => "<",
            BinaryOp::LessEqual => "<=",
            BinaryOp::BinaryOr => "|",
            BinaryOp::BinaryAnd => "&",
            BinaryOp::ShiftLeft => "<<",
            BinaryOp::ShiftRight => ">>",
            BinaryOp::Plus => "+",
            BinaryOp::Minus => "-",
            BinaryOp::Multiply => "*",
            BinaryOp::Divide => "/",
            BinaryOp::Modulo => "%",
            BinaryOp::Exponent => "^",
        }
    }

    fn from_token(k: K) -> Option<Self> {
        Some(match k {
            K::OrOr => BinaryOp::LogicalOr,
            K::AndAnd => BinaryOp::LogicalAnd,
            K::EqEq => BinaryOp::Equal,
            K::Ne => BinaryOp::NotEqual,
            K::Gt => BinaryOp::Greater,
            K::Ge => BinaryOp::GreaterEqual,
            K::Lt => BinaryOp::Less,
            K::Le => BinaryOp::LessEqual,
            K::Pipe => BinaryOp::BinaryOr,
            K::Amp => BinaryOp::BinaryAnd,
            K::Shl => BinaryOp::ShiftLeft,
            K::Shr => BinaryOp::ShiftRight,
            K::Plus => BinaryOp::Plus,
            K::Minus => BinaryOp::Minus,
            K::Star => BinaryOp::Multiply,
            K::Slash => BinaryOp::Divide,
            K::Percent => BinaryOp::Modulo,
            K::Caret => BinaryOp::Exponent,
            _ => return None,
        })
    }
}

/// A named or positional argument.
#[derive(Debug, Clone, PartialEq)]
pub struct Arg {
    pub name: Option<Name>,
    pub expr: ExprId,
    pub span: Span,
}

/// A parameter of a module, function or function literal.
#[derive(Debug, Clone, PartialEq)]
pub struct Param {
    pub name: Name,
    pub default: Option<ExprId>,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ExprKind {
    Undef,
    Bool(bool),
    Number(f64),
    /// A byte string (see [`crate::syntax::lexer::string_value`]).
    String(Box<[u8]>),
    Var(Name),
    Unary(UnaryOp, ExprId),
    Binary(BinaryOp, ExprId, ExprId),
    Ternary(ExprId, ExprId, ExprId),
    Index(ExprId, ExprId),
    Member(ExprId, Name),
    Call(ExprId, Vec<Arg>),
    Range { begin: ExprId, step: Option<ExprId>, end: ExprId },
    Vector(Vec<ExprId>),
    Function(Vec<Param>, ExprId),
    Let(Vec<Arg>, ExprId),
    Assert(Vec<Arg>, Option<ExprId>),
    Echo(Vec<Arg>, Option<ExprId>),
    LcIf(ExprId, ExprId, Option<ExprId>),
    LcEach(ExprId),
    LcFor(Vec<Arg>, ExprId),
    LcForC { init: Vec<Arg>, cond: ExprId, incr: Vec<Arg>, body: ExprId },
    LcLet(Vec<Arg>, ExprId),
    /// Stands in for a missing or broken expression after a syntax error.
    Invalid,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Expr {
    pub kind: ExprKind,
    pub span: Span,
}

/// A source location as OpenSCAD tracks it for assignments.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Loc {
    pub span: Span,
    /// 1-based line of the first token.
    pub line: u32,
}

/// A customizer annotation (`//Group(...)`, `//Description(...)`,
/// `//Parameter(...)`) attached to an assignment.
#[derive(Debug, Clone, PartialEq)]
pub struct Annotation {
    pub name: &'static str,
    pub expr: ExprId,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Assignment {
    pub name: Name,
    pub expr: ExprId,
    /// Where the name was first assigned.
    pub loc: Loc,
    /// Where it was last reassigned, if it was.
    pub overwrite: Option<Loc>,
    pub annotations: Vec<Annotation>,
}

impl Assignment {
    pub fn annotation(&self, name: &str) -> Option<ExprId> {
        self.annotations.iter().find(|a| a.name == name).map(|a| a.expr)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ModuleDef {
    pub name: Name,
    pub params: Vec<Param>,
    pub body: Scope,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FunctionDef {
    pub name: Name,
    pub params: Vec<Param>,
    pub body: ExprId,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub enum InstKind {
    Module,
    /// `if (args[0]) children else else_children`.
    If { else_children: Option<Box<Scope>> },
}

#[derive(Debug, Clone, PartialEq)]
pub struct Instantiation {
    /// The module name; `if` for an if-statement.
    pub name: Name,
    pub args: Vec<Arg>,
    pub children: Scope,
    pub kind: InstKind,
    /// `!`
    pub tag_root: bool,
    /// `#`
    pub tag_highlight: bool,
    /// `%`
    pub tag_background: bool,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Scope {
    pub functions: Vec<FunctionDef>,
    pub modules: Vec<ModuleDef>,
    pub assignments: Vec<Assignment>,
    pub instantiations: Vec<Instantiation>,
}

impl Scope {
    /// OpenSCAD's `numElements`: what decides how a child scope is printed.
    pub fn num_elements(&self) -> usize {
        self.assignments.len() + self.instantiations.len()
    }
}

#[derive(Debug, Default)]
pub struct Ast {
    pub exprs: Vec<Expr>,
    pub names: Interner,
    pub root: Scope,
    /// `use`d libraries, most recent first, without duplicates
    /// (`SourceFile::registerUse`).
    pub uses: Vec<String>,
}

impl Ast {
    pub fn expr(&self, id: ExprId) -> &Expr {
        &self.exprs[id.0 as usize]
    }

    pub fn add(&mut self, kind: ExprKind, span: Span) -> ExprId {
        self.exprs.push(Expr { kind, span });
        ExprId(self.exprs.len() as u32 - 1)
    }

    pub fn name(&self, n: Name) -> &str {
        self.names.resolve(n)
    }

    /// OpenSCAD's `Expression::isLiteral`: literals, and vectors, ranges and
    /// unary operations made only of literals.
    pub fn is_literal(&self, id: ExprId) -> bool {
        match &self.expr(id).kind {
            ExprKind::Undef | ExprKind::Bool(_) | ExprKind::Number(_) | ExprKind::String(_) => true,
            ExprKind::Unary(_, e) => self.is_literal(*e),
            ExprKind::Vector(v) => v.iter().all(|&e| self.is_literal(e)),
            ExprKind::Range { begin, step, end } => {
                self.is_literal(*begin) && self.is_literal(*end) && step.is_none_or(|s| self.is_literal(s))
            }
            _ => false,
        }
    }
}

/// Lower a parsed program. `main` is the path OpenSCAD treats as the main
/// file for reassignment warnings: the program's own file, or for a `use`d
/// library the program that uses it.
pub fn lower(cst: &Cst, sources: &SourceMap, main: &Path, uses: &[crate::loader::UseRef]) -> (Ast, Vec<Diagnostic>) {
    let mut l = Lower { sources, main, ast: Ast::default(), diags: Vec::new(), file_ended: false, uses, next_use: 0 };
    let mut root = Scope::default();
    l.statements(cst.root(), &mut root);
    l.ast.root = root;
    (l.ast, l.diags)
}

struct Lower<'a> {
    sources: &'a SourceMap,
    main: &'a Path,
    ast: Ast,
    diags: Vec<Diagnostic>,
    /// Set once the `\x03` statement has been seen: assignments after it
    /// come from `-D` and overwrite silently.
    file_ended: bool,
    uses: &'a [crate::loader::UseRef],
    next_use: usize,
}

fn lossy(b: &[u8]) -> String {
    match std::str::from_utf8(b) {
        Ok(s) => s.to_owned(),
        Err(_) => String::from_utf8_lossy(b).into_owned(),
    }
}

impl<'a> Lower<'a> {
    fn text(&self, t: TokenRef<'_>) -> &'a [u8] {
        t.text(self.sources)
    }

    fn span(&self, n: Node<'_>) -> Span {
        n.span().unwrap_or_default()
    }

    fn loc(&self, n: Node<'_>) -> Loc {
        let span = self.span(n);
        let line = self.sources.get(span.file).line_of(span.start);
        Loc { span, line }
    }

    fn intern(&mut self, t: TokenRef<'_>) -> Name {
        match std::str::from_utf8(self.text(t)) {
            Ok(s) => self.ast.names.intern(s),
            Err(_) => {
                let s = lossy(self.text(t));
                self.ast.names.intern(&s)
            }
        }
    }

    fn invalid(&mut self, span: Span) -> ExprId {
        self.ast.add(ExprKind::Invalid, span)
    }

    // --- statements -----------------------------------------------------

    /// Children of the root, a block or a module body.
    fn statements(&mut self, n: Node<'_>, scope: &mut Scope) {
        for c in n.children() {
            self.statement(c, scope);
        }
    }

    fn statement(&mut self, n: Node<'_>, scope: &mut Scope) {
        match n.kind() {
            K::BlockStmt => self.statements(n, scope),
            K::Assignment => self.assignment(n, scope),
            K::ModuleDef => {
                let Some(name) = n.token(K::Ident) else { return };
                let name = self.intern(name);
                let params = n.children().find(|c| c.kind() == K::ParamList).map(|p| self.params(p)).unwrap_or_default();
                let mut body = Scope::default();
                if let Some(b) = n.children().filter(|c| c.kind() != K::ParamList).last() {
                    self.statement(b, &mut body);
                }
                let span = self.span(n);
                scope.modules.push(ModuleDef { name, params, body, span });
            }
            K::FunctionDef => {
                let Some(name) = n.token(K::Ident) else { return };
                let name = self.intern(name);
                let params = n.children().find(|c| c.kind() == K::ParamList).map(|p| self.params(p)).unwrap_or_default();
                let span = self.span(n);
                let body = match n.children().find(|c| c.kind() != K::ParamList) {
                    Some(b) => self.expr(b),
                    None => self.invalid(span),
                };
                scope.functions.push(FunctionDef { name, params, body, span });
            }
            K::EotStmt => self.file_ended = true,
            K::UseStmt => {
                if let Some(u) = self.uses.get(self.next_use) {
                    let path = u.path.clone();
                    self.next_use += 1;
                    self.ast.uses.retain(|p| *p != path);
                    self.ast.uses.insert(0, path);
                }
            }
            K::ModuleInst | K::ModifierInst | K::IfInst => {
                if let Some(i) = self.instantiation(n) {
                    scope.instantiations.push(i);
                }
            }
            _ => {}
        }
    }

    /// `handle_assignment` in parser.y.
    fn assignment(&mut self, n: Node<'_>, scope: &mut Scope) {
        let Some(id) = n.token(K::Ident) else { return };
        let name = self.intern(id);
        let loc = self.loc(n);
        let expr = match n.children().next() {
            Some(e) => self.expr(e),
            None => self.invalid(loc.span),
        };
        let seq = n.tokens().last().map_or(0, |t| seq_for_token(t.index()) + 1);
        if let Some(a) = scope.assignments.iter_mut().find(|a| a.name == name) {
            let prev = a.loc;
            if let Some(d) = self.reassignment_warning(name, prev, loc) {
                self.diags.push(d.with_seq(seq));
            }
            a.expr = expr;
            a.overwrite = Some(loc);
            return;
        }
        scope.assignments.push(Assignment { name, expr, loc, overwrite: None, annotations: Vec::new() });
    }

    fn reassignment_warning(&self, name: Name, prev: Loc, cur: Loc) -> Option<Diagnostic> {
        if self.file_ended {
            return None;
        }
        let main = self.main;
        let prev_path = self.sources.path(prev.span.file);
        let cur_path = self.sources.path(cur.span.file);
        let quoted = format!("\"{}\"", self.ast.name(name));
        let message = if prev_path == main && cur_path == main {
            format!("{quoted} was assigned on line {} but was overwritten", prev.line)
        } else if prev_path == cur_path || prev_path == main {
            // Same (included) file: a file included twice reassigns at the
            // same line, which is not worth a warning.
            if prev_path == cur_path && prev.line == cur.line {
                return None;
            }
            let main_dir = main.parent().unwrap_or(main);
            let rel = crate::diag::relative_path(prev_path, main_dir);
            format!(
                "{quoted} was assigned on line {} of {} but was overwritten",
                prev.line,
                cpp_quoted_path(&rel.to_string_lossy())
            )
        } else {
            return None;
        };
        Some(
            Diagnostic::new(DiagCode::Reassignment, Severity::Warning, message)
                .at(cur.span, cur.line)
                .with_base(PathBase::MainFileDir)
                .with_hint("remove one of the assignments; OpenSCAD keeps the last value at the first position"),
        )
    }

    fn params(&mut self, n: Node<'_>) -> Vec<Param> {
        n.children()
            .filter(|p| p.kind() == K::Param)
            .filter_map(|p| {
                let name = self.intern(p.token(K::Ident)?);
                let default = p.children().next().map(|e| self.expr(e));
                Some(Param { name, default, span: self.span(p) })
            })
            .collect()
    }

    fn args(&mut self, n: Option<Node<'_>>) -> Vec<Arg> {
        let Some(n) = n else { return Vec::new() };
        n.children()
            .filter(|a| a.kind() == K::Arg)
            .map(|a| {
                let name = a.token(K::Ident).map(|t| self.intern(t));
                let span = self.span(a);
                let expr = match a.children().next() {
                    Some(e) => self.expr(e),
                    None => self.invalid(span),
                };
                Arg { name, expr, span }
            })
            .collect()
    }

    fn instantiation(&mut self, n: Node<'_>) -> Option<Instantiation> {
        let span = self.span(n);
        match n.kind() {
            K::ModifierInst => {
                let modifier = n.tokens().next()?.kind();
                let mut inst = self.instantiation(n.children().next()?)?;
                match modifier {
                    K::Bang => inst.tag_root = true,
                    K::Hash => inst.tag_highlight = true,
                    K::Percent => inst.tag_background = true,
                    // `*` disables the instantiation entirely.
                    _ => return None,
                }
                Some(inst)
            }
            K::ModuleInst => {
                let name_tok = n.tokens().next()?;
                let name = self.intern(name_tok);
                let args = self.args(n.children().find(|c| c.kind() == K::ArgList));
                let mut children = Scope::default();
                if let Some(c) = n.children().find(|c| c.kind() != K::ArgList) {
                    self.child(c, &mut children);
                }
                Some(Instantiation {
                    name,
                    args,
                    children,
                    kind: InstKind::Module,
                    tag_root: false,
                    tag_highlight: false,
                    tag_background: false,
                    span,
                })
            }
            K::IfInst => {
                let mut kids = n.children();
                let cond_node = kids.next()?;
                let cond_span = self.span(cond_node);
                let cond = self.expr(cond_node);
                let mut children = Scope::default();
                let mut else_children = None;
                for c in kids {
                    if c.kind() == K::ElseClause {
                        let mut e = Scope::default();
                        if let Some(b) = c.children().next() {
                            self.child(b, &mut e);
                        }
                        else_children = Some(Box::new(e));
                    } else {
                        self.child(c, &mut children);
                    }
                }
                let name = self.ast.names.intern("if");
                Some(Instantiation {
                    name,
                    args: vec![Arg { name: None, expr: cond, span: cond_span }],
                    children,
                    kind: InstKind::If { else_children },
                    tag_root: false,
                    tag_highlight: false,
                    tag_background: false,
                    span,
                })
            }
            _ => None,
        }
    }

    /// A `child_statement`: `;`, `{ ... }` or an instantiation.
    fn child(&mut self, n: Node<'_>, scope: &mut Scope) {
        match n.kind() {
            K::ChildBlock => {
                for c in n.children() {
                    if c.kind() == K::Assignment {
                        self.assignment(c, scope);
                    } else {
                        self.child(c, scope);
                    }
                }
            }
            K::ModuleInst | K::ModifierInst | K::IfInst => {
                if let Some(i) = self.instantiation(n) {
                    scope.instantiations.push(i);
                }
            }
            _ => {}
        }
    }

    // --- expressions ----------------------------------------------------

    fn nth_expr(&mut self, kids: &Kids<'_>, i: usize, span: Span) -> ExprId {
        match kids.get(i) {
            Some(k) => self.expr(k),
            None => self.invalid(span),
        }
    }

    fn expr(&mut self, n: Node<'_>) -> ExprId {
        let span = self.span(n);
        let kids = &Kids::of(n);
        let kind = match n.kind() {
            K::Literal => {
                let Some(t) = n.tokens().next() else { return self.invalid(span) };
                match t.kind() {
                    K::KwTrue => ExprKind::Bool(true),
                    K::KwFalse => ExprKind::Bool(false),
                    K::KwUndef => ExprKind::Undef,
                    K::Number => ExprKind::Number(number_value(self.text(t))),
                    K::String => ExprKind::String(string_value(self.text(t)).into()),
                    _ => ExprKind::Invalid,
                }
            }
            K::NameRef => match n.tokens().next() {
                Some(t) => ExprKind::Var(self.intern(t)),
                None => ExprKind::Invalid,
            },
            K::ParenExpr | K::LcParen => return self.nth_expr(kids, 0, span),
            K::UnaryExpr => {
                let op = n.tokens().next().map(|t| t.kind());
                let e = self.nth_expr(kids, 0, span);
                match op {
                    Some(K::Plus) => return e,
                    Some(K::Minus) => {
                        // parser.y folds `-<number literal>` into the literal.
                        if let ExprKind::Number(v) = self.ast.expr(e).kind {
                            let x = &mut self.ast.exprs[e.0 as usize];
                            x.kind = ExprKind::Number(-v);
                            x.span = span;
                            return e;
                        }
                        ExprKind::Unary(UnaryOp::Negate, e)
                    }
                    Some(K::Bang) => ExprKind::Unary(UnaryOp::Not, e),
                    _ => ExprKind::Unary(UnaryOp::BinaryNot, e),
                }
            }
            K::BinaryExpr => {
                let op = n.tokens().find_map(|t| BinaryOp::from_token(t.kind()));
                let l = self.nth_expr(kids, 0, span);
                let r = self.nth_expr(kids, 1, span);
                match op {
                    Some(op) => ExprKind::Binary(op, l, r),
                    None => ExprKind::Invalid,
                }
            }
            K::TernaryExpr => {
                let c = self.nth_expr(kids, 0, span);
                let a = self.nth_expr(kids, 1, span);
                let b = self.nth_expr(kids, 2, span);
                ExprKind::Ternary(c, a, b)
            }
            K::CallExpr => {
                let callee = self.nth_expr(kids, 0, span);
                let args = self.args(kids.get(1));
                ExprKind::Call(callee, args)
            }
            K::IndexExpr => {
                let a = self.nth_expr(kids, 0, span);
                let i = self.nth_expr(kids, 1, span);
                ExprKind::Index(a, i)
            }
            K::MemberExpr => {
                let a = self.nth_expr(kids, 0, span);
                match n.token(K::Ident) {
                    Some(t) => ExprKind::Member(a, self.intern(t)),
                    None => ExprKind::Invalid,
                }
            }
            K::RangeExpr => {
                let begin = self.nth_expr(kids, 0, span);
                if kids.len >= 3 {
                    let step = self.nth_expr(kids, 1, span);
                    let end = self.nth_expr(kids, 2, span);
                    ExprKind::Range { begin, step: Some(step), end }
                } else {
                    let end = self.nth_expr(kids, 1, span);
                    ExprKind::Range { begin, step: None, end }
                }
            }
            K::VectorExpr => ExprKind::Vector(n.children().map(|k| self.expr(k)).collect()),
            K::FunctionExpr => {
                let params = kids.get(0).filter(|p| p.kind() == K::ParamList).map(|p| self.params(p)).unwrap_or_default();
                let body = self.nth_expr(kids, 1, span);
                ExprKind::Function(params, body)
            }
            K::LetExpr | K::AssertExpr | K::EchoExpr | K::LcLet | K::LcFor => {
                let args = self.args(kids.get(0).filter(|k| k.kind() == K::ArgList));
                let body = kids.get(1).map(|b| self.expr(b));
                match n.kind() {
                    K::AssertExpr => ExprKind::Assert(args, body),
                    K::EchoExpr => ExprKind::Echo(args, body),
                    k => {
                        let body = body.unwrap_or_else(|| self.invalid(span));
                        match k {
                            K::LetExpr => ExprKind::Let(args, body),
                            K::LcLet => ExprKind::LcLet(args, body),
                            _ => ExprKind::LcFor(args, body),
                        }
                    }
                }
            }
            K::LcForC => {
                let init = self.args(kids.get(0));
                let cond = self.nth_expr(kids, 1, span);
                let incr = self.args(kids.get(2));
                let body = self.nth_expr(kids, 3, span);
                ExprKind::LcForC { init, cond, incr, body }
            }
            K::LcEach => ExprKind::LcEach(self.nth_expr(kids, 0, span)),
            K::LcIf => {
                let c = self.nth_expr(kids, 0, span);
                let a = self.nth_expr(kids, 1, span);
                let b = kids.get(2).map(|k| self.expr(k));
                ExprKind::LcIf(c, a, b)
            }
            _ => ExprKind::Invalid,
        };
        self.ast.add(kind, span)
    }
}

/// The first four child nodes of an expression node, without allocating
/// (no expression kind but a vector needs more).
struct Kids<'a> {
    buf: [Option<Node<'a>>; 4],
    len: usize,
}

impl<'a> Kids<'a> {
    fn of(n: Node<'a>) -> Self {
        let mut k = Kids { buf: [None; 4], len: 0 };
        if n.kind() != K::VectorExpr {
            for c in n.children() {
                if k.len < 4 {
                    k.buf[k.len] = Some(c);
                }
                k.len += 1;
            }
        }
        k
    }

    fn get(&self, i: usize) -> Option<Node<'a>> {
        self.buf.get(i).copied().flatten()
    }
}

/// `operator<<(ostream&, const std::filesystem::path&)`: the path in double
/// quotes with `"` and `\` escaped.
fn cpp_quoted_path(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        if c == '"' || c == '\\' {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('"');
    out
}
