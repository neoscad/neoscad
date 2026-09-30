//! The bench kit: the models `neoscad bench` times, as a release asset
//! (`neoscad-bench-kit-<version>.tar.gz`, built by
//! `scripts/release/bench-kit.sh` from `conformance/bench.json`).
//!
//! A kit is a directory holding `kit.json` ([`Kit`]), each model's source
//! under `models/`, the sources of the files models import under
//! `inputs/`, a pinned BOSL2 under `libraries/` (the `OPENSCADPATH` of
//! every run) and the licences of what it carries. Shipping the models as
//! one versioned, hashed file, rather than having each user fetch BOSL2 and
//! OpenSCAD's examples, is what makes two users' times comparable: both ran
//! the same bytes, and the result's `kit.archive_sha256` says so.

use std::collections::BTreeMap;
use std::fs;
use std::io::Read;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The layout of `kit.json`; bump it with any change a reader must know.
pub const KIT_SCHEMA: u32 = 1;

/// The kit asset's file name for a version.
pub fn asset_name(version: &str) -> String {
    format!("neoscad-bench-kit-{version}.tar.gz")
}

/// `kit.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Kit {
    pub kit_schema: u32,
    /// The neoscad version whose release carries this kit.
    pub version: String,
    pub sources: KitSources,
    pub runs: u32,
    pub single_run_over_s: f64,
    pub timeout_s: f64,
    pub quick: QuickConfig,
    /// The directory (relative to the kit) every run gets as
    /// `OPENSCADPATH`.
    pub library_path: String,
    pub cold_start: ColdStart,
    pub models: BTreeMap<String, Model>,
}

/// Where the kit's contents came from.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KitSources {
    /// The neoscad commit whose `conformance/bench.json` the kit was built
    /// from.
    pub neoscad_commit: String,
    /// The BOSL2 commit under `libraries/BOSL2`.
    pub bosl2_commit: String,
    /// The OpenSCAD commit the example models were copied from.
    pub openscad_commit: String,
}

/// What `--quick` changes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QuickConfig {
    pub runs: u32,
    pub cold_start_runs: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ColdStart {
    pub description: String,
    pub file: String,
    pub runs: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Model {
    pub description: String,
    /// The model's source, relative to the kit.
    pub file: String,
    #[serde(default)]
    pub requires: Vec<String>,
    /// Files the model imports: name to the kit-relative source that
    /// `neoscad` exports it from (as ASCII STL) before the first run.
    #[serde(default)]
    pub inputs: BTreeMap<String, String>,
    /// Whether `--quick` runs it.
    #[serde(default)]
    pub quick: bool,
}

/// A kit read from disk.
#[derive(Debug, Clone)]
pub struct LoadedKit {
    pub root: PathBuf,
    pub kit: Kit,
    /// [`content_sha256`] of the kit directory.
    pub content_sha256: String,
}

/// Read the kit at `dir`, or at the single directory inside it (an
/// extracted archive holds `neoscad-bench-kit-<version>/`).
pub fn load_dir(dir: &Path) -> Result<LoadedKit, String> {
    let root = if dir.join("kit.json").is_file() {
        dir.to_path_buf()
    } else {
        let subdirs: Vec<PathBuf> = fs::read_dir(dir)
            .map_err(|e| format!("{}: {e}", dir.display()))?
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.join("kit.json").is_file())
            .collect();
        match subdirs.as_slice() {
            [one] => one.clone(),
            _ => return Err(format!("{}: no kit.json in this directory", dir.display())),
        }
    };
    let text = fs::read_to_string(root.join("kit.json"))
        .map_err(|e| format!("{}: {e}", root.join("kit.json").display()))?;
    let kit: Kit = serde_json::from_str(&text).map_err(|e| format!("kit.json: {e}"))?;
    if kit.kit_schema != KIT_SCHEMA {
        return Err(format!(
            "kit.json has kit_schema {}; this neoscad reads {KIT_SCHEMA} (use the kit of this release)",
            kit.kit_schema
        ));
    }
    for (id, m) in &kit.models {
        for p in std::iter::once(&m.file).chain(m.inputs.values()) {
            safe_relative(p).map_err(|e| format!("kit.json: model {id}: {e}"))?;
            if !root.join(p).is_file() {
                return Err(format!("kit: model {id}: {p} is missing"));
            }
        }
    }
    safe_relative(&kit.cold_start.file)?;
    safe_relative(&kit.library_path)?;
    let content_sha256 = content_sha256(&root)?;
    Ok(LoadedKit {
        root,
        kit,
        content_sha256,
    })
}

/// A path inside the kit: relative, with no `..`.
fn safe_relative(p: &str) -> Result<PathBuf, String> {
    let path = Path::new(p);
    if p.is_empty()
        || !path
            .components()
            .all(|c| matches!(c, Component::Normal(_) | Component::CurDir))
    {
        return Err(format!("'{p}' is not a path inside the kit"));
    }
    Ok(path.to_path_buf())
}

/// A digest of every regular file under `root`: SHA-256 of the sorted
/// lines `<sha256 of file>  <relative path with />`. It names a kit's
/// contents whether it came as the archive or as a directory, so a result
/// run from an unpacked kit can still be matched to its release.
pub fn content_sha256(root: &Path) -> Result<String, String> {
    let mut files = Vec::new();
    collect(root, root, &mut files)?;
    files.sort();
    let mut listing = String::new();
    for rel in files {
        let h = crate::sha256_file(&root.join(&rel))?;
        listing.push_str(&format!("{h}  {rel}\n"));
    }
    Ok(crate::sha256_hex(listing.as_bytes()))
}

fn collect(root: &Path, dir: &Path, out: &mut Vec<String>) -> Result<(), String> {
    for e in fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))? {
        let e = e.map_err(|e| e.to_string())?;
        let t = e.file_type().map_err(|e| e.to_string())?;
        let p = e.path();
        if t.is_dir() {
            collect(root, &p, out)?;
        } else if t.is_file() {
            let rel = p.strip_prefix(root).map_err(|e| e.to_string())?;
            let parts: Vec<String> = rel
                .components()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .collect();
            out.push(parts.join("/"));
        }
    }
    Ok(())
}

/// The most an archive may unpack to. The real kit is about 5 MB; a
/// corrupt or hostile archive must not fill the disk.
const MAX_UNPACKED: u64 = 256 << 20;
const MAX_ENTRIES: usize = 20_000;

/// Unpack a `.tar.gz` kit into `dest` (which must exist). Only regular
/// files and directories with relative paths are accepted: the archive
/// comes over the network, and an entry naming `../` or `/` (or a link
/// pointing there) would write outside `dest`.
pub fn extract_tar_gz(archive: &[u8], dest: &Path) -> Result<(), String> {
    let mut gz = flate2::read::GzDecoder::new(archive).take(MAX_UNPACKED + (1 << 20));
    let mut tar = Vec::new();
    gz.read_to_end(&mut tar)
        .map_err(|e| format!("kit archive: {e}"))?;
    if tar.len() as u64 > MAX_UNPACKED {
        return Err("kit archive: unpacks to more than 256 MB".into());
    }
    let mut at = 0usize;
    let mut entries = 0usize;
    while at + 512 <= tar.len() {
        let h = &tar[at..at + 512];
        if h.iter().all(|&b| b == 0) {
            break;
        }
        entries += 1;
        if entries > MAX_ENTRIES {
            return Err("kit archive: too many entries".into());
        }
        let field = |r: std::ops::Range<usize>| {
            let f = &h[r];
            let end = f.iter().position(|&b| b == 0).unwrap_or(f.len());
            String::from_utf8_lossy(&f[..end]).into_owned()
        };
        let size = u64::from_str_radix(field(124..136).trim(), 8)
            .map_err(|_| "kit archive: bad entry size".to_string())?;
        let name = field(0..100);
        let prefix = if &h[257..262] == b"ustar" {
            field(345..500)
        } else {
            String::new()
        };
        let full = if prefix.is_empty() {
            name
        } else {
            format!("{prefix}/{name}")
        };
        let kind = h[156];
        let data_at = at + 512;
        let data_end = data_at
            .checked_add(size as usize)
            .filter(|&e| e <= tar.len())
            .ok_or("kit archive: truncated")?;
        match kind {
            b'0' | 0 => {
                let rel = safe_relative(full.trim_end_matches('/'))?;
                let target = dest.join(rel);
                if let Some(parent) = target.parent() {
                    fs::create_dir_all(parent).map_err(|e| e.to_string())?;
                }
                fs::write(&target, &tar[data_at..data_end])
                    .map_err(|e| format!("{}: {e}", target.display()))?;
            }
            b'5' => {
                let trimmed = full.trim_end_matches('/');
                if trimmed != "." {
                    let rel = safe_relative(trimmed)?;
                    fs::create_dir_all(dest.join(rel)).map_err(|e| e.to_string())?;
                }
            }
            // pax headers describe the next entry; the kit script writes
            // plain ustar, so their extras are not needed.
            b'x' | b'g' => {}
            other => {
                return Err(format!(
                    "kit archive: entry '{full}' has type '{}'; only files and directories are allowed",
                    other as char
                ));
            }
        }
        at = data_at + (size as usize).div_ceil(512) * 512;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A one-file ustar archive, gzipped.
    fn tiny_tar(name: &str, kind: u8, body: &[u8]) -> Vec<u8> {
        let mut h = [0u8; 512];
        h[..name.len()].copy_from_slice(name.as_bytes());
        h[100..107].copy_from_slice(b"0000644");
        let size = format!("{:011o}", body.len());
        h[124..135].copy_from_slice(size.as_bytes());
        h[156] = kind;
        h[257..263].copy_from_slice(b"ustar\0");
        let mut tar = h.to_vec();
        tar.extend_from_slice(body);
        tar.resize(512 + body.len().div_ceil(512) * 512 + 1024, 0);
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        std::io::Write::write_all(&mut gz, &tar).unwrap();
        gz.finish().unwrap()
    }

    fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("bench-core-kit-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn extracts_files() {
        let d = scratch("ok");
        extract_tar_gz(&tiny_tar("k/a.txt", b'0', b"hello"), &d).unwrap();
        assert_eq!(fs::read(d.join("k/a.txt")).unwrap(), b"hello");
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn refuses_escapes_and_links() {
        let d = scratch("bad");
        assert!(extract_tar_gz(&tiny_tar("../evil", b'0', b"x"), &d).is_err());
        assert!(extract_tar_gz(&tiny_tar("/abs", b'0', b"x"), &d).is_err());
        assert!(extract_tar_gz(&tiny_tar("link", b'2', b""), &d).is_err());
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn content_hash_ignores_how_the_tree_was_made() {
        let a = scratch("ha");
        let b = scratch("hb");
        for d in [&a, &b] {
            fs::create_dir_all(d.join("m")).unwrap();
        }
        fs::write(a.join("m/x.scad"), "cube(1);").unwrap();
        fs::write(a.join("kit.json"), "{}").unwrap();
        fs::write(b.join("kit.json"), "{}").unwrap();
        fs::write(b.join("m/x.scad"), "cube(1);").unwrap();
        assert_eq!(content_sha256(&a).unwrap(), content_sha256(&b).unwrap());
        fs::write(b.join("m/x.scad"), "cube(2);").unwrap();
        assert_ne!(content_sha256(&a).unwrap(), content_sha256(&b).unwrap());
        fs::remove_dir_all(&a).unwrap();
        fs::remove_dir_all(&b).unwrap();
    }
}
