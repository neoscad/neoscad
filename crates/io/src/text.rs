//! The text-handling primitives OpenSCAD's readers are built from, with
//! their C++ semantics: `std::getline` and its end-of-file flag,
//! `boost::trim`, `boost::lexical_cast`, and the number formats writers use.

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
    if !v.is_finite() {
        return String::new();
    }
    if v == 0.0 {
        return "0".into();
    }
    // Rust's `{:e}` prints the shortest round-trip digits.
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
}
