//! Resolution: from the project's manifest (and its dependencies' own
//! manifests) to one revision per dependency key.
//!
//! The graph is walked breadth first from the project, every
//! requirement becoming a node (a git requirement resolved to a commit in
//! its mirror, a path requirement to its directory); a dependency's
//! `scad.toml`, at its commit, adds its own requirements. Then each key
//! gets one node, by minimal version selection as Go modules do it: when
//! every requirement of a key is a tag that reads as a version
//! (`v1.2.3`), the highest wins; otherwise they must all name the same
//! commit, or the key is a conflict and the error names two requirers
//! that disagree. Sources must agree too (one URL, one `include_root`,
//! one folder name). The selected graph is then pruned to what the
//! project reaches through selected nodes.
//!
//! A requirement that matches a `scad.lock` entry exactly (key, URL,
//! revision, `include_root`) keeps the locked commit unless the key is
//! being updated, so a fetch never moves a branch by itself; an update
//! asks the remotes again.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::{Path, PathBuf};

use crate::error::{Error, Result, bail};
use crate::lock::{Lock, Locked, LockedSource};
use crate::manifest::{Dep, GitRef, MANIFEST, Manifest, Source};
use crate::store::{Store, normalize_url};
use crate::version::Version;

/// Which keys an update asks the remotes about.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Update {
    /// None: keep what the lockfile has where it still applies.
    #[default]
    Nothing,
    All,
    Names(Vec<String>),
}

impl Update {
    fn covers(&self, key: &str) -> bool {
        match self {
            Update::Nothing => false,
            Update::All => true,
            Update::Names(n) => n.iter().any(|k| k == key),
        }
    }
}

/// Git work done, for reports and tests.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Stats {
    pub cloned: usize,
    pub fetched: usize,
    pub checked_out: usize,
}

/// One requirement: `requirer` wants `dep`.
#[derive(Debug, Clone)]
struct Req {
    requirer: String,
    key: String,
    folder: String,
    include_root: Option<String>,
    source: ReqSource,
}

#[derive(Debug, Clone)]
enum ReqSource {
    Git {
        url: String,
        reference: GitRef,
    },
    /// The directory, and how the lockfile writes it (relative to the
    /// project root where it can be).
    Path {
        dir: PathBuf,
        written: String,
    },
}

impl Req {
    fn from_dep(requirer: &str, base: &Path, root: &Path, d: &Dep) -> Result<Req> {
        let source = match &d.source {
            Source::Git { url, reference } => ReqSource::Git {
                url: url.clone(),
                reference: reference.clone(),
            },
            Source::Path(p) => {
                let joined = base.join(p);
                let dir = std::fs::canonicalize(&joined).map_err(|e| {
                    Error(format!(
                        "path dependency `{}` of `{requirer}`: {}: {e}",
                        d.key,
                        joined.display()
                    ))
                })?;
                if !dir.is_dir() {
                    bail!(
                        "path dependency `{}` of `{requirer}`: {} is not a directory",
                        d.key,
                        dir.display()
                    );
                }
                let written = relative(root, &dir);
                ReqSource::Path { dir, written }
            }
        };
        Ok(Req {
            requirer: requirer.to_string(),
            key: d.key.clone(),
            folder: d.folder.clone(),
            include_root: d.include_root.clone(),
            source,
        })
    }

    /// What makes two requirements the same node.
    fn identity(&self) -> String {
        let src = match &self.source {
            ReqSource::Git { url, reference } => {
                format!("git {} {reference:?}", normalize_url(url))
            }
            ReqSource::Path { dir, .. } => format!("path {}", dir.display()),
        };
        format!(
            "{}\0{}\0{:?}\0{src}",
            self.key, self.folder, self.include_root
        )
    }

    /// For the lockfile's `inputs`: the requirement as declared, with no
    /// machine-specific path in it.
    fn input_line(&self) -> String {
        let src = match &self.source {
            ReqSource::Git { url, reference } => format!("git+{url} {}", reference.toml()),
            ReqSource::Path { written, .. } => format!("path+{written}"),
        };
        format!(
            "{}\0{}\0{}\0{}\0{src}",
            self.requirer,
            self.key,
            self.folder,
            self.include_root.as_deref().unwrap_or("")
        )
    }

    fn describe(&self) -> String {
        match &self.source {
            ReqSource::Git { url, reference } => format!("{} of {url}", reference.describe()),
            ReqSource::Path { written, .. } => format!("path {written}"),
        }
    }
}

/// A resolved requirement.
#[derive(Debug, Clone)]
struct Node {
    req: Req,
    requirers: Vec<String>,
    /// Git: the commit.
    commit: Option<String>,
    /// The keys this node's own manifest requires.
    deps: Vec<String>,
}

/// The project's requirements and its path dependencies', transitively
/// (these need no git): the lockfile's `inputs` hash covers them.
pub fn inputs_hash(manifest: &Manifest) -> Result<String> {
    let mut lines = BTreeSet::new();
    let mut seen = BTreeSet::new();
    // The project itself is the empty requirer, so renaming it changes
    // nothing.
    let mut queue = VecDeque::from([(manifest.clone(), String::new())]);
    while let Some((m, name)) = queue.pop_front() {
        for d in &m.deps {
            let r = Req::from_dep(&name, &m.root, &manifest.root, d)?;
            lines.insert(r.input_line());
            if let ReqSource::Path { dir, .. } = &r.source
                && seen.insert(dir.clone())
                && dir.join(MANIFEST).is_file()
            {
                queue.push_back((Manifest::read(dir)?, d.key.clone()));
            }
        }
    }
    let joined: Vec<String> = lines.into_iter().collect();
    Ok(format!(
        "{}{}",
        crate::hash::PREFIX,
        crate::hash::digest(&joined.join("\n"))
    ))
}

/// Resolve `manifest`'s graph. The lock it returns has an empty `hash`
/// for git packages that are not in `prefer` unchanged (the caller
/// materialises and hashes them).
pub struct Resolver<'a> {
    pub store: &'a Store,
    pub offline: bool,
    pub prefer: Option<&'a Lock>,
    pub update: &'a Update,
    /// The command line's name, for the commands messages suggest.
    pub program: &'a str,
    pub stats: Stats,
    fetched: BTreeSet<PathBuf>,
}

impl<'a> Resolver<'a> {
    pub fn new(
        store: &'a Store,
        offline: bool,
        prefer: Option<&'a Lock>,
        update: &'a Update,
        program: &'a str,
    ) -> Resolver<'a> {
        Resolver {
            store,
            offline,
            prefer,
            update,
            program,
            stats: Stats::default(),
            fetched: BTreeSet::new(),
        }
    }

    pub fn resolve(&mut self, manifest: &Manifest) -> Result<Lock> {
        let root_name = manifest.display_name();
        let mut queue: VecDeque<Req> = VecDeque::new();
        for d in &manifest.deps {
            queue.push_back(Req::from_dep(
                &root_name,
                &manifest.root,
                &manifest.root,
                d,
            )?);
        }
        let roots: Vec<String> = manifest.deps.iter().map(|d| d.key.clone()).collect();
        let mut nodes: Vec<Node> = Vec::new();
        let mut by_identity: BTreeMap<String, usize> = BTreeMap::new();
        while let Some(req) = queue.pop_front() {
            let id = req.identity();
            if let Some(&i) = by_identity.get(&id) {
                if !nodes[i].requirers.contains(&req.requirer) {
                    nodes[i].requirers.push(req.requirer.clone());
                }
                continue;
            }
            let (commit, children) = match &req.source {
                ReqSource::Git { url, reference } => {
                    let (mirror, commit) =
                        self.commit_for(&req.key, url, reference, &req.include_root)?;
                    let git = self.store.git();
                    if let Some(r) = &req.include_root
                        && !git.has_dir(&mirror, &commit, r)
                    {
                        bail!(
                            "`{}`: include_root '{r}' is not a folder at {} of {url}",
                            req.key,
                            short(&commit)
                        );
                    }
                    let text = git.show(&mirror, &commit, MANIFEST).or_else(|| {
                        req.include_root
                            .as_ref()
                            .and_then(|r| git.show(&mirror, &commit, &format!("{r}/{MANIFEST}")))
                    });
                    let mut children = Vec::new();
                    if let Some(text) = text {
                        let text = String::from_utf8_lossy(&text);
                        let m = Manifest::parse(Path::new("."), &text).map_err(|e| {
                            Error(format!(
                                "`{}`'s scad.toml at {}: {e}",
                                req.key,
                                short(&commit)
                            ))
                        })?;
                        for d in &m.deps {
                            if let Source::Path(p) = &d.source {
                                bail!(
                                    "`{}` (a git dependency) declares the path dependency `{}` ({p}), which cannot be found outside its repository; make it a git dependency",
                                    req.key,
                                    d.key
                                );
                            }
                            children.push(Req::from_dep(
                                &req.key,
                                Path::new("."),
                                &manifest.root,
                                d,
                            )?);
                        }
                    }
                    (Some(commit), children)
                }
                ReqSource::Path { dir, .. } => {
                    let mut children = Vec::new();
                    if dir.join(MANIFEST).is_file() {
                        let m = Manifest::read(dir)?;
                        for d in &m.deps {
                            children.push(Req::from_dep(&req.key, dir, &manifest.root, d)?);
                        }
                    }
                    (None, children)
                }
            };
            let mut deps: Vec<String> = children.iter().map(|c| c.key.clone()).collect();
            deps.sort();
            deps.dedup();
            by_identity.insert(id, nodes.len());
            nodes.push(Node {
                requirers: vec![req.requirer.clone()],
                req,
                commit,
                deps,
            });
            queue.extend(children);
        }

        // One node per key.
        let mut by_key: BTreeMap<String, Vec<usize>> = BTreeMap::new();
        for (i, n) in nodes.iter().enumerate() {
            by_key.entry(n.req.key.clone()).or_default().push(i);
        }
        let mut selected: BTreeMap<String, usize> = BTreeMap::new();
        for (key, ix) in &by_key {
            selected.insert(key.clone(), select(key, ix, &nodes)?);
        }
        // What the project reaches through the selected nodes.
        let mut reached = BTreeSet::new();
        let mut stack: Vec<String> = roots;
        while let Some(k) = stack.pop() {
            if reached.insert(k.clone())
                && let Some(&i) = selected.get(&k)
            {
                stack.extend(nodes[i].deps.iter().cloned());
            }
        }
        let mut folders: BTreeMap<String, String> = BTreeMap::new();
        let mut packages = Vec::new();
        for key in &reached {
            let n = &nodes[selected[key]];
            if let Some(other) = folders.insert(n.req.folder.clone(), key.clone()) {
                bail!(
                    "dependencies `{other}` and `{key}` both use the folder name `{}`; give one a different `name`",
                    n.req.folder
                );
            }
            let source = match &n.req.source {
                ReqSource::Git { url, reference } => {
                    let commit = n.commit.clone().expect("git nodes have commits");
                    let hash = self
                        .prefer
                        .and_then(|l| l.get(key))
                        .and_then(|l| match &l.source {
                            LockedSource::Git {
                                url: u,
                                commit: c,
                                hash,
                                ..
                            } if *c == commit
                                && normalize_url(u) == normalize_url(url)
                                && l.include_root == n.req.include_root =>
                            {
                                Some(hash.clone())
                            }
                            _ => None,
                        })
                        .unwrap_or_default();
                    LockedSource::Git {
                        url: url.clone(),
                        reference: reference.clone(),
                        commit,
                        hash,
                    }
                }
                ReqSource::Path { written, .. } => LockedSource::Path(written.clone()),
            };
            packages.push(Locked {
                name: key.clone(),
                folder: n.req.folder.clone(),
                source,
                include_root: n.req.include_root.clone(),
                dependencies: n.deps.clone(),
            });
        }
        Ok(Lock {
            inputs: inputs_hash(manifest)?,
            packages,
        })
    }

    /// Fetch `mirror` from `url` once per resolution.
    fn fetch_once(&mut self, mirror: &Path, url: &str) -> Result<bool> {
        if self.offline || self.fetched.contains(mirror) {
            return Ok(false);
        }
        self.store.git().fetch(mirror, url)?;
        self.stats.fetched += 1;
        self.fetched.insert(mirror.to_path_buf());
        Ok(true)
    }

    /// The mirror of `url`, cloned if the store has none.
    pub fn mirror(&mut self, key: &str, url: &str) -> Result<PathBuf> {
        let mirror = self.store.mirror_path(url);
        if !mirror.exists() {
            if self.offline {
                bail!(
                    "`{key}` ({url}) is not in the store, and --offline does not fetch; run `{} fetch` once with the network",
                    self.program
                );
            }
            self.store.clone_mirror(url)?;
            self.stats.cloned += 1;
            self.fetched.insert(mirror.clone());
        }
        Ok(mirror)
    }

    /// Make sure `mirror` has `commit` (fetching it if not).
    pub fn ensure_commit(
        &mut self,
        key: &str,
        url: &str,
        mirror: &Path,
        commit: &str,
    ) -> Result<()> {
        let git = self.store.git();
        if git.commit_of(mirror, commit).is_some() {
            return Ok(());
        }
        if self.fetch_once(mirror, url)? && self.store.git().commit_of(mirror, commit).is_some() {
            return Ok(());
        }
        if !self.offline {
            let _ = self.store.git().fetch_commit(mirror, url, commit);
            if self.store.git().commit_of(mirror, commit).is_some() {
                return Ok(());
            }
        }
        bail!(
            "`{key}`: commit {commit} is not in {url}{}",
            if self.offline {
                " as the store has it (--offline does not fetch)"
            } else {
                " (was the history rewritten?)"
            }
        )
    }

    fn commit_for(
        &mut self,
        key: &str,
        url: &str,
        reference: &GitRef,
        include_root: &Option<String>,
    ) -> Result<(PathBuf, String)> {
        let mirror = self.mirror(key, url)?;
        let updating = self.update.covers(key);
        if updating {
            self.fetch_once(&mirror, url)?;
        }
        let locked = self
            .prefer
            .and_then(|l| l.get(key))
            .and_then(|l| match &l.source {
                LockedSource::Git {
                    url: u,
                    reference: r,
                    commit,
                    ..
                } if normalize_url(u) == normalize_url(url)
                    && r == reference
                    && l.include_root == *include_root =>
                {
                    Some(commit.clone())
                }
                _ => None,
            });
        if let Some(c) = locked
            && !updating
        {
            self.ensure_commit(key, url, &mirror, &c)?;
            return Ok((mirror, c));
        }
        if reference.moves() {
            self.fetch_once(&mirror, url)?;
        }
        let spec = match reference {
            GitRef::Tag(t) => format!("refs/tags/{t}"),
            GitRef::Branch(b) => format!("refs/heads/{b}"),
            GitRef::Default => "HEAD".into(),
            GitRef::Rev(r) => r.clone(),
        };
        if let Some(c) = self.store.git().commit_of(&mirror, &spec) {
            return Ok((mirror, c));
        }
        if self.fetch_once(&mirror, url)?
            && let Some(c) = self.store.git().commit_of(&mirror, &spec)
        {
            return Ok((mirror, c));
        }
        if let GitRef::Rev(r) = reference
            && r.len() == 40
            && !self.offline
        {
            let _ = self.store.git().fetch_commit(&mirror, url, r);
            if let Some(c) = self.store.git().commit_of(&mirror, r) {
                return Ok((mirror, c));
            }
        }
        bail!(
            "`{key}`: {} is not in {url}{}",
            reference.describe(),
            if self.offline {
                " as the store has it (--offline does not fetch)"
            } else {
                ""
            }
        )
    }
}

/// The node `key` resolves to, among its requirements `ix`.
fn select(key: &str, ix: &[usize], nodes: &[Node]) -> Result<usize> {
    let first = &nodes[ix[0]];
    let conflict = |a: &Node, b: &Node, why: &str| -> Error {
        Error(format!(
            "conflicting requirements for `{key}`: `{}` wants {}{}, `{}` wants {}{}; {why}",
            a.requirers[0],
            a.req.describe(),
            a.commit
                .as_ref()
                .map(|c| format!(" (commit {})", short(c)))
                .unwrap_or_default(),
            b.requirers[0],
            b.req.describe(),
            b.commit
                .as_ref()
                .map(|c| format!(" (commit {})", short(c)))
                .unwrap_or_default(),
        ))
    };
    if ix.len() == 1 {
        return Ok(ix[0]);
    }
    for &i in &ix[1..] {
        let n = &nodes[i];
        let same_source = match (&first.req.source, &n.req.source) {
            (ReqSource::Git { url: a, .. }, ReqSource::Git { url: b, .. }) => {
                normalize_url(a) == normalize_url(b)
            }
            (ReqSource::Path { dir: a, .. }, ReqSource::Path { dir: b, .. }) => a == b,
            _ => false,
        };
        if !same_source {
            return Err(conflict(
                first,
                n,
                "one library is used per name, so point both at the same source",
            ));
        }
        if n.req.include_root != first.req.include_root || n.req.folder != first.req.folder {
            return Err(conflict(
                first,
                n,
                "they give different `include_root` or `name` values",
            ));
        }
    }
    // Minimal version selection over tags that read as versions.
    let versions: Option<Vec<(Version, usize)>> = ix
        .iter()
        .map(|&i| match &nodes[i].req.source {
            ReqSource::Git {
                reference: GitRef::Tag(t),
                ..
            } => Version::parse(t).map(|v| (v, i)),
            _ => None,
        })
        .collect();
    if let Some(vs) = versions {
        let mut best = vs[0].clone();
        for v in &vs[1..] {
            if v.0 > best.0 {
                best = v.clone();
            }
        }
        return Ok(best.1);
    }
    for &i in &ix[1..] {
        if nodes[i].commit != first.commit {
            return Err(conflict(
                first,
                &nodes[i],
                "exact revisions must agree (only version tags like v1.2.3 are reconciled), so make them name the same commit",
            ));
        }
    }
    Ok(ix[0])
}

/// The first 7 characters of a commit id.
pub fn short(c: &str) -> &str {
    &c[..c.len().min(7)]
}

/// `target` relative to `base` (`/`-separated), or absolute where it
/// cannot be (another drive).
pub fn relative(base: &Path, target: &Path) -> String {
    let b: Vec<_> = base.components().collect();
    let t: Vec<_> = target.components().collect();
    let common = b.iter().zip(&t).take_while(|(x, y)| x == y).count();
    if common == 0 {
        return target.to_string_lossy().replace('\\', "/");
    }
    let mut parts: Vec<String> = Vec::new();
    for _ in common..b.len() {
        parts.push("..".into());
    }
    for c in &t[common..] {
        parts.push(c.as_os_str().to_string_lossy().into_owned());
    }
    if parts.is_empty() {
        ".".into()
    } else {
        parts.join("/")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_paths() {
        assert_eq!(relative(Path::new("/a/b"), Path::new("/a/c/d")), "../c/d");
        assert_eq!(relative(Path::new("/a/b"), Path::new("/a/b/x")), "x");
        assert_eq!(relative(Path::new("/a/b"), Path::new("/a/b")), ".");
    }
}
