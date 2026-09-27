//! The value of a top-level constant, for hover, when it is cheap to
//! know: literals, vectors, arithmetic, comparisons, conditionals and
//! other such constants, folded from the syntax alone. Anything else
//! (a function call, a special variable) has no value here, and hover
//! shows the expression as written instead. Folding is bounded, so a
//! long chain of constants or a huge vector costs microseconds, never
//! an evaluation.

use std::sync::Arc;

use lang::ast::{Ast, BinaryOp, ExprId, ExprKind, UnaryOp};

use crate::index::Ns;
use crate::world::{Analyzed, World};

#[derive(Debug, Clone, PartialEq)]
enum V {
    Undef,
    Bool(bool),
    Num(f64),
    Str(String),
    Vec(Vec<V>),
}

/// Constants followed through before giving up.
const DEPTH: u32 = 16;
/// Expression nodes folded in all.
const STEPS: u32 = 2000;
/// Vector elements shown.
const SHOWN: usize = 12;

/// The value of top-level variable `name` of `file`, as OpenSCAD would
/// print it, if it folds.
pub fn constant(world: &World, file: &Arc<Analyzed>, name: &str) -> Option<String> {
    let mut f = Folder { world, steps: 0 };
    let v = f.var(file, name, 0)?;
    let mut out = String::new();
    show(&v, &mut out);
    Some(out)
}

struct Folder<'a> {
    world: &'a World,
    steps: u32,
}

fn assignment(ast: &Ast, name: &str) -> Option<ExprId> {
    let n = ast.names.get(name)?;
    ast.root
        .assignments
        .iter()
        .find(|a| a.name == n)
        .map(|a| a.expr)
}

impl Folder<'_> {
    fn var(&mut self, file: &Arc<Analyzed>, name: &str, depth: u32) -> Option<V> {
        if depth > DEPTH || name.starts_with('$') {
            return None;
        }
        let found = self.world.top(name, Ns::Variable, file)?;
        let ast = &found.file.program.ast;
        let e = assignment(ast, name)?;
        let file = found.file.clone();
        self.expr(&file, e, depth + 1)
    }

    fn expr(&mut self, file: &Arc<Analyzed>, id: ExprId, depth: u32) -> Option<V> {
        self.steps += 1;
        if self.steps > STEPS {
            return None;
        }
        let ast = &file.program.ast;
        Some(match &ast.expr(id).kind {
            ExprKind::Undef => V::Undef,
            ExprKind::Bool(b) => V::Bool(*b),
            ExprKind::Number(n) => V::Num(*n),
            ExprKind::String(s) => V::Str(String::from_utf8_lossy(s).into_owned()),
            ExprKind::Var(n) => {
                let name = ast.name(*n).to_string();
                return match name.as_str() {
                    "PI" if self.world.top("PI", Ns::Variable, file).is_none() => {
                        Some(V::Num(std::f64::consts::PI))
                    }
                    _ => self.var(file, &name, depth),
                };
            }
            ExprKind::Vector(items) => {
                let mut out = Vec::with_capacity(items.len());
                for &i in items {
                    out.push(self.expr(file, i, depth)?);
                }
                V::Vec(out)
            }
            ExprKind::Unary(op, a) => {
                let a = self.expr(file, *a, depth)?;
                match (op, a) {
                    (UnaryOp::Negate, V::Num(x)) => V::Num(-x),
                    (UnaryOp::Negate, V::Vec(v)) => V::Vec(
                        v.into_iter()
                            .map(|x| match x {
                                V::Num(n) => Some(V::Num(-n)),
                                _ => None,
                            })
                            .collect::<Option<_>>()?,
                    ),
                    (UnaryOp::Not, a) => V::Bool(!truthy(&a)),
                    _ => return None,
                }
            }
            ExprKind::Binary(op, a, b) => {
                let a = self.expr(file, *a, depth)?;
                let b = self.expr(file, *b, depth)?;
                binary(*op, a, b)?
            }
            ExprKind::Ternary(c, a, b) => {
                let c = self.expr(file, *c, depth)?;
                let pick = if truthy(&c) { *a } else { *b };
                self.expr(file, pick, depth)?
            }
            ExprKind::Index(a, i) => {
                let a = self.expr(file, *a, depth)?;
                let V::Num(i) = self.expr(file, *i, depth)? else {
                    return None;
                };
                match a {
                    V::Vec(v) if i >= 0.0 => v.get(i as usize).cloned().unwrap_or(V::Undef),
                    _ => return None,
                }
            }
            _ => return None,
        })
    }
}

fn truthy(v: &V) -> bool {
    match v {
        V::Undef => false,
        V::Bool(b) => *b,
        V::Num(n) => *n != 0.0,
        V::Str(s) => !s.is_empty(),
        V::Vec(v) => !v.is_empty(),
    }
}

fn binary(op: BinaryOp, a: V, b: V) -> Option<V> {
    use BinaryOp as B;
    Some(match (op, a, b) {
        (B::LogicalAnd, a, b) => V::Bool(truthy(&a) && truthy(&b)),
        (B::LogicalOr, a, b) => V::Bool(truthy(&a) || truthy(&b)),
        (B::Equal, a, b) => V::Bool(a == b),
        (B::NotEqual, a, b) => V::Bool(a != b),
        (B::Less, V::Num(x), V::Num(y)) => V::Bool(x < y),
        (B::LessEqual, V::Num(x), V::Num(y)) => V::Bool(x <= y),
        (B::Greater, V::Num(x), V::Num(y)) => V::Bool(x > y),
        (B::GreaterEqual, V::Num(x), V::Num(y)) => V::Bool(x >= y),
        (B::Plus, V::Num(x), V::Num(y)) => V::Num(x + y),
        (B::Minus, V::Num(x), V::Num(y)) => V::Num(x - y),
        (B::Multiply, V::Num(x), V::Num(y)) => V::Num(x * y),
        (B::Divide, V::Num(x), V::Num(y)) => V::Num(x / y),
        (B::Modulo, V::Num(x), V::Num(y)) => V::Num(x % y),
        (B::Exponent, V::Num(x), V::Num(y)) => V::Num(x.powf(y)),
        (B::Plus | B::Minus, V::Vec(x), V::Vec(y)) => V::Vec(
            x.iter()
                .zip(&y)
                .map(|(p, q)| match (p, q) {
                    (V::Num(p), V::Num(q)) => {
                        Some(V::Num(if op == B::Plus { p + q } else { p - q }))
                    }
                    _ => None,
                })
                .collect::<Option<_>>()?,
        ),
        (B::Multiply | B::Divide, V::Vec(x), V::Num(k)) => V::Vec(
            x.iter()
                .map(|p| match p {
                    V::Num(p) => Some(V::Num(if op == B::Multiply { p * k } else { p / k })),
                    _ => None,
                })
                .collect::<Option<_>>()?,
        ),
        (B::Multiply, V::Num(k), V::Vec(x)) => V::Vec(
            x.iter()
                .map(|p| match p {
                    V::Num(p) => Some(V::Num(k * p)),
                    _ => None,
                })
                .collect::<Option<_>>()?,
        ),
        _ => return None,
    })
}

fn show(v: &V, out: &mut String) {
    match v {
        V::Undef => out.push_str("undef"),
        V::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        V::Num(n) => lang::number::write_number(out, *n),
        V::Str(s) => {
            out.push('"');
            for c in s.chars() {
                match c {
                    '"' => out.push_str("\\\""),
                    '\\' => out.push_str("\\\\"),
                    '\n' => out.push_str("\\n"),
                    '\t' => out.push_str("\\t"),
                    c => out.push(c),
                }
            }
            out.push('"');
        }
        V::Vec(items) => {
            out.push('[');
            for (i, x) in items.iter().take(SHOWN).enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                show(x, out);
            }
            if items.len() > SHOWN {
                out.push_str(&format!(", ... ({} more)", items.len() - SHOWN));
            }
            out.push(']');
        }
    }
}
