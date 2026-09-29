//! Windows path spellings, kept in one place.
//!
//! `std::fs::canonicalize` on Windows answers in the verbatim form
//! (`\\?\C:\Users\me\a.scad`). That form reaches the file system unparsed,
//! so `/` is not a separator and `..` is not folded inside it, and it
//! compares unequal to the plain `C:\Users\me\a.scad` everything else
//! produces (the working directory, paths joined from it, paths a user
//! types). Left alone it broke `neoscad mcp`'s root checks (an allowed
//! `base_dir` of `../other` stayed a literal `in\../other`), wrote
//! `\\?\C:\...` and `//?/C:/...` into `-d` dependency files, where make
//! cannot use them, and put them in `file://` URIs. OpenSCAD never sees the
//! form: MSVC's `std::filesystem::canonical` returns the plain path.
//!
//! The helpers work on strings so that their rules are the same, and are
//! tested, on every host; [`plain`] applies them only when compiled for
//! Windows, where a leading `\\?\` means a prefix rather than a file name.

use std::path::PathBuf;

/// Windows' `MAX_PATH`: a plain path at or past this length needs the
/// verbatim prefix (or a long-path opt-in the process may not have).
const MAX_PATH: usize = 260;

/// `\\?\C:\x` as `C:\x`, and `\\?\UNC\server\share\x` as
/// `\\server\share\x`, when the plain spelling names the same file; `None`
/// when `s` is not verbatim or the plain spelling would not be the same
/// file (a component Windows would reinterpret without the prefix, such as
/// a reserved device name, `..`, or a trailing dot or space, or a path too
/// long for `MAX_PATH`). This is the `dunce` crate's rule.
pub fn strip_verbatim(s: &str) -> Option<String> {
    let rest = s.strip_prefix(r"\\?\")?;
    let (plain, tail) = if let Some(unc) = rest
        .strip_prefix(r"UNC\")
        .or_else(|| rest.strip_prefix(r"unc\"))
    {
        // `server\share` must both be there and both be ordinary names.
        let mut parts = unc.splitn(3, '\\');
        let (server, share) = (parts.next()?, parts.next()?);
        if !ordinary(server) || !ordinary(share) {
            return None;
        }
        let tail = parts.next().unwrap_or("");
        (format!(r"\\{server}\{share}"), tail)
    } else {
        let b = rest.as_bytes();
        if b.len() < 2 || !b[0].is_ascii_alphabetic() || b[1] != b':' {
            return None;
        }
        match rest.get(2..3) {
            None => (rest.to_string(), ""),
            Some(r"\") => (rest[..2].to_string(), &rest[3..]),
            Some(_) => return None,
        }
    };
    let mut out = plain;
    out.push('\\');
    if !tail.is_empty() {
        // A trailing separator is kept; an empty component anywhere else
        // (`x\\y`) is not a plain path's.
        let (body, trailing) = match tail.strip_suffix('\\') {
            Some(b) => (b, true),
            None => (tail, false),
        };
        if !body.split('\\').all(ordinary) {
            return None;
        }
        out.push_str(body);
        if trailing {
            out.push('\\');
        }
    }
    (out.len() < MAX_PATH).then_some(out)
}

/// A path component that means the same with or without the verbatim
/// prefix: non-empty, not `.`/`..`, no character the Win32 layer treats
/// specially, no trailing dot or space (which it strips), and not a
/// reserved device name (`NUL`, `com1.txt`, ...), which it redirects.
fn ordinary(c: &str) -> bool {
    if c.is_empty() || c == "." || c == ".." || c.ends_with(['.', ' ']) {
        return false;
    }
    if c.chars()
        .any(|ch| ch < ' ' || matches!(ch, '<' | '>' | ':' | '"' | '/' | '|' | '?' | '*'))
    {
        return false;
    }
    let stem = c.split('.').next().unwrap_or(c).trim_end_matches(' ');
    let upper = stem.to_ascii_uppercase();
    let reserved = matches!(
        upper.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
    ) || ((upper.starts_with("COM") || upper.starts_with("LPT"))
        && upper.len() == 4
        && upper.as_bytes()[3].is_ascii_digit());
    !reserved
}

/// A canonical path (from `canonicalize`) as Windows spells it everywhere
/// else: without the verbatim prefix when that names the same file.
/// Unchanged on other hosts, where `\\?\` would be part of a file name.
pub fn plain(p: PathBuf) -> PathBuf {
    if cfg!(windows)
        && let Some(s) = p.to_str().and_then(strip_verbatim)
    {
        return PathBuf::from(s);
    }
    p
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verbatim_drive_and_unc_paths_become_plain() {
        let cases = [
            (r"\\?\C:\Users\me\a.scad", Some(r"C:\Users\me\a.scad")),
            (r"\\?\c:\x\u 1.scad", Some(r"c:\x\u 1.scad")),
            (r"\\?\C:\", Some(r"C:\")),
            (r"\\?\C:", Some(r"C:\")),
            (r"\\?\C:\dir\", Some(r"C:\dir\")),
            (
                r"\\?\UNC\server\share\a.scad",
                Some(r"\\server\share\a.scad"),
            ),
            (r"\\?\UNC\server\share", Some(r"\\server\share\")),
            // Not verbatim at all.
            (r"C:\Users\me", None),
            ("/usr/lib", None),
            (r"\\server\share\x", None),
            // Verbatim forms whose plain spelling is another file.
            (r"\\?\C:\x\NUL", None),
            (r"\\?\C:\x\com1.txt", None),
            (r"\\?\C:\x\lpt9", None),
            (r"\\?\C:\x\trailing.", None),
            (r"\\?\C:\x\trailing ", None),
            (r"\\?\C:\x\..\y", None),
            (r"\\?\C:\x\.\y", None),
            (r"\\?\C:\x\\y", None),
            (r"\\?\C:\x/y", None),
            (r"\\?\C:x", None),
            (r"\\?\Volume{0123}\x", None),
            (r"\\?\UNC\server", None),
            (r"\\?\UNC\ser:ver\share", None),
        ];
        for (input, want) in cases {
            assert_eq!(strip_verbatim(input).as_deref(), want, "{input}");
        }
        // Ordinary names that only look special stay plain-able.
        assert!(strip_verbatim(r"\\?\C:\COM10\console.scad").is_some());
        assert!(strip_verbatim(r"\\?\C:\x\.hidden").is_some());
        // MAX_PATH: a plain path must be shorter.
        let long = format!(r"\\?\C:\{}", "a".repeat(MAX_PATH));
        assert_eq!(strip_verbatim(&long), None);
        let fits = format!(r"\\?\C:\{}", "a".repeat(MAX_PATH - 4));
        assert_eq!(strip_verbatim(&fits).map(|s| s.len()), Some(MAX_PATH - 1));
    }

    #[test]
    fn plain_leaves_other_hosts_alone() {
        let p = PathBuf::from(r"\\?\C:\x");
        let want = if cfg!(windows) { r"C:\x" } else { r"\\?\C:\x" };
        assert_eq!(plain(p), PathBuf::from(want));
    }
}
