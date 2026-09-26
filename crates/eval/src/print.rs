//! Printing values as OpenSCAD does.
//!
//! There are three flavours, and the differences are visible in echo
//! output:
//!
//! - `toString` (`str()`): a string is its raw bytes;
//! - `toEchoString` (`echo()`, messages): a top-level string is wrapped in
//!   quotes, unescaped;
//! - `operator<<` (traces): a top-level string is quoted *and* escaped.
//!
//! Inside a vector a string is always quoted and never escaped. Numbers use
//! `lang::number` (double-conversion's shortest form at 6 digits).
//!
//! Printing a deeply nested vector recurses; like OpenSCAD it checks the
//! stack at each level and fails with "Stack exhausted" instead of
//! overflowing.

use lang::ast::ExprKind;
use lang::number::write_number;

use crate::eval::Evaluator;
use crate::value::{Range, Value};

/// The stack ran out while printing.
#[derive(Debug)]
pub(crate) struct Exhausted;

pub(crate) fn push_number(out: &mut Vec<u8>, n: f64) {
    let mut s = String::new();
    write_number(&mut s, n);
    out.extend_from_slice(s.as_bytes());
}

fn push_range(out: &mut Vec<u8>, r: &Range) {
    out.push(b'[');
    push_number(out, r.begin);
    out.extend_from_slice(b" : ");
    push_number(out, r.step);
    out.extend_from_slice(b" : ");
    push_number(out, r.end);
    out.push(b']');
}

/// OpenSCAD's own stack limit (8 MiB minus a 128 KiB buffer). Printing a
/// nested vector costs about as much stack per level here as there, so
/// printing uses this limit rather than [`crate::Options::stack_limit`]
/// (which is larger to give calls the same depth as OpenSCAD). Otherwise a
/// runaway nesting like `issue4172` would print vectors five times deeper
/// (and output hundreds of MB) before failing.
const PRINT_STACK_LIMIT: usize = (8 << 20) - (128 << 10);

impl Evaluator<'_> {
    fn print_stack_exhausted(&self) -> bool {
        self.stack_used() >= self.opts.stack_limit.min(PRINT_STACK_LIMIT)
    }

    /// `tostream_visitor`: nested values.
    fn write_nested(&self, v: &Value, out: &mut Vec<u8>) -> Result<(), Exhausted> {
        match v {
            Value::Undef => out.extend_from_slice(b"undef"),
            Value::Bool(b) => out.extend_from_slice(if *b { b"true" } else { b"false" }),
            Value::Number(n) => push_number(out, *n),
            Value::Str(s) => {
                out.push(b'"');
                out.extend_from_slice(s.as_bytes());
                out.push(b'"');
            }
            Value::Vector(items) => {
                if self.print_stack_exhausted() {
                    return Err(Exhausted);
                }
                out.push(b'[');
                for (i, e) in items.iter().enumerate() {
                    if i > 0 {
                        out.extend_from_slice(b", ");
                    }
                    self.write_nested(e, out)?;
                }
                out.push(b']');
            }
            Value::Range(r) => push_range(out, r),
            Value::Function(f) => {
                let ast = self.units[f.unit as usize].ast;
                if let ExprKind::Function(params, body) = &ast.expr(f.expr).kind {
                    out.extend_from_slice(b"function(");
                    lang::dump::write_params(ast, params, out);
                    out.extend_from_slice(b") ");
                    lang::dump::write_expr(ast, *body, out);
                }
            }
        }
        Ok(())
    }

    /// `Value::toString`: a string prints raw.
    pub fn write_string(&self, v: &Value, out: &mut Vec<u8>) -> Result<(), Exhausted> {
        match v {
            Value::Str(s) => {
                out.extend_from_slice(s.as_bytes());
                Ok(())
            }
            _ => self.write_nested(v, out),
        }
    }

    /// `Value::toEchoString`, failing on stack exhaustion.
    pub fn write_echo_checked(&self, v: &Value, out: &mut Vec<u8>) -> Result<(), Exhausted> {
        self.write_nested(v, out)
    }

    /// `toEchoString` where OpenSCAD cannot fail in practice.
    pub fn write_echo(&mut self, v: &Value, out: &mut Vec<u8>) {
        self.write_echo_nothrow(v, out);
    }

    /// `Value::toEchoStringNoThrow`: `...` when the value cannot be printed.
    /// OpenSCAD still logs the exhaustion error on the way (the conversion
    /// logs before it throws), so this does too.
    pub fn write_echo_nothrow(&mut self, v: &Value, out: &mut Vec<u8>) {
        let start = out.len();
        if self.write_nested(v, out).is_err() {
            out.truncate(start);
            out.extend_from_slice(b"...");
            self.log_exhausted();
        }
    }

    /// The error `tostring_visitor` logs when printing runs out of stack.
    pub fn log_exhausted(&mut self) {
        self.error(None, lang::diag::DiagCode::RecursionLimit, "Stack exhausted while trying to convert a vector to EchoString");
    }

    /// `operator<<(ostream&, const Value&)`: a string is quoted and escaped.
    pub fn write_quoted(&self, v: &Value, out: &mut Vec<u8>) -> Result<(), Exhausted> {
        match v {
            Value::Str(s) => {
                lang::dump::quoted(out, s.as_bytes());
                Ok(())
            }
            _ => self.write_nested(v, out),
        }
    }
}
