//! Number formatting as OpenSCAD prints it.
//!
//! [`fmt_number`] is `DoubleConvert` in src/core/Value.cc: double-conversion's
//! `ToPrecision(value, 6)` with `UNIQUE_ZERO | EMIT_POSITIVE_EXPONENT_SIGN`,
//! at most 5 leading and 0 trailing padding zeros, followed by OpenSCAD's
//! own trimming of trailing zeros. So `1e-6` is written `1e-6`, `0.00001`
//! stays decimal, `1000000` becomes `1e+6` and `-0` prints as `0`. Ties
//! round half up on the exact binary value (`1234565` -> `1.23457e+6`),
//! which differs from Rust's round-half-even formatting.
//!
//! [`fmt_g`] is C++'s default `ostream << double` (`%g`, precision 6), which
//! a few places use instead (customizer comment text, enum keys).

use std::fmt::Write as _;

/// Decimal digits (no leading zero) and the position of the decimal point,
/// such that the value is `0.d1d2d3... * 10^point`.
struct Digits {
    digits: Vec<u8>,
    point: i32,
}

/// Round `v` (finite, non-zero magnitude) to `precision` significant digits,
/// half away from zero on the exact value.
fn round_half_up(v: f64, precision: usize) -> Digits {
    let a = v.abs();
    // 21 significant digits: enough to decide the rounding direction except
    // when the tail looks like an exact tie, which is then checked exactly.
    let s = format!("{a:.20e}");
    let (mant, exp) = s.split_once('e').unwrap_or((&s, "0"));
    let exp: i32 = exp.parse().unwrap_or(0);
    let mut digits: Vec<u8> = mant.bytes().filter(u8::is_ascii_digit).map(|b| b - b'0').collect();
    let round_up = {
        let next = digits[precision];
        let rest_zero = digits[precision + 1..].iter().all(|&d| d == 0);
        if next == 5 && rest_zero {
            // Possibly an exact tie: look at the full exact expansion (a
            // double has at most 767 significant digits).
            let exact = format!("{a:.800e}");
            let ed: Vec<u8> = exact.split('e').next().unwrap_or("").bytes().filter(u8::is_ascii_digit).map(|b| b - b'0').collect();
            ed[precision] >= 5
        } else {
            next >= 5
        }
    };
    digits.truncate(precision);
    let mut point = exp + 1;
    if round_up {
        let mut i = precision;
        loop {
            if i == 0 {
                digits.insert(0, 1);
                digits.truncate(precision);
                point += 1;
                break;
            }
            i -= 1;
            if digits[i] == 9 {
                digits[i] = 0;
            } else {
                digits[i] += 1;
                break;
            }
        }
    }
    Digits { digits, point }
}

/// Format like OpenSCAD's `DoubleConvert` (see the module docs).
pub fn fmt_number(v: f64) -> String {
    let mut s = String::new();
    write_number(&mut s, v);
    s
}

/// Append [`fmt_number`]'s output to `out`.
pub fn write_number(out: &mut String, v: f64) {
    if v.is_nan() {
        out.push_str("nan");
        return;
    }
    if v.is_infinite() {
        out.push_str(if v < 0.0 { "-inf" } else { "inf" });
        return;
    }
    if v == 0.0 {
        // UNIQUE_ZERO: -0 prints as 0.
        out.push('0');
        return;
    }
    // Fast path for integers that print exactly: the common case in models.
    if v.fract() == 0.0 && v.abs() < 1e6 {
        let _ = write!(out, "{}", v as i64);
        return;
    }
    const PRECISION: i32 = 6;
    let d = round_half_up(v, PRECISION as usize);
    if v < 0.0 {
        out.push('-');
    }
    let point = d.point;
    let exponent = point - 1;
    let as_exponential = -point + 1 > 5 || point - PRECISION > 0;
    // Trailing zeros are trimmed afterwards anyway; drop them now.
    let mut digits = d.digits;
    while digits.len() > 1 && digits.last() == Some(&0) {
        digits.pop();
    }
    let digit = |d: u8| (b'0' + d) as char;
    if as_exponential {
        out.push(digit(digits[0]));
        if digits.len() > 1 {
            out.push('.');
            digits[1..].iter().for_each(|&d| out.push(digit(d)));
        }
        out.push('e');
        out.push(if exponent < 0 { '-' } else { '+' });
        let _ = write!(out, "{}", exponent.abs());
    } else if point <= 0 {
        out.push_str("0.");
        (0..-point).for_each(|_| out.push('0'));
        digits.iter().for_each(|&d| out.push(digit(d)));
    } else {
        let p = point as usize;
        for (i, &d) in digits.iter().enumerate() {
            if i == p {
                out.push('.');
            }
            out.push(digit(d));
        }
        (digits.len()..p).for_each(|_| out.push('0'));
    }
}

/// C++ `std::ostream << double` with default flags: `%g` at precision 6.
pub fn fmt_g(v: f64) -> String {
    if v.is_nan() {
        return if v.is_sign_negative() { "-nan".into() } else { "nan".into() };
    }
    if v.is_infinite() {
        return if v < 0.0 { "-inf".into() } else { "inf".into() };
    }
    if v == 0.0 {
        return if v.is_sign_negative() { "-0".into() } else { "0".into() };
    }
    // printf rounds half-to-even on the exact value, as Rust does.
    let e = format!("{v:.5e}");
    let (mant, exp) = e.split_once('e').unwrap_or((&e, "0"));
    let x: i32 = exp.parse().unwrap_or(0);
    let trim = |s: &str| -> String {
        if s.contains('.') { s.trim_end_matches('0').trim_end_matches('.').to_string() } else { s.to_string() }
    };
    if (-4..6).contains(&x) {
        let decimals = (5 - x).max(0) as usize;
        trim(&format!("{v:.decimals$}"))
    } else {
        let sign = if x < 0 { '-' } else { '+' };
        format!("{}e{sign}{:02}", trim(mant), x.abs())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn openscad_number_format() {
        let cases: &[(f64, &str)] = &[
            (0.0, "0"),
            (-0.0, "0"),
            (1.0, "1"),
            (-2.5, "-2.5"),
            (0.1, "0.1"),
            (1.0 / 3.0, "0.333333"),
            (1234565.0, "1.23457e+6"),
            (1234575.0, "1.23458e+6"),
            (12345.65, "12345.6"),
            (1e6, "1e+6"),
            (999999.0, "999999"),
            (9999995.0, "1e+7"),
            (1e21, "1e+21"),
            (1e20, "1e+20"),
            (0.00001, "0.00001"),
            (0.000001, "1e-6"),
            (6e-9, "6e-9"),
            (6.00001e9, "6.00001e+9"),
            (1.2345678901234568e29, "1.23457e+29"),
            (f64::MAX, "1.79769e+308"),
            (2.2250738585072014e-308, "2.22507e-308"),
            (3.5e-10, "3.5e-10"),
            (123456.7, "123457"),
            (0.953674, "0.953674"),
            (6.5432109, "6.54321"),
            (std::f64::consts::PI * 2.0, "6.28319"),
            (f64::INFINITY, "inf"),
            (f64::NEG_INFINITY, "-inf"),
            (f64::NAN, "nan"),
            (100.0, "100"),
            (-1.5e-5, "-0.000015"),
        ];
        for &(v, s) in cases {
            assert_eq!(fmt_number(v), s, "{v:e}");
        }
    }

    #[test]
    fn printf_g_format() {
        assert_eq!(fmt_g(1.0), "1");
        assert_eq!(fmt_g(1.5), "1.5");
        assert_eq!(fmt_g(1234567.0), "1.23457e+06");
        assert_eq!(fmt_g(0.0001), "0.0001");
        assert_eq!(fmt_g(0.00001), "1e-05");
        assert_eq!(fmt_g(100000.0), "100000");
        assert_eq!(fmt_g(-3.25), "-3.25");
    }
}
