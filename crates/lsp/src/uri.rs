//! `file://` URIs and paths. The protocol names documents by URI; the
//! session and the loader by absolute path. Only `file` URIs map to
//! paths: an editor's other schemes (`untitled:`) have no directory for
//! includes to be relative to, and are refused.
//!
//! On Windows a drive path is `file:///C:/dir/a.scad` (RFC 8089 E.2; VS
//! Code writes the colon as `%3A`, which decodes the same). Without the
//! mapping the server took `/C:/dir/a.scad` for the path, which names
//! nothing, and answered hover and definitions with nothing.

use std::path::{Path, PathBuf};

/// The path of a `file://` URI (percent-decoded), normalised.
pub fn to_path(uri: &str) -> Option<PathBuf> {
    let s = decoded_path(uri, cfg!(windows))?;
    Some(session::normal(Path::new(&s)))
}

/// A `file://` URI's path as a string: `/`-separated, or when `windows`,
/// `C:\`-style for a drive letter. Apart from [`to_path`] so that both
/// hosts' rules are tested on either.
fn decoded_path(uri: &str, windows: bool) -> Option<String> {
    let rest = uri.strip_prefix("file://")?;
    // `file://host/path` names another machine; only the empty host and
    // `localhost` are this one.
    let rest = rest.strip_prefix("localhost").unwrap_or(rest);
    if !rest.starts_with('/') {
        return None;
    }
    let bytes = rest.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let (Some(h), Some(l)) = (hex(bytes[i + 1]), hex(bytes[i + 2]))
        {
            out.push(h * 16 + l);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    let s = String::from_utf8(out).ok()?;
    // A query or fragment has no place in a file URI's path.
    let s = s.split(['?', '#']).next().unwrap_or("");
    if windows && let Some(drive) = drive_path(s) {
        let mut p = drive.replace('/', "\\");
        // `C:` alone is the drive's current directory, not its root.
        if p.len() == 2 {
            p.push('\\');
        }
        return Some(p);
    }
    Some(s.to_string())
}

/// `/C:/x` (or `/C:`) as `C:/x`: the drive a Windows file URI's path
/// starts with, if it starts with one.
fn drive_path(s: &str) -> Option<&str> {
    let rest = s.strip_prefix('/')?;
    let b = rest.as_bytes();
    let drive = b.len() >= 2
        && b[0].is_ascii_alphabetic()
        && b[1] == b':'
        && (b.len() == 2 || b[2] == b'/');
    drive.then_some(rest)
}

fn hex(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// The `file://` URI of an absolute path, escaping what RFC 3986 does
/// not allow in a path (VS Code's spelling: `/` kept, everything outside
/// the unreserved set and `/` percent-encoded).
pub fn from_path(p: &Path) -> String {
    encode(&p.to_string_lossy(), cfg!(windows))
}

/// [`from_path`] of a path string, by Windows' rules when `windows`.
fn encode(s: &str, windows: bool) -> String {
    let mut path = s.to_string();
    let mut drive = false;
    if windows {
        // `\\?\C:\x` is `C:\x` to an editor, and `\` is a URI's `/`.
        path = lang::paths::strip_verbatim(&path)
            .unwrap_or(path)
            .replace('\\', "/");
        if !path.starts_with('/') {
            path.insert(0, '/');
        }
        drive = drive_path(&path).is_some();
    }
    let mut out = String::from("file://");
    for (i, b) in path.bytes().enumerate() {
        // The drive's colon stays as written, as RFC 8089 spells it.
        if b.is_ascii_alphanumeric()
            || matches!(b, b'/' | b'-' | b'.' | b'_' | b'~')
            || (drive && i == 2)
        {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips() {
        for p in [
            "/a/b c/ü.scad",
            "/x/100%.scad",
            "/NeoSCAD.resources/libraries/MCAD/units.scad",
        ] {
            let u = encode(p, false);
            assert_eq!(decoded_path(&u, false).unwrap(), p, "{u}");
        }
        assert_eq!(encode("/a b", false), "file:///a%20b");
        assert_eq!(
            to_path("file:///a/./b/../c.scad").unwrap(),
            PathBuf::from("/a/c.scad")
        );
        assert_eq!(
            to_path("file://localhost/x.scad").unwrap(),
            PathBuf::from("/x.scad")
        );
        assert!(to_path("untitled:Untitled-1").is_none());
        assert!(to_path("file://server/share/x.scad").is_none());
    }

    #[test]
    fn windows_drive_paths() {
        for (p, u) in [
            (r"C:\Users\me\a b.scad", "file:///C:/Users/me/a%20b.scad"),
            (r"\\?\C:\Users\me\a.scad", "file:///C:/Users/me/a.scad"),
            (r"d:\x\y:z.scad", "file:///d:/x/y%3Az.scad"),
        ] {
            assert_eq!(encode(p, true), u, "{p}");
            assert_eq!(
                decoded_path(u, true).unwrap(),
                lang::paths::strip_verbatim(p).unwrap_or(p.into()),
                "{u}"
            );
        }
        for (u, p) in [
            // VS Code's spelling.
            ("file:///c%3A/Users/me/a.scad", r"c:\Users\me\a.scad"),
            ("file:///C:", r"C:\"),
            // Not a drive: left as a path.
            ("file:///CC:/x", "/CC:/x"),
        ] {
            assert_eq!(decoded_path(u, true).unwrap(), p, "{u}");
        }
        // Elsewhere `/C:/x` is an ordinary path.
        assert_eq!(decoded_path("file:///C:/x", false).unwrap(), "/C:/x");
    }
}
