//! The `.ast` export: the AST printed back as OpenSCAD source, byte for byte
//! as OpenSCAD's `print` methods write it (src/core/LocalScope.cc,
//! ModuleInstantiation.cc, UserModule.cc, function.cc, Assignment.cc,
//! Expression.cc, customizer/Annotation.cc).
//!
//! Notable quirks reproduced here: binary and ternary operations are fully
//! parenthesised; a child scope with one element is printed inline after
//! the call; `else;` has no newline; a call through anything but a plain
//! name prints the callee in parentheses; `use` statements and included
//! files leave no trace except the spliced-in definitions.

use crate::ast::{
    Arg, Assignment, Ast, ExprId, ExprKind, FunctionDef, InstKind, Instantiation, ModuleDef, Param,
    Scope, UnaryOp,
};
use crate::number::write_number;

/// Dump the whole program. The result is bytes because OpenSCAD strings
/// are: a Latin-1 string literal is written back unchanged.
pub fn dump(ast: &Ast) -> Vec<u8> {
    let mut p = Printer {
        ast,
        out: Vec::with_capacity(4096),
        num: String::new(),
    };
    p.scope(&ast.root, "", false);
    p.out
}

/// Print one expression (lossily as UTF-8; for messages and tests).
pub fn expr_to_string(ast: &Ast, e: ExprId) -> String {
    let mut out = Vec::new();
    write_expr(ast, e, &mut out);
    String::from_utf8_lossy(&out).into_owned()
}

/// Append one expression as OpenSCAD's `Expression::print` writes it. The
/// evaluator needs the exact bytes: messages such as `Assertion '...'
/// failed` and printed function literals quote source strings verbatim,
/// which may not be UTF-8.
pub fn write_expr(ast: &Ast, e: ExprId, out: &mut Vec<u8>) {
    let mut p = Printer {
        ast,
        out: std::mem::take(out),
        num: String::new(),
    };
    p.expr(e);
    *out = p.out;
}

/// Append a parameter list (`a, b = 1`) as function literals print it.
pub fn write_params(ast: &Ast, params: &[Param], out: &mut Vec<u8>) {
    let mut p = Printer {
        ast,
        out: std::mem::take(out),
        num: String::new(),
    };
    p.params(params);
    *out = p.out;
}

struct Printer<'a> {
    ast: &'a Ast,
    out: Vec<u8>,
    /// Scratch buffer for number formatting.
    num: String,
}

impl Printer<'_> {
    fn s(&mut self, s: &str) {
        self.out.extend_from_slice(s.as_bytes());
    }

    fn scope(&mut self, s: &Scope, indent: &str, inlined: bool) {
        for f in &s.functions {
            self.function(f, indent);
        }
        for m in &s.modules {
            self.module(m, indent);
        }
        for a in &s.assignments {
            self.assignment(a, indent);
        }
        for i in &s.instantiations {
            self.instantiation(i, indent, inlined);
        }
    }

    fn function(&mut self, f: &FunctionDef, indent: &str) {
        self.s(indent);
        self.s("function ");
        self.s(self.ast.name(f.name));
        self.out.push(b'(');
        self.params(&f.params);
        self.s(") = ");
        self.expr(f.body);
        self.s(";\n");
    }

    fn module(&mut self, m: &ModuleDef, indent: &str) {
        self.s(indent);
        self.s("module ");
        self.s(self.ast.name(m.name));
        self.out.push(b'(');
        self.params(&m.params);
        self.s(") {\n");
        let inner = format!("{indent}\t");
        self.scope(&m.body, &inner, false);
        self.s(indent);
        self.s("}\n");
    }

    fn assignment(&mut self, a: &Assignment, indent: &str) {
        for name in ["Group", "Description", "Parameter"] {
            if let Some(e) = a.annotation(name) {
                self.s(indent);
                self.s("//");
                self.s(name);
                self.out.push(b'(');
                self.expr(e);
                self.s(")\n");
            }
        }
        self.s(indent);
        self.s(self.ast.name(a.name));
        self.s(" = ");
        self.expr(a.expr);
        self.s(";\n");
    }

    fn instantiation(&mut self, i: &Instantiation, indent: &str, inlined: bool) {
        if !inlined {
            self.s(indent);
        }
        self.s(self.ast.name(i.name));
        self.out.push(b'(');
        for (k, a) in i.args.iter().enumerate() {
            if k > 0 {
                self.s(", ");
            }
            if let Some(n) = a.name {
                self.s(self.ast.name(n));
                self.s(" = ");
            }
            self.expr(a.expr);
        }
        self.body(&i.children, indent, ");\n", ") ", ") {\n");
        if let InstKind::If {
            else_children: Some(e),
        } = &i.kind
        {
            self.s(indent);
            if e.num_elements() == 0 {
                self.s("else;");
            } else {
                self.s("else ");
                self.body(e, indent, "", "", "{\n");
            }
        }
    }

    /// A child scope: nothing, one element inline, or a braced block.
    fn body(&mut self, s: &Scope, indent: &str, empty: &str, one: &str, many: &str) {
        match s.num_elements() {
            0 => self.s(empty),
            1 => {
                self.s(one);
                self.scope(s, indent, true);
            }
            _ => {
                self.s(many);
                let inner = format!("{indent}\t");
                self.scope(s, &inner, false);
                self.s(indent);
                self.s("}\n");
            }
        }
    }

    fn params(&mut self, params: &[Param]) {
        for (k, p) in params.iter().enumerate() {
            if k > 0 {
                self.s(", ");
            }
            self.s(self.ast.name(p.name));
            if let Some(d) = p.default {
                self.s(" = ");
                self.expr(d);
            }
        }
    }

    /// `operator<<(ostream&, const AssignmentList&)`.
    fn args(&mut self, args: &[Arg]) {
        for (k, a) in args.iter().enumerate() {
            if k > 0 {
                self.s(", ");
            }
            if let Some(n) = a.name {
                self.s(self.ast.name(n));
                self.s(" = ");
            }
            self.expr(a.expr);
        }
    }

    fn expr(&mut self, id: ExprId) {
        let ast = self.ast;
        match &ast.expr(id).kind {
            ExprKind::Undef => self.s("undef"),
            ExprKind::Bool(b) => self.s(if *b { "true" } else { "false" }),
            ExprKind::Number(v) => {
                self.num.clear();
                write_number(&mut self.num, *v);
                self.out.extend_from_slice(self.num.as_bytes());
            }
            ExprKind::String(s) => quoted(&mut self.out, s),
            ExprKind::Var(n) => self.s(ast.name(*n)),
            ExprKind::Unary(op, e) => {
                self.s(match op {
                    UnaryOp::Not => "!",
                    UnaryOp::Negate => "-",
                    UnaryOp::BinaryNot => "~",
                });
                self.expr(*e);
            }
            ExprKind::Binary(op, l, r) => {
                self.out.push(b'(');
                self.expr(*l);
                self.out.push(b' ');
                self.s(op.as_str());
                self.out.push(b' ');
                self.expr(*r);
                self.out.push(b')');
            }
            ExprKind::Ternary(c, a, b) => {
                self.out.push(b'(');
                self.expr(*c);
                self.s(" ? ");
                self.expr(*a);
                self.s(" : ");
                self.expr(*b);
                self.out.push(b')');
            }
            ExprKind::Index(a, i) => {
                self.expr(*a);
                self.out.push(b'[');
                self.expr(*i);
                self.out.push(b']');
            }
            ExprKind::Member(a, n) => {
                self.expr(*a);
                self.out.push(b'.');
                self.s(ast.name(*n));
            }
            ExprKind::Call(callee, args) => {
                // FunctionCall's name: the identifier, or "(callee)".
                if let ExprKind::Var(n) = ast.expr(*callee).kind {
                    self.s(ast.name(n));
                } else {
                    self.out.push(b'(');
                    self.expr(*callee);
                    self.out.push(b')');
                }
                self.out.push(b'(');
                self.args(args);
                self.out.push(b')');
            }
            ExprKind::Range { begin, step, end } => {
                self.out.push(b'[');
                self.expr(*begin);
                if let Some(s) = step {
                    self.s(" : ");
                    self.expr(*s);
                }
                self.s(" : ");
                self.expr(*end);
                self.out.push(b']');
            }
            ExprKind::Vector(v) => {
                self.out.push(b'[');
                for (k, e) in v.iter().enumerate() {
                    if k > 0 {
                        self.s(", ");
                    }
                    self.expr(*e);
                }
                self.out.push(b']');
            }
            ExprKind::Function(params, body) => {
                self.s("function(");
                self.params(params);
                self.s(") ");
                self.expr(*body);
            }
            ExprKind::Let(args, body) => {
                self.s("let(");
                self.args(args);
                self.s(") ");
                self.expr(*body);
            }
            ExprKind::Assert(args, body) | ExprKind::Echo(args, body) => {
                self.s(if matches!(ast.expr(id).kind, ExprKind::Assert(..)) {
                    "assert("
                } else {
                    "echo("
                });
                self.args(args);
                self.out.push(b')');
                if let Some(b) = body {
                    self.out.push(b' ');
                    self.expr(*b);
                }
            }
            ExprKind::LcIf(c, a, b) => {
                self.s("if(");
                self.expr(*c);
                self.s(") (");
                self.expr(*a);
                self.out.push(b')');
                if let Some(b) = b {
                    self.s(" else (");
                    self.expr(*b);
                    self.out.push(b')');
                }
            }
            ExprKind::LcEach(e) => {
                self.s("each (");
                self.expr(*e);
                self.out.push(b')');
            }
            ExprKind::LcFor(args, body) => {
                self.s("for(");
                self.args(args);
                self.s(") (");
                self.expr(*body);
                self.out.push(b')');
            }
            ExprKind::LcForC {
                init,
                cond,
                incr,
                body,
            } => {
                self.s("for(");
                self.args(init);
                self.out.push(b';');
                self.expr(*cond);
                self.out.push(b';');
                self.args(incr);
                self.s(") ");
                self.expr(*body);
            }
            ExprKind::LcLet(args, body) => {
                self.s("let(");
                self.args(args);
                self.s(") (");
                self.expr(*body);
                self.out.push(b')');
            }
            ExprKind::Invalid => self.s("<invalid>"),
        }
    }
}

/// `QuotedString`: double quotes with `\t \n \r " \\` escaped.
pub fn quoted(out: &mut Vec<u8>, s: &[u8]) {
    out.push(b'"');
    for &c in s {
        match c {
            b'\t' => out.extend_from_slice(b"\\t"),
            b'\n' => out.extend_from_slice(b"\\n"),
            b'\r' => out.extend_from_slice(b"\\r"),
            b'"' => out.extend_from_slice(b"\\\""),
            b'\\' => out.extend_from_slice(b"\\\\"),
            c => out.push(c),
        }
    }
    out.push(b'"');
}
