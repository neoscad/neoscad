//! Fontconfig font names (`"Liberation Sans:style=Bold"`), parsed as
//! fontconfig's `FcNameParse` does and matched the way `FcFontMatch`
//! ranks fonts, reduced to the properties a font file can tell us.
//!
//! OpenSCAD hands the `font` string to fontconfig unchanged
//! (`FontCache::find_face_fontconfig`), so these names are part of the
//! language. What is reproduced (fontconfig `src/fcname.c`, `fcmatch.c`,
//! `fcdefault.c`, and the configuration files the nightly ships in
//! `Resources/fonts`):
//!
//! - the grammar: families separated by `,`, an optional `-size`, then
//!   `:element=value[,value]` pairs or bare constants (`:bold`), with `\`
//!   escaping; a value that does not convert (a `charset` that is not hex,
//!   a `weight` that is neither a number nor a weight name) makes the whole
//!   name unparseable, which OpenSCAD reports as "Could not parse font";
//! - the ranking, in fontconfig's priority order: charset coverage, family
//!   (ignoring case and blanks), style name (ignoring case), slant, weight,
//!   width, then the order fonts were added;
//! - the defaults `FcDefaultSubstitute` adds (regular weight, roman slant,
//!   normal width), and the family substitutions of the bundled
//!   configuration: the metric aliases (`Arial` -> `Liberation Sans`,
//!   `30-metric-aliases.conf`), the generic families (`10-liberation.conf`)
//!   and the `sans-serif` fallback every name gets (`49-sansserif.conf`),
//!   which is why an unknown family renders in Liberation Sans.
//!
//! Language coverage (`FC_LANG`) is not modelled: it ranks below a strong
//! family match, and every name here ends with a family that matches.

/// A parsed font name.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Pattern {
    pub families: Vec<String>,
    pub styles: Vec<String>,
    /// Fontconfig weights (`FC_WEIGHT_*`, regular = 80, bold = 200).
    pub weight: Option<f64>,
    /// `FC_SLANT_*`: roman 0, italic 100, oblique 110.
    pub slant: Option<f64>,
    /// `FC_WIDTH_*`: normal 100.
    pub width: Option<f64>,
    /// Each `charset=` value: inclusive code point ranges.
    pub charsets: Vec<Vec<(u32, u32)>>,
    /// `fontfeatures=` values, as HarfBuzz feature strings.
    pub features: Vec<String>,
}

/// The type fontconfig converts an element's value to (`FcNameConvert`).
#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Str,
    Int,
    Double,
    Range,
    Bool,
    CharSet,
    LangSet,
    Matrix,
}

/// Fontconfig's object table (`fcobjs.h`), by name. Elements not listed
/// are unknown to fontconfig and their values are dropped.
const OBJECTS: &[(&str, Kind)] = &[
    ("family", Kind::Str),
    ("familylang", Kind::Str),
    ("style", Kind::Str),
    ("stylelang", Kind::Str),
    ("fullname", Kind::Str),
    ("fullnamelang", Kind::Str),
    ("slant", Kind::Int),
    ("weight", Kind::Range),
    ("width", Kind::Range),
    ("size", Kind::Range),
    ("aspect", Kind::Double),
    ("pixelsize", Kind::Double),
    ("spacing", Kind::Int),
    ("foundry", Kind::Str),
    ("antialias", Kind::Bool),
    ("hintstyle", Kind::Int),
    ("hinting", Kind::Bool),
    ("verticallayout", Kind::Bool),
    ("autohint", Kind::Bool),
    ("globaladvance", Kind::Bool),
    ("file", Kind::Str),
    ("index", Kind::Int),
    ("rasterizer", Kind::Str),
    ("outline", Kind::Bool),
    ("scalable", Kind::Bool),
    ("dpi", Kind::Double),
    ("rgba", Kind::Int),
    ("scale", Kind::Double),
    ("minspace", Kind::Bool),
    ("charwidth", Kind::Int),
    ("charheight", Kind::Int),
    ("matrix", Kind::Matrix),
    ("charset", Kind::CharSet),
    ("lang", Kind::LangSet),
    ("fontversion", Kind::Int),
    ("capability", Kind::Str),
    ("fontformat", Kind::Str),
    ("embolden", Kind::Bool),
    ("embeddedbitmap", Kind::Bool),
    ("decorative", Kind::Bool),
    ("lcdfilter", Kind::Int),
    ("namelang", Kind::Str),
    ("fontfeatures", Kind::Str),
    ("prgname", Kind::Str),
    ("hash", Kind::Str),
    ("postscriptname", Kind::Str),
    ("color", Kind::Bool),
    ("symbol", Kind::Bool),
    ("fontvariations", Kind::Str),
    ("variable", Kind::Bool),
    ("fonthashint", Kind::Bool),
    ("order", Kind::Int),
    ("desktop", Kind::Str),
    ("namedinstance", Kind::Bool),
    ("fontwrapper", Kind::Str),
];

/// Fontconfig's named constants (`_FcBaseConstants` in `fcname.c`) for the
/// objects this matcher uses: `(name, object, value)`.
const CONSTANTS: &[(&str, &str, f64)] = &[
    ("thin", "weight", 0.0),
    ("extralight", "weight", 40.0),
    ("ultralight", "weight", 40.0),
    ("demilight", "weight", 55.0),
    ("semilight", "weight", 55.0),
    ("light", "weight", 50.0),
    ("book", "weight", 75.0),
    ("regular", "weight", 80.0),
    ("normal", "weight", 80.0),
    ("medium", "weight", 100.0),
    ("demibold", "weight", 180.0),
    ("semibold", "weight", 180.0),
    ("bold", "weight", 200.0),
    ("extrabold", "weight", 205.0),
    ("ultrabold", "weight", 205.0),
    ("black", "weight", 210.0),
    ("heavy", "weight", 210.0),
    ("extrablack", "weight", 215.0),
    ("ultrablack", "weight", 215.0),
    ("roman", "slant", 0.0),
    ("italic", "slant", 100.0),
    ("oblique", "slant", 110.0),
    ("ultracondensed", "width", 50.0),
    ("extracondensed", "width", 63.0),
    ("condensed", "width", 75.0),
    ("semicondensed", "width", 87.0),
    ("semiexpanded", "width", 113.0),
    ("expanded", "width", 125.0),
    ("extraexpanded", "width", 150.0),
    ("ultraexpanded", "width", 200.0),
    ("proportional", "spacing", 0.0),
    ("dual", "spacing", 90.0),
    ("mono", "spacing", 100.0),
    ("charcell", "spacing", 110.0),
];

fn constant(name: &str) -> Option<(&'static str, f64)> {
    CONSTANTS
        .iter()
        .find(|(n, _, _)| n.eq_ignore_ascii_case(name))
        .map(|&(_, o, v)| (o, v))
}

/// `FcNameFindNext`: skip leading white space, then take characters up to
/// one of `delims`, resolving `\` escapes. Returns the token, the delimiter
/// that ended it (`None` at the end) and the rest after it.
fn find_next<'a>(s: &'a str, delims: &[char]) -> (String, Option<char>, &'a str) {
    let s = s.trim_start_matches(|c: char| c.is_ascii_whitespace());
    let mut out = String::new();
    let mut it = s.char_indices();
    while let Some((i, c)) = it.next() {
        if c == '\\' {
            match it.next() {
                Some((_, e)) => out.push(e),
                None => return (out, None, ""),
            }
        } else if delims.contains(&c) {
            return (out, Some(c), &s[i + c.len_utf8()..]);
        } else {
            out.push(c);
        }
    }
    (out, None, "")
}

/// C `strtod` over the whole string: `Some` only if every character was
/// used, the rule `FcNameConvert` applies to ranges.
fn full_number(s: &str) -> Option<f64> {
    s.trim_start().parse::<f64>().ok()
}

/// `FcNameParseCharSet`: hex code points and `a-b` ranges separated by
/// white space.
fn parse_charset(s: &str) -> Option<Vec<(u32, u32)>> {
    let mut out = Vec::new();
    let mut rest = s;
    loop {
        rest = rest.trim_start();
        if rest.is_empty() {
            return Some(out);
        }
        let hex = |r: &str| -> Option<(u32, usize)> {
            let n = r.bytes().take_while(u8::is_ascii_hexdigit).count();
            if n == 0 {
                return None;
            }
            u32::from_str_radix(&r[..n], 16).ok().map(|v| (v, n))
        };
        let (first, n) = hex(rest)?;
        rest = &rest[n..];
        let mut last = first;
        if let Some(r) = rest.strip_prefix('-') {
            let (l, n) = hex(r)?;
            last = l;
            rest = &r[n..];
        }
        out.push((first, last));
    }
}

/// `FcNameParse`. `None` when a value cannot be converted, which makes
/// fontconfig reject the whole name.
pub fn parse(name: &str) -> Option<Pattern> {
    let mut p = Pattern::default();
    let mut rest = name;
    let mut delim;
    loop {
        let (tok, d, r) = find_next(rest, &['-', ',', ':']);
        rest = r;
        delim = d;
        if !tok.is_empty() {
            p.families.push(tok);
        }
        if delim != Some(',') {
            break;
        }
    }
    if delim == Some('-') {
        // Point sizes: parsed and ignored, since the size comes from
        // `text(size=...)`.
        loop {
            let (_, d, r) = find_next(rest, &['-', ',', ':']);
            rest = r;
            delim = d;
            if delim != Some(',') {
                break;
            }
        }
    }
    while delim == Some(':') {
        let (tok, d, r) = find_next(rest, &['=', '_', ':']);
        rest = r;
        delim = d;
        if tok.is_empty() {
            continue;
        }
        if matches!(delim, Some('=') | Some('_')) {
            let object = OBJECTS
                .iter()
                .find(|(n, _)| n.eq_ignore_ascii_case(&tok))
                .map(|&(n, k)| (n, k));
            loop {
                let (val, d, r) = find_next(rest, &[':', ',']);
                rest = r;
                delim = d;
                if let Some((obj, kind)) = object {
                    add_value(&mut p, obj, kind, &val)?;
                }
                if delim != Some(',') {
                    break;
                }
            }
        } else if let Some((obj, v)) = constant(&tok) {
            set_number(&mut p, obj, v);
        }
    }
    Some(p)
}

fn set_number(p: &mut Pattern, obj: &str, v: f64) {
    // Only the first value of an element takes part in matching here;
    // fontconfig would rank later ones after it.
    let slot = match obj {
        "weight" => &mut p.weight,
        "slant" => &mut p.slant,
        "width" => &mut p.width,
        _ => return,
    };
    slot.get_or_insert(v);
}

fn add_value(p: &mut Pattern, obj: &str, kind: Kind, val: &str) -> Option<()> {
    match kind {
        Kind::Str => match obj {
            "family" => p.families.push(val.to_string()),
            "style" => p.styles.push(val.to_string()),
            "fontfeatures" => p.features.push(val.to_string()),
            _ => {}
        },
        Kind::Int => {
            let v = match constant(val) {
                Some((o, v)) if o == obj => v,
                // `atoi`: leading digits, 0 when there are none.
                _ => {
                    let t = val.trim_start();
                    let n = t
                        .char_indices()
                        .take_while(|&(i, c)| c.is_ascii_digit() || (i == 0 && c == '-'))
                        .count();
                    t[..n].parse::<i64>().unwrap_or(0) as f64
                }
            };
            set_number(p, obj, v);
        }
        Kind::Range => {
            let v = match constant(val) {
                Some((o, v)) if o == obj => v,
                _ => {
                    if let Some(r) = val.strip_prefix('[') {
                        // `[begin end]`: matched by its midpoint here.
                        let r = r.trim_end_matches(']');
                        let mut it = r
                            .split_whitespace()
                            .map(|x| full_number(x).or_else(|| constant(x).map(|(_, v)| v)));
                        let (a, b) = (it.next()??, it.next()??);
                        (a + b) / 2.0
                    } else {
                        full_number(val)?
                    }
                }
            };
            set_number(p, obj, v);
        }
        Kind::CharSet => p.charsets.push(parse_charset(val)?),
        Kind::Double | Kind::Bool | Kind::LangSet | Kind::Matrix => {}
    }
    Some(())
}

/// Blanks and case do not matter in family names (`FcCompareFamily`,
/// `FcStrCmpIgnoreBlanksAndCase`).
pub fn family_key(s: &str) -> String {
    s.chars()
        .filter(|c| *c != ' ')
        .flat_map(char::to_lowercase)
        .collect()
}

/// The family list after the bundled configuration's substitutions, in
/// preference order: each requested family followed by its metric-
/// compatible Liberation font, then `sans-serif`'s preferred family.
pub fn family_candidates(p: &Pattern) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let push = |s: &str, out: &mut Vec<String>| {
        let k = family_key(s);
        if !out.contains(&k) {
            out.push(k);
        }
    };
    let mut generic = false;
    for f in &p.families {
        let k = family_key(f);
        let alias = match k.as_str() {
            "sans-serif" | "sans" | "sansserif" => {
                generic = true;
                Some("Liberation Sans")
            }
            "serif" => {
                generic = true;
                Some("Liberation Serif")
            }
            "monospace" | "mono" => {
                generic = true;
                Some("Liberation Mono")
            }
            "arial" | "arimo" | "helvetica" | "albany" | "albanyamt" => Some("Liberation Sans"),
            "arialnarrow" | "helveticanarrow" => Some("Liberation Sans Narrow"),
            "timesnewroman" | "tinos" | "times" | "thorndale" | "thorndaleamt" => {
                Some("Liberation Serif")
            }
            "couriernew" | "cousine" | "courier" | "cumberland" | "cumberlandamt" => {
                Some("Liberation Mono")
            }
            _ => None,
        };
        push(f, &mut out);
        if let Some(a) = alias {
            push(a, &mut out);
        }
    }
    if !generic {
        push("Liberation Sans", &mut out);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        let p = parse("Liberation Sans:style=Bold").unwrap();
        assert_eq!(p.families, ["Liberation Sans"]);
        assert_eq!(p.styles, ["Bold"]);
        let p = parse("Liberation Sans:bold:italic").unwrap();
        assert_eq!((p.weight, p.slant), (Some(200.0), Some(100.0)));
        let p = parse("A,B-12:weight=180").unwrap();
        assert_eq!(p.families, ["A", "B"]);
        assert_eq!(p.weight, Some(180.0));
        let p = parse("Liberation Sans:charset=76,78").unwrap();
        assert_eq!(p.charsets, [vec![(0x76, 0x76)], vec![(0x78, 0x78)]]);
        let p = parse("Amiri:style=Regular:fontfeatures=+liga").unwrap();
        assert_eq!(p.features, ["+liga"]);
        assert_eq!(parse(":charset=xxx"), None);
        assert_eq!(parse("A:weight=heavyish"), None);
        let p = parse(r"Foo\:Bar:unknown=1").unwrap();
        assert_eq!(p.families, ["Foo:Bar"]);
        assert!(parse("").unwrap().families.is_empty());
    }

    #[test]
    fn families() {
        let c = family_candidates(&parse("Arial").unwrap());
        assert_eq!(c, ["arial", "liberationsans"]);
        let c = family_candidates(&parse("serif").unwrap());
        assert_eq!(c, ["serif", "liberationserif"]);
        let c = family_candidates(&parse(":bold").unwrap());
        assert_eq!(c, ["liberationsans"]);
    }
}
