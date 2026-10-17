//! The per-machine store every project's dependencies come from:
//!
//! ```text
//! <store>/
//!   git/<label>-<url hash>.git          one bare mirror per remote URL
//!   checkouts/<label>-<commit>[-<root hash>]/
//!                                       one read-only tree per commit
//!                                       (and include_root), no .git
//!   projects/<root hash>.toml           which checkouts and mirrors each
//!                                       project links, for `store gc`
//!   tmp/                                checkouts and clones being made
//!   lock                                held while the store changes
//!   empty-gitconfig                     the configuration checkouts see
//! ```
//!
//! `<store>` is `NEOSCAD_STORE` or `--store`, else the platform's cache
//! directory: `~/Library/Caches/NeoSCAD/store` on macOS,
//! `$XDG_CACHE_HOME/neoscad/store` (`~/.cache/neoscad/store`) on Linux and
//! `%LOCALAPPDATA%\NeoSCAD\store` on Windows.
//!
//! A new revision of a known remote costs a `git fetch` of the delta into
//! its mirror. A checkout is written into `tmp/`, hashed, renamed into
//! place and made read-only, so a reader never sees half of one; all of
//! this happens under `lock`, so two projects fetching at once take turns
//! and the second finds the first's checkout.

use std::collections::BTreeSet;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde::Deserialize;

use crate::error::{Context, Result, bail};
use crate::git::Git;
use crate::hash::digest;
use crate::manifest::{MANIFEST, quote};

/// The environment variable that moves the store.
pub const STORE_ENV: &str = "NEOSCAD_STORE";

/// An open store.
#[derive(Debug, Clone)]
pub struct Store {
    root: PathBuf,
    git: Git,
}

/// Held while the store changes; released when dropped.
#[derive(Debug)]
pub struct StoreLock {
    _file: File,
}

/// Where the store is by default (see the module documentation).
pub fn default_root() -> Option<PathBuf> {
    let cache = dirs::cache_dir()?;
    Some(if cfg!(any(target_os = "macos", windows)) {
        cache.join("NeoSCAD").join("store")
    } else {
        cache.join("neoscad").join("store")
    })
}

/// The store's location: `explicit`, else `NEOSCAD_STORE`, else
/// [`default_root`].
pub fn locate(explicit: Option<&Path>) -> Result<PathBuf> {
    if let Some(p) = explicit {
        return Ok(absolute(p));
    }
    if let Some(p) = std::env::var_os(STORE_ENV).filter(|v| !v.is_empty()) {
        return Ok(absolute(Path::new(&p)));
    }
    match default_root() {
        Some(p) => Ok(p),
        None => bail!("cannot find a cache directory for the store; set {STORE_ENV}"),
    }
}

fn absolute(p: &Path) -> PathBuf {
    std::path::absolute(p).unwrap_or_else(|_| p.to_path_buf())
}

impl Store {
    /// Open (creating) the store at [`locate`]`(explicit)`.
    pub fn open(explicit: Option<&Path>) -> Result<Store> {
        let root = locate(explicit)?;
        for d in ["git", "checkouts", "projects", "tmp"] {
            let p = root.join(d);
            std::fs::create_dir_all(&p).at("create", &p)?;
        }
        let empty = root.join("empty-gitconfig");
        if !empty.exists() {
            std::fs::write(&empty, b"").at("write", &empty)?;
        }
        // Canonical, so links and the index name the store the same way
        // whatever spelling found it.
        let root = std::fs::canonicalize(&root).unwrap_or(root);
        Ok(Store {
            git: Git::new(root.join("empty-gitconfig")),
            root,
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn git(&self) -> &Git {
        &self.git
    }

    /// Wait for the store's lock; `waiting` is called first if another
    /// process holds it.
    pub fn lock(&self, waiting: impl FnOnce()) -> Result<StoreLock> {
        let p = self.root.join("lock");
        let f = File::options()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&p)
            .at("open", &p)?;
        match f.try_lock() {
            Ok(()) => {}
            Err(std::fs::TryLockError::WouldBlock) => {
                waiting();
                f.lock().at("lock", &p)?;
            }
            Err(std::fs::TryLockError::Error(e)) => {
                return Err(e).at("lock", &p);
            }
        }
        Ok(StoreLock { _file: f })
    }

    /// The mirror of `url`.
    pub fn mirror_path(&self, url: &str) -> PathBuf {
        let n = normalize_url(url);
        self.root
            .join("git")
            .join(format!("{}-{}.git", label(&n), &digest(&n)[..16]))
    }

    /// The checkout of `commit` of `url` (its `include_root` folder).
    pub fn checkout_path(&self, url: &str, commit: &str, include_root: Option<&str>) -> PathBuf {
        let n = normalize_url(url);
        let mut name = format!("{}-{}", label(&n), &commit[..commit.len().min(12)]);
        if let Some(r) = include_root {
            name.push('-');
            name.push_str(&digest(r)[..8]);
        }
        self.root.join("checkouts").join(name)
    }

    /// A fresh directory under `tmp/`.
    fn scratch(&self) -> Result<PathBuf> {
        static N: AtomicU64 = AtomicU64::new(0);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.subsec_nanos());
        let p = self.root.join("tmp").join(format!(
            "{}-{}-{nanos}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&p).at("create", &p)?;
        Ok(p)
    }

    /// Clone `url` into its mirror (through `tmp/`, renamed into place).
    pub fn clone_mirror(&self, url: &str) -> Result<PathBuf> {
        let dest = self.mirror_path(url);
        let tmp = self.scratch()?;
        let staged = tmp.join("mirror.git");
        let r = self
            .git
            .clone_bare(url, &staged)
            .and_then(|()| std::fs::rename(&staged, &dest).at("create", &dest));
        let _ = std::fs::remove_dir_all(&tmp);
        r.map(|()| dest)
    }

    /// Write `commit` (its `include_root` folder) of `mirror` to `dest`,
    /// read-only, and return its content hash.
    pub fn materialize(
        &self,
        mirror: &Path,
        commit: &str,
        include_root: Option<&str>,
        dest: &Path,
    ) -> Result<String> {
        let tmp = self.scratch()?;
        let tree = tmp.join("tree");
        let r = (|| {
            std::fs::create_dir_all(&tree).at("create", &tree)?;
            self.git
                .checkout(mirror, commit, include_root, &tree, &tmp)?;
            let hash = crate::hash::tree_hash(&tree)?;
            // Renamed first: moving a directory needs it writable.
            std::fs::rename(&tree, dest).at("create", dest)?;
            set_tree_readonly(dest);
            Ok(hash)
        })();
        make_writable(&tmp);
        let _ = std::fs::remove_dir_all(&tmp);
        r
    }

    /// Record that the project at `root` links `checkouts` and uses
    /// `mirrors` (paths in this store), replacing what it linked before.
    pub fn record_project(
        &self,
        root: &Path,
        checkouts: &[PathBuf],
        mirrors: &[PathBuf],
    ) -> Result<()> {
        let name = |p: &PathBuf| {
            p.file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default()
        };
        let list = |v: &[PathBuf]| {
            let set: BTreeSet<String> = v.iter().map(name).collect();
            set.iter().map(|s| quote(s)).collect::<Vec<_>>().join(", ")
        };
        let text = format!(
            "root = {}\ncheckouts = [{}]\nmirrors = [{}]\n",
            quote(&root.to_string_lossy()),
            list(checkouts),
            list(mirrors)
        );
        let path = self.project_file(root);
        let tmp = path.with_extension(format!("toml.{}", std::process::id()));
        std::fs::write(&tmp, text).at("write", &tmp)?;
        std::fs::rename(&tmp, &path).at("write", &path)
    }

    fn project_file(&self, root: &Path) -> PathBuf {
        self.root
            .join("projects")
            .join(format!("{}.toml", &digest(&root.to_string_lossy())[..16]))
    }

    /// Every project the index knows, with whether it still exists (its
    /// `scad.toml` is there).
    pub fn projects(&self) -> Vec<ProjectRecord> {
        let mut out = Vec::new();
        let dir = self.root.join("projects");
        let Ok(rd) = std::fs::read_dir(&dir) else {
            return out;
        };
        for e in rd.flatten() {
            let p = e.path();
            if p.extension().is_none_or(|x| x != "toml") {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&p) else {
                continue;
            };
            let Ok(r) = basic_toml::from_str::<RawRecord>(&text) else {
                continue;
            };
            let root = PathBuf::from(&r.root);
            out.push(ProjectRecord {
                live: root.join(MANIFEST).is_file(),
                root,
                checkouts: r.checkouts,
                mirrors: r.mirrors,
                file: p,
            });
        }
        out.sort_by(|a, b| a.root.cmp(&b.root));
        out
    }

    /// What the store holds and who uses it.
    pub fn list(&self) -> Listing {
        let projects = self.projects();
        let users = |kind: &str, name: &str| -> Vec<PathBuf> {
            projects
                .iter()
                .filter(|p| p.live)
                .filter(|p| {
                    let v = if kind == "checkouts" {
                        &p.checkouts
                    } else {
                        &p.mirrors
                    };
                    v.iter().any(|n| n == name)
                })
                .map(|p| p.root.clone())
                .collect()
        };
        let mut entries = Vec::new();
        for kind in ["git", "checkouts"] {
            for name in dir_names(&self.root.join(kind)) {
                let path = self.root.join(kind).join(&name);
                entries.push(Entry {
                    kind: if kind == "git" { "mirror" } else { "checkout" },
                    bytes: dir_size(&path),
                    used_by: users(
                        if kind == "git" {
                            "mirrors"
                        } else {
                            "checkouts"
                        },
                        &name,
                    ),
                    name,
                    path,
                });
            }
        }
        Listing {
            root: self.root.clone(),
            entries,
            projects: projects
                .into_iter()
                .filter(|p| p.live)
                .map(|p| p.root)
                .collect(),
        }
    }

    /// Remove the checkouts and mirrors no live project uses, the index
    /// entries of projects that are gone and anything left in `tmp/`.
    /// With `dry_run`, only say what would go.
    pub fn gc(&self, dry_run: bool) -> Result<GcReport> {
        let _lock = self.lock(|| {})?;
        let projects = self.projects();
        let mut keep_checkouts = BTreeSet::new();
        let mut keep_mirrors = BTreeSet::new();
        let mut report = GcReport::default();
        for p in &projects {
            if p.live {
                keep_checkouts.extend(p.checkouts.iter().cloned());
                keep_mirrors.extend(p.mirrors.iter().cloned());
            } else {
                report.forgotten.push(p.root.clone());
                if !dry_run {
                    let _ = std::fs::remove_file(&p.file);
                }
            }
        }
        for (kind, keep) in [("checkouts", &keep_checkouts), ("git", &keep_mirrors)] {
            for name in dir_names(&self.root.join(kind)) {
                if keep.contains(&name) {
                    continue;
                }
                let path = self.root.join(kind).join(&name);
                report.bytes += dir_size(&path);
                if !dry_run {
                    make_writable(&path);
                    std::fs::remove_dir_all(&path).at("remove", &path)?;
                }
                report.removed.push(path);
            }
        }
        if !dry_run {
            for name in dir_names(&self.root.join("tmp")) {
                let p = self.root.join("tmp").join(name);
                make_writable(&p);
                let _ = std::fs::remove_dir_all(&p);
            }
        }
        Ok(report)
    }
}

#[derive(Deserialize)]
struct RawRecord {
    root: String,
    #[serde(default)]
    checkouts: Vec<String>,
    #[serde(default)]
    mirrors: Vec<String>,
}

/// One project in the store's index.
#[derive(Debug, Clone)]
pub struct ProjectRecord {
    pub root: PathBuf,
    pub live: bool,
    pub checkouts: Vec<String>,
    pub mirrors: Vec<String>,
    file: PathBuf,
}

/// `store list`.
#[derive(Debug, Clone)]
pub struct Listing {
    pub root: PathBuf,
    pub entries: Vec<Entry>,
    /// The live projects the index knows.
    pub projects: Vec<PathBuf>,
}

/// A mirror or a checkout.
#[derive(Debug, Clone)]
pub struct Entry {
    /// `mirror` or `checkout`.
    pub kind: &'static str,
    pub name: String,
    pub path: PathBuf,
    pub bytes: u64,
    pub used_by: Vec<PathBuf>,
}

/// `store gc`.
#[derive(Debug, Clone, Default)]
pub struct GcReport {
    pub removed: Vec<PathBuf>,
    pub bytes: u64,
    /// Projects dropped from the index because their `scad.toml` is gone.
    pub forgotten: Vec<PathBuf>,
}

/// A URL as the store keys mirrors: no trailing `/` or `.git`, the scheme
/// and host in lower case.
pub fn normalize_url(url: &str) -> String {
    let mut u = url.trim().trim_end_matches('/').to_string();
    if let Some(s) = u.strip_suffix(".git") {
        u = s.to_string();
    }
    if let Some(i) = u.find("://") {
        let rest = &u[i + 3..];
        let host_end = rest.find('/').map_or(u.len(), |j| i + 3 + j);
        let (head, tail) = u.split_at(host_end);
        return format!("{}{tail}", head.to_ascii_lowercase());
    }
    // scp-like `git@host:path`.
    if let Some((head, tail)) = u.split_once(':')
        && head.contains('@')
        && !head.contains('/')
    {
        return format!("{}:{tail}", head.to_ascii_lowercase());
    }
    u
}

/// A readable prefix for store names: the URL's last path component.
fn label(normal_url: &str) -> String {
    let last = normal_url
        .rsplit(['/', ':', '\\'])
        .find(|s| !s.is_empty())
        .unwrap_or("repo");
    let l: String = last
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
                c
            } else {
                '_'
            }
        })
        .take(40)
        .collect();
    let l = l.trim_start_matches('.').to_string();
    if l.is_empty() { "repo".into() } else { l }
}

fn dir_names(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir)
        .map(|rd| {
            rd.flatten()
                .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v
}

/// The bytes of the files under `p` (links not followed).
pub fn dir_size(p: &Path) -> u64 {
    let Ok(m) = std::fs::symlink_metadata(p) else {
        return 0;
    };
    if !m.is_dir() {
        return m.len();
    }
    std::fs::read_dir(p)
        .map(|rd| rd.flatten().map(|e| dir_size(&e.path())).sum())
        .unwrap_or(0)
}

/// Make one file writable by its owner (or read-only).
pub fn set_writable(p: &Path, writable: bool) {
    let Ok(m) = std::fs::symlink_metadata(p) else {
        return;
    };
    if m.file_type().is_symlink() {
        return;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = m.permissions().mode();
        let mode = if writable {
            mode | 0o200
        } else {
            mode & !0o222
        };
        let _ = std::fs::set_permissions(p, std::fs::Permissions::from_mode(mode));
    }
    #[cfg(not(unix))]
    {
        let mut perm = m.permissions();
        #[allow(clippy::permissions_set_readonly_false)]
        perm.set_readonly(!writable);
        let _ = std::fs::set_permissions(p, perm);
    }
}

/// Make a checkout read-only: every file, and on Unix every directory
/// (so nothing can be added or renamed in it either).
fn set_tree_readonly(p: &Path) {
    if let Ok(rd) = std::fs::read_dir(p) {
        for e in rd.flatten() {
            let path = e.path();
            if e.file_type().is_ok_and(|t| t.is_dir()) {
                set_tree_readonly(&path);
            } else {
                set_writable(&path, false);
            }
        }
    }
    if cfg!(unix) {
        set_writable(p, false);
    }
}

/// Undo [`set_tree_readonly`] under `p`, so it can be removed.
pub fn make_writable(p: &Path) {
    let Ok(m) = std::fs::symlink_metadata(p) else {
        return;
    };
    if m.file_type().is_symlink() {
        return;
    }
    set_writable(p, true);
    if m.is_dir()
        && let Ok(rd) = std::fs::read_dir(p)
    {
        for e in rd.flatten() {
            make_writable(&e.path());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_normalize() {
        assert_eq!(
            normalize_url("HTTPS://GitHub.com/BelfrySCAD/BOSL2.git/"),
            "https://github.com/BelfrySCAD/BOSL2"
        );
        assert_eq!(
            normalize_url("git@GitHub.com:a/B.git"),
            "git@github.com:a/B"
        );
        assert_eq!(normalize_url("file:///tmp/x/repo"), "file:///tmp/x/repo");
        assert_eq!(label("https://github.com/BelfrySCAD/BOSL2"), "BOSL2");
        assert_eq!(label("git@github.com:a/B"), "B");
    }
}
