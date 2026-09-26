//! Operators, as `Value.cc` defines them.
//!
//! Every operator returns either a value or the reasons its result is
//! undefined ([`Why`]). The expression evaluator prints the reasons as one
//! warning at the operator's location (`Expression::checkUndef`); when an
//! operator runs element-wise inside another (vector addition, say) the
//! inner reasons are dropped and the element is plain `undef`, exactly as
//! OpenSCAD stores an undef it never checks.
//!
//! The comparison wording is deliberately inconsistent, because OpenSCAD's
//! is: mixed types say `undefined operation (number < string)` while
//! `undef < undef` says `operation undefined (undefined < undefined)`.

use std::rc::Rc;

use crate::fma::mul_add;
use crate::value::{Type, Value, Vector};

/// Why an operation produced `undef`: messages joined with `"\n\t"` when
/// printed.
// Boxed so an operator's `Result` stays as small as a `Value` on the hot
// path where nothing goes wrong.
#[allow(clippy::box_collection)]
#[derive(Debug, Clone, PartialEq)]
pub struct Why(pub Box<Vec<String>>);

impl Why {
    pub fn new(s: String) -> Self {
        Why(Box::new(vec![s]))
    }

    fn append(mut self, s: String) -> Self {
        self.0.push(s);
        self
    }

    pub fn message(&self) -> String {
        self.0.join("\n\t")
    }
}

pub type OpResult = Result<Value, Why>;

fn undefined_op(a: &Value, op: &str, b: &Value) -> Why {
    Why::new(format!(
        "undefined operation ({} {op} {})",
        a.type_name(),
        b.type_name()
    ))
}

/// Drop the reasons: an element stored inside a vector.
fn elem(r: OpResult) -> Value {
    r.unwrap_or(Value::Undef)
}

// --- equality -------------------------------------------------------------

/// `==`, which is always defined.
pub fn equals(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Undef, Value::Undef) => true,
        (Value::Bool(x), Value::Bool(y)) => x == y,
        (Value::Number(x), Value::Number(y)) => x == y,
        (Value::Str(x), Value::Str(y)) => x.as_bytes() == y.as_bytes(),
        (Value::Vector(x), Value::Vector(y)) => {
            x.len() == y.len() && x.iter().zip(y.iter()).all(|(p, q)| equals(p, q))
        }
        (Value::Range(x), Value::Range(y)) => x.equals(y),
        // Function literals are equal only to themselves (FunctionType
        // compares addresses).
        (Value::Function(x), Value::Function(y)) => Rc::ptr_eq(x, y),
        _ => false,
    }
}

// --- ordering -------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Cmp {
    Less,
    LessEqual,
    Greater,
    GreaterEqual,
}

impl Cmp {
    fn symbol(self) -> &'static str {
        match self {
            Cmp::Less => "<",
            Cmp::LessEqual => "<=",
            Cmp::Greater => ">",
            Cmp::GreaterEqual => ">=",
        }
    }
}

/// `<`, `<=`, `>`, `>=`.
pub fn compare(a: &Value, b: &Value, op: Cmp) -> Result<bool, Why> {
    match (a, b) {
        (Value::Bool(x), Value::Bool(y)) => Ok(match op {
            Cmp::Less => x < y,
            Cmp::LessEqual => x <= y,
            Cmp::Greater => x > y,
            Cmp::GreaterEqual => x >= y,
        }),
        (Value::Number(x), Value::Number(y)) => Ok(match op {
            Cmp::Less => x < y,
            Cmp::LessEqual => x <= y,
            Cmp::Greater => x > y,
            Cmp::GreaterEqual => x >= y,
        }),
        (Value::Str(x), Value::Str(y)) => {
            let (x, y) = (x.as_bytes(), y.as_bytes());
            Ok(match op {
                Cmp::Less => x < y,
                Cmp::LessEqual => x <= y,
                Cmp::Greater => x > y,
                Cmp::GreaterEqual => x >= y,
            })
        }
        (Value::Vector(x), Value::Vector(y)) => match op {
            Cmp::Less => vec_less(x, y),
            Cmp::Greater => vec_less(y, x),
            Cmp::LessEqual => vec_less(y, x).map(|r| !r),
            Cmp::GreaterEqual => vec_less(x, y).map(|r| !r),
        },
        (Value::Range(x), Value::Range(y)) => Ok(match op {
            Cmp::Less => x.less(y, false),
            Cmp::LessEqual => x.less(y, true),
            Cmp::Greater => x.greater(y, false),
            Cmp::GreaterEqual => x.greater(y, true),
        }),
        (Value::Undef, Value::Undef) => Err(Why::new(format!(
            "operation undefined (undefined {} undefined)",
            op.symbol()
        ))),
        (Value::Function(_), Value::Function(_)) => Err(Why::new(format!(
            "operation undefined (function {} function)",
            op.symbol()
        ))),
        _ => Err(undefined_op(a, op.symbol(), b)),
    }
}

/// `VectorType::operator<`: lexicographic, undefined if an element
/// comparison is.
fn vec_less(x: &Vector, y: &Vector) -> Result<bool, Why> {
    for (i, (p, q)) in x.iter().zip(y.iter()).enumerate() {
        match compare(p, q, Cmp::Less) {
            Err(w) => return Err(w.append(format!("in vector comparison at index {i}"))),
            Ok(true) => return Ok(true),
            Ok(false) => {}
        }
        if compare(q, p, Cmp::Less).unwrap_or(false) {
            return Ok(false);
        }
    }
    Ok(x.len() < y.len())
}

// --- arithmetic -----------------------------------------------------------

fn zip_with(x: &Vector, y: &Vector, f: impl Fn(&Value, &Value) -> OpResult) -> Value {
    Value::vector(x.iter().zip(y.iter()).map(|(p, q)| elem(f(p, q))).collect())
}

pub fn add(a: &Value, b: &Value) -> OpResult {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => Ok(Value::Number(x + y)),
        (Value::Vector(x), Value::Vector(y)) => Ok(zip_with(x, y, add)),
        _ => Err(undefined_op(a, "+", b)),
    }
}

pub fn sub(a: &Value, b: &Value) -> OpResult {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => Ok(Value::Number(x - y)),
        (Value::Vector(x), Value::Vector(y)) => Ok(zip_with(x, y, sub)),
        _ => Err(undefined_op(a, "-", b)),
    }
}

/// Vector times number, element by element (`multvecnum`: the element is
/// always the left operand).
fn mul_vec_num(v: &Vector, n: &Value, warn: &mut Vec<String>) -> Value {
    Value::vector(v.iter().map(|e| elem(mul(e, n, warn))).collect())
}

/// Matrix times vector (`multmatvec`).
fn mul_mat_vec(m: &Vector, v: &Vector) -> OpResult {
    let mut out = Vec::with_capacity(m.len());
    for (i, row) in m.iter().enumerate() {
        let row = match row {
            Value::Vector(r) if r.len() == v.len() => r,
            _ => {
                return Err(Why::new(format!(
                    "Matrix must be rectangular. Problem at row {i}"
                )));
            }
        };
        let mut sum = 0.0;
        for (j, e) in row.iter().enumerate() {
            let Value::Number(a) = e else {
                return Err(Why::new(format!(
                    "Matrix must contain only numbers. Problem at row {i}, col {j}"
                )));
            };
            let Value::Number(b) = v[j] else {
                return Err(Why::new(format!(
                    "Vector must contain only numbers. Problem at index {j}"
                )));
            };
            sum = mul_add(*a, b, sum);
        }
        out.push(Value::Number(sum));
    }
    Ok(Value::vector(out))
}

/// Vector times matrix (`multvecmat`). OpenSCAD also logs each failure as
/// a warning without a location; those go to `warn`.
fn mul_vec_mat(v: &Vector, m: &Vector, warn: &mut Vec<String>) -> OpResult {
    let first = m[0].as_vector().map_or(0, |r| r.len());
    let mut out = Vec::with_capacity(first);
    for i in 0..first {
        let mut sum = 0.0;
        for j in 0..v.len() {
            let row = match &m[j] {
                Value::Vector(r) if r.len() == first => r,
                _ => {
                    let s = format!("Matrix must be rectangular. Problem at row {j}");
                    warn.push(s.clone());
                    return Err(Why::new(s));
                }
            };
            let Value::Number(a) = v[j] else {
                let s = format!("Vector must contain only numbers. Problem at index {j}");
                warn.push(s.clone());
                return Err(Why::new(s));
            };
            let Value::Number(b) = row[i] else {
                let s = format!("Matrix must contain only numbers. Problem at row {j}, col {i}");
                warn.push(s.clone());
                return Err(Why::new(s));
            };
            sum = mul_add(a, b, sum);
        }
        out.push(Value::Number(sum));
    }
    Ok(Value::vector(out))
}

/// `*`. `warn` collects the location-less warnings of `multvecmat`.
pub fn mul(a: &Value, b: &Value, warn: &mut Vec<String>) -> OpResult {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => Ok(Value::Number(x * y)),
        (Value::Number(_), Value::Vector(v)) => Ok(mul_vec_num(v, a, warn)),
        (Value::Vector(v), Value::Number(_)) => Ok(mul_vec_num(v, b, warn)),
        (Value::Vector(x), Value::Vector(y)) => mul_vectors(x, y, warn),
        _ => Err(undefined_op(a, "*", b)),
    }
}

fn mul_vectors(x: &Vector, y: &Vector, warn: &mut Vec<String>) -> OpResult {
    if x.is_empty() || y.is_empty() {
        return Err(Why::new(
            "Multiplication is undefined on empty vectors".into(),
        ));
    }
    let (t1, t2) = (x[0].ty(), y[0].ty());
    match (t1, t2) {
        (Type::Number, Type::Number) => {
            if x.len() != y.len() {
                return Err(Why::new(format!(
                    "vector*vector requires matching lengths ({} != {})",
                    x.len(),
                    y.len()
                )));
            }
            let mut r = 0.0;
            for (p, q) in x.iter().zip(y.iter()) {
                match (p, q) {
                    (Value::Number(p), Value::Number(q)) => r = mul_add(*p, *q, r),
                    _ => {
                        return Err(Why::new(format!(
                            "undefined operation ({} * {})",
                            p.type_name(),
                            q.type_name()
                        )));
                    }
                }
            }
            Ok(Value::Number(r))
        }
        (Type::Number, Type::Vector) => {
            if x.len() != y.len() {
                return Err(Why::new(format!(
                    "vector*matrix requires vector length to match matrix row count ({} != {})",
                    x.len(),
                    y.len()
                )));
            }
            mul_vec_mat(x, y, warn)
        }
        (Type::Vector, Type::Number) => {
            let cols = x[0].as_vector().map_or(0, |r| r.len());
            if cols != y.len() {
                return Err(Why::new(format!(
                    "matrix*vector requires matrix column count to match vector length ({} != {})",
                    cols,
                    y.len()
                )));
            }
            mul_mat_vec(x, y)
        }
        (Type::Vector, Type::Vector) => {
            let cols = x[0].as_vector().map_or(0, |r| r.len());
            if cols != y.len() {
                return Err(Why::new(format!(
                    "matrix*matrix requires left operand column count to match right operand row count ({} != {})",
                    cols,
                    y.len()
                )));
            }
            let mut out = Vec::with_capacity(x.len());
            for (i, row) in x.iter().enumerate() {
                let empty = Vector::empty();
                let r = row.as_vector().unwrap_or(&empty);
                if r.len() != y.len() {
                    return Err(Why::new(format!(
                        "matrix*matrix left operand row length does not match right operand row count ({} != {}) at row {i}",
                        r.len(),
                        y.len()
                    )));
                }
                match mul_vec_mat(r, y, warn) {
                    Ok(v) => out.push(v),
                    Err(w) => {
                        return Err(w.append(format!("while processing left operand at row {i}")));
                    }
                }
            }
            Ok(Value::vector(out))
        }
        _ => Err(Why::new(format!(
            "undefined vector*vector multiplication where first elements are types {} and {}",
            t1.name(),
            t2.name()
        ))),
    }
}

pub fn div(a: &Value, b: &Value) -> OpResult {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => Ok(Value::Number(x / y)),
        (Value::Vector(v), Value::Number(_)) => {
            Ok(Value::vector(v.iter().map(|e| elem(div(e, b))).collect()))
        }
        (Value::Number(_), Value::Vector(v)) => {
            Ok(Value::vector(v.iter().map(|e| elem(div(a, e))).collect()))
        }
        _ => Err(undefined_op(a, "/", b)),
    }
}

/// `%` is C's `fmod`.
pub fn rem(a: &Value, b: &Value) -> OpResult {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => Ok(Value::Number(x % y)),
        _ => Err(undefined_op(a, "%", b)),
    }
}

pub fn pow(a: &Value, b: &Value) -> OpResult {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => Ok(Value::Number(x.powf(*y))),
        _ => Err(undefined_op(a, "^", b)),
    }
}

/// `Value::toInt64`: truncate, saturating like the hardware conversion.
fn to_i64(x: f64) -> i64 {
    x.trunc() as i64
}

#[derive(Clone, Copy)]
pub enum Bitwise {
    And,
    Or,
    Shl,
    Shr,
}

pub fn bitwise(a: &Value, b: &Value, op: Bitwise) -> OpResult {
    let sym = match op {
        Bitwise::And => "&",
        Bitwise::Or => "|",
        Bitwise::Shl => "<<",
        Bitwise::Shr => ">>",
    };
    let (Value::Number(x), Value::Number(y)) = (a, b) else {
        return Err(undefined_op(a, sym, b));
    };
    let (l, r) = (to_i64(*x), to_i64(*y));
    let v = match op {
        Bitwise::And => l & r,
        Bitwise::Or => l | r,
        Bitwise::Shl | Bitwise::Shr => {
            if r < 0 {
                return Err(Why::new("negative shift".into()));
            }
            if r >= 64 {
                return Err(Why::new("shift too large".into()));
            }
            if matches!(op, Bitwise::Shl) {
                l << r
            } else {
                l >> r
            }
        }
    };
    Ok(Value::Number(v as f64))
}

pub fn neg(a: &Value) -> OpResult {
    match a {
        Value::Number(x) => Ok(Value::Number(-x)),
        Value::Vector(v) => Ok(Value::vector(v.iter().map(|e| elem(neg(e))).collect())),
        _ => Err(Why::new(format!(
            "undefined operation (-{})",
            a.type_name()
        ))),
    }
}

pub fn bit_not(a: &Value) -> OpResult {
    match a {
        Value::Number(x) => Ok(Value::Number(!to_i64(*x) as f64)),
        _ => Err(Why::new(format!(
            "undefined operation (~{})",
            a.type_name()
        ))),
    }
}

/// `convert_to_uint32`: an index, or `u32::MAX` when it is not a finite
/// number in range (boost::numeric_cast truncates toward zero).
fn to_index(d: f64) -> u32 {
    if d.is_finite() && d > -1.0 && d < 4_294_967_296.0 {
        d as u32
    } else {
        u32::MAX
    }
}

/// `a[i]`. An out-of-range or ill-typed index gives `undef`; its reason is
/// never printed by OpenSCAD, so none is kept.
pub fn index(a: &Value, i: &Value) -> Value {
    let Value::Number(d) = i else {
        return Value::Undef;
    };
    let i = to_index(*d) as usize;
    match a {
        Value::Str(s) => s.char_at(i).map_or(Value::Undef, Value::str),
        Value::Vector(v) => v.get(i).cloned().unwrap_or(Value::Undef),
        Value::Range(r) => match i {
            0 => Value::Number(r.begin),
            1 => Value::Number(r.step),
            2 => Value::Number(r.end),
            _ => Value::Undef,
        },
        _ => Value::Undef,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn n(x: f64) -> Value {
        Value::Number(x)
    }
    fn v(x: &[f64]) -> Value {
        Value::vector(x.iter().map(|&e| n(e)).collect())
    }

    #[test]
    fn vector_arithmetic_truncates_to_shortest() {
        let r = add(&v(&[1.0, 2.0, 3.0]), &v(&[10.0, 20.0])).unwrap();
        assert!(equals(&r, &v(&[11.0, 22.0])));
        let r = add(&n(1.0), &Value::str(b"a")).unwrap_err();
        assert_eq!(r.message(), "undefined operation (number + string)");
    }

    #[test]
    fn matrix_products() {
        let m = Value::vector(vec![v(&[1.0, 2.0]), v(&[3.0, 4.0])]);
        let mut w = Vec::new();
        assert!(equals(
            &mul(&m, &v(&[1.0, 1.0]), &mut w).unwrap(),
            &v(&[3.0, 7.0])
        ));
        assert!(equals(
            &mul(&v(&[1.0, 1.0]), &m, &mut w).unwrap(),
            &v(&[4.0, 6.0])
        ));
        assert!(equals(
            &mul(&v(&[1.0, 2.0]), &v(&[3.0, 4.0]), &mut w).unwrap(),
            &n(11.0)
        ));
        let e = mul(&v(&[1.0]), &v(&[1.0, 2.0]), &mut w).unwrap_err();
        assert_eq!(
            e.message(),
            "vector*vector requires matching lengths (1 != 2)"
        );
        assert!(w.is_empty());
    }

    #[test]
    fn comparisons_and_their_messages() {
        assert_eq!(
            compare(&v(&[1.0, 2.0]), &v(&[1.0, 3.0]), Cmp::Less),
            Ok(true)
        );
        assert_eq!(compare(&v(&[1.0]), &v(&[1.0, 0.0]), Cmp::Less), Ok(true));
        let e = compare(&Value::Undef, &Value::Undef, Cmp::Less).unwrap_err();
        assert_eq!(e.message(), "operation undefined (undefined < undefined)");
        let e = compare(
            &Value::vector(vec![n(1.0), Value::str(b"a")]),
            &v(&[1.0, 2.0]),
            Cmp::Less,
        )
        .unwrap_err();
        assert_eq!(
            e.message(),
            "undefined operation (string < number)\n\tin vector comparison at index 1"
        );
        assert!(equals(&Value::Undef, &Value::Undef));
        assert!(!equals(&n(f64::NAN), &n(f64::NAN)));
    }

    #[test]
    fn bitwise_and_shifts() {
        assert!(equals(
            &bitwise(&n(6.0), &n(3.0), Bitwise::And).unwrap(),
            &n(2.0)
        ));
        assert!(equals(
            &bitwise(&n(1.0), &n(4.0), Bitwise::Shl).unwrap(),
            &n(16.0)
        ));
        assert_eq!(
            bitwise(&n(1.0), &n(-1.0), Bitwise::Shl)
                .unwrap_err()
                .message(),
            "negative shift"
        );
        assert!(equals(&bit_not(&n(0.0)).unwrap(), &n(-1.0)));
    }

    #[test]
    fn indexing() {
        assert!(equals(&index(&v(&[1.0, 2.0]), &n(1.9)), &n(2.0)));
        assert!(index(&v(&[1.0, 2.0]), &n(-1.0)).is_undef());
        assert!(equals(
            &index(&Value::range(1.0, 2.0, 9.0), &n(2.0)),
            &n(9.0)
        ));
    }
}
