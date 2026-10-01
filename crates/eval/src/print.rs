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
//!
//! Lists share their elements, so a list built as `c = t(n - 1); [c, c]`
//! is small in memory but prints as 2^n elements. Printing therefore
//! stops as soon as one value's text passes the string limit (nothing
//! can use more: `str()` could only fail, and a message that long is
//! useless), and polls the cancel flag and the time limit as it goes.
//! Before this, `echo(str(t(40)))` built gigabytes of text under the
//! agent limits before the string limit was ever checked.

use lang::ast::ExprKind;
use lang::number::write_number;

use crate::eval::Evaluator;
use crate::value::{Range, Value};

/// Why printing a value stopped before the end.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Exhausted {
    /// The stack ran out (OpenSCAD's "Stack exhausted" error).
    Stack,
    /// The value's text passed the string limit: the output would have
    /// been at least this many bytes (the buffer's whole length).
    Long(usize),
    /// Cancelled, or past the time limit; the next check raises it.
    Stopped,
}

/// How many list elements printing visits between looks at the cancel
/// flag and the clock (as `chr()` does).
const POLL_STEPS: u64 = 4096;

/// One value's printing: where its text must end, and the elements
/// visited so far.
struct Walk {
    end: usize,
    steps: u64,
}

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
    /// Whether printing a vector nested `depth` levels inside the value
    /// being printed must stop. Each level is also a frame of the frame
    /// budget ([`crate::recursion`]), on top of the frames the evaluation
    /// holds: a printing level costs less stack than an evaluation frame,
    /// so this is conservative, and it is what keeps printing a deeply
    /// nested value from overflowing a WASM engine's stack.
    fn print_stack_exhausted(&self, depth: u32) -> bool {
        self.stack_used() >= self.stack_limit().min(PRINT_STACK_LIMIT)
            || self
                .frames
                .saturating_add(depth.saturating_mul(self.weights.expression))
                >= self.opts.frame_limit
    }

    /// Whether printing should stop for a cancel or the time limit.
    fn print_stopped(&self) -> bool {
        self.interrupted()
            || self
                .opts
                .guard
                .as_deref()
                .is_some_and(crate::limits::Guard::over_time)
    }

    /// `tostream_visitor`: nested values. The text this adds to `out` is
    /// at most the string limit (plus one element's worth).
    fn write_nested(&self, v: &Value, out: &mut Vec<u8>) -> Result<(), Exhausted> {
        let mut w = Walk {
            end: out.len().saturating_add(self.caps.string),
            steps: 0,
        };
        self.write_nested_at(v, out, 0, &mut w)
    }

    fn write_nested_at(
        &self,
        v: &Value,
        out: &mut Vec<u8>,
        depth: u32,
        w: &mut Walk,
    ) -> Result<(), Exhausted> {
        match v {
            Value::Undef => out.extend_from_slice(b"undef"),
            Value::Bool(b) => out.extend_from_slice(if *b { b"true" } else { b"false" }),
            Value::Number(n) => push_number(out, *n),
            Value::Str(s) => {
                // Checked before the copy: a list of one long string
                // repeated would otherwise copy it once more past the end.
                // A string printed on its own is exempt: it is a value
                // that already fits the limit, and `echo(s)` of one at the
                // limit must print it (quoted, so two bytes longer).
                let n = out.len().saturating_add(s.as_bytes().len() + 2);
                if depth > 0 && n > w.end {
                    return Err(Exhausted::Long(n));
                }
                out.push(b'"');
                out.extend_from_slice(s.as_bytes());
                out.push(b'"');
            }
            Value::Vector(items) => {
                if self.print_stack_exhausted(depth) {
                    return Err(Exhausted::Stack);
                }
                // A list whose halves are shared prints as 2^depth
                // elements, so the text can pass the memory limit long
                // before any value does. Past it, printing stops quietly
                // (every level returns here at once) and the evaluator
                // reports the limit instead of the cut text.
                if crate::limits::live::passes(out.len() as u64) {
                    return Ok(());
                }
                out.push(b'[');
                for (i, e) in items.iter().enumerate() {
                    if out.len() > w.end {
                        return Err(Exhausted::Long(out.len()));
                    }
                    w.steps += 1;
                    if w.steps.is_multiple_of(POLL_STEPS) && self.print_stopped() {
                        return Err(Exhausted::Stopped);
                    }
                    if i > 0 {
                        out.extend_from_slice(b", ");
                    }
                    self.write_nested_at(e, out, depth + 1, w)?;
                }
                out.push(b']');
            }
            Value::Object(o) => {
                // `tostream_visitor` on an object: `{ key = value; ... }`,
                // keys raw and values as inside a list. Objects share
                // their values as lists do, so the same stops apply.
                if self.print_stack_exhausted(depth) {
                    return Err(Exhausted::Stack);
                }
                if crate::limits::live::passes(out.len() as u64) {
                    return Ok(());
                }
                out.extend_from_slice(b"{ ");
                for (k, e) in o.keys().iter().zip(o.values()) {
                    if out.len() > w.end {
                        return Err(Exhausted::Long(out.len()));
                    }
                    w.steps += 1;
                    if w.steps.is_multiple_of(POLL_STEPS) && self.print_stopped() {
                        return Err(Exhausted::Stopped);
                    }
                    out.extend_from_slice(k.as_bytes());
                    out.extend_from_slice(b" = ");
                    self.write_nested_at(e, out, depth + 1, w)?;
                    out.extend_from_slice(b"; ");
                }
                out.push(b'}');
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
        if let Err(e) = self.write_nested(v, out) {
            out.truncate(start);
            out.extend_from_slice(b"...");
            self.print_failed(e, "a message");
        }
    }

    /// Report why printing a value stopped, where the text is dropped
    /// (replaced by `...` or not printed at all): stack exhaustion as
    /// OpenSCAD logs it, the string limit as made by `what`. A cancel or
    /// the time limit is left to the next check, which raises it.
    pub(crate) fn print_failed(&mut self, e: Exhausted, what: &str) {
        match e {
            Exhausted::Stack => self.log_exhausted(),
            Exhausted::Long(n) => self.printed_too_long(n, None, what),
            Exhausted::Stopped => {}
        }
    }

    /// The string limit, passed by text of at least `n` bytes that
    /// printing stopped building.
    pub(crate) fn printed_too_long(
        &mut self,
        n: usize,
        loc: Option<crate::message::Loc>,
        what: &str,
    ) {
        let Some(g) = self.opts.guard.clone() else {
            return;
        };
        if let Some(mut e) = g.exceeds(crate::limits::Limit::String, n as f64, what) {
            e.at_least = true;
            self.limit_exceeded(loc, e);
        }
    }

    /// The error `tostring_visitor` logs when printing runs out of stack.
    pub fn log_exhausted(&mut self) {
        self.error(
            None,
            lang::diag::DiagCode::RecursionLimit,
            "Stack exhausted while trying to convert a vector to EchoString",
        );
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
