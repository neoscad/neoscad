//! NeoSCAD's `fillet_edges()` and `chamfer_edges()` (`--enable fillet`,
//! `docs/fillets.md`): the evaluator's side.
//!
//! A call checks its arguments, parses its selectors ([`selector`]) and
//! makes a [`NodeKind::Fillet`] node over its children; once they are
//! instantiated, `child(i)` indices are checked against them and `@name`
//! anchors resolved onto the node. What the node holds is everything the
//! result will depend on, so the `.csg` label and the cache key (one
//! writer, `crate::dump`) cover it. Selection and the blends are
//! geometry's (`geom::fillet`), and so are the messages about them, which
//! need the child's shape.
//!
//! A call whose arguments are wrong is an error (`docs/fillets.md`,
//! section 18, decision 2): it reports at the argument, with the column
//! inside a selector string, and becomes a plain group, so its children
//! still render, sharp.

pub mod selector;

use lang::diag::{DiagCode, Hint, Severity};
use lang::source::Span;

use crate::builtins::modules::{BuiltinModule, Params};
use crate::context::ScopeRef;
use crate::eval::Evaluator;
use crate::message::Loc;
use crate::node::{Anchor, Discretizer, Node, NodeKind};
use crate::value::Value;
pub use selector::{Item, Selector};

/// Fillet or chamfer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FilletKind {
    Fillet,
    Chamfer,
}

impl FilletKind {
    /// The module's name.
    pub fn module(self) -> &'static str {
        match self {
            FilletKind::Fillet => "fillet_edges",
            FilletKind::Chamfer => "chamfer_edges",
        }
    }

    /// The size parameter's name: the radius, or the chamfer's distance
    /// along each face.
    pub fn size_name(self) -> &'static str {
        match self {
            FilletKind::Fillet => "r",
            FilletKind::Chamfer => "d",
        }
    }
}

/// A `fillet_edges()` or `chamfer_edges()` call that checked out.
#[derive(Debug, Clone, PartialEq)]
pub struct FilletNode {
    pub kind: FilletKind,
    /// `r` or `d`: positive and finite.
    pub size: f64,
    pub edges: Selector,
    pub except: Option<Selector>,
    pub expect: Option<u32>,
    /// The blend arcs' `$fn`, `$fa`, `$fs`.
    pub disc: Discretizer,
    /// The anchors the selectors name (`@name`), resolved against the
    /// children's anchors when the call was instantiated, in the call's
    /// frame. Anchors are a side field geometry never reads
    /// (`docs/fillets.md`, section 5.3), so the ones selection needs are
    /// copied here, where the cache key covers them.
    pub anchors: Vec<Anchor>,
}

impl FilletNode {
    /// The anchor `name` resolves to.
    pub fn anchor(&self, name: &str) -> Option<&Anchor> {
        self.anchors.iter().find(|a| a.name == name)
    }
}

/// The parameters in positional order, then the optional ones.
pub(crate) fn params(b: BuiltinModule) -> (&'static [&'static str], &'static [&'static str]) {
    match b {
        BuiltinModule::ChamferEdges => (&["d", "edges", "except", "expect"], &["r"]),
        _ => (&["r", "edges", "except", "expect"], &[]),
    }
}

/// A failed argument: where, what, and an optional fix.
struct Problem {
    loc: Loc,
    text: String,
    hints: Vec<Hint>,
}

impl<'a> Evaluator<'a> {
    /// The start of a `fillet_edges()`/`chamfer_edges()` call: its node
    /// kind from the bound arguments, with the call's diagnostics.
    pub(crate) fn fillet_kind(
        &mut self,
        b: BuiltinModule,
        p: &Params,
        sr: ScopeRef,
        i: usize,
    ) -> NodeKind {
        let kind = if b == BuiltinModule::ChamferEdges {
            FilletKind::Chamfer
        } else {
            FilletKind::Fillet
        };
        let disc = self.discretizer(p);
        match self.fillet_node(kind, p, sr, i, disc) {
            Ok(node) => NodeKind::Fillet(Box::new(node)),
            Err(e) => {
                self.emit_with_hints(
                    Severity::Error,
                    e.code,
                    e.problem.text.as_bytes(),
                    Some(e.problem.loc),
                    e.problem.hints,
                );
                NodeKind::Group { name: None }
            }
        }
    }

    fn fillet_node(
        &mut self,
        kind: FilletKind,
        p: &Params,
        sr: ScopeRef,
        i: usize,
        disc: Discretizer,
    ) -> Result<FilletNode, Failed> {
        let m = kind.module();
        let size_name = kind.size_name();
        // `chamfer_edges(r = 1)` is accepted so that an agent can swap the
        // module name without renaming the argument (`docs/fillets.md`,
        // section 4).
        let mut size_value = self.get(p, size_name);
        let mut size_param = size_name;
        if kind == FilletKind::Chamfer {
            let r = self.get(p, "r");
            if !r.is_undef() {
                if !size_value.is_undef() {
                    return Err(Failed::args(Problem {
                        loc: self.arg_loc(sr, i, "r", None),
                        text: format!("{m}(): give d or r, not both"),
                        hints: vec![hint(
                            "remove one of them: r is accepted as another name for d",
                        )],
                    }));
                }
                size_value = r;
                size_param = "r";
            }
        }
        let size = match size_value {
            Value::Number(x) if x.is_finite() && x > 0.0 => x,
            Value::Undef => {
                let what = if kind == FilletKind::Fillet {
                    "the fillet radius"
                } else {
                    "the chamfer distance along each face"
                };
                return Err(Failed::args(Problem {
                    loc: p.loc,
                    text: format!("{m}(): {size_name} is required: {what}, a positive number"),
                    hints: vec![hint(format!(
                        "write {m}({size_name} = 1) or the size you need"
                    ))],
                }));
            }
            v => {
                let mut t =
                    format!("{m}(): {size_param} must be a positive number, found ").into_bytes();
                self.write_echo_nothrow(&v, &mut t);
                return Err(Failed::args(Problem {
                    loc: self.arg_loc(sr, i, size_param, Some(0)),
                    text: String::from_utf8_lossy(&t).into_owned(),
                    hints: Vec::new(),
                }));
            }
        };
        let edges = match self.get(p, "edges") {
            Value::Undef => Selector::all(),
            v => self.selector_arg(m, "edges", &v, sr, i, 1)?,
        };
        let except = match self.get(p, "except") {
            Value::Undef => None,
            v => Some(self.selector_arg(m, "except", &v, sr, i, 2)?),
        };
        let expect = match self.get(p, "expect") {
            Value::Undef => None,
            Value::Number(x) if x >= 0.0 && x.fract() == 0.0 && x <= f64::from(u32::MAX) => {
                Some(x as u32)
            }
            v => {
                let mut t =
                    format!("{m}(): expect must be a whole number of edges, found ").into_bytes();
                self.write_echo_nothrow(&v, &mut t);
                return Err(Failed::args(Problem {
                    loc: self.arg_loc(sr, i, "expect", Some(3)),
                    text: String::from_utf8_lossy(&t).into_owned(),
                    hints: Vec::new(),
                }));
            }
        };
        Ok(FilletNode {
            kind,
            size,
            edges,
            except,
            expect,
            disc,
            anchors: Vec::new(),
        })
    }

    /// The end of a fillet call, once its children are instantiated: what
    /// the selectors say about the children is checked (`child(i)` in
    /// range, `@name` an anchor of theirs) and the anchors are resolved
    /// onto the node. A problem is an error at the call, which then
    /// becomes a plain group, as for a bad argument.
    pub(crate) fn fillet_close(&mut self, node: &mut Node) {
        let NodeKind::Fillet(f) = &node.kind else {
            return;
        };
        let loc = node.origin.as_ref().map(|o| Loc {
            unit: o.unit,
            span: o.span,
        });
        let m = f.kind.module();
        let count = node.children.len();
        // An `anchor()` written directly among the children lands on the
        // call's own node (in its frame, which is the call's); the rest
        // are below, placed into the call's frame.
        let mut found: Vec<Anchor> = node.anchors.as_deref().cloned().unwrap_or_default();
        found.extend(crate::query::anchors_in(&node.children));
        let mut problem: Option<(String, Vec<Hint>)> = None;
        let mut resolved: Vec<Anchor> = Vec::new();
        let atoms = f.edges.atoms().into_iter().map(|a| ("edges", a)).chain(
            f.except
                .iter()
                .flat_map(|s| s.atoms())
                .map(|a| ("except", a)),
        );
        for (param, atom) in atoms {
            match atom {
                selector::Atom::Child(i, j) => {
                    let bad = std::iter::once(*i).chain(*j).find(|&k| k as usize >= count);
                    if let Some(k) = bad {
                        let have = match count {
                            0 => "has no children".to_string(),
                            1 => "has 1 child, child(0)".to_string(),
                            n => format!("has {n} children, child(0) to child({})", n - 1),
                        };
                        problem = Some((
                            format!("{m}(): {param}: {atom} names child {k}, but the call {have}"),
                            vec![hint(
                                "children are counted from 0 in the order they are written",
                            )],
                        ));
                        break;
                    }
                }
                selector::Atom::Anchor(name) => {
                    if resolved.iter().any(|a| &a.name == name) {
                        continue;
                    }
                    match found.iter().find(|a| &a.name == name) {
                        Some(a) => resolved.push(a.clone()),
                        None => {
                            let mut names: Vec<&str> =
                                found.iter().map(|a| a.name.as_str()).collect();
                            names.sort_unstable();
                            names.dedup();
                            let have = if names.is_empty() {
                                "the children declare no anchors".to_string()
                            } else {
                                let list: Vec<String> =
                                    names.iter().map(|n| format!("@{n}")).collect();
                                format!("the children's anchors are {}", list.join(", "))
                            };
                            problem = Some((
                                format!(
                                    "{m}(): {param}: no anchor named '{name}' among the children; {have}"
                                ),
                                vec![hint(
                                    "declare it with anchor(\"name\", point, direction) inside a child",
                                )],
                            ));
                            break;
                        }
                    }
                }
                _ => {}
            }
        }
        if let Some((text, hints)) = problem {
            self.emit_with_hints(
                Severity::Error,
                DiagCode::FilletSelector,
                text.as_bytes(),
                loc,
                hints,
            );
            node.kind = NodeKind::Group { name: None };
            return;
        }
        let NodeKind::Fillet(f) = &mut node.kind else {
            return;
        };
        f.anchors = resolved;
    }

    /// An `edges` or `except` value: a selector string, a BOSL2 direction
    /// vector, or a list of them.
    fn selector_arg(
        &mut self,
        m: &str,
        param: &str,
        v: &Value,
        sr: ScopeRef,
        i: usize,
        position: usize,
    ) -> Result<Selector, Failed> {
        let allowed = selector::Allowed {
            part: self.opts.extensions.has(crate::Extension::Part),
            anchor: self.opts.extensions.has(crate::Extension::Query),
        };
        let arg = self.arg_expr(sr, i, param, Some(position));
        let shape = "a selector string such as \"|z and >x\", a direction vector such as [0, 0, 1], or a list of them";
        let items: Vec<(Option<usize>, &Value)> = match v {
            Value::Vector(list) if list.iter().all(|x| matches!(x, Value::Number(_))) => {
                vec![(None, v)]
            }
            Value::Vector(list) => list.iter().enumerate().map(|(k, x)| (Some(k), x)).collect(),
            other => vec![(None, other)],
        };
        let mut out = Vec::with_capacity(items.len());
        for (k, item) in items {
            let expr = arg.and_then(|e| match k {
                Some(k) => self.list_item_expr(sr, e, k),
                None => Some(e),
            });
            match item {
                Value::Str(s) => {
                    let text = String::from_utf8_lossy(s.as_bytes()).into_owned();
                    match selector::parse(&text, allowed) {
                        Ok(e) => out.push(Item::Expr(e)),
                        Err(e) => {
                            // An empty selector has no column to point
                            // at: the whole string is the problem.
                            let (loc, exact) = if text.trim().is_empty() {
                                (self.expr_or_call(sr, i, expr), false)
                            } else {
                                self.string_loc(sr, i, expr, e.start, e.end)
                            };
                            let mut hints = Vec::new();
                            if let Some(fix) = e.suggestion {
                                hints.push(Hint {
                                    message: format!("did you mean '{fix}'?"),
                                    replacement: exact.then(|| (loc.span, fix.clone())),
                                });
                            }
                            let col = e.start + 1;
                            return Err(Failed {
                                code: DiagCode::FilletSelector,
                                problem: Problem {
                                    loc,
                                    text: format!(
                                        "{m}(): {param} = \"{text}\", column {col}: {}",
                                        e.message
                                    ),
                                    hints,
                                },
                            });
                        }
                    }
                }
                Value::Vector(d) => match descriptor(d.as_slice()) {
                    Some(d) => out.push(Item::Descriptor(d)),
                    None => {
                        let mut t = format!("{m}(): {param}: a direction vector is three entries, each -1, 0 or 1, not all 0; found ").into_bytes();
                        self.write_echo_nothrow(item, &mut t);
                        return Err(Failed {
                            code: DiagCode::FilletSelector,
                            problem: Problem {
                                loc: self.expr_or_call(sr, i, expr),
                                text: String::from_utf8_lossy(&t).into_owned(),
                                hints: vec![hint(
                                    "[0, 0, 1] is the top face's edges, [1, 0, 1] the top right edge, [1, 1, 1] the edges at that corner",
                                )],
                            },
                        });
                    }
                },
                _ => {
                    let mut t = format!("{m}(): {param} must be {shape}; found ").into_bytes();
                    self.write_echo_nothrow(item, &mut t);
                    return Err(Failed {
                        code: DiagCode::FilletSelector,
                        problem: Problem {
                            loc: self.expr_or_call(sr, i, expr),
                            text: String::from_utf8_lossy(&t).into_owned(),
                            hints: Vec::new(),
                        },
                    });
                }
            }
        }
        Ok(Selector { items: out })
    }

    /// The expression of argument `name` of call `i` in `sr`: the named
    /// argument, else the positional one at `position`.
    fn arg_expr(
        &self,
        sr: ScopeRef,
        i: usize,
        name: &str,
        position: Option<usize>,
    ) -> Option<lang::ast::ExprId> {
        let ast = self.units[sr.unit as usize].ast;
        let args = &self.inst(sr, i).args;
        args.iter()
            .rev()
            .find(|a| a.name.is_some_and(|n| ast.name(n) == name))
            .or_else(|| position.and_then(|k| args.iter().filter(|a| a.name.is_none()).nth(k)))
            .map(|a| a.expr)
    }

    fn arg_loc(&self, sr: ScopeRef, i: usize, name: &str, position: Option<usize>) -> Loc {
        let e = self.arg_expr(sr, i, name, position);
        self.expr_or_call(sr, i, e)
    }

    fn expr_or_call(&self, sr: ScopeRef, i: usize, e: Option<lang::ast::ExprId>) -> Loc {
        match e {
            Some(e) => Loc {
                unit: sr.unit,
                span: self.units[sr.unit as usize].ast.expr(e).span,
            },
            None => self.inst_loc(sr, i),
        }
    }

    /// Item `k` of a list literal, when the list is written out item by
    /// item (not a comprehension, whose items are not in the source).
    fn list_item_expr(
        &self,
        sr: ScopeRef,
        e: lang::ast::ExprId,
        k: usize,
    ) -> Option<lang::ast::ExprId> {
        let ast = self.units[sr.unit as usize].ast;
        match &ast.expr(e).kind {
            lang::ast::ExprKind::Vector(items)
                if items.iter().all(|x| {
                    !matches!(
                        ast.expr(*x).kind,
                        lang::ast::ExprKind::LcFor(..)
                            | lang::ast::ExprKind::LcForC { .. }
                            | lang::ast::ExprKind::LcEach(_)
                            | lang::ast::ExprKind::LcIf(..)
                            | lang::ast::ExprKind::LcLet(..)
                    )
                }) =>
            {
                items.get(k).copied()
            }
            _ => None,
        }
    }

    /// Where bytes `start..end` of a selector string are in the source:
    /// exactly, when the argument is a string literal written without
    /// escapes (so the string's bytes are the source's), and otherwise the
    /// whole expression (or the call). The flag says which.
    fn string_loc(
        &self,
        sr: ScopeRef,
        i: usize,
        e: Option<lang::ast::ExprId>,
        start: usize,
        end: usize,
    ) -> (Loc, bool) {
        let whole = self.expr_or_call(sr, i, e);
        let Some(e) = e else {
            return (whole, false);
        };
        let unit = &self.units[sr.unit as usize];
        let expr = unit.ast.expr(e);
        let lang::ast::ExprKind::String(value) = &expr.kind else {
            return (whole, false);
        };
        let src = unit.program.sources.text(expr.span);
        let inner = src.len() >= 2 && src[0] == b'"' && src[src.len() - 1] == b'"';
        if !inner || src[1..src.len() - 1] != value[..] {
            return (whole, false);
        }
        let base = expr.span.start + 1;
        let span = Span {
            file: expr.span.file,
            start: base + start as u32,
            end: base + end as u32,
        };
        (
            Loc {
                unit: sr.unit,
                span,
            },
            true,
        )
    }
}

/// A failed call: the code and the problem.
struct Failed {
    code: DiagCode,
    problem: Problem,
}

impl Failed {
    fn args(problem: Problem) -> Failed {
        Failed {
            code: DiagCode::InvalidArgument,
            problem,
        }
    }
}

fn hint(m: impl Into<String>) -> Hint {
    Hint {
        message: m.into(),
        replacement: None,
    }
}

/// A BOSL2 edge descriptor: three numbers, each -1, 0 or 1, not all 0.
fn descriptor(v: &[Value]) -> Option<[i8; 3]> {
    if v.len() != 3 {
        return None;
    }
    let mut d = [0i8; 3];
    for (k, x) in v.iter().enumerate() {
        d[k] = match x {
            Value::Number(n) if *n == -1.0 => -1,
            Value::Number(n) if *n == 0.0 => 0,
            Value::Number(n) if *n == 1.0 => 1,
            _ => return None,
        };
    }
    (d != [0; 3]).then_some(d)
}
