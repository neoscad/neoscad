//! One file's symbols, from its lossless syntax tree: definitions, the
//! lexical scopes they live in, the names that refer to them, and the
//! `include`/`use` directives. The tree is the file alone (`include`s
//! are not spliced in), so each file is indexed once however many
//! programs include it, and [`crate::world`] joins the files.
//!
//! Scopes follow OpenSCAD's (`parser.y`): the file, a module's body, the
//! children of an instantiation (and of `if`/`else`), a function's
//! parameters, and in expressions `let`, list-comprehension `for` and
//! function literals. A bare `{ }` block adds its statements to the
//! scope it is in (`'{' inner_input '}'` pushes no scope). Assignments
//! are visible throughout their scope (OpenSCAD hoists them), while a
//! `let` or `for` binding is visible only after itself.
//!
//! The walk accepts broken code: statements the parser could not finish
//! sit in error nodes, and their names are still collected, so a file
//! being typed keeps its outline, completion and navigation.

use lang::Program;
use lang::syntax::lexer::directive_path;
use lang::syntax::{Node, SyntaxKind as K, TokenRef};

pub type DefId = usize;
pub type ScopeId = usize;

/// OpenSCAD's three namespaces: a module, a function and a variable may
/// share a name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Ns {
    Module,
    Function,
    Variable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DefKind {
    Module,
    Function,
    /// An assignment.
    Variable,
    /// A module's, function's or function literal's parameter.
    Parameter,
    /// A `let`, `for` or `intersection_for` binding.
    Binding,
}

impl DefKind {
    pub fn ns(self) -> Ns {
        match self {
            DefKind::Module => Ns::Module,
            DefKind::Function => Ns::Function,
            _ => Ns::Variable,
        }
    }
}

/// A parameter of a module or function.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Param {
    pub name: String,
    /// The default as written.
    pub default: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Def {
    pub name: String,
    pub kind: DefKind,
    /// Byte range of the name.
    pub name_span: (u32, u32),
    /// Byte range of the whole definition.
    pub span: (u32, u32),
    /// The scope the name is visible in.
    pub scope: ScopeId,
    /// Where it becomes visible (bindings only after themselves).
    pub visible_from: u32,
    /// A module's or function's parameters.
    pub params: Vec<Param>,
    /// A module's or function's own scope (its parameters and body).
    pub inner: Option<ScopeId>,
    /// A variable's expression, a parameter's default.
    pub value: Option<(u32, u32)>,
    /// A parameter's module or function.
    pub owner: Option<DefId>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScopeKind {
    File,
    Module,
    Function,
    Children,
    Expr,
}

#[derive(Debug, Clone)]
pub struct Scope {
    pub parent: Option<ScopeId>,
    pub start: u32,
    pub end: u32,
    pub kind: ScopeKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefKind {
    Module,
    Function,
    Variable,
    /// `name=` in a call: the callee's parameter.
    NamedArg,
}

/// A use of a name.
#[derive(Debug, Clone)]
pub struct Ref {
    pub name: String,
    pub span: (u32, u32),
    pub kind: RefKind,
    pub scope: ScopeId,
    /// For a named argument: the ref of the module or function called.
    pub callee: Option<usize>,
}

/// An `include <...>` or `use <...>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Directive {
    pub include: bool,
    pub span: (u32, u32),
    /// The path as written (an include's directory part joined on).
    pub path: String,
}

/// A foldable region (byte range, the first and last line of which fold).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fold {
    pub start: u32,
    pub end: u32,
    pub comment: bool,
}

#[derive(Debug, Clone, Default)]
pub struct FileIndex {
    pub defs: Vec<Def>,
    pub scopes: Vec<Scope>,
    pub refs: Vec<Ref>,
    pub directives: Vec<Directive>,
    pub folds: Vec<Fold>,
}

impl FileIndex {
    /// The innermost scope around `offset` (scope ends are inclusive, so
    /// a name being typed at the end of a block is still in it).
    pub fn scope_at(&self, offset: u32) -> ScopeId {
        // Scopes are created in preorder, so the last one containing the
        // offset is the innermost.
        self.scopes
            .iter()
            .rposition(|s| s.start <= offset && offset <= s.end)
            .unwrap_or(0)
    }

    /// The definitions made directly in `scope`.
    pub fn defs_in(&self, scope: ScopeId) -> impl Iterator<Item = (DefId, &Def)> {
        self.defs
            .iter()
            .enumerate()
            .filter(move |(_, d)| d.scope == scope)
    }

    /// The definition whose name covers `offset`.
    pub fn def_at(&self, offset: u32) -> Option<DefId> {
        self.defs
            .iter()
            .position(|d| d.name_span.0 <= offset && offset <= d.name_span.1)
    }

    /// The reference covering `offset`.
    pub fn ref_at(&self, offset: u32) -> Option<usize> {
        self.refs
            .iter()
            .position(|r| r.span.0 <= offset && offset <= r.span.1)
    }

    /// Whether `scope` is `inner` or encloses it.
    pub fn encloses(&self, scope: ScopeId, inner: ScopeId) -> bool {
        let mut s = Some(inner);
        while let Some(i) = s {
            if i == scope {
                return true;
            }
            s = self.scopes[i].parent;
        }
        false
    }
}

/// Index a file parsed alone ([`lang::parse_file`]).
pub fn build(p: &Program) -> FileIndex {
    let text = &p.sources.get(p.main).text;
    let mut b = Builder {
        p,
        out: FileIndex::default(),
    };
    b.out.scopes.push(Scope {
        parent: None,
        start: 0,
        end: text.len() as u32,
        kind: ScopeKind::File,
    });
    b.stmt(p.cst.root(), 0);
    b.directives_and_comments();
    b.out
}

struct Builder<'a> {
    p: &'a Program,
    out: FileIndex,
}

fn range(n: Node<'_>) -> Option<(u32, u32)> {
    n.span().map(|s| (s.start, s.end))
}

fn tok_range(t: TokenRef<'_>) -> (u32, u32) {
    let s = t.span();
    (s.start, s.end)
}

/// The node right after the direct child token `kind` (a module's body
/// after its `)`).
fn node_after(n: Node<'_>, kind: K) -> Option<Node<'_>> {
    let mut seen = false;
    for c in n.children_with_tokens() {
        match c {
            lang::syntax::Element::Token(t) if t.kind() == kind => seen = true,
            lang::syntax::Element::Node(c) if seen => return Some(c),
            _ => {}
        }
    }
    None
}

impl Builder<'_> {
    fn text(&self, t: TokenRef<'_>) -> String {
        String::from_utf8_lossy(t.text(&self.p.sources)).into_owned()
    }

    fn node_text(&self, n: Node<'_>) -> String {
        match range(n) {
            Some((a, b)) => {
                String::from_utf8_lossy(self.p.sources.get(self.p.main).slice(a, b)).into_owned()
            }
            None => String::new(),
        }
    }

    fn scope(&mut self, parent: ScopeId, start: u32, end: u32, kind: ScopeKind) -> ScopeId {
        self.out.scopes.push(Scope {
            parent: Some(parent),
            start,
            end,
            kind,
        });
        self.out.scopes.len() - 1
    }

    fn def(&mut self, d: Def) -> DefId {
        self.out.defs.push(d);
        self.out.defs.len() - 1
    }

    fn reference(&mut self, t: TokenRef<'_>, kind: RefKind, scope: ScopeId) -> usize {
        self.out.refs.push(Ref {
            name: self.text(t),
            span: tok_range(t),
            kind,
            scope,
            callee: None,
        });
        self.out.refs.len() - 1
    }

    /// A multi-line node folds.
    fn fold(&mut self, n: Node<'_>) {
        if let Some((a, b)) = range(n) {
            let f = self.p.sources.get(self.p.main);
            if f.line_of(a) < f.line_of(b) {
                self.out.folds.push(Fold {
                    start: a,
                    end: b,
                    comment: false,
                });
            }
        }
    }

    fn stmt(&mut self, n: Node<'_>, scope: ScopeId) {
        match n.kind() {
            K::SourceFile | K::ErrorNode => {
                for c in n.children() {
                    self.stmt(c, scope);
                }
            }
            K::BlockStmt | K::ChildBlock => {
                self.fold(n);
                for c in n.children() {
                    self.stmt(c, scope);
                }
            }
            K::Assignment => self.assignment(n, scope),
            K::ModuleDef => self.module_def(n, scope),
            K::FunctionDef => self.function_def(n, scope),
            K::ModuleInst => self.instantiation(n, scope),
            K::ModifierInst => {
                for c in n.children() {
                    self.stmt(c, scope);
                }
            }
            K::IfInst | K::ElseClause => {
                for c in n.children() {
                    match c.kind() {
                        K::ElseClause => self.stmt(c, scope),
                        k if is_statement(k) => self.child(c, scope),
                        _ => self.expr(c, scope),
                    }
                }
            }
            K::EmptyStmt | K::UseStmt | K::EotStmt => {}
            // Expressions inside an error node.
            _ => self.expr(n, scope),
        }
    }

    /// A child statement: a scope of its own.
    fn child(&mut self, n: Node<'_>, scope: ScopeId) {
        let (a, b) = range(n).unwrap_or((0, 0));
        let s = self.scope(scope, a, b, ScopeKind::Children);
        self.stmt(n, s);
    }

    fn assignment(&mut self, n: Node<'_>, scope: ScopeId) {
        let Some(name) = n.token(K::Ident) else {
            return;
        };
        let value = n.children().next();
        let start = self.out.scopes[scope].start;
        self.def(Def {
            name: self.text(name),
            kind: DefKind::Variable,
            name_span: tok_range(name),
            span: range(n).unwrap_or_else(|| tok_range(name)),
            scope,
            visible_from: start,
            params: Vec::new(),
            inner: None,
            value: value.and_then(range),
            owner: None,
        });
        if let Some(v) = value {
            self.expr(v, scope);
        }
    }

    /// A parameter list in `inner`, owned by `owner`.
    fn params(
        &mut self,
        list: Option<Node<'_>>,
        inner: ScopeId,
        owner: Option<DefId>,
    ) -> Vec<Param> {
        let mut out = Vec::new();
        let Some(list) = list else {
            return out;
        };
        let from = self.out.scopes[inner].start;
        for p in list.children().filter(|c| c.kind() == K::Param) {
            let Some(name) = p.token(K::Ident) else {
                continue;
            };
            let default = p.children().next();
            out.push(Param {
                name: self.text(name),
                default: default.map(|d| self.node_text(d)),
            });
            self.def(Def {
                name: self.text(name),
                kind: DefKind::Parameter,
                name_span: tok_range(name),
                span: range(p).unwrap_or_else(|| tok_range(name)),
                scope: inner,
                visible_from: from,
                params: Vec::new(),
                inner: None,
                value: default.and_then(range),
                owner,
            });
            if let Some(d) = default {
                self.expr(d, inner);
            }
        }
        out
    }

    /// A module or function definition: the name in `scope`, the
    /// parameters and body in a scope of their own.
    fn callable(&mut self, n: Node<'_>, scope: ScopeId, kind: DefKind) -> Option<(DefId, ScopeId)> {
        let name = n.token(K::Ident)?;
        let (_, end) = range(n)?;
        let start = n
            .token(K::LParen)
            .map_or(tok_range(name).1, |t| t.span().start);
        let sk = if kind == DefKind::Module {
            ScopeKind::Module
        } else {
            ScopeKind::Function
        };
        let inner = self.scope(scope, start, end, sk);
        let id = self.def(Def {
            name: self.text(name),
            kind,
            name_span: tok_range(name),
            span: range(n).unwrap_or_else(|| tok_range(name)),
            scope,
            visible_from: self.out.scopes[scope].start,
            params: Vec::new(),
            inner: Some(inner),
            value: None,
            owner: None,
        });
        let list = n.children().find(|c| c.kind() == K::ParamList);
        let params = self.params(list, inner, Some(id));
        self.out.defs[id].params = params;
        Some((id, inner))
    }

    fn module_def(&mut self, n: Node<'_>, scope: ScopeId) {
        self.fold(n);
        let Some((_, inner)) = self.callable(n, scope, DefKind::Module) else {
            return;
        };
        if let Some(body) = node_after(n, K::RParen) {
            // A body block is the module's scope itself.
            self.stmt(body, inner);
        }
    }

    fn function_def(&mut self, n: Node<'_>, scope: ScopeId) {
        self.fold(n);
        let Some((_, inner)) = self.callable(n, scope, DefKind::Function) else {
            return;
        };
        if let Some(body) = node_after(n, K::Eq) {
            self.expr(body, inner);
        }
    }

    fn instantiation(&mut self, n: Node<'_>, scope: ScopeId) {
        let Some(name) = n.tokens().next() else {
            return;
        };
        let callee = self.reference(name, RefKind::Module, scope);
        // `for`, `let` and `intersection_for` bind their named arguments
        // in their children; other modules take them as parameters.
        let binds = matches!(
            self.out.refs[callee].name.as_str(),
            "for" | "let" | "intersection_for" | "assign"
        );
        let child = node_after(n, K::RParen);
        let child_scope = child.map(|c| {
            let (a, b) = range(c).unwrap_or((0, 0));
            self.scope(scope, a, b, ScopeKind::Children)
        });
        if let Some(args) = n.children().find(|c| c.kind() == K::ArgList) {
            self.fold(args);
            for arg in args.children().filter(|c| c.kind() == K::Arg) {
                let value = arg.children().next();
                if let (Some(nm), Some(_)) = (arg.token(K::Ident), arg.token(K::Eq)) {
                    match (binds, child_scope) {
                        (true, Some(cs)) => {
                            let from = self.out.scopes[cs].start;
                            self.def(Def {
                                name: self.text(nm),
                                kind: DefKind::Binding,
                                name_span: tok_range(nm),
                                span: range(arg).unwrap_or_else(|| tok_range(nm)),
                                scope: cs,
                                visible_from: from,
                                params: Vec::new(),
                                inner: None,
                                value: value.and_then(range),
                                owner: None,
                            });
                        }
                        _ => {
                            let r = self.reference(nm, RefKind::NamedArg, scope);
                            self.out.refs[r].callee = Some(callee);
                        }
                    }
                }
                if let Some(v) = value {
                    self.expr(v, scope);
                }
            }
        }
        if let (Some(c), Some(cs)) = (child, child_scope) {
            self.stmt(c, cs);
        }
    }

    /// `let (a = 1, b = a) body`, a comprehension `let` or `for`: each
    /// binding is visible after itself, in a scope over the whole node.
    fn bindings(&mut self, n: Node<'_>, scope: ScopeId) {
        let (a, b) = range(n).unwrap_or((0, 0));
        let s = self.scope(scope, a, b, ScopeKind::Expr);
        let mut lists = 0;
        for c in n.children() {
            if c.kind() != K::ArgList {
                self.expr(c, s);
                continue;
            }
            lists += 1;
            self.fold(c);
            for arg in c.children().filter(|x| x.kind() == K::Arg) {
                let value = arg.children().next();
                if let Some(v) = value {
                    self.expr(v, s);
                }
                let (Some(nm), Some(_)) = (arg.token(K::Ident), arg.token(K::Eq)) else {
                    continue;
                };
                // A C-style `for`'s update list assigns the bindings of its
                // first list; it defines nothing.
                if n.kind() == K::LcForC && lists > 1 {
                    self.reference(nm, RefKind::Variable, s);
                    continue;
                }
                let end = range(arg).map_or(b, |r| r.1);
                self.def(Def {
                    name: self.text(nm),
                    kind: DefKind::Binding,
                    name_span: tok_range(nm),
                    span: range(arg).unwrap_or_else(|| tok_range(nm)),
                    scope: s,
                    visible_from: end,
                    params: Vec::new(),
                    inner: None,
                    value: value.and_then(range),
                    owner: None,
                });
            }
        }
    }

    fn expr(&mut self, n: Node<'_>, scope: ScopeId) {
        match n.kind() {
            K::NameRef => {
                if let Some(t) = n.token(K::Ident) {
                    self.reference(t, RefKind::Variable, scope);
                }
            }
            K::CallExpr => {
                let mut kids = n.children();
                let callee = kids.next();
                let callee_ref = match callee {
                    Some(c) if c.kind() == K::NameRef => c
                        .token(K::Ident)
                        .map(|t| self.reference(t, RefKind::Function, scope)),
                    Some(c) => {
                        self.expr(c, scope);
                        None
                    }
                    None => None,
                };
                for c in kids {
                    if c.kind() != K::ArgList {
                        self.expr(c, scope);
                        continue;
                    }
                    self.fold(c);
                    for arg in c.children().filter(|x| x.kind() == K::Arg) {
                        if let (Some(nm), Some(_)) = (arg.token(K::Ident), arg.token(K::Eq)) {
                            let r = self.reference(nm, RefKind::NamedArg, scope);
                            self.out.refs[r].callee = callee_ref;
                        }
                        for v in arg.children() {
                            self.expr(v, scope);
                        }
                    }
                }
            }
            K::FunctionExpr => {
                let (_, end) = range(n).unwrap_or((0, 0));
                let start = n.token(K::LParen).map_or(0, |t| t.span().start);
                let inner = self.scope(scope, start, end, ScopeKind::Function);
                let list = n.children().find(|c| c.kind() == K::ParamList);
                self.params(list, inner, None);
                if let Some(body) = node_after(n, K::RParen) {
                    self.expr(body, inner);
                }
            }
            K::LetExpr | K::LcLet | K::LcFor | K::LcForC => self.bindings(n, scope),
            K::VectorExpr => {
                self.fold(n);
                for c in n.children() {
                    self.expr(c, scope);
                }
            }
            K::ArgList => {
                // `echo(...)`, `assert(...)`: their names label values.
                for arg in n.children() {
                    for v in arg.children() {
                        self.expr(v, scope);
                    }
                }
            }
            k if is_statement(k) => self.stmt(n, scope),
            _ => {
                for c in n.children() {
                    self.expr(c, scope);
                }
            }
        }
    }

    /// Directives (trivia to the parser) and comment folds.
    fn directives_and_comments(&mut self) {
        let f = self.p.sources.get(self.p.main);
        let toks = self.p.cst.tokens();
        let mut run: Option<(u32, u32, u32)> = None; // start, end, last line
        let flush = |run: &mut Option<(u32, u32, u32)>, out: &mut Vec<Fold>| {
            if let Some((a, b, _)) = run.take()
                && f.line_of(a) < f.line_of(b)
            {
                out.push(Fold {
                    start: a,
                    end: b,
                    comment: true,
                });
            }
        };
        for t in toks {
            match t.kind {
                K::IncludeDirective | K::UseDirective => {
                    let include = t.kind == K::IncludeDirective;
                    let parts = directive_path(f.slice(t.start, t.end()), include);
                    let path = match (parts.dir, parts.name) {
                        (Some(d), Some(n)) => format!("{d}{n}"),
                        (None, Some(n)) => n,
                        (Some(d), None) => d,
                        (None, None) => String::new(),
                    };
                    self.out.directives.push(Directive {
                        include,
                        span: (t.start, t.end()),
                        path,
                    });
                }
                K::LineComment => {
                    let line = f.line_of(t.start);
                    match &mut run {
                        Some((_, end, last)) if *last + 1 == line => {
                            *end = t.end();
                            *last = line;
                        }
                        _ => {
                            flush(&mut run, &mut self.out.folds);
                            run = Some((t.start, t.end(), line));
                        }
                    }
                }
                K::BlockComment => {
                    flush(&mut run, &mut self.out.folds);
                    run = Some((t.start, t.end(), f.line_of(t.end())));
                    flush(&mut run, &mut self.out.folds);
                }
                K::Whitespace => {}
                _ => flush(&mut run, &mut self.out.folds),
            }
        }
        flush(&mut run, &mut self.out.folds);
    }
}

fn is_statement(k: K) -> bool {
    matches!(
        k,
        K::Assignment
            | K::ModuleDef
            | K::FunctionDef
            | K::ModuleInst
            | K::ModifierInst
            | K::IfInst
            | K::BlockStmt
            | K::ChildBlock
            | K::EmptyStmt
            | K::UseStmt
            | K::EotStmt
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn index(src: &str) -> FileIndex {
        build(&lang::parse_file("/t.scad".into(), src.as_bytes().to_vec()))
    }

    fn names(ix: &FileIndex, kind: DefKind) -> Vec<&str> {
        ix.defs
            .iter()
            .filter(|d| d.kind == kind)
            .map(|d| d.name.as_str())
            .collect()
    }

    #[test]
    fn definitions_and_scopes() {
        let src = "w = 2;\nmodule box(s = 1, c) { h = s; cube(h); }\nfunction f(x) = let(y = x) y + w;\nfor (i = [0:3]) translate([i, 0, 0]) box(s = i);\n";
        let ix = index(src);
        assert_eq!(names(&ix, DefKind::Variable), ["w", "h"]);
        assert_eq!(names(&ix, DefKind::Module), ["box"]);
        assert_eq!(names(&ix, DefKind::Parameter), ["s", "c", "x"]);
        assert_eq!(names(&ix, DefKind::Binding), ["y", "i"]);
        let bx = &ix.defs[ix.defs.iter().position(|d| d.name == "box").unwrap()];
        assert_eq!(bx.params[0].default.as_deref(), Some("1"));
        // `h` lives in the module's scope, `w` in the file's.
        let h = ix.defs.iter().find(|d| d.name == "h").unwrap();
        assert_eq!(Some(h.scope), bx.inner);
        // `i` is visible in the loop's children only.
        let i = ix.defs.iter().find(|d| d.name == "i").unwrap();
        let use_i = src.rfind("s = i").unwrap() as u32 + 4;
        assert!(ix.encloses(i.scope, ix.scope_at(use_i)));
        assert!(!ix.encloses(i.scope, ix.scope_at(3)));
        // `s = i` in the call is a named argument, not a binding.
        let named: Vec<&str> = ix
            .refs
            .iter()
            .filter(|r| r.kind == RefKind::NamedArg)
            .map(|r| r.name.as_str())
            .collect();
        assert_eq!(named, ["s"]);
        let calls: Vec<&str> = ix
            .refs
            .iter()
            .filter(|r| r.kind == RefKind::Module)
            .map(|r| r.name.as_str())
            .collect();
        assert_eq!(calls, ["cube", "for", "translate", "box"]);
    }

    #[test]
    fn let_bindings_see_only_earlier_ones() {
        let src = "a = 1;\nb = let(a = a + 1, c = a) c;\n";
        let ix = index(src);
        let inner_a = ix
            .defs
            .iter()
            .find(|d| d.name == "a" && d.kind == DefKind::Binding)
            .unwrap();
        // The `a` in `a + 1` comes before the binding is visible.
        let first_use = src.find("a + 1").unwrap() as u32;
        assert!(first_use < inner_a.visible_from);
        let second_use = src.find("c = a").unwrap() as u32 + 4;
        assert!(second_use >= inner_a.visible_from);
    }

    #[test]
    fn directives_and_folds() {
        let src = "include <BOSL2/std.scad>\nuse <lib.scad>\n// one\n// two\nmodule m() {\n  cube(1);\n}\n";
        let ix = index(src);
        assert_eq!(
            ix.directives
                .iter()
                .map(|d| (d.include, d.path.as_str()))
                .collect::<Vec<_>>(),
            [(true, "BOSL2/std.scad"), (false, "lib.scad")]
        );
        assert!(ix.folds.iter().any(|f| f.comment));
        assert!(ix.folds.iter().any(|f| !f.comment));
    }

    #[test]
    fn broken_code_keeps_its_names() {
        let ix = index("module m(a) {\n  cu\n}\nx = 1;\nfunction g( = ;\n");
        assert!(names(&ix, DefKind::Module).contains(&"m"));
        assert!(names(&ix, DefKind::Variable).contains(&"x"));
    }
}
