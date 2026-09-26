//! The script and direction OpenSCAD resolves for `text()` at
//! instantiation (`FreetypeRenderer::Params::detect_properties`), before any
//! font is loaded.
//!
//! The `.csg` dump prints the resolved values (`script = "Latn"`,
//! `direction = "rtl"`), and the geometry phase shapes with them, so both
//! need the same answer. OpenSCAD gets it from HarfBuzz's `hb_script_*` and
//! `hb_direction_*` helpers; this ports their behaviour (HarfBuzz
//! `src/hb-common.cc`: `hb_tag_from_string`, `hb_script_from_iso15924_tag`,
//! `hb_script_get_horizontal_direction`, `hb_direction_from_string`) and
//! takes each character's Unicode `Script` property from `unicode-script`,
//! which is what `hb_unicode_script` reports.

use unicode_script::{Script, UnicodeScript};

use crate::node::Text;

/// A HarfBuzz script: an ISO 15924 tag as a big-endian `u32`.
type Tag = u32;

const fn tag(s: &[u8; 4]) -> Tag {
    u32::from_be_bytes(*s)
}

/// `HB_SCRIPT_INVALID` (`HB_TAG_NONE`).
const INVALID: Tag = 0;
/// `HB_SCRIPT_UNKNOWN`.
const UNKNOWN: Tag = tag(b"Zzzz");
const COMMON: Tag = tag(b"Zyyy");
const INHERITED: Tag = tag(b"Zinh");

/// `hb_tag_from_string`: the first four bytes (up to a NUL), padded with
/// spaces; an empty string is `HB_TAG_NONE`.
fn tag_from_string(s: &str) -> Tag {
    let b = s.as_bytes();
    if b.first().is_none_or(|&c| c == 0) {
        return INVALID;
    }
    let mut t = [b' '; 4];
    for (slot, &c) in t.iter_mut().zip(b.iter().take_while(|&&c| c != 0)) {
        *slot = c;
    }
    tag(&t)
}

/// `hb_script_from_iso15924_tag`: case-adjusted, with ISO 15924 variants
/// folded onto their base script; anything not shaped like a tag is
/// `Zzzz`.
fn script_from_tag(t: Tag) -> Tag {
    if t == INVALID {
        return INVALID;
    }
    let t = (t & 0xDFDF_DFDF) | 0x0020_2020;
    let alias = match &t.to_be_bytes() {
        b"Aran" => Some(b"Arab"),
        b"Cyrs" => Some(b"Cyrl"),
        b"Geok" => Some(b"Geor"),
        b"Hans" | b"Hant" => Some(b"Hani"),
        b"Jamo" => Some(b"Hang"),
        b"Latf" | b"Latg" => Some(b"Latn"),
        b"Syre" | b"Syrj" | b"Syrn" => Some(b"Syrc"),
        _ => None,
    };
    if let Some(a) = alias {
        return tag(a);
    }
    if t & 0xE0E0_E0E0 == 0x4060_6060 {
        t
    } else {
        UNKNOWN
    }
}

/// `is_ignored_script` in FreetypeRenderer.cc.
fn is_ignored(t: Tag) -> bool {
    matches!(t, COMMON | INHERITED | UNKNOWN | INVALID)
}

fn char_script(c: char) -> Tag {
    match c.script() {
        Script::Common => COMMON,
        Script::Inherited => INHERITED,
        Script::Unknown => UNKNOWN,
        s => s.short_name().as_bytes().try_into().map_or(UNKNOWN, tag),
    }
}

/// `detect_script`: an explicit `script=` wins if it parses; otherwise the
/// one script all non-common characters share, `Zzzz` if they disagree, or
/// `HB_SCRIPT_INVALID` for text without such characters.
fn detect_script(text: &str, script: &str) -> Tag {
    let explicit = script_from_tag(tag_from_string(script));
    if explicit != INVALID {
        return explicit;
    }
    let mut found = INVALID;
    // HarfBuzz is handed the text as a C string, so it ends at a NUL.
    let text = text.split('\0').next().unwrap_or("");
    for s in text.chars().map(char_script).filter(|&s| !is_ignored(s)) {
        if found == INVALID {
            found = s;
        } else if found != s && found != UNKNOWN {
            found = UNKNOWN;
        }
    }
    found
}

/// `hb_script_get_horizontal_direction`: `Some(true)` for right-to-left,
/// `None` for scripts written either way, `Some(false)` otherwise
/// (including unknown scripts).
fn script_rtl(t: Tag) -> Option<bool> {
    match &t.to_be_bytes() {
        b"Arab" | b"Hebr" | b"Syrc" | b"Thaa" | b"Cprt" | b"Khar" | b"Phnx" | b"Nkoo" | b"Lydi"
        | b"Avst" | b"Armi" | b"Phli" | b"Prti" | b"Sarb" | b"Orkh" | b"Samr" | b"Mand"
        | b"Merc" | b"Mero" | b"Mani" | b"Mend" | b"Nbat" | b"Narb" | b"Palm" | b"Phlp"
        | b"Hatr" | b"Adlm" | b"Rohg" | b"Sogo" | b"Sogd" | b"Elym" | b"Chrs" | b"Yezi"
        | b"Ougr" | b"Gara" | b"Sidt" => Some(true),
        b"Hung" | b"Ital" | b"Runr" | b"Tfng" => None,
        _ => Some(false),
    }
}

/// `detect_direction`, as `hb_direction_to_string` names the result: an
/// explicit `direction=` is matched on its first letter only (so
/// `"right-to-left"` works), then the script's own direction, then LTR.
fn detect_direction(direction: &str, script: Tag) -> &'static str {
    match direction.bytes().next().map(|c| c.to_ascii_lowercase()) {
        Some(b'l') => return "ltr",
        Some(b'r') => return "rtl",
        Some(b't') => return "ttb",
        Some(b'b') => return "btt",
        _ => {}
    }
    match script_rtl(script) {
        Some(true) => "rtl",
        _ => "ltr",
    }
}

/// The `script` and `direction` a `text()` node carries after
/// `detect_properties`. A detected (or explicit, parseable) script replaces
/// the given string with its canonical tag; otherwise the string as given
/// is kept, which may be empty.
pub fn resolve(t: &Text) -> (String, &'static str) {
    let s = detect_script(&t.text, &t.script);
    let script = if is_ignored(s) {
        t.script.clone()
    } else {
        String::from_utf8_lossy(&s.to_be_bytes()).into_owned()
    };
    (script, detect_direction(&t.direction, s))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::Discretizer;

    fn text(text: &str, script: &str, direction: &str) -> Text {
        Text {
            text: text.into(),
            size: 10.0,
            spacing: 1.0,
            font: String::new(),
            direction: direction.into(),
            language: "en".into(),
            script: script.into(),
            halign: "default".into(),
            valign: "default".into(),
            disc: Discretizer {
                fn_: 0.0,
                fa: 12.0,
                fs: 2.0,
            },
        }
    }

    #[test]
    fn detection() {
        let r = |t: &str, s: &str, d: &str| {
            let (a, b) = resolve(&text(t, s, d));
            (a, b.to_string())
        };
        assert_eq!(r("Hello, world!", "", ""), ("Latn".into(), "ltr".into()));
        assert_eq!(r("", "", ""), (String::new(), "ltr".into()));
        assert_eq!(r("123 !?", "", ""), (String::new(), "ltr".into()));
        assert_eq!(r("مرحبا", "", ""), ("Arab".into(), "rtl".into()));
        assert_eq!(r("Привет", "", ""), ("Cyrl".into(), "ltr".into()));
        // Mixed scripts become Zzzz, which is ignored: the given string stays.
        assert_eq!(r("abc Привет", "", ""), (String::new(), "ltr".into()));
        assert_eq!(r("abc", "arab", "Right"), ("Arab".into(), "rtl".into()));
        assert_eq!(r("abc", "latf", "x"), ("Latn".into(), "ltr".into()));
        // Not tag-shaped: unknown, so the string is kept and the text decides nothing.
        assert_eq!(r("abc", "ab", ""), ("ab".into(), "ltr".into()));
        assert_eq!(r("abc", "", "btt"), ("Latn".into(), "btt".into()));
    }
}
