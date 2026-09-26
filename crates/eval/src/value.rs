//! OpenSCAD values.
//!
//! A [`Value`] is 16 bytes and cheap to clone: numbers and booleans are
//! inline, strings, vectors and function literals are reference counted,
//! and a range is kept lazy as `(begin, step, end)`. Values never borrow the
//! AST (a function literal names its expression by unit and id), so they
//! carry no lifetime and could later be cached across evaluations.
//!
//! OpenSCAD's `UndefType` also carries "reasons" for how an undef came to
//! be. They can only be observed by the operator that produced them (it
//! prints them as a warning), so here operators return them separately
//! (see `ops`) and a stored `Undef` carries nothing.

use std::cell::Cell;
use std::fmt;
use std::rc::Rc;

use lang::ast::ExprId;

use crate::context::Ctx;
use crate::utf8;

#[derive(Clone, Debug, Default)]
pub enum Value {
    #[default]
    Undef,
    Bool(bool),
    Number(f64),
    Str(Str),
    Vector(Vector),
    Range(Rc<Range>),
    Function(Rc<FunctionValue>),
}

/// The value kinds, named as OpenSCAD names them in messages.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Type {
    Undef,
    Bool,
    Number,
    Str,
    Vector,
    Range,
    Function,
}

impl Type {
    pub fn name(self) -> &'static str {
        match self {
            Type::Undef => "undefined",
            Type::Bool => "bool",
            Type::Number => "number",
            Type::Str => "string",
            Type::Vector => "vector",
            Type::Range => "range",
            Type::Function => "function",
        }
    }
}

impl Value {
    pub fn ty(&self) -> Type {
        match self {
            Value::Undef => Type::Undef,
            Value::Bool(_) => Type::Bool,
            Value::Number(_) => Type::Number,
            Value::Str(_) => Type::Str,
            Value::Vector(_) => Type::Vector,
            Value::Range(_) => Type::Range,
            Value::Function(_) => Type::Function,
        }
    }

    pub fn type_name(&self) -> &'static str {
        self.ty().name()
    }

    pub fn is_undef(&self) -> bool {
        matches!(self, Value::Undef)
    }

    pub fn is_defined(&self) -> bool {
        !self.is_undef()
    }

    /// `Value::toBool`.
    pub fn to_bool(&self) -> bool {
        match self {
            Value::Undef => false,
            Value::Bool(b) => *b,
            Value::Number(n) => *n != 0.0,
            Value::Str(s) => !s.is_empty(),
            Value::Vector(v) => !v.is_empty(),
            Value::Range(_) | Value::Function(_) => true,
        }
    }

    /// `Value::toDouble`: the number, or 0.
    pub fn to_f64(&self) -> f64 {
        match self {
            Value::Number(n) => *n,
            _ => 0.0,
        }
    }

    pub fn as_number(&self) -> Option<f64> {
        match self {
            Value::Number(n) => Some(*n),
            _ => None,
        }
    }

    pub fn as_finite(&self) -> Option<f64> {
        self.as_number().filter(|n| n.is_finite())
    }

    pub fn as_vector(&self) -> Option<&Vector> {
        match self {
            Value::Vector(v) => Some(v),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&Str> {
        match self {
            Value::Str(s) => Some(s),
            _ => None,
        }
    }

    /// `Value::getUnsignedInt`.
    pub fn as_unsigned(&self) -> Option<u32> {
        self.as_finite()
            .filter(|&v| (0.0..=f64::from(u32::MAX)).contains(&v))
            .map(|v| v as u32)
    }

    /// `Value::getPositiveInt`.
    pub fn as_positive_int(&self) -> Option<u32> {
        self.as_finite()
            .filter(|&v| (1.0..=f64::from(u32::MAX)).contains(&v))
            .map(|v| v as u32)
    }

    /// `getVec2`: a two-element vector of numbers (finite ones when
    /// `finite` is set).
    pub fn as_vec2(&self, finite: bool) -> Option<[f64; 2]> {
        let v = self.as_vector()?;
        if v.len() != 2 {
            return None;
        }
        let get = |x: &Value| if finite { x.as_finite() } else { x.as_number() };
        Some([get(&v[0])?, get(&v[1])?])
    }

    /// `getDouble`: store the number in `out` if this is one.
    pub fn get_f64(&self, out: &mut f64) -> bool {
        match self {
            Value::Number(n) => {
                *out = *n;
                true
            }
            _ => false,
        }
    }

    /// `getVec2(x, y, ignoreInfinite)`: stores both or neither.
    pub fn get_vec2(&self, x: &mut f64, y: &mut f64, finite: bool) -> bool {
        match self.as_vec2(finite) {
            Some([a, b]) => {
                *x = a;
                *y = b;
                true
            }
            None => false,
        }
    }

    /// `getVec3(x, y, z)`: exactly three numbers. Like OpenSCAD it stores
    /// the leading numbers even when a later element fails, and callers
    /// that ignore the failure see them.
    pub fn get_vec3(&self, out: &mut [f64; 3]) -> bool {
        let Some(v) = self.as_vector() else {
            return false;
        };
        v.len() == 3 && v.iter().zip(out.iter_mut()).all(|(e, o)| e.get_f64(o))
    }

    /// `getVec3(x, y, z, defaultval)`: three numbers, or two with the third
    /// defaulted. A two-element vector succeeds even when its elements are
    /// not numbers; `x` and `y` are then left untouched.
    pub fn get_vec3_or2(&self, out: &mut [f64; 3], default: f64) -> bool {
        let Some(v) = self.as_vector() else {
            return false;
        };
        if v.len() == 2 {
            let (mut x, mut y) = (out[0], out[1]);
            if self.get_vec2(&mut x, &mut y, false) {
                out[0] = x;
                out[1] = y;
            }
            out[2] = default;
            return true;
        }
        self.get_vec3(out)
    }

    pub fn str(s: &[u8]) -> Value {
        Value::Str(Str::new(s))
    }

    pub fn vector(v: Vec<Value>) -> Value {
        Value::Vector(Vector::from(v))
    }

    pub fn range(begin: f64, step: f64, end: f64) -> Value {
        Value::Range(Rc::new(Range { begin, step, end }))
    }
}

impl From<f64> for Value {
    fn from(n: f64) -> Self {
        Value::Number(n)
    }
}

impl From<bool> for Value {
    fn from(b: bool) -> Self {
        Value::Bool(b)
    }
}

/// A byte string with a cached character count.
#[derive(Clone)]
pub struct Str(Rc<StrData>);

struct StrData {
    /// `usize::MAX` until first needed.
    chars: Cell<usize>,
    bytes: Box<[u8]>,
}

/// A large string counts towards the evaluator's memory estimate while it
/// lives (see `crate::limits::live`).
impl Drop for StrData {
    fn drop(&mut self) {
        if self.bytes.len() >= crate::limits::live::STR_MIN {
            crate::limits::live::credit(self.bytes.len() as u64);
        }
    }
}

impl Str {
    pub fn new(bytes: &[u8]) -> Self {
        Self::from_vec(bytes.to_vec())
    }

    pub fn from_vec(bytes: Vec<u8>) -> Self {
        if bytes.len() >= crate::limits::live::STR_MIN {
            crate::limits::live::charge(bytes.len() as u64);
        }
        Str(Rc::new(StrData {
            chars: Cell::new(usize::MAX),
            bytes: bytes.into_boxed_slice(),
        }))
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.0.bytes
    }

    pub fn is_empty(&self) -> bool {
        self.0.bytes.is_empty()
    }

    /// The number of characters as GLib counts them.
    pub fn char_count(&self) -> usize {
        let c = self.0.chars.get();
        if c != usize::MAX {
            return c;
        }
        let c = utf8::char_count(&self.0.bytes);
        self.0.chars.set(c);
        c
    }

    /// Character `i`, as a string.
    pub fn char_at(&self, i: usize) -> Option<&[u8]> {
        let b = self.as_bytes();
        if i < b.len() && self.char_count() == b.len() {
            return Some(&b[i..=i]);
        }
        utf8::char_at(b, i)
    }
}

impl fmt::Debug for Str {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}", String::from_utf8_lossy(self.as_bytes()))
    }
}

impl PartialEq for Str {
    fn eq(&self, other: &Self) -> bool {
        self.as_bytes() == other.as_bytes()
    }
}

/// A list of values, shared on clone.
#[derive(Clone, Debug)]
pub struct Vector(Rc<Vec<Value>>);

impl Vector {
    pub fn empty() -> Self {
        Vector(Rc::new(Vec::new()))
    }

    pub fn as_slice(&self) -> &[Value] {
        &self.0
    }

    /// Mutable access, copying the elements if they are shared.
    pub fn make_mut(&mut self) -> &mut Vec<Value> {
        Rc::make_mut(&mut self.0)
    }

    /// The elements, without copying when this is the only reference.
    pub fn into_vec(mut self) -> Vec<Value> {
        match Rc::get_mut(&mut self.0) {
            Some(v) => {
                // They leave the count with the list; whoever builds a new
                // list from them counts them again.
                credit_list(v);
                std::mem::take(v)
            }
            None => (*self.0).clone(),
        }
    }
}

/// Bytes a list counts for towards the evaluator's memory estimate
/// (`crate::limits::live`): large lists only, with the small lists and
/// strings they hold (a path of a million `[x, y, z]` points is 16 bytes
/// of outer list per point but about 120 with each point's own vector).
/// Values never change once in a list, so this is the same number when
/// the list is built and when it is freed.
fn list_bytes(items: &[Value]) -> u64 {
    use crate::limits::live::{LIST_MIN, STR_MIN};
    if items.len() < LIST_MIN {
        return 0;
    }
    const SLOT: u64 = std::mem::size_of::<Value>() as u64;
    // An `Rc` allocation (counts and header) with the allocator's rounding.
    const BOX: u64 = 64;
    items
        .iter()
        .map(|v| match v {
            Value::Vector(inner) if inner.len() < LIST_MIN => {
                SLOT + BOX + SLOT * inner.len() as u64
            }
            Value::Str(t) if t.as_bytes().len() < STR_MIN => SLOT + BOX + t.as_bytes().len() as u64,
            _ => SLOT,
        })
        .sum()
}

fn credit_list(items: &[Value]) {
    let b = list_bytes(items);
    if b > 0 {
        crate::limits::live::credit(b);
    }
}

/// Frees nested vectors with a loop instead of recursion. A tail-recursive
/// function can nest a vector a million levels deep (`f(n, acc) = n == 0 ?
/// acc : f(n - 1, [acc])`) without using any stack, and freeing it
/// recursively would then overflow the stack: natively only at depths far
/// beyond that, but in a WASM engine at a few thousand. (The nightly
/// crashes on such a value even natively.)
impl Drop for Vector {
    fn drop(&mut self) {
        let Some(items) = Rc::get_mut(&mut self.0) else {
            return;
        };
        credit_list(items);
        if !items.iter().any(|v| matches!(v, Value::Vector(_))) {
            return;
        }
        let mut pending = vec![std::mem::take(items)];
        while let Some(mut items) = pending.pop() {
            for v in items.drain(..) {
                if let Value::Vector(mut inner) = v
                    && let Some(inner) = Rc::get_mut(&mut inner.0)
                    && !inner.is_empty()
                {
                    credit_list(inner);
                    pending.push(std::mem::take(inner));
                }
            }
        }
    }
}

impl From<Vec<Value>> for Vector {
    fn from(v: Vec<Value>) -> Self {
        let b = list_bytes(&v);
        if b > 0 {
            crate::limits::live::charge(b);
        }
        Vector(Rc::new(v))
    }
}

impl std::ops::Deref for Vector {
    type Target = [Value];
    fn deref(&self) -> &[Value] {
        &self.0
    }
}

/// `RangeType`: a lazy arithmetic sequence.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Range {
    pub begin: f64,
    pub step: f64,
    pub end: f64,
}

/// `RangeType::MAX_RANGE_STEPS`: the limit for `children()` and `chr()`.
pub const MAX_RANGE_STEPS: u32 = 10000;

impl Range {
    /// `RangeType::numValues`, saturating at `u32::MAX`.
    pub fn num_values(&self) -> u32 {
        let (b, s, e) = (self.begin, self.step, self.end);
        if b.is_nan() || e.is_nan() || s.is_nan() {
            return 0;
        }
        if s < 0.0 {
            if b < e {
                return 0;
            }
        } else if b > e {
            return 0;
        }
        if b == e || s.is_infinite() {
            return 1;
        }
        if b.is_infinite() || e.is_infinite() || s == 0.0 {
            return u32::MAX;
        }
        // nextafter compensates for a quotient just below a whole number.
        let q = next_after((e - b) / s, f64::from(u32::MAX));
        let steps = q as u32;
        if steps == u32::MAX {
            u32::MAX
        } else {
            steps + 1
        }
    }

    /// The values, as `RangeType::iterator` produces them.
    pub fn iter(&self) -> impl Iterator<Item = f64> + '_ {
        let n =
            if self.begin.is_nan() || self.end.is_nan() || self.step.is_nan() || self.step == 0.0 {
                0
            } else {
                self.num_values()
            };
        (0..n).map(move |i| {
            if i == 0 {
                self.begin
            } else {
                // `begin_val + step_val * ++i_step`, fused on arm64 like
                // the nightly (see `fma`).
                crate::fma::mul_add(self.step, f64::from(i), self.begin)
            }
        })
    }

    fn cmp_key(&self) -> (f64, f64, u32) {
        (self.begin, self.step, self.num_values())
    }

    /// `RangeType::operator==`.
    pub fn equals(&self, o: &Range) -> bool {
        let (n1, n2) = (self.num_values(), o.num_values());
        if n1 == 0 {
            return n2 == 0;
        }
        if n2 == 0 {
            return false;
        }
        self.begin == o.begin && self.step == o.step && n1 == n2
    }

    /// `RangeType::operator<` (`or_equal`: `<=`). Empty ranges sort
    /// first; otherwise by begin, then step, then count.
    pub fn less(&self, o: &Range, or_equal: bool) -> bool {
        let (b1, s1, n1) = self.cmp_key();
        let (b2, s2, n2) = o.cmp_key();
        if n1 == 0 {
            return or_equal || n2 > 0;
        }
        if n2 == 0 {
            return false;
        }
        b1 < b2
            || (b1 == b2 && (s1 < s2 || (s1 == s2 && if or_equal { n1 <= n2 } else { n1 < n2 })))
    }

    /// `RangeType::operator>` (`or_equal`: `>=`).
    pub fn greater(&self, o: &Range, or_equal: bool) -> bool {
        let (b1, s1, n1) = self.cmp_key();
        let (b2, s2, n2) = o.cmp_key();
        if n2 == 0 {
            return or_equal || n1 > 0;
        }
        if n1 == 0 {
            return false;
        }
        b1 > b2
            || (b1 == b2 && (s1 > s2 || (s1 == s2 && if or_equal { n1 >= n2 } else { n1 > n2 })))
    }
}

/// C's `nextafter`.
pub fn next_after(x: f64, toward: f64) -> f64 {
    if x.is_nan() || toward.is_nan() {
        return f64::NAN;
    }
    if x == toward {
        return toward;
    }
    if x == 0.0 {
        let tiny = f64::from_bits(1);
        return if toward > 0.0 { tiny } else { -tiny };
    }
    let bits = x.to_bits();
    let up = (toward > x) == (x > 0.0);
    f64::from_bits(if up { bits + 1 } else { bits - 1 })
}

/// A function literal: its expression and the context it closed over.
pub struct FunctionValue {
    pub(crate) unit: u32,
    /// The `ExprKind::Function` expression.
    pub(crate) expr: ExprId,
    pub(crate) ctx: Rc<Ctx>,
}

impl fmt::Debug for FunctionValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "FunctionValue(unit {}, expr {})", self.unit, self.expr.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn range_counts_like_openscad() {
        let r = |b, s, e| Range {
            begin: b,
            step: s,
            end: e,
        };
        assert_eq!(r(0.0, 1.0, 5.0).num_values(), 6);
        assert_eq!(r(0.0, 0.1, 1.0).num_values(), 11);
        assert_eq!(r(5.0, 1.0, 0.0).num_values(), 0);
        assert_eq!(r(5.0, -1.0, 0.0).num_values(), 6);
        assert_eq!(r(0.0, 0.0, 1.0).num_values(), u32::MAX);
        assert_eq!(r(1.0, 0.0, 1.0).num_values(), 1);
        assert_eq!(r(0.0, 1.0, f64::INFINITY).num_values(), u32::MAX);
        assert_eq!(
            r(0.0, 1.0, 5.0).iter().collect::<Vec<_>>(),
            vec![0.0, 1.0, 2.0, 3.0, 4.0, 5.0]
        );
        assert_eq!(r(0.0, 0.0, 1.0).iter().count(), 0);
        assert!(r(1.0, 1.0, 0.0).equals(&r(2.0, 1.0, 0.0)));
    }

    #[test]
    fn strings_count_characters() {
        let s = Str::new("ab\u{e4}".as_bytes());
        assert_eq!(s.char_count(), 3);
        assert_eq!(s.char_at(2), Some("\u{e4}".as_bytes()));
        assert_eq!(Str::new(b"abc").char_at(1), Some(&b"b"[..]));
    }

    #[test]
    fn truthiness() {
        assert!(!Value::Undef.to_bool());
        assert!(!Value::Number(0.0).to_bool());
        assert!(Value::Number(f64::NAN).to_bool());
        assert!(!Value::str(b"").to_bool());
        assert!(Value::range(0.0, 1.0, -1.0).to_bool());
        assert!(!Value::vector(vec![]).to_bool());
    }
}
