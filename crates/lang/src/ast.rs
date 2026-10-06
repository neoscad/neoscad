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
use crate::loader::{FileSystem, seq_for_token};
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
            self.0 = (self.0.rotate_left(5) ^ u64::from_le_bytes(b))
                .wrapping_mul(0x51_7c_c1_b7_27_22_0a_95);
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
    Range {
        begin: ExprId,
        step: Option<ExprId>,
        end: ExprId,
    },
    Vector(Vec<ExprId>),
    Function(Vec<Param>, ExprId),
    Let(Vec<Arg>, ExprId),
    Assert(Vec<Arg>, Option<ExprId>),
    Echo(Vec<Arg>, Option<ExprId>),
    LcIf(ExprId, ExprId, Option<ExprId>),
    LcEach(ExprId),
    LcFor(Vec<Arg>, ExprId),
    LcForC {
        init: Vec<Arg>,
        cond: ExprId,
        incr: Vec<Arg>,
        body: ExprId,
    },
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
        self.annotations
            .iter()
            .find(|a| a.name == name)
            .map(|a| a.expr)
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
    If {
        else_children: Option<Box<Scope>>,
    },
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
                self.is_literal(*begin)
                    && self.is_literal(*end)
                    && step.is_none_or(|s| self.is_literal(s))
            }
            _ => false,
        }
    }
}

/// Lower a parsed program. `main` is the path OpenSCAD treats as the main
/// file for reassignment warnings: the program's own file, or for a `use`d
/// library the program that uses it. `fs` resolves the paths those
/// warnings print ([`crate::diag::relative_path`]).
pub fn lower(
    cst: &Cst,
    sources: &SourceMap,
    main: &Path,
    uses: &[crate::loader::UseRef],
    fs: &dyn FileSystem,
) -> (Ast, Vec<Diagnostic>) {
    let (ast, diags, _) = lower_with(cst, sources, main, uses, &[], false, fs);
    (ast, diags)
}

/// An included file's statements lowered once, for every program that
/// includes it between top-level statements to take without lowering the
/// file again (`crate::fragment`).
///
/// Lowered on its own, a file numbers its expressions, names and files
/// from zero; [`Placed`] says where they go in a program. What depends on
/// the statements before the include is not resolved but recorded, in
/// order, as [`Event`]s: a top-level assignment may reassign a name the
/// including file assigned earlier (the value moves to the first
/// position, with a warning naming both places), and a `use` joins the
/// program's list of libraries at its turn.
#[derive(Debug, Default)]
pub(crate) struct FragmentAst {
    exprs: Vec<Expr>,
    /// The file's names in [`Name`] order.
    names: Vec<Box<str>>,
    functions: Vec<FunctionDef>,
    modules: Vec<ModuleDef>,
    instantiations: Vec<Instantiation>,
    events: Vec<Event>,
}

impl FragmentAst {
    /// Expressions held, for cost estimates.
    pub(crate) fn expr_count(&self) -> usize {
        self.exprs.len()
    }
}

/// A step of a [`FragmentAst`] that depends on the including program, in
/// lowering order.
#[derive(Debug)]
enum Event {
    /// A top-level assignment, before reassignment is resolved.
    Assign {
        name: Name,
        expr: ExprId,
        loc: Loc,
        seq: u64,
    },
    /// A warning from inside the file, kept in order with the others.
    Diag(Diagnostic),
    /// A `use` statement: it takes the program's next [`crate::loader::UseRef`].
    Use,
}

/// A [`FragmentAst`] in a program: the entry range its statements take in
/// the program's tree (the lowering skips them), and what its file ids and
/// token indices start from.
#[derive(Debug, Clone)]
pub(crate) struct Placed<'a> {
    pub entries: std::ops::Range<u32>,
    pub ast: &'a FragmentAst,
    pub file_base: u32,
    pub token_base: u32,
}

/// [`lower`], taking the statements in each of `frags` from their lowered
/// fragment instead of the tree. With `record`, top-level assignments,
/// warnings and `use`s are recorded rather than resolved and the result
/// includes the [`FragmentAst`]; the diagnostics are then in it.
pub(crate) fn lower_with(
    cst: &Cst,
    sources: &SourceMap,
    main: &Path,
    uses: &[crate::loader::UseRef],
    frags: &[Placed<'_>],
    record: bool,
    fs: &dyn FileSystem,
) -> (Ast, Vec<Diagnostic>, Option<FragmentAst>) {
    let mut l = Lower {
        sources,
        main,
        fs,
        ast: Ast::default(),
        diags: Vec::new(),
        file_ended: false,
        uses,
        next_use: 0,
        events: record.then(Vec::new),
        depth: 0,
    };
    let mut root = Scope::default();
    l.root(cst.root(), &mut root, frags);
    match l.events.take() {
        Some(events) => {
            let Scope {
                functions,
                modules,
                assignments: _,
                instantiations,
            } = root;
            let frag = FragmentAst {
                exprs: std::mem::take(&mut l.ast.exprs),
                names: std::mem::take(&mut l.ast.names.names),
                functions,
                modules,
                instantiations,
                events,
            };
            (l.ast, l.diags, Some(frag))
        }
        None => {
            l.ast.root = root;
            (l.ast, l.diags, None)
        }
    }
}

struct Lower<'a> {
    sources: &'a SourceMap,
    main: &'a Path,
    /// Resolves the other file a reassignment warning names.
    fs: &'a dyn FileSystem,
    ast: Ast,
    diags: Vec<Diagnostic>,
    /// Set once the `\x03` statement has been seen: assignments after it
    /// come from `-D` and overwrite silently.
    file_ended: bool,
    uses: &'a [crate::loader::UseRef],
    next_use: usize,
    /// Recording a [`FragmentAst`]: the steps that depend on the includer.
    events: Option<Vec<Event>>,
    /// Scopes entered below the root (module bodies, instantiations'
    /// children): only the root's assignments can meet the includer's.
    depth: u32,
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

    /// The root's statements, taking each fragment's from its lowering at
    /// the point where its statements begin. Its expressions then take
    /// the same arena positions and its names the same numbers as they
    /// would lowered in place: a fragment's statements are consecutive,
    /// so nothing else is lowered in between.
    fn root(&mut self, n: Node<'_>, scope: &mut Scope, frags: &[Placed<'_>]) {
        let mut next = 0;
        for c in n.children() {
            let at = c.index();
            while let Some(f) = frags.get(next).filter(|f| f.entries.start <= at) {
                self.merge(f, scope);
                next += 1;
            }
            if next > 0 && frags[next - 1].entries.contains(&at) {
                continue;
            }
            self.statement(c, scope);
        }
        for f in &frags[next..] {
            self.merge(f, scope);
        }
    }

    /// Take a lowered fragment into the root scope: its definitions and
    /// instantiations as they are (renumbered), and its assignments,
    /// warnings and `use`s replayed in order against what came before.
    fn merge(&mut self, p: &Placed<'_>, scope: &mut Scope) {
        let f = p.ast;
        let names: Vec<Name> = f.names.iter().map(|s| self.ast.names.intern(s)).collect();
        let rb = Rebase {
            expr: self.ast.exprs.len() as u32,
            names: &names,
            file: p.file_base,
            seq: u64::from(p.token_base) << 2,
        };
        self.ast.exprs.extend(f.exprs.iter().map(|e| rb.expr(e)));
        scope
            .functions
            .extend(f.functions.iter().map(|d| rb.function(d)));
        scope.modules.extend(f.modules.iter().map(|d| rb.module(d)));
        scope
            .instantiations
            .extend(f.instantiations.iter().map(|i| rb.inst(i)));
        for ev in &f.events {
            match ev {
                Event::Assign {
                    name,
                    expr,
                    loc,
                    seq,
                } => self.assign(
                    scope,
                    rb.name(*name),
                    rb.id(*expr),
                    rb.loc(*loc),
                    seq + rb.seq,
                ),
                Event::Diag(d) => {
                    let d = rb.diag(d);
                    self.emit(d);
                }
                Event::Use => self.use_stmt(),
            }
        }
    }

    fn emit(&mut self, d: Diagnostic) {
        match &mut self.events {
            Some(ev) => ev.push(Event::Diag(d)),
            None => self.diags.push(d),
        }
    }

    fn use_stmt(&mut self) {
        if let Some(ev) = &mut self.events {
            ev.push(Event::Use);
            return;
        }
        if let Some(u) = self.uses.get(self.next_use) {
            let path = u.path.clone();
            self.next_use += 1;
            self.ast.uses.retain(|p| *p != path);
            self.ast.uses.insert(0, path);
        }
    }

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
                let Some(name) = n.token(K::Ident) else {
                    return;
                };
                let name = self.intern(name);
                let params = n
                    .children()
                    .find(|c| c.kind() == K::ParamList)
                    .map(|p| self.params(p))
                    .unwrap_or_default();
                let mut body = Scope::default();
                if let Some(b) = n.children().filter(|c| c.kind() != K::ParamList).last() {
                    self.depth += 1;
                    self.statement(b, &mut body);
                    self.depth -= 1;
                }
                let span = self.span(n);
                scope.modules.push(ModuleDef {
                    name,
                    params,
                    body,
                    span,
                });
            }
            K::FunctionDef => {
                let Some(name) = n.token(K::Ident) else {
                    return;
                };
                let name = self.intern(name);
                let params = n
                    .children()
                    .find(|c| c.kind() == K::ParamList)
                    .map(|p| self.params(p))
                    .unwrap_or_default();
                let span = self.span(n);
                let body = match n.children().find(|c| c.kind() != K::ParamList) {
                    Some(b) => self.expr(b),
                    None => self.invalid(span),
                };
                scope.functions.push(FunctionDef {
                    name,
                    params,
                    body,
                    span,
                });
            }
            K::EotStmt => self.file_ended = true,
            K::UseStmt => self.use_stmt(),
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
        let seq = n
            .tokens()
            .last()
            .map_or(0, |t| seq_for_token(t.index()) + 1);
        self.assign(scope, name, expr, loc, seq);
    }

    /// Add an assignment to `scope`, or reassign the name in place.
    fn assign(&mut self, scope: &mut Scope, name: Name, expr: ExprId, loc: Loc, seq: u64) {
        if self.depth == 0
            && let Some(ev) = &mut self.events
        {
            ev.push(Event::Assign {
                name,
                expr,
                loc,
                seq,
            });
            return;
        }
        if let Some(a) = scope.assignments.iter_mut().find(|a| a.name == name) {
            let prev = a.loc;
            a.expr = expr;
            a.overwrite = Some(loc);
            if let Some(d) = self.reassignment_warning(name, prev, loc) {
                self.emit(d.with_seq(seq));
            }
            return;
        }
        scope.assignments.push(Assignment {
            name,
            expr,
            loc,
            overwrite: None,
            annotations: Vec::new(),
        });
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
            format!(
                "{quoted} was assigned on line {} but was overwritten",
                prev.line
            )
        } else if prev_path == cur_path || prev_path == main {
            // Same (included) file: a file included twice reassigns at the
            // same line, which is not worth a warning.
            if prev_path == cur_path && prev.line == cur.line {
                return None;
            }
            let main_dir = main.parent().unwrap_or(main);
            let rel = crate::diag::relative_path(prev_path, main_dir, self.fs);
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
                Some(Param {
                    name,
                    default,
                    span: self.span(p),
                })
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
        self.depth += 1;
        let i = self.instantiation_in(n);
        self.depth -= 1;
        i
    }

    fn instantiation_in(&mut self, n: Node<'_>) -> Option<Instantiation> {
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
                    args: vec![Arg {
                        name: None,
                        expr: cond,
                        span: cond_span,
                    }],
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
                let Some(t) = n.tokens().next() else {
                    return self.invalid(span);
                };
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
                    ExprKind::Range {
                        begin,
                        step: Some(step),
                        end,
                    }
                } else {
                    let end = self.nth_expr(kids, 1, span);
                    ExprKind::Range {
                        begin,
                        step: None,
                        end,
                    }
                }
            }
            K::VectorExpr => ExprKind::Vector(n.children().map(|k| self.expr(k)).collect()),
            K::FunctionExpr => {
                let params = kids
                    .get(0)
                    .filter(|p| p.kind() == K::ParamList)
                    .map(|p| self.params(p))
                    .unwrap_or_default();
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
                ExprKind::LcForC {
                    init,
                    cond,
                    incr,
                    body,
                }
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

/// Renumbers a [`FragmentAst`]'s pieces into a program: expressions by an
/// offset, names through the program's interner, files by an offset and
/// diagnostic order keys by the fragment's first token.
struct Rebase<'a> {
    expr: u32,
    names: &'a [Name],
    file: u32,
    seq: u64,
}

impl Rebase<'_> {
    fn id(&self, e: ExprId) -> ExprId {
        ExprId(e.0 + self.expr)
    }

    fn opt(&self, e: Option<ExprId>) -> Option<ExprId> {
        e.map(|e| self.id(e))
    }

    fn name(&self, n: Name) -> Name {
        self.names[n.0 as usize]
    }

    /// A node without tokens gets `Span::default()`, which is in the
    /// program's main file wherever it was lowered; so it stays put.
    fn span(&self, s: Span) -> Span {
        if s == Span::default() {
            return s;
        }
        Span {
            file: crate::source::FileId(s.file.0 + self.file),
            ..s
        }
    }

    fn loc(&self, l: Loc) -> Loc {
        Loc {
            span: self.span(l.span),
            line: l.line,
        }
    }

    fn args(&self, args: &[Arg]) -> Vec<Arg> {
        args.iter()
            .map(|a| Arg {
                name: a.name.map(|n| self.name(n)),
                expr: self.id(a.expr),
                span: self.span(a.span),
            })
            .collect()
    }

    fn params(&self, params: &[Param]) -> Vec<Param> {
        params
            .iter()
            .map(|p| Param {
                name: self.name(p.name),
                default: self.opt(p.default),
                span: self.span(p.span),
            })
            .collect()
    }

    fn expr(&self, e: &Expr) -> Expr {
        use ExprKind as E;
        let kind = match &e.kind {
            E::Undef => E::Undef,
            E::Bool(b) => E::Bool(*b),
            E::Number(v) => E::Number(*v),
            E::String(s) => E::String(s.clone()),
            E::Var(n) => E::Var(self.name(*n)),
            E::Unary(op, a) => E::Unary(*op, self.id(*a)),
            E::Binary(op, a, b) => E::Binary(*op, self.id(*a), self.id(*b)),
            E::Ternary(c, a, b) => E::Ternary(self.id(*c), self.id(*a), self.id(*b)),
            E::Index(a, i) => E::Index(self.id(*a), self.id(*i)),
            E::Member(a, n) => E::Member(self.id(*a), self.name(*n)),
            E::Call(f, args) => E::Call(self.id(*f), self.args(args)),
            E::Range { begin, step, end } => E::Range {
                begin: self.id(*begin),
                step: self.opt(*step),
                end: self.id(*end),
            },
            E::Vector(v) => E::Vector(v.iter().map(|&x| self.id(x)).collect()),
            E::Function(p, b) => E::Function(self.params(p), self.id(*b)),
            E::Let(a, b) => E::Let(self.args(a), self.id(*b)),
            E::Assert(a, b) => E::Assert(self.args(a), self.opt(*b)),
            E::Echo(a, b) => E::Echo(self.args(a), self.opt(*b)),
            E::LcIf(c, a, b) => E::LcIf(self.id(*c), self.id(*a), self.opt(*b)),
            E::LcEach(a) => E::LcEach(self.id(*a)),
            E::LcFor(a, b) => E::LcFor(self.args(a), self.id(*b)),
            E::LcForC {
                init,
                cond,
                incr,
                body,
            } => E::LcForC {
                init: self.args(init),
                cond: self.id(*cond),
                incr: self.args(incr),
                body: self.id(*body),
            },
            E::LcLet(a, b) => E::LcLet(self.args(a), self.id(*b)),
            E::Invalid => E::Invalid,
        };
        Expr {
            kind,
            span: self.span(e.span),
        }
    }

    fn scope(&self, s: &Scope) -> Scope {
        let Scope {
            functions,
            modules,
            assignments,
            instantiations,
        } = s;
        Scope {
            functions: functions.iter().map(|d| self.function(d)).collect(),
            modules: modules.iter().map(|d| self.module(d)).collect(),
            assignments: assignments
                .iter()
                .map(|a| {
                    let Assignment {
                        name,
                        expr,
                        loc,
                        overwrite,
                        annotations,
                    } = a;
                    Assignment {
                        name: self.name(*name),
                        expr: self.id(*expr),
                        loc: self.loc(*loc),
                        overwrite: overwrite.map(|l| self.loc(l)),
                        annotations: annotations
                            .iter()
                            .map(|x| Annotation {
                                name: x.name,
                                expr: self.id(x.expr),
                            })
                            .collect(),
                    }
                })
                .collect(),
            instantiations: instantiations.iter().map(|i| self.inst(i)).collect(),
        }
    }

    fn function(&self, d: &FunctionDef) -> FunctionDef {
        let FunctionDef {
            name,
            params,
            body,
            span,
        } = d;
        FunctionDef {
            name: self.name(*name),
            params: self.params(params),
            body: self.id(*body),
            span: self.span(*span),
        }
    }

    fn module(&self, d: &ModuleDef) -> ModuleDef {
        let ModuleDef {
            name,
            params,
            body,
            span,
        } = d;
        ModuleDef {
            name: self.name(*name),
            params: self.params(params),
            body: self.scope(body),
            span: self.span(*span),
        }
    }

    fn inst(&self, i: &Instantiation) -> Instantiation {
        let Instantiation {
            name,
            args,
            children,
            kind,
            tag_root,
            tag_highlight,
            tag_background,
            span,
        } = i;
        Instantiation {
            name: self.name(*name),
            args: self.args(args),
            children: self.scope(children),
            kind: match kind {
                InstKind::Module => InstKind::Module,
                InstKind::If { else_children } => InstKind::If {
                    else_children: else_children.as_ref().map(|s| Box::new(self.scope(s))),
                },
            },
            tag_root: *tag_root,
            tag_highlight: *tag_highlight,
            tag_background: *tag_background,
            span: self.span(*span),
        }
    }

    fn diag(&self, d: &Diagnostic) -> Diagnostic {
        rebase_diag(d, self.file, self.seq)
    }
}

/// A diagnostic from an included file's own parse, in a program where the
/// file's ids start at `file` and its order keys at `seq`. Every span
/// here is real (a diagnostic's location is never a default span), so all
/// move.
pub(crate) fn rebase_diag(d: &Diagnostic, file: u32, seq: u64) -> Diagnostic {
    let mv = |s: Span| Span {
        file: crate::source::FileId(s.file.0 + file),
        ..s
    };
    let mut d = d.clone();
    d.span = d.span.map(mv);
    for h in &mut d.hints {
        if let Some((s, _)) = &mut h.replacement {
            *s = mv(*s);
        }
    }
    d.seq += seq;
    d
}

/// The first four child nodes of an expression node, without allocating
/// (no expression kind but a vector needs more).
struct Kids<'a> {
    buf: [Option<Node<'a>>; 4],
    len: usize,
}

impl<'a> Kids<'a> {
    fn of(n: Node<'a>) -> Self {
        let mut k = Kids {
            buf: [None; 4],
            len: 0,
        };
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
