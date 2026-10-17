//! `scad.lock`: the dependency graph as resolved, written by
//! a fetch and meant to be committed:
//!
//! ```toml
//! version = 1
//! inputs = "sha256:…"
//!
//! [[package]]
//! name = "BOSL2"
//! source = "git+https://github.com/BelfrySCAD/BOSL2"
//! tag = "v2.0.716"
//! commit = "…"   # the full commit id
//! hash = "sha256:…"   # the checkout's content hash (src/hash.rs)
//! dependencies = []
//! ```
//!
//! One `[[package]]` per selected dependency, sorted by name, with what
//! was asked for (`tag`/`rev`/`branch`, `include_root`, the folder `name`
//! when it differs from the key) beside what it resolved to. A path
//! dependency has `source = "path+DIR"` and no commit or hash.
//! `inputs` is a hash of every requirement the project and its path
//! dependencies declare: while it matches, a fetch links the
//! locked packages without resolving anything (and without running git
//! when their checkouts are in the store).
//!
//! The file is written here byte for byte (not by a serializer), so the
//! same graph always gives the same file.
//!
//! A lockfile arrives with a cloned project, so reading one checks every
//! field that later becomes a git argument or a path: a `commit` is a
//! full lowercase hex id, `name` and `folder` are folder names
//! ([`check_folder`]), `include_root` stays inside the repository, and a
//! `hash` is `sha256:` and 64 hex digits. Whether the project's own
//! entries still say what `scad.toml` says is the fetch's check, which
//! re-resolves when they do not.

use std::path::Path;

use serde::Deserialize;

use crate::error::{Context, Error, Result, bail};
use crate::manifest::{GitRef, check_folder, normal_root, quote};

/// The lockfile format this writes and reads.
pub const VERSION: u32 = 1;

/// The most packages a graph (and so a lockfile) may hold: far more than
/// any OpenSCAD project uses, and a bound on the clones and checkouts a
/// cloned project's dependencies can ask for.
pub const MAX_PACKAGES: usize = 1000;

/// Whether `c` is a full commit id as git prints it: 40 (SHA-1) or 64
/// (SHA-256) lowercase hex digits. Nothing else may reach git as a
/// commit: a lockfile's `commit = "--upload-pack=…"` would otherwise be
/// an option of `git fetch`, which runs it.
pub fn is_commit_id(c: &str) -> bool {
    (c.len() == 40 || c.len() == 64) && c.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

fn is_hash(h: &str) -> bool {
    h.strip_prefix(crate::hash::PREFIX)
        .is_some_and(|d| d.len() == 64 && d.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')))
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Lock {
    /// See the module documentation.
    pub inputs: String,
    /// Sorted by name.
    pub packages: Vec<Locked>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Locked {
    /// The dependency's key.
    pub name: String,
    /// Its folder in `.scad/libraries`.
    pub folder: String,
    pub source: LockedSource,
    pub include_root: Option<String>,
    /// The keys it depends on, sorted.
    pub dependencies: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LockedSource {
    Git {
        url: String,
        reference: GitRef,
        commit: String,
        hash: String,
    },
    /// As the requiring manifest wrote it, relative to the project root
    /// (or absolute).
    Path(String),
}

#[derive(Deserialize)]
struct Raw {
    version: u32,
    #[serde(default)]
    inputs: String,
    #[serde(default)]
    package: Vec<RawPackage>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawPackage {
    name: String,
    folder: Option<String>,
    source: String,
    tag: Option<String>,
    rev: Option<String>,
    branch: Option<String>,
    commit: Option<String>,
    hash: Option<String>,
    include_root: Option<String>,
    #[serde(default)]
    dependencies: Vec<String>,
}

impl Lock {
    /// Read `path`; Ok(None) if there is no such file.
    pub fn read(path: &Path) -> Result<Option<Lock>> {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e).at("read", path),
        };
        Lock::parse(&text)
            .map(Some)
            .map_err(|e| Error(format!("{}: {e}", path.display())))
    }

    pub fn parse(text: &str) -> Result<Lock> {
        let raw: Raw = basic_toml::from_str(text).map_err(|e| Error(e.to_string()))?;
        if raw.version != VERSION {
            bail!(
                "lockfile version {} is not one this version reads ({VERSION}); update the tool, or delete scad.lock to resolve again",
                raw.version
            );
        }
        if raw.package.len() > MAX_PACKAGES {
            bail!(
                "{} packages, more than the {MAX_PACKAGES} a graph may hold",
                raw.package.len()
            );
        }
        let mut packages = Vec::new();
        for p in raw.package {
            check_folder(&p.name).map_err(|e| Error(format!("package `{}`: {e}", p.name)))?;
            let folder = p.folder.unwrap_or_else(|| p.name.clone());
            check_folder(&folder)
                .map_err(|e| Error(format!("package `{}`: `folder`: {e}", p.name)))?;
            let include_root = match &p.include_root {
                None => None,
                Some(r) => {
                    normal_root(r).map_err(|e| Error(format!("package `{}`: {e}", p.name)))?
                }
            };
            for d in &p.dependencies {
                check_folder(d)
                    .map_err(|e| Error(format!("package `{}`: dependency {e}", p.name)))?;
            }
            let source = if let Some(url) = p.source.strip_prefix("git+") {
                let reference = match (p.tag, p.rev, p.branch) {
                    (Some(t), None, None) => GitRef::Tag(t),
                    (None, Some(r), None) => GitRef::Rev(r),
                    (None, None, Some(b)) => GitRef::Branch(b),
                    (None, None, None) => GitRef::Default,
                    _ => bail!("package `{}`: more than one of tag, rev and branch", p.name),
                };
                let (Some(commit), Some(hash)) = (p.commit, p.hash) else {
                    bail!(
                        "package `{}`: a git package needs `commit` and `hash`",
                        p.name
                    );
                };
                if !is_commit_id(&commit) {
                    bail!(
                        "package `{}`: `commit` must be a full commit id (40 or 64 lowercase hex digits)",
                        p.name
                    );
                }
                if !is_hash(&hash) {
                    bail!(
                        "package `{}`: `hash` must be {}<64 lowercase hex digits>",
                        p.name,
                        crate::hash::PREFIX
                    );
                }
                if url.is_empty() || url.trim_start().starts_with('-') {
                    bail!("package `{}`: '{url}' is not a git URL", p.name);
                }
                LockedSource::Git {
                    url: url.to_string(),
                    reference,
                    commit,
                    hash,
                }
            } else if let Some(dir) = p.source.strip_prefix("path+") {
                LockedSource::Path(dir.to_string())
            } else {
                bail!("package `{}`: unknown source '{}'", p.name, p.source);
            };
            packages.push(Locked {
                folder,
                name: p.name,
                source,
                include_root,
                dependencies: p.dependencies,
            });
        }
        packages.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(Lock {
            inputs: raw.inputs,
            packages,
        })
    }

    /// The file's text.
    pub fn render(&self) -> String {
        let mut s = String::from(
            "# scad.lock: the exact revisions of scad.toml's dependencies, written by\n\
             # a fetch (scad-deps, NeoSCAD). Commit it with scad.toml; do not edit it.\n",
        );
        s.push_str(&format!("version = {VERSION}\n"));
        s.push_str(&format!("inputs = {}\n", quote(&self.inputs)));
        for p in &self.packages {
            s.push_str("\n[[package]]\n");
            s.push_str(&format!("name = {}\n", quote(&p.name)));
            if p.folder != p.name {
                s.push_str(&format!("folder = {}\n", quote(&p.folder)));
            }
            match &p.source {
                LockedSource::Git {
                    url,
                    reference,
                    commit,
                    hash,
                } => {
                    s.push_str(&format!("source = {}\n", quote(&format!("git+{url}"))));
                    let r = reference.toml();
                    if !r.is_empty() {
                        s.push_str(&r);
                        s.push('\n');
                    }
                    if let Some(r) = &p.include_root {
                        s.push_str(&format!("include_root = {}\n", quote(r)));
                    }
                    s.push_str(&format!("commit = {}\n", quote(commit)));
                    s.push_str(&format!("hash = {}\n", quote(hash)));
                }
                LockedSource::Path(dir) => {
                    s.push_str(&format!("source = {}\n", quote(&format!("path+{dir}"))));
                    if let Some(r) = &p.include_root {
                        s.push_str(&format!("include_root = {}\n", quote(r)));
                    }
                }
            }
            let deps: Vec<String> = p.dependencies.iter().map(|d| quote(d)).collect();
            s.push_str(&format!("dependencies = [{}]\n", deps.join(", ")));
        }
        s
    }

    pub fn get(&self, name: &str) -> Option<&Locked> {
        self.packages.iter().find(|p| p.name == name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips() {
        let lock = Lock {
            inputs: "sha256:00".into(),
            packages: vec![
                Locked {
                    name: "A".into(),
                    folder: "A-lib".into(),
                    source: LockedSource::Git {
                        url: "file:///r/a".into(),
                        reference: GitRef::Tag("v1.0.0".into()),
                        commit: "c".repeat(40),
                        hash: format!("sha256:{}", "1".repeat(64)),
                    },
                    include_root: Some("src".into()),
                    dependencies: vec!["B".into()],
                },
                Locked {
                    name: "B".into(),
                    folder: "B".into(),
                    source: LockedSource::Path("../b".into()),
                    include_root: None,
                    dependencies: vec![],
                },
            ],
        };
        let text = lock.render();
        assert!(text.contains("[[package]]\nname = \"A\"\nfolder = \"A-lib\"\nsource = \"git+file:///r/a\"\ntag = \"v1.0.0\"\ninclude_root = \"src\"\n"));
        assert_eq!(Lock::parse(&text).unwrap(), lock);
        assert!(
            Lock::parse("version = 9\n")
                .unwrap_err()
                .0
                .contains("version 9")
        );
    }

    /// What a cloned project's lockfile could use to run a command or
    /// reach outside the project is refused when it is read.
    #[test]
    fn refuses_unsafe_fields() {
        let hash = format!("sha256:{}", "1".repeat(64));
        let pkg = |fields: &str| {
            format!("version = 1\ninputs = \"\"\n\n[[package]]\n{fields}\ndependencies = []\n")
        };
        let git = |name: &str, extra: &str, commit: &str| {
            pkg(&format!(
                "name = \"{name}\"\n{extra}source = \"git+https://h/r\"\ncommit = \"{commit}\"\nhash = \"{hash}\""
            ))
        };
        let good = "c".repeat(40);
        assert!(Lock::parse(&git("A", "", &good)).is_ok());
        assert!(Lock::parse(&git("A", "", &"d".repeat(64))).is_ok());
        for (text, want) in [
            (
                git("A", "", "--upload-pack=touch PWNED; git-upload-pack"),
                "full commit id",
            ),
            (git("A", "", &"C".repeat(40)), "full commit id"),
            (git("A", "", "abc1234"), "full commit id"),
            (git("A", "", "\u{e9}".repeat(40).as_str()), "full commit id"),
            (git("A", "folder = \"../..\"\n", &good), "folder"),
            (git("A", "folder = \"x&y\"\n", &good), "folder"),
            (git("../x", "", &good), "folder name"),
            (
                git("A", "include_root = \"../up\"\n", &good),
                "inside the repository",
            ),
            (
                pkg(&format!(
                    "name = \"A\"\nsource = \"git+https://h/r\"\ncommit = \"{good}\"\nhash = \"md5:00\""
                )),
                "`hash` must be",
            ),
            (
                pkg(&format!(
                    "name = \"A\"\nsource = \"git+--upload-pack=x\"\ncommit = \"{good}\"\nhash = \"{hash}\""
                )),
                "not a git URL",
            ),
        ] {
            let e = Lock::parse(&text).unwrap_err().0;
            assert!(e.contains(want), "{text}: {e}");
        }
        let many: String = (0..=MAX_PACKAGES)
            .map(|i| format!("\n[[package]]\nname = \"P{i}\"\nsource = \"path+p\"\n"))
            .collect();
        let e = Lock::parse(&format!("version = 1\n{many}")).unwrap_err().0;
        assert!(e.contains("more than the 1000"), "{e}");
    }
}
