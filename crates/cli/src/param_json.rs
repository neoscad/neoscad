//! The `.param` export: customizer parameters as JSON (`export_param.cc`
//! with `ParameterObject::jsonValue`), in the bytes nlohmann::json's
//! `operator<<` writes: compact, object keys in sorted order (its default
//! `std::map`), doubles in shortest round-trip form with a `.0` on whole
//! numbers.

use std::collections::BTreeMap;

use lang::customizer::Parameters;
use lang::customizer::params::{EnumValue, ParamKind};

/// A JSON value, just what the export needs.
enum Json {
    Bool(bool),
    Double(f64),
    Unsigned(usize),
    Str(String),
    Array(Vec<Json>),
    Object(BTreeMap<&'static str, Json>),
}

fn doubles(v: &[f64]) -> Json {
    Json::Array(v.iter().map(|&x| Json::Double(x)).collect())
}

fn enum_value(v: &EnumValue) -> Json {
    match v {
        EnumValue::Number(n) => Json::Double(*n),
        EnumValue::String(s) => Json::Str(String::from_utf8_lossy(s).into_owned()),
    }
}

/// `jsonValue` merged into the name, caption and group.
fn parameter(p: &lang::customizer::params::Parameter) -> Json {
    let mut o = BTreeMap::new();
    o.insert("name", Json::Str(p.name.clone()));
    if !p.description.is_empty() {
        o.insert("caption", Json::Str(p.description.clone()));
    }
    if !p.group.is_empty() {
        o.insert("group", Json::Str(p.group.clone()));
    }
    // `max` decides whether a range is written; a missing minimum is 0 and
    // a missing step 1 (`NumberParameter::jsonValue`,
    // `VectorParameter::jsonValue`).
    let range = |o: &mut BTreeMap<_, _>, min: Option<f64>, max: Option<f64>| {
        if let Some(max) = max {
            o.insert("max", Json::Double(max));
            o.insert("min", Json::Double(min.unwrap_or(0.0)));
        }
    };
    match &p.kind {
        ParamKind::Bool { default, .. } => {
            o.insert("type", Json::Str("boolean".into()));
            o.insert("initial", Json::Bool(*default));
        }
        ParamKind::String {
            default, max_len, ..
        } => {
            o.insert("type", Json::Str("string".into()));
            o.insert(
                "initial",
                Json::Str(String::from_utf8_lossy(default).into_owned()),
            );
            if let Some(n) = max_len {
                o.insert("maxLength", Json::Unsigned(*n));
            }
        }
        ParamKind::Number {
            default,
            min,
            max,
            step,
            ..
        } => {
            o.insert("type", Json::Str("number".into()));
            o.insert("initial", Json::Double(*default));
            range(&mut o, *min, *max);
            o.insert("step", Json::Double(step.unwrap_or(1.0)));
        }
        ParamKind::Vector {
            default,
            min,
            max,
            step,
            ..
        } => {
            o.insert("type", Json::Str("number".into()));
            o.insert("initial", doubles(default));
            if max.is_some() {
                range(&mut o, *min, *max);
                o.insert("step", Json::Double(step.unwrap_or(1.0)));
            }
        }
        ParamKind::Enum { default, items, .. } => {
            let initial = &items[*default].value;
            let kind = match initial {
                EnumValue::Number(_) => "number",
                EnumValue::String(_) => "string",
            };
            o.insert("type", Json::Str(kind.into()));
            o.insert("initial", enum_value(initial));
            let options = items
                .iter()
                .map(|it| {
                    let mut option = BTreeMap::new();
                    option.insert("name", Json::Str(it.key.clone()));
                    option.insert("value", enum_value(&it.value));
                    Json::Object(option)
                })
                .collect();
            o.insert("options", Json::Array(options));
        }
    }
    Json::Object(o)
}

/// The whole `.param` file: `title` (the input's stem) and, when there are
/// any, `parameters`.
pub fn export(params: &Parameters, title: &str) -> String {
    let mut file = BTreeMap::new();
    file.insert("title", Json::Str(title.to_string()));
    if !params.params.is_empty() {
        file.insert(
            "parameters",
            Json::Array(params.params.iter().map(parameter).collect()),
        );
    }
    let mut out = String::new();
    write(&Json::Object(file), &mut out);
    out
}

fn write(v: &Json, out: &mut String) {
    match v {
        Json::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Json::Double(d) => out.push_str(&double(*d)),
        Json::Unsigned(n) => out.push_str(&n.to_string()),
        Json::Str(s) => string(s, out),
        Json::Array(items) => {
            out.push('[');
            for (i, x) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write(x, out);
            }
            out.push(']');
        }
        Json::Object(map) => {
            out.push('{');
            for (i, (k, x)) in map.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                string(k, out);
                out.push(':');
                write(x, out);
            }
            out.push('}');
        }
    }
}

/// nlohmann's string escaping: quotes, backslashes and control characters;
/// other UTF-8 as is.
fn string(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if u32::from(c) < 0x20 => out.push_str(&format!("\\u{:04x}", u32::from(c))),
            c => out.push(c),
        }
    }
    out.push('"');
}

/// nlohmann's `dump_float`: the shortest digits that round-trip, laid out
/// by `format_buffer` with a decimal point while the point falls within
/// the first 15 digits (and down to 4 leading zeros), and
/// scientific notation (at least two exponent digits) outside; whole
/// numbers get `.0`. Non-finite values are `null`.
fn double(v: f64) -> String {
    if !v.is_finite() {
        return "null".into();
    }
    if v == 0.0 {
        return if v.is_sign_negative() { "-0.0" } else { "0.0" }.into();
    }
    // `{:e}` gives the shortest round-trip digits: `d.ddde<exp>`.
    let sci = format!("{:e}", v.abs());
    let (mantissa, exp) = sci.split_once('e').expect("{:e} has an exponent");
    let digits: String = mantissa.chars().filter(char::is_ascii_digit).collect();
    let k = digits.len() as i32;
    // Position of the decimal point after the first `n` digits.
    let n = exp.parse::<i32>().expect("{:e} exponent") + 1;
    let sign = if v < 0.0 { "-" } else { "" };
    let body = if k <= n && n <= 15 {
        format!("{digits}{}.0", "0".repeat((n - k) as usize))
    } else if 0 < n && n <= 15 {
        format!("{}.{}", &digits[..n as usize], &digits[n as usize..])
    } else if -4 < n && n <= 0 {
        format!("0.{}{digits}", "0".repeat((-n) as usize))
    } else {
        let e = n - 1;
        let m = if k == 1 {
            digits.clone()
        } else {
            format!("{}.{}", &digits[..1], &digits[1..])
        };
        let es = if e < 0 { '-' } else { '+' };
        format!("{m}e{es}{:02}", e.abs())
    };
    format!("{sign}{body}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn doubles_print_like_nlohmann() {
        assert_eq!(double(0.0), "0.0");
        assert_eq!(double(5.0), "5.0");
        assert_eq!(double(-10.0), "-10.0");
        assert_eq!(double(0.1111119), "0.1111119");
        assert_eq!(double(5.5), "5.5");
        assert_eq!(double(0.0001), "0.0001");
        assert_eq!(double(0.00001), "1e-05");
        assert_eq!(double(1e14), "100000000000000.0");
        assert_eq!(double(1e15), "1e+15");
        assert_eq!(double(1e16), "1e+16");
        assert_eq!(double(1.5e20), "1.5e+20");
        assert_eq!(double(f64::NAN), "null");
    }

    #[test]
    fn strings_escape_controls_only() {
        let mut s = String::new();
        string("a\"b\\c\n\u{1}☠", &mut s);
        assert_eq!(s, "\"a\\\"b\\\\c\\n\\u0001☠\"");
    }
}
