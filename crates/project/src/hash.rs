//! The content hash of a materialised dependency, recorded in
//! `scad.lock` and checked whenever a project is linked to the tree.
//!
//! Definition (version 1), written `sha256:<64 lowercase hex digits>`:
//! list every regular file and every symbolic link under the tree's root,
//! recursively, without following links; directories contribute only
//! through what they contain (an empty one contributes nothing, as git
//! does not track it). Name each entry by its path relative to the root,
//! components joined by `/`, as UTF-8 bytes. Sort the entries by those
//! bytes. Then SHA-256 the concatenation, over the entries in order, of
//!
//! ```text
//! path bytes · 0x00 · content length as 8 bytes big-endian · content bytes
//! ```
//!
//! where a file's content is its bytes and a link's content is its target
//! as written (`/`-separated). File modes (the executable bit, read-only),
//! times and owners are not part of it, and a link hashes like a file
//! holding its target, so the hash is the same on a checkout that git
//! wrote as a link and on a Windows one where git wrote that file.

use std::path::Path;

use sha2::{Digest, Sha256};

use crate::error::{Context, Result};

/// The hash prefix (the algorithm).
pub const PREFIX: &str = "sha256:";

/// The content hash of the tree at `root`.
pub fn tree_hash(root: &Path) -> Result<String> {
    let mut entries: Vec<(String, Vec<u8>)> = Vec::new();
    walk(root, "", &mut entries)?;
    entries.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
    let mut h = Sha256::new();
    for (path, content) in &entries {
        h.update(path.as_bytes());
        h.update([0u8]);
        h.update((content.len() as u64).to_be_bytes());
        h.update(content);
    }
    Ok(format!("{PREFIX}{}", hex(&h.finalize())))
}

fn walk(dir: &Path, rel: &str, out: &mut Vec<(String, Vec<u8>)>) -> Result<()> {
    for e in std::fs::read_dir(dir).at("read", dir)? {
        let e = e.at("read", dir)?;
        let name = e.file_name().to_string_lossy().into_owned();
        let path = e.path();
        let rel_path = if rel.is_empty() {
            name
        } else {
            format!("{rel}/{name}")
        };
        let ft = e.file_type().at("read", &path)?;
        if ft.is_symlink() {
            let t = std::fs::read_link(&path).at("read", &path)?;
            let t = t.to_string_lossy().replace('\\', "/");
            out.push((rel_path, t.into_bytes()));
        } else if ft.is_dir() {
            walk(&path, &rel_path, out)?;
        } else {
            out.push((rel_path, std::fs::read(&path).at("read", &path)?));
        }
    }
    Ok(())
}

/// Lowercase hex.
pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// SHA-256 of `s` in hex: the store's names for mirrors and projects.
pub fn digest(s: &str) -> String {
    hex(&Sha256::digest(s.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_hash_is_the_documented_one() {
        let d = std::env::temp_dir().join(format!("nsc-hash-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("sub/empty")).unwrap();
        std::fs::write(d.join("b.scad"), "cube(1);\n").unwrap();
        std::fs::write(d.join("sub/a.scad"), "x").unwrap();
        let got = tree_hash(&d).unwrap();
        // By hand: "b.scad" then "sub/a.scad" (bytes order), each framed.
        let mut h = Sha256::new();
        for (p, c) in [("b.scad", &b"cube(1);\n"[..]), ("sub/a.scad", b"x")] {
            h.update(p.as_bytes());
            h.update([0]);
            h.update((c.len() as u64).to_be_bytes());
            h.update(c);
        }
        assert_eq!(got, format!("sha256:{}", hex(&h.finalize())));
        std::fs::write(d.join("sub/a.scad"), "y").unwrap();
        assert_ne!(tree_hash(&d).unwrap(), got);
        let _ = std::fs::remove_dir_all(&d);
    }
}
