//! The regular files of an uncompressed tar archive (ustar, as `tar`
//! and GNU tar write them), for `addFiles`' `tar`: BOSL2 arrives as one
//! `bosl2.tar.gz` that the page gunzips with `DecompressionStream` and
//! hands over as one buffer, rather than sixty-odd separate files.
//!
//! Only what a library archive needs: regular files, with ustar's
//! `prefix` and GNU's long names (`L` entries). Directories, links and
//! pax headers are skipped. A pax header can carry a long name too, so an
//! archive whose names exceed ustar's 255 bytes must be made with GNU's
//! long names (`tar --format=gnu`); BOSL2's names are far shorter.

/// `(path, contents)` of each regular file, in archive order; or why the
/// archive is malformed. Paths are as stored (relative, `/`-separated),
/// with a leading `./` removed.
pub fn files(data: &[u8]) -> Result<Vec<(String, &[u8])>, String> {
    let mut out = Vec::new();
    let mut at = 0usize;
    let mut long_name: Option<String> = None;
    while at + 512 <= data.len() {
        let header = &data[at..at + 512];
        if header.iter().all(|&b| b == 0) {
            break;
        }
        let size = octal(&header[124..136])
            .ok_or_else(|| format!("tar: bad size in the header at byte {at}"))?;
        let start = at + 512;
        let end = start
            .checked_add(size)
            .filter(|&e| e <= data.len())
            .ok_or_else(|| format!("tar: an entry at byte {at} runs past the end"))?;
        let body = &data[start..end];
        let kind = header[156];
        let name = match long_name.take() {
            Some(n) => n,
            None => {
                let name = cstr(&header[0..100]);
                let prefix = if &header[257..262] == b"ustar" {
                    cstr(&header[345..500])
                } else {
                    String::new()
                };
                if prefix.is_empty() {
                    name
                } else {
                    format!("{prefix}/{name}")
                }
            }
        };
        match kind {
            b'0' | 0 | b'7' => {
                let name = name.strip_prefix("./").unwrap_or(&name).to_string();
                if !name.is_empty() && !name.ends_with('/') {
                    out.push((name, body));
                }
            }
            b'L' => long_name = Some(cstr(body)),
            _ => {}
        }
        at = start + size.div_ceil(512) * 512;
    }
    Ok(out)
}

/// A NUL-terminated field as text.
fn cstr(b: &[u8]) -> String {
    let end = b.iter().position(|&c| c == 0).unwrap_or(b.len());
    String::from_utf8_lossy(&b[..end]).into_owned()
}

/// An octal number field (spaces and NULs around it allowed).
fn octal(b: &[u8]) -> Option<usize> {
    let s = cstr(b);
    let s = s.trim();
    if s.is_empty() {
        return Some(0);
    }
    usize::from_str_radix(s, 8).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One ustar header for a file `name` of `len` bytes.
    fn header(name: &str, len: usize, kind: u8) -> Vec<u8> {
        let mut h = vec![0u8; 512];
        h[..name.len()].copy_from_slice(name.as_bytes());
        let size = format!("{len:011o}\0");
        h[124..136].copy_from_slice(size.as_bytes());
        h[156] = kind;
        h[257..263].copy_from_slice(b"ustar\0");
        h
    }

    fn entry(out: &mut Vec<u8>, name: &str, body: &[u8], kind: u8) {
        out.extend(header(name, body.len(), kind));
        out.extend_from_slice(body);
        out.resize(out.len().div_ceil(512) * 512, 0);
    }

    #[test]
    fn reads_files_and_long_names_and_skips_directories() {
        let mut t = Vec::new();
        entry(&mut t, "./BOSL2/", b"", b'5');
        entry(&mut t, "./BOSL2/std.scad", b"include <x.scad>\n", b'0');
        let long = format!("BOSL2/{}.scad", "a".repeat(120));
        entry(
            &mut t,
            "././@LongLink",
            format!("{long}\0").as_bytes(),
            b'L',
        );
        entry(&mut t, "truncated", b"x", b'0');
        t.extend(vec![0u8; 1024]);
        let f = files(&t).unwrap();
        assert_eq!(f.len(), 2);
        assert_eq!(
            f[0],
            ("BOSL2/std.scad".to_string(), &b"include <x.scad>\n"[..])
        );
        assert_eq!(f[1].0, long);
    }

    #[test]
    fn a_truncated_archive_is_an_error() {
        let mut t = header("a.scad", 4096, b'0');
        t.extend_from_slice(b"short");
        assert!(files(&t).is_err());
    }
}
