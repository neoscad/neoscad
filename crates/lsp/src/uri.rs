//! `file://` URIs and paths. The protocol names documents by URI; the
//! session and the loader by absolute path. Only `file` URIs map to
//! paths: an editor's other schemes (`untitled:`) have no directory for
//! includes to be relative to, and are refused.

use std::path::{Path, PathBuf};

/// The path of a `file://` URI (percent-decoded), normalised.
pub fn to_path(uri: &str) -> Option<PathBuf> {
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
    Some(session::normal(Path::new(s)))
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
    let s = p.to_string_lossy();
    let mut out = String::from("file://");
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'/' | b'-' | b'.' | b'_' | b'~') {
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
            let u = from_path(Path::new(p));
            assert_eq!(to_path(&u).unwrap(), PathBuf::from(p), "{u}");
        }
        assert_eq!(from_path(Path::new("/a b")), "file:///a%20b");
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
}
