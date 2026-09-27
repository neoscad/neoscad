//! The text-handling primitives OpenSCAD's readers are built from, with
//! their C++ semantics: `std::getline` and its end-of-file flag,
//! `boost::trim`, `boost::lexical_cast`, and the number formats writers use.

use std::fmt::Write;

pub use lang::number::fmt_g;

/// `std::getline` over a byte buffer, with the stream's `eof()` flag.
///
/// The flag matters because the readers loop on `while (!f.eof())`: a file
/// ending in a newline yields one more, empty, line before the flag is set,
/// and one without a final newline sets it on its last line.
#[derive(Debug)]
pub struct Lines<'a> {
    data: &'a [u8],
    pos: usize,
    pub eof: bool,
}

impl<'a> Lines<'a> {
    pub fn new(data: &'a [u8]) -> Lines<'a> {
        Lines {
            data,
            pos: 0,
            eof: false,
        }
    }

    /// The next line without its `\n` (a `\r` stays, as in C++).
    pub fn next_raw(&mut self) -> &'a [u8] {
        if self.pos >= self.data.len() {
            self.eof = true;
            return &[];
        }
        let rest = &self.data[self.pos..];
        match rest.iter().position(|&b| b == b'\n') {
            Some(i) => {
                self.pos += i + 1;
                &rest[..i]
            }
            None => {
                self.pos = self.data.len();
                self.eof = true;
                rest
            }
        }
    }

    /// The next line as text (bytes that are not UTF-8 become U+FFFD; the
    /// readers only compare ASCII keywords and numbers).
    pub fn next_line(&mut self) -> String {
        String::from_utf8_lossy(self.next_raw()).into_owned()
    }
}

/// `std::isspace` in the C locale.
pub fn is_space(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\n' | '\x0b' | '\x0c' | '\r')
}

/// `boost::trim`.
pub fn trim(s: &str) -> &str {
    s.trim_matches(is_space)
}

/// `boost::lexical_cast<double>`: the whole string must be a number
/// (`inf`, `infinity` and `nan` in any case, with a sign, are accepted).
/// A value out of range fails, as the stream conversion underneath reports
/// `ERANGE` for it: one that overflows to infinity, and one that underflows
/// to a subnormal or to zero (the nightly rejects `2.22507e-308`, which
/// OpenSCAD's own DXF export writes).
pub fn parse_f64(s: &str) -> Option<f64> {
    if s.is_empty() || s.starts_with(is_space) || s.ends_with(is_space) {
        return None;
    }
    let v = s.parse::<f64>().ok()?;
    let literal = s.trim_start_matches(['+', '-']).to_ascii_lowercase();
    if v.is_infinite() && !literal.starts_with("inf") {
        return None;
    }
    if v.is_subnormal() {
        return None;
    }
    if v == 0.0 && !literal.starts_with("nan") {
        let mantissa = literal.split('e').next().unwrap_or("");
        if mantissa.bytes().any(|b| (b'1'..=b'9').contains(&b)) {
            return None;
        }
    }
    Some(v)
}

/// `boost::lexical_cast<int>`.
pub fn parse_i32(s: &str) -> Option<i32> {
    s.parse::<i32>().ok()
}

/// `boost::lexical_cast<unsigned long>`. Boost accepts a leading minus
/// and wraps it, as `strtoul` does.
pub fn parse_u64(s: &str) -> Option<u64> {
    match s.strip_prefix('-') {
        Some(rest) if !rest.starts_with(['-', '+']) => {
            rest.parse::<u64>().ok().map(|v| v.wrapping_neg())
        }
        _ => s.parse::<u64>().ok(),
    }
}

/// `QuotedString`'s `operator<<`: the text in double quotes with `\t`,
/// `\n`, `\r`, `"` and `\` escaped.
pub fn quoted(s: &str) -> String {
    let mut out = Vec::new();
    lang::dump::quoted(&mut out, s.as_bytes());
    String::from_utf8_lossy(&out).into_owned()
}

/// double-conversion `ToShortest` as `export_stl.cc:52-61` configures it:
/// shortest round-trip digits, decimal notation for exponents -6..=20,
/// otherwise `1.5e-7` / `1e21` (no `+`), `-0` printed as `0`, and nothing at
/// all for infinities and NaN (the converter has no symbols for them).
pub fn shortest(v: f64) -> String {
    let mut out = Vec::new();
    write_shortest(&mut out, v);
    // Only ASCII digits, `-`, `.` and `e` are ever written.
    String::from_utf8(out).unwrap_or_default()
}

/// Append [`shortest`]'s output to `out`, without allocating.
///
/// This is the ASCII STL writer's inner loop: a million-triangle export
/// formats twelve million numbers, and a `String` per number (plus the
/// intermediate `format!` and digit `collect`) was a third of the export.
pub fn write_shortest(out: &mut Vec<u8>, v: f64) {
    if !v.is_finite() {
        return;
    }
    if v == 0.0 {
        out.push(b'0');
        return;
    }
    // Rust's `{:e}` prints the shortest round-trip digits, `d[.ddd]e<exp>`.
    let mut buf = StackBuf::new();
    let _ = write!(buf, "{:e}", v.abs());
    let (digits, exp) = sci_parts(buf.as_bytes());
    let d = digits.as_slice();
    let n = d.len() as i32;
    if v < 0.0 {
        out.push(b'-');
    }
    if (-6..21).contains(&exp) {
        if exp < 0 {
            out.extend_from_slice(b"0.");
            out.resize(out.len() + (-exp - 1) as usize, b'0');
            out.extend_from_slice(d);
        } else if exp >= n - 1 {
            out.extend_from_slice(d);
            out.resize(out.len() + (exp - (n - 1)) as usize, b'0');
        } else {
            let p = (exp + 1) as usize;
            out.extend_from_slice(&d[..p]);
            out.push(b'.');
            out.extend_from_slice(&d[p..]);
        }
    } else {
        out.push(d[0]);
        if n > 1 {
            out.push(b'.');
            out.extend_from_slice(&d[1..]);
        }
        out.push(b'e');
        write_int(out, exp);
    }
}

/// Append [`fmt_g`]'s output (`%g` at precision 6, as a C++ stream prints
/// a double) to `out`, without allocating. The OFF, OBJ and WRL writers
/// print every vertex this way; `write_g_matches_fmt_g` pins the two
/// together, so `fmt_g` stays the one definition of the format.
pub fn write_g(out: &mut Vec<u8>, v: f64) {
    if !v.is_finite() || v == 0.0 {
        // Rare: `nan`, `inf` and the signed zeros.
        out.extend_from_slice(fmt_g(v).as_bytes());
        return;
    }
    // `%g` takes its exponent X from the `%e` conversion at precision 5,
    // then prints fixed with 5 - X decimals. Both round the exact value at
    // the same decimal place (half to even, in Rust as in printf), so the
    // fixed form's digits are the `%e` digits: format once and lay them
    // out, rather than formatting a second time as `fmt_g` does.
    let mut buf = StackBuf::new();
    let _ = write!(buf, "{:.5e}", v.abs());
    let (digits, x) = sci_parts(buf.as_bytes());
    // `%g` drops trailing zeros (and then a bare point).
    let mut d = digits.as_slice();
    while let [rest @ .., b'0'] = d
        && !rest.is_empty()
    {
        d = rest;
    }
    if v < 0.0 {
        out.push(b'-');
    }
    if (-4..6).contains(&x) {
        if x < 0 {
            out.extend_from_slice(b"0.");
            out.resize(out.len() + (-x - 1) as usize, b'0');
            out.extend_from_slice(d);
        } else {
            let int = x as usize + 1;
            if d.len() <= int {
                out.extend_from_slice(d);
                out.resize(out.len() + (int - d.len()), b'0');
            } else {
                out.extend_from_slice(&d[..int]);
                out.push(b'.');
                out.extend_from_slice(&d[int..]);
            }
        }
    } else {
        out.push(d[0]);
        if d.len() > 1 {
            out.push(b'.');
            out.extend_from_slice(&d[1..]);
        }
        out.push(b'e');
        out.push(if x < 0 { b'-' } else { b'+' });
        let _ = write!(ByteSink(out), "{:02}", x.unsigned_abs());
    }
}

/// Append an integer's decimal form to `out` (face indices, counts and
/// colour bytes in the mesh writers), with no `String` in between.
pub fn write_int(out: &mut Vec<u8>, v: impl std::fmt::Display) {
    let _ = write!(ByteSink(out), "{v}");
}

/// `fmt::Write` straight into a byte vector: `core::fmt` hands over the
/// text in pieces, so nothing is formatted into a temporary `String`.
struct ByteSink<'a>(&'a mut Vec<u8>);

impl Write for ByteSink<'_> {
    fn write_str(&mut self, s: &str) -> std::fmt::Result {
        self.0.extend_from_slice(s.as_bytes());
        Ok(())
    }
}

/// A buffer on the stack for one formatted number. The longest `{:e}` of a
/// positive double is 17 digits, a point and `e-308`: 23 bytes.
struct StackBuf {
    buf: [u8; 32],
    len: usize,
}

impl StackBuf {
    fn new() -> StackBuf {
        StackBuf {
            buf: [0; 32],
            len: 0,
        }
    }

    fn as_bytes(&self) -> &[u8] {
        &self.buf[..self.len]
    }
}

impl Write for StackBuf {
    fn write_str(&mut self, s: &str) -> std::fmt::Result {
        let end = self.len + s.len();
        let dst = self.buf.get_mut(self.len..end).ok_or(std::fmt::Error)?;
        dst.copy_from_slice(s.as_bytes());
        self.len = end;
        Ok(())
    }
}

/// The significant digits of a double (at most 17), as ASCII.
struct Digits {
    buf: [u8; 17],
    len: usize,
}

impl Digits {
    fn as_slice(&self) -> &[u8] {
        &self.buf[..self.len]
    }
}

/// Split a positive `{:e}` or `{:.Ne}` rendering, `d[.ddd]e<exp>`, into
/// its digits (the point removed) and its exponent.
fn sci_parts(s: &[u8]) -> (Digits, i32) {
    let e = s.iter().position(|&b| b == b'e').unwrap_or(s.len());
    let mut digits = Digits {
        buf: [0; 17],
        len: 0,
    };
    for &b in s[..e].iter().filter(|b| b.is_ascii_digit()) {
        if let Some(slot) = digits.buf.get_mut(digits.len) {
            *slot = b;
            digits.len += 1;
        }
    }
    let exp = s
        .get(e + 1..)
        .and_then(|x| std::str::from_utf8(x).ok())
        .and_then(|x| x.parse().ok())
        .unwrap_or(0);
    (digits, exp)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn getline_sets_eof_like_cpp() {
        let mut l = Lines::new(b"a\nb\n");
        assert_eq!(l.next_line(), "a");
        assert_eq!(l.next_line(), "b");
        assert!(!l.eof);
        assert_eq!(l.next_line(), "");
        assert!(l.eof);
        let mut l = Lines::new(b"a\nb");
        l.next_line();
        assert_eq!(l.next_line(), "b");
        assert!(l.eof);
    }

    #[test]
    fn lexical_cast_is_strict() {
        assert_eq!(parse_f64("1e5"), Some(1e5));
        assert_eq!(parse_f64("-INF"), Some(f64::NEG_INFINITY));
        assert_eq!(parse_f64(" 1"), None);
        assert_eq!(parse_f64("1x"), None);
        assert_eq!(parse_f64(""), None);
        assert_eq!(parse_u64("-1"), Some(u64::MAX));
        assert_eq!(parse_f64("2.22507e-308"), None);
        assert_eq!(parse_f64("1e400"), None);
        assert_eq!(parse_f64("1e-400"), None);
        assert_eq!(parse_f64("0.000"), Some(0.0));
    }

    #[test]
    fn shortest_matches_double_conversion() {
        let cases: &[(f64, &str)] = &[
            (0.0, "0"),
            (-0.0, "0"),
            (1.0, "1"),
            (-2.5, "-2.5"),
            (0.1, "0.1"),
            (1e-6, "0.000001"),
            (1.5e-7, "1.5e-7"),
            (123456.0, "123456"),
            (1e20, "100000000000000000000"),
            (1e21, "1e21"),
            (0.8660254037844386, "0.8660254037844386"),
            (f64::NAN, ""),
        ];
        for &(v, s) in cases {
            assert_eq!(shortest(v), s, "{v:e}");
        }
    }

    /// Doubles over the whole range, with the rounding edges: halfway
    /// cases at 6 digits, values that round up into the next decade, the
    /// `%g` and `ToShortest` notation boundaries, and random bit patterns.
    fn samples() -> Vec<f64> {
        let mut v = vec![
            0.0,
            -0.0,
            f64::NAN,
            -f64::NAN,
            f64::INFINITY,
            f64::NEG_INFINITY,
            f64::MAX,
            f64::MIN_POSITIVE,
            5e-324,
            1234565.0,
            1234575.0,
            12345.65,
            999999.0,
            999999.5,
            9999995.0,
            0.000099999995,
            0.0001,
            0.00001,
            1e-6,
            1.5e-7,
            1e20,
            1e21,
            123456.7,
            0.8660254037844386,
        ];
        for k in -30..30 {
            let p = 10f64.powi(k);
            v.extend([p, -p, p * 9.999995, p * 9.9999949, p * 1.000005, p / 3.0]);
        }
        // A fixed xorshift, so the run is reproducible.
        let mut x: u64 = 0x9E37_79B9_7F4A_7C15;
        for _ in 0..50_000 {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            v.push(f64::from_bits(x));
            // Model-sized coordinates with short decimals, as meshes have.
            v.push((x % 2_000_001) as f64 / 1000.0 - 1000.0);
            v.push(((x >> 11) as f64 / (1u64 << 53) as f64 - 0.5) * 200.0);
        }
        v
    }

    /// The `format!`-based `shortest` this module had before
    /// `write_shortest`, kept as the reference for its layout.
    fn shortest_reference(v: f64) -> String {
        if !v.is_finite() {
            return String::new();
        }
        if v == 0.0 {
            return "0".into();
        }
        let e = format!("{:e}", v.abs());
        let (mant, exp) = e.split_once('e').unwrap_or((&e, "0"));
        let exp: i32 = exp.parse().unwrap_or(0);
        let digits: String = mant.chars().filter(|c| c.is_ascii_digit()).collect();
        let n = digits.len() as i32;
        let mut out = String::new();
        if v < 0.0 {
            out.push('-');
        }
        if (-6..21).contains(&exp) {
            if exp < 0 {
                out.push_str("0.");
                (0..(-exp - 1)).for_each(|_| out.push('0'));
                out.push_str(&digits);
            } else if exp >= n - 1 {
                out.push_str(&digits);
                (0..(exp - (n - 1))).for_each(|_| out.push('0'));
            } else {
                let p = (exp + 1) as usize;
                out.push_str(&digits[..p]);
                out.push('.');
                out.push_str(&digits[p..]);
            }
        } else {
            out.push_str(&digits[..1]);
            if n > 1 {
                out.push('.');
                out.push_str(&digits[1..]);
            }
            out.push_str(&format!("e{exp}"));
        }
        out
    }

    #[test]
    fn write_shortest_matches_the_format_based_layout() {
        for v in samples() {
            assert_eq!(shortest(v), shortest_reference(v), "{v:e}");
        }
    }

    #[test]
    fn write_g_matches_fmt_g() {
        let mut out = Vec::new();
        for v in samples() {
            out.clear();
            write_g(&mut out, v);
            assert_eq!(String::from_utf8_lossy(&out), fmt_g(v), "{v:e}");
        }
    }
}
