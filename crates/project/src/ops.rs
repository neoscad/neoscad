//! The operations behind `neoscad add`, `remove`, `fetch`, `update`,
//! `tree` and `vendor`.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::error::{Context, Error, Result, bail};
use crate::link;
use crate::lock::{Lock, LockedSource};
use crate::manifest::{
    Dep, GitRef, LIBRARIES, LOCKFILE, MANIFEST, Manifest, Source, check_folder, new_manifest,
    remove_dependency, set_dependency,
};
pub use crate::resolve::Update;
use crate::resolve::{Resolver, Stats, inputs_hash, short};
use crate::store::Store;

/// The nearest directory at or above `start` with a `scad.toml`.
pub fn find_root(start: &Path) -> Option<PathBuf> {
    crate::search::find_root(start)
}

/// `<root>/.scad/libraries`.
pub fn managed_dir(root: &Path) -> PathBuf {
    root.join(LIBRARIES)
}

/// How `neoscad fetch` (and `update`, `add`, `remove`, `vendor`) runs.
#[derive(Debug, Clone, Default)]
pub struct FetchOptions {
    /// `--store`.
    pub store: Option<PathBuf>,
    /// Never reach the network: mirrors and checkouts already in the
    /// store only.
    pub offline: bool,
    /// Fail rather than change `scad.lock`.
    pub locked: bool,
    pub update: Update,
}

/// What a fetch did.
#[derive(Debug, Clone, Default)]
pub struct FetchReport {
    pub root: PathBuf,
    pub store: PathBuf,
    pub packages: Vec<Package>,
    /// Whether the lockfile was resolved again (not linked as it was).
    pub resolved: bool,
    pub lock_changed: bool,
    pub cloned: usize,
    pub fetched: usize,
    pub checked_out: usize,
    /// Links made or changed.
    pub linked: usize,
    /// Stale entries removed from `.scad/libraries`.
    pub unlinked: Vec<String>,
}

/// One dependency as fetched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Package {
    pub name: String,
    pub folder: String,
    /// `git+URL` or `path+DIR`.
    pub source: String,
    /// `tag v1`, `rev …`, `branch main`, `the default branch`, or `path`.
    pub requested: String,
    pub commit: Option<String>,
    pub hash: Option<String>,
    /// What `.scad/libraries/<folder>` leads to.
    pub target: PathBuf,
}

/// Resolve (where needed), check out and link `root`'s dependencies.
/// `progress` gets a line for each slow step.
pub fn fetch(
    root: &Path,
    opts: &FetchOptions,
    progress: &mut dyn FnMut(&str),
) -> Result<FetchReport> {
    let manifest = Manifest::read(root)?;
    let store = Store::open(opts.store.as_deref())?;
    let _guard = store.lock(|| progress("waiting for another neoscad to finish with the store"))?;
    let lock_path = root.join(LOCKFILE);
    let old = Lock::read(&lock_path)?;
    if let Update::Names(names) = &opts.update {
        for n in names {
            let known = manifest.deps.iter().any(|d| d.key == *n)
                || old.as_ref().is_some_and(|l| l.get(n).is_some());
            if !known {
                bail!("`{n}` is not a dependency of this project (see `neoscad tree`)");
            }
        }
    }
    let inputs = inputs_hash(&manifest)?;
    let current = old
        .as_ref()
        .filter(|l| l.inputs == inputs && opts.update == Update::Nothing);
    let mut report = FetchReport {
        root: root.to_path_buf(),
        store: store.root().to_path_buf(),
        ..Default::default()
    };
    let mut resolver;
    let mut lock = match current {
        Some(l) => {
            resolver = Resolver::new(&store, opts.offline, None, &opts.update);
            l.clone()
        }
        None => {
            if opts.locked {
                return Err(Error(match &old {
                    None => "--locked: there is no scad.lock; run `neoscad fetch` without --locked and commit it".to_string(),
                    Some(_) if opts.update != Update::Nothing => "--locked: an update would change scad.lock".into(),
                    Some(_) => "--locked: scad.lock is out of date with scad.toml; run `neoscad fetch` without --locked and commit scad.lock".into(),
                }));
            }
            report.resolved = true;
            resolver = Resolver::new(&store, opts.offline, old.as_ref(), &opts.update);
            resolver.resolve(&manifest)?
        }
    };

    // Check out what the store lacks, verify, and link.
    let libs = managed_dir(root);
    std::fs::create_dir_all(&libs).at("create", &libs)?;
    write_gitignore(root);
    let mut checkouts = Vec::new();
    let mut mirrors = Vec::new();
    let mut wanted = BTreeSet::new();
    for p in &mut lock.packages {
        let target = match &mut p.source {
            LockedSource::Git {
                url,
                reference,
                commit,
                hash,
            } => {
                let dir = store.checkout_path(url, commit, p.include_root.as_deref());
                let mirror = store.mirror_path(url);
                let actual = if dir.exists() {
                    crate::hash::tree_hash(&dir)?
                } else {
                    progress(&format!(
                        "checking out {} {} ({})",
                        p.name,
                        describe_ref(reference),
                        short(commit)
                    ));
                    resolver.mirror(&p.name, url)?;
                    resolver.ensure_commit(&p.name, url, &mirror, commit)?;
                    let h = store.materialize(&mirror, commit, p.include_root.as_deref(), &dir)?;
                    resolver.stats.checked_out += 1;
                    if !hash.is_empty() && h != *hash {
                        bail!(
                            "`{}` at {} checks out to content hash {h}, but scad.lock records {hash}; the lockfile is wrong or the commit's content changed (run `neoscad update {}` to accept it)",
                            p.name,
                            short(commit),
                            p.name
                        );
                    }
                    h
                };
                if hash.is_empty() {
                    *hash = actual;
                } else if actual != *hash {
                    bail!(
                        "the store's copy of `{}` ({}) was modified: its content hash is {actual}, scad.lock records {hash}. Delete that directory (it is read-only: make it writable first) and run `neoscad fetch` again",
                        p.name,
                        dir.display()
                    );
                }
                if mirror.exists() {
                    mirrors.push(mirror);
                }
                checkouts.push(dir.clone());
                report.packages.push(Package {
                    name: p.name.clone(),
                    folder: p.folder.clone(),
                    source: format!("git+{url}"),
                    requested: reference.describe(),
                    commit: Some(commit.clone()),
                    hash: Some(hash.clone()),
                    target: dir.clone(),
                });
                dir
            }
            LockedSource::Path(dir) => {
                let joined = root.join(&*dir);
                let mut target = std::fs::canonicalize(&joined).map_err(|e| {
                    Error(format!(
                        "path dependency `{}`: {}: {e}",
                        p.name,
                        joined.display()
                    ))
                })?;
                if let Some(r) = &p.include_root {
                    target = target.join(r);
                }
                report.packages.push(Package {
                    name: p.name.clone(),
                    folder: p.folder.clone(),
                    source: format!("path+{dir}"),
                    requested: "path".into(),
                    commit: None,
                    hash: None,
                    target: target.clone(),
                });
                target
            }
        };
        let at = libs.join(&p.folder);
        wanted.insert(p.folder.clone());
        if !link::points_to(&at, &target) {
            link::link_dir(&target, &at)?;
            report.linked += 1;
        }
    }
    // Entries no dependency has any more.
    if let Ok(rd) = std::fs::read_dir(&libs) {
        let mut stale: Vec<String> = rd
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| !wanted.contains(n))
            .collect();
        stale.sort();
        for n in stale {
            link::remove(&libs.join(&n))?;
            report.unlinked.push(n);
        }
    }
    let text = lock.render();
    let old_text = std::fs::read_to_string(&lock_path).ok();
    report.lock_changed = old_text.as_deref() != Some(text.as_str());
    if report.lock_changed {
        if opts.locked {
            bail!("--locked: scad.lock would change");
        }
        let tmp = root.join(format!("{LOCKFILE}.tmp"));
        std::fs::write(&tmp, &text).at("write", &tmp)?;
        std::fs::rename(&tmp, &lock_path).at("write", &lock_path)?;
    }
    store.record_project(root, &checkouts, &mirrors)?;
    let Stats {
        cloned,
        fetched,
        checked_out,
    } = resolver.stats;
    report.cloned = cloned;
    report.fetched = fetched;
    report.checked_out = checked_out;
    Ok(report)
}

fn describe_ref(r: &GitRef) -> String {
    match r {
        GitRef::Tag(t) => t.clone(),
        GitRef::Rev(_) => "rev".into(),
        GitRef::Branch(b) => format!("branch {b}"),
        GitRef::Default => "default branch".into(),
    }
}

/// `.scad/.gitignore` ignoring everything in `.scad`, so the links (which
/// name this machine's store) are never committed, with no change to the
/// project's own `.gitignore`.
fn write_gitignore(root: &Path) {
    let p = root.join(".scad").join(".gitignore");
    if !p.exists() {
        let _ = std::fs::write(
            &p,
            "# Written by neoscad: .scad/ holds this machine's links to its library store.\n*\n",
        );
    }
}

/// `neoscad add`: what to add.
#[derive(Debug, Clone)]
pub enum AddSpec {
    Git { url: String, reference: GitRef },
    Path(String),
}

/// Add (or replace) dependency `key` in the manifest at or above `start`,
/// creating `start/scad.toml` if there is none. Returns the project root.
pub fn add(
    start: &Path,
    key: &str,
    spec: AddSpec,
    include_root: Option<String>,
    name: Option<String>,
) -> Result<PathBuf> {
    check_folder(key).map_err(|e| Error(format!("`{key}`: {e}")))?;
    let root = match find_root(start) {
        Some(r) => r,
        None => {
            let name = start
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| "project".into());
            let p = start.join(MANIFEST);
            std::fs::write(&p, new_manifest(&name)).at("write", &p)?;
            start.to_path_buf()
        }
    };
    let path = root.join(MANIFEST);
    let text = std::fs::read_to_string(&path).at("read", &path)?;
    let source = match spec {
        AddSpec::Git { url, reference } => Source::Git { url, reference },
        AddSpec::Path(p) => Source::Path(p),
    };
    let dep = Dep {
        key: key.to_string(),
        folder: name.unwrap_or_else(|| key.to_string()),
        source,
        include_root,
    };
    let new = set_dependency(&text, &dep).map_err(|e| Error(format!("{}: {e}", path.display())))?;
    // Check it as the file it will be (folder clashes and so on).
    Manifest::parse(&root, &new).map_err(|e| Error(format!("{}: {e}", path.display())))?;
    std::fs::write(&path, new).at("write", &path)?;
    Ok(root)
}

/// Remove dependency `key` from the manifest of `root`.
pub fn remove(root: &Path, key: &str) -> Result<()> {
    let path = root.join(MANIFEST);
    let text = std::fs::read_to_string(&path).at("read", &path)?;
    match remove_dependency(&text, key)? {
        Some(new) => std::fs::write(&path, new).at("write", &path),
        None => bail!("`{key}` is not in {}", path.display()),
    }
}

/// `neoscad tree`: the locked graph, from the project down.
pub fn tree(root: &Path) -> Result<(String, Vec<Package>, Vec<String>)> {
    let manifest = Manifest::read(root)?;
    let Some(lock) = Lock::read(&root.join(LOCKFILE))? else {
        bail!("there is no scad.lock yet; run `neoscad fetch`");
    };
    let stale = lock.inputs != inputs_hash(&manifest)?;
    let mut out = manifest.display_name();
    out.push('\n');
    let roots: Vec<String> = manifest.deps.iter().map(|d| d.key.clone()).collect();
    fn walk(lock: &Lock, keys: &[String], prefix: &str, path: &mut Vec<String>, out: &mut String) {
        for (i, k) in keys.iter().enumerate() {
            let last = i + 1 == keys.len();
            out.push_str(prefix);
            out.push_str(if last { "└── " } else { "├── " });
            let Some(p) = lock.get(k) else {
                out.push_str(&format!("{k} (not in scad.lock)\n"));
                continue;
            };
            out.push_str(&describe(p));
            let seen = path.contains(k);
            if seen {
                out.push_str(" (*)\n");
                continue;
            }
            out.push('\n');
            path.push(k.clone());
            let next = format!("{prefix}{}", if last { "    " } else { "│   " });
            walk(lock, &p.dependencies, &next, path, out);
            path.pop();
        }
    }
    walk(&lock, &roots, "", &mut Vec::new(), &mut out);
    let mut notes = Vec::new();
    if stale {
        notes.push("scad.lock is out of date with scad.toml; run `neoscad fetch`".to_string());
    }
    let packages = lock
        .packages
        .iter()
        .map(|p| {
            let (source, requested, commit, hash) = match &p.source {
                LockedSource::Git {
                    url,
                    reference,
                    commit,
                    hash,
                } => (
                    format!("git+{url}"),
                    reference.describe(),
                    Some(commit.clone()),
                    Some(hash.clone()),
                ),
                LockedSource::Path(d) => (format!("path+{d}"), "path".into(), None, None),
            };
            Package {
                name: p.name.clone(),
                folder: p.folder.clone(),
                source,
                requested,
                commit,
                hash,
                target: managed_dir(root).join(&p.folder),
            }
        })
        .collect();
    Ok((out, packages, notes))
}

fn describe(p: &crate::lock::Locked) -> String {
    let folder = if p.folder != p.name {
        format!(" as {}", p.folder)
    } else {
        String::new()
    };
    match &p.source {
        LockedSource::Git {
            url,
            reference,
            commit,
            ..
        } => {
            let r = match reference {
                GitRef::Tag(t) => format!("{t} "),
                GitRef::Branch(b) => format!("{b} "),
                _ => String::new(),
            };
            format!("{}{folder} {r}({}) {url}", p.name, short(commit))
        }
        LockedSource::Path(d) => format!("{}{folder} (path {d})", p.name),
    }
}

/// What `neoscad vendor` wrote.
#[derive(Debug, Clone)]
pub struct VendorReport {
    pub dir: PathBuf,
    pub folders: Vec<String>,
}

/// Copy the fetched dependencies of `root` into `dir` (default
/// `<root>/libraries`), one folder each, as plain files: a library folder
/// stock OpenSCAD reads through `OPENSCADPATH`. Folders of `dir` that are
/// not dependencies are left alone.
pub fn vendor(root: &Path, report: &FetchReport, dir: Option<&Path>) -> Result<VendorReport> {
    let dir = match dir {
        Some(d) if d.is_absolute() => d.to_path_buf(),
        Some(d) => std::env::current_dir().unwrap_or_default().join(d),
        None => root.join("libraries"),
    };
    std::fs::create_dir_all(&dir).at("create", &dir)?;
    let dir = std::fs::canonicalize(&dir).unwrap_or(dir);
    if dir.starts_with(managed_dir(root)) {
        bail!("vendor into a folder outside .scad/");
    }
    let mut folders = Vec::new();
    for p in &report.packages {
        let to = dir.join(&p.folder);
        link::remove(&to)?;
        link::copy_tree(&p.target, &to)?;
        folders.push(p.folder.clone());
    }
    Ok(VendorReport { dir, folders })
}
