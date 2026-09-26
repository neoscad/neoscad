//! Text comparison, ported from OpenSCAD's `tests/test_cmdline_tool.py`
//! (reference commit 28fe66bc) so a pass here means a pass under ctest.
//!
//! - `normalize_string` (lines 106-135): strip `, timestamp = N`. The float
//!   truncation and `file = "..."` path rewriting in that function are
//!   commented out upstream, so numbers and paths must match exactly.
//! - `get_normalized_lines` (lines 137-169): read the file (UTF-8, falling
//!   back to latin-1, with Python's universal newlines), normalise, strip
//!   leading/trailing CR/LF, add one `\n`, in the expected file only replace
//!   the golden build path `../../tests` with the runtime one, split with
//!   `str.splitlines()` and drop lines matching `OPENSCAD_TEST_EXCLUDE_LINE`.
//! - `compare_default` (lines 171-184): the two line lists must be equal.

use std::path::Path;
use std::sync::OnceLock;

use regex::Regex;

/// `build_to_test_sources` in test_cmdline_tool.py: the tests directory as
/// seen from the build directory the goldens were generated in.
pub const BUILD_TO_TEST_SOURCES: &str = "../../tests";

fn timestamp_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(", timestamp = -?[0-9]+").expect("valid regex"))
}

/// Python's `open(f).read()` under `-Xutf8=1`: strict UTF-8, retried as
/// latin-1 (the fallback exists for `ord-tests.scad` output), with universal
/// newline translation.
pub fn read_text(path: &Path) -> std::io::Result<String> {
    let bytes = std::fs::read(path)?;
    let text = match String::from_utf8(bytes) {
        Ok(s) => s,
        Err(e) => e.into_bytes().iter().map(|&b| b as char).collect(),
    };
    Ok(text.replace("\r\n", "\n").replace('\r', "\n"))
}

/// `str.splitlines()`: Python splits on more than `\n`.
fn py_splitlines(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut chars = s.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        let is_break = matches!(
            c,
            '\n' | '\r' | '\x0b' | '\x0c' | '\x1c' | '\x1d' | '\x1e' | '\u{85}' | '\u{2028}' | '\u{2029}'
        );
        if is_break {
            out.push(&s[start..i]);
            let mut end = i + c.len_utf8();
            if c == '\r' && chars.peek().is_some_and(|&(_, n)| n == '\n') {
                chars.next();
                end += 1;
            }
            start = end;
        }
    }
    if start < s.len() {
        out.push(&s[start..]);
    }
    out
}

/// `get_normalized_lines`. `replace_paths` carries the runtime path to the
/// tests directory when normalising an expected file.
pub fn normalized_lines(text: &str, replace_paths: Option<&str>, exclude: Option<&Regex>) -> Vec<String> {
    let s = timestamp_re().replace_all(text, "");
    let mut t = s.trim_matches(['\r', '\n']).replace("\r\n", "\n");
    t.push('\n');
    if let Some(runtime) = replace_paths
        && runtime != BUILD_TO_TEST_SOURCES
    {
        t = t.replace(BUILD_TO_TEST_SOURCES, runtime);
    }
    py_splitlines(&t)
        .into_iter()
        .filter(|l| exclude.is_none_or(|re| !re.is_match(l)))
        .map(String::from)
        .collect()
}

/// A failed comparison, with the first differing region for reports.
#[derive(Debug, Clone)]
pub struct Mismatch {
    /// 1-based line number of the first difference.
    pub line: usize,
    pub expected_lines: usize,
    pub actual_lines: usize,
    /// A few `-expected` / `+actual` lines from the first difference.
    pub excerpt: Vec<String>,
}

/// `compare_default`: exact equality of the normalised line lists.
pub fn compare(expected: &[String], actual: &[String]) -> Result<(), Mismatch> {
    if expected == actual {
        return Ok(());
    }
    let first = expected
        .iter()
        .zip(actual)
        .position(|(e, a)| e != a)
        .unwrap_or(expected.len().min(actual.len()));
    const CONTEXT: usize = 3;
    let mut excerpt = Vec::new();
    for l in expected.iter().skip(first).take(CONTEXT) {
        excerpt.push(format!("-{l}"));
    }
    for l in actual.iter().skip(first).take(CONTEXT) {
        excerpt.push(format!("+{l}"));
    }
    Err(Mismatch {
        line: first + 1,
        expected_lines: expected.len(),
        actual_lines: actual.len(),
        excerpt,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_timestamps_and_outer_newlines() {
        let l = normalized_lines("\r\n\ngroup(), timestamp = -12345 {\r\n}\n\n", None, None);
        assert_eq!(l, ["group() {", "}"]);
    }

    #[test]
    fn replaces_golden_paths_only_when_different() {
        let t = "in file ../../tests/data/x.scad, line 1\n";
        assert_eq!(
            normalized_lines(t, Some("../../.reference/openscad/tests"), None),
            ["in file ../../.reference/openscad/tests/data/x.scad, line 1"]
        );
        assert_eq!(normalized_lines(t, Some("../../tests"), None), ["in file ../../tests/data/x.scad, line 1"]);
    }

    #[test]
    fn excludes_lines_with_upstream_default_regex() {
        let re = Regex::new(r"^TRACE:\s*\*\*\* Excluding \d+ frames \*\*\*\s*$").unwrap();
        let l = normalized_lines("a\nTRACE:   *** Excluding 12 frames ***\nb", None, Some(&re));
        assert_eq!(l, ["a", "b"]);
    }

    #[test]
    fn splitlines_matches_python() {
        assert_eq!(py_splitlines("a\x0cb\u{2028}c\r\nd"), ["a", "b", "c", "d"]);
        assert_eq!(py_splitlines("\n"), [""]);
        assert!(py_splitlines("").is_empty());
    }

    #[test]
    fn empty_output_equals_empty_expected() {
        assert_eq!(normalized_lines("", None, None), [""]);
        assert!(compare(&normalized_lines("\n\n", None, None), &normalized_lines("", None, None)).is_ok());
    }

    #[test]
    fn mismatch_reports_first_difference() {
        let e: Vec<String> = ["a", "b", "c"].map(String::from).to_vec();
        let a: Vec<String> = ["a", "x"].map(String::from).to_vec();
        let m = compare(&e, &a).unwrap_err();
        assert_eq!(m.line, 2);
        assert_eq!(m.excerpt, ["-b", "-c", "+x"]);
    }
}
