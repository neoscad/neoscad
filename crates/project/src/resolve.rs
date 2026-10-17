//! Resolution: from the project's manifest (and its dependencies' own
//! manifests) to one revision per dependency key.
//!
//! The graph is walked breadth first from the project, every
//! requirement becoming a node (a git requirement resolved to a commit in
//! its mirror, a path requirement to its directory); a dependency's
//! `scad.toml`, at its commit, adds its own requirements. Then each key
//! gets one node, by minimal version selection as Go modules do it: when
//! every requirement of a key is a tag that reads as a version
//! (`v1.2.3`), the highest wins (of tags that are equal as versions,
//! `v1` and `1.0.0`, the first by byte order, so the choice does not
//! depend on the order the graph was walked in); otherwise they must all
//! name the same commit, or the key is a conflict and the error names
//! two requirers that disagree, with the requirer's own version. Sources
//! must agree too (one URL, one `include_root`, one folder name).
//!
//! Only requirements in force count: the project's, and those of nodes
//! that are selected and that the project reaches through selected
//! nodes. A version that lost (A v1, when something else wants A v2)
//! takes its own requirements with it, so they can neither win nor
//! conflict. The selection is repeated from the project outwards until it
//! no longer changes.
//!
//! A git dependency's manifest may only name remote repositories: a path,
//! `file://` or a remote helper there is refused
//! ([`crate::manifest::transitive_url_problem`]), and its remotes are
//! fetched under git's stricter policy ([`Remote::Dependency`]). A graph
//! holds at most [`MAX_PACKAGES`] nodes.
//!
//! A requirement that matches a `scad.lock` entry exactly (key, URL,
//! revision, `include_root`) keeps the locked commit unless the key is
//! being updated, so a fetch never moves a branch by itself; an update
//! asks the remotes again.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::{Path, PathBuf};

use crate::error::{Error, Result, bail};
use crate::git::Remote;
use crate::lock::{Lock, Locked, LockedSource, MAX_PACKAGES};
use crate::manifest::{Dep, GitRef, MANIFEST, Manifest, Source, transitive_url_problem};
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
                let dir = crate::search::canonicalize(&joined).map_err(|e| {
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
    /// Git: the commit.
    commit: Option<String>,
    /// The keys this node's own manifest requires.
    deps: Vec<String>,
}

/// The project's requirements and its path dependencies', transitively:
/// the ones this machine's own files make, which need no git. The project
/// itself is the empty requirer, so renaming it changes nothing.
fn local_reqs(manifest: &Manifest) -> Result<Vec<Req>> {
    let mut out = Vec::new();
    let mut seen = BTreeSet::new();
    let mut queue = VecDeque::from([(manifest.clone(), String::new())]);
    while let Some((m, name)) = queue.pop_front() {
        for d in &m.deps {
            let r = Req::from_dep(&name, &m.root, &manifest.root, d)?;
            if let ReqSource::Path { dir, .. } = &r.source
                && seen.insert(dir.clone())
                && dir.join(MANIFEST).is_file()
            {
                queue.push_back((Manifest::read(dir)?, d.key.clone()));
            }
            out.push(r);
        }
    }
    Ok(out)
}

/// The git remotes (normalised) that the project's own files name: its
/// `scad.toml` and its path dependencies'. Every other remote is a
/// dependency's choice and is fetched as [`Remote::Dependency`].
pub fn project_urls(manifest: &Manifest) -> Result<BTreeSet<String>> {
    Ok(local_reqs(manifest)?
        .into_iter()
        .filter_map(|r| match r.source {
            ReqSource::Git { url, .. } => Some(normalize_url(&url)),
            ReqSource::Path { .. } => None,
        })
        .collect())
}

/// The lockfile's `inputs`: a hash of [`local_reqs`].
pub fn inputs_hash(manifest: &Manifest) -> Result<String> {
    let lines: BTreeSet<String> = local_reqs(manifest)?.iter().map(Req::input_line).collect();
    let joined: Vec<String> = lines.into_iter().collect();
    Ok(format!(
        "{}{}",
        crate::hash::PREFIX,
        crate::hash::digest(&joined.join("\n"))
    ))
}

/// Why `lock` cannot be linked as it is for `manifest`, if it cannot:
/// a dependency the project's own `scad.toml` declares whose entry names
/// another source, revision, `include_root` or folder, or a remote no
/// file of this project names that a dependency's manifest could not
/// have named either. The `inputs` hash alone does not show this: it
/// covers what the manifests ask for, not what the lockfile answers, and
/// a lockfile that came with a cloned project can be written to match
/// any manifest. The other entries (what the project's dependencies
/// require) are trusted as locked, as Cargo trusts its lockfile.
pub fn lock_mismatch(
    lock: &Lock,
    manifest: &Manifest,
    trusted: &BTreeSet<String>,
) -> Result<Option<String>> {
    for d in &manifest.deps {
        let Some(l) = lock.get(&d.key) else {
            return Ok(Some(format!("`{}` is not in it", d.key)));
        };
        let req = Req::from_dep("", &manifest.root, &manifest.root, d)?;
        let same = l.folder == d.folder
            && l.include_root == d.include_root
            && match (&req.source, &l.source) {
                (
                    ReqSource::Git { url, reference },
                    LockedSource::Git {
                        url: lu,
                        reference: lr,
                        ..
                    },
                ) => {
                    normalize_url(url) == normalize_url(lu)
                        && (lr == reference
                            // Minimal version selection may have chosen a
                            // higher version tag that a dependency asks for.
                            || matches!((reference, lr), (GitRef::Tag(a), GitRef::Tag(b))
                                if Version::parse(a).zip(Version::parse(b)).is_some_and(|(a, b)| b >= a)))
                }
                (ReqSource::Path { written, .. }, LockedSource::Path(lw)) => written == lw,
                _ => false,
            };
        if !same {
            return Ok(Some(format!(
                "its entry for `{}` is not what scad.toml declares",
                d.key
            )));
        }
    }
    for l in &lock.packages {
        if let LockedSource::Git { url, .. } = &l.source
            && !trusted.contains(&normalize_url(url))
            && let Some(why) = transitive_url_problem(url)
        {
            return Ok(Some(format!(
                "`{}` names '{url}', which a dependency may not ({why})",
                l.name
            )));
        }
    }
    Ok(None)
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
    /// [`project_urls`]: the remotes fetched as [`Remote::Project`].
    trusted: BTreeSet<String>,
    /// The most nodes a resolution may make ([`MAX_PACKAGES`]).
    pub(crate) limit: usize,
}

impl<'a> Resolver<'a> {
    pub fn new(
        store: &'a Store,
        offline: bool,
        prefer: Option<&'a Lock>,
        update: &'a Update,
        program: &'a str,
        trusted: BTreeSet<String>,
    ) -> Resolver<'a> {
        Resolver {
            store,
            offline,
            prefer,
            update,
            program,
            stats: Stats::default(),
            fetched: BTreeSet::new(),
            trusted,
            limit: MAX_PACKAGES,
        }
    }

    /// Who named `url`, for git's transport policy.
    fn remote(&self, url: &str) -> Remote {
        if self.trusted.contains(&normalize_url(url)) {
            Remote::Project
        } else {
            Remote::Dependency
        }
    }

    pub fn resolve(&mut self, manifest: &Manifest) -> Result<Lock> {
        let root_name = manifest.display_name();
        // Each requirement with the node that makes it (None: the
        // project).
        let mut queue: VecDeque<(Option<usize>, Req)> = VecDeque::new();
        for d in &manifest.deps {
            queue.push_back((
                None,
                Req::from_dep(&root_name, &manifest.root, &manifest.root, d)?,
            ));
        }
        let roots: Vec<String> = manifest.deps.iter().map(|d| d.key.clone()).collect();
        let mut nodes: Vec<Node> = Vec::new();
        // (requiring node, required node), once each.
        let mut edges: BTreeSet<(Option<usize>, usize)> = BTreeSet::new();
        let mut by_identity: BTreeMap<String, usize> = BTreeMap::new();
        while let Some((from, req)) = queue.pop_front() {
            let id = req.identity();
            if let Some(&i) = by_identity.get(&id) {
                edges.insert((from, i));
                continue;
            }
            if nodes.len() >= self.limit {
                bail!(
                    "the dependency graph has more than {} packages; a project's dependencies may hold at most that many",
                    self.limit
                );
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
                            match &d.source {
                                Source::Path(p) => bail!(
                                    "`{}` (a git dependency) declares the path dependency `{}` ({p}), which cannot be found outside its repository; make it a git dependency",
                                    req.key,
                                    d.key
                                ),
                                Source::Git { url, .. } => {
                                    if let Some(why) = transitive_url_problem(url) {
                                        bail!(
                                            "`{}` (a git dependency) declares `{}` at '{url}', which is refused: {why}. A dependency's own scad.toml may only name remote repositories (https, ssh, git)",
                                            req.key,
                                            d.key
                                        );
                                    }
                                }
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
            let at = nodes.len();
            by_identity.insert(id, at);
            edges.insert((from, at));
            nodes.push(Node { req, commit, deps });
            queue.extend(children.into_iter().map(|c| (Some(at), c)));
        }

        let (selected, reached) = settle(&nodes, &edges, &roots, &root_name)?;
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
        self.store.git().fetch(mirror, url, self.remote(url))?;
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
            self.store.clone_mirror(url, self.remote(url))?;
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
            let _ = self
                .store
                .git()
                .fetch_commit(mirror, url, commit, self.remote(url));
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
            let _ = self
                .store
                .git()
                .fetch_commit(&mirror, url, r, self.remote(url));
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

/// One node per key, from the requirements in force (see the module
/// documentation): the selection, and the keys the project reaches
/// through it. A conflict is an error only for a key that is still
/// reached once the selection has settled.
fn settle(
    nodes: &[Node],
    edges: &BTreeSet<(Option<usize>, usize)>,
    roots: &[String],
    root_name: &str,
) -> Result<(BTreeMap<String, usize>, BTreeSet<String>)> {
    let mut selected: BTreeMap<String, usize> = BTreeMap::new();
    let mut reached: BTreeSet<usize> = BTreeSet::new();
    // Each round can only change the selection by what the last one
    // reached; a graph that keeps changing past one round per node (a
    // cycle of versions that unselect each other) is refused rather than
    // looped on.
    for _ in 0..=nodes.len() + 1 {
        let mut cands: BTreeMap<&str, Vec<(usize, Option<usize>)>> = BTreeMap::new();
        for &(from, to) in edges {
            if from.is_none_or(|f| reached.contains(&f)) {
                let v = cands.entry(nodes[to].req.key.as_str()).or_default();
                if !v.iter().any(|&(n, _)| n == to) {
                    v.push((to, from));
                }
            }
        }
        let mut sel = BTreeMap::new();
        let mut conflicts = BTreeMap::new();
        for (key, c) in &cands {
            match select(key, c, nodes, root_name) {
                Ok(i) => {
                    sel.insert(key.to_string(), i);
                }
                Err(e) => {
                    conflicts.insert(key.to_string(), e);
                }
            }
        }
        let mut now = BTreeSet::new();
        let mut keys = BTreeSet::new();
        let mut stack: Vec<String> = roots.to_vec();
        while let Some(k) = stack.pop() {
            if keys.insert(k.clone())
                && let Some(&i) = sel.get(&k)
            {
                now.insert(i);
                stack.extend(nodes[i].deps.iter().cloned());
            }
        }
        if now == reached && sel == selected {
            if let Some(e) = keys.iter().find_map(|k| conflicts.remove(k)) {
                return Err(e);
            }
            return Ok((selected, keys));
        }
        selected = sel;
        reached = now;
    }
    bail!(
        "the dependency graph does not settle on one revision per dependency: selecting a version keeps changing which other versions are required"
    )
}

/// How a requirer is named in a conflict: the project, or a dependency
/// with the version of it that makes the requirement (`` `A` v1 ``), so
/// two versions of one dependency are told apart.
fn requirer(from: Option<usize>, nodes: &[Node], root_name: &str) -> String {
    let Some(i) = from else {
        return format!("`{root_name}`");
    };
    let n = &nodes[i];
    let at = n.commit.as_deref().map(short).unwrap_or_default();
    let version = match &n.req.source {
        ReqSource::Git { reference, .. } => match reference {
            GitRef::Tag(t) => t.clone(),
            GitRef::Branch(b) => format!("{b} {at}"),
            GitRef::Rev(_) | GitRef::Default => at.to_string(),
        },
        ReqSource::Path { written, .. } => format!("at {written}"),
    };
    format!("`{}` {version}", n.req.key)
}

/// The node `key` resolves to, among the requirements in force `cands`
/// (each node with the first node requiring it).
fn select(
    key: &str,
    cands: &[(usize, Option<usize>)],
    nodes: &[Node],
    root_name: &str,
) -> Result<usize> {
    let (first_ix, first_from) = cands[0];
    let first = &nodes[first_ix];
    let conflict = |(bi, bfrom): (usize, Option<usize>), why: &str| -> Error {
        let b = &nodes[bi];
        let commit = |n: &Node| {
            n.commit
                .as_ref()
                .map(|c| format!(" (commit {})", short(c)))
                .unwrap_or_default()
        };
        Error(format!(
            "conflicting requirements for `{key}`: {} wants {}{}, {} wants {}{}; {why}",
            requirer(first_from, nodes, root_name),
            first.req.describe(),
            commit(first),
            requirer(bfrom, nodes, root_name),
            b.req.describe(),
            commit(b),
        ))
    };
    if cands.len() == 1 {
        return Ok(first_ix);
    }
    for &c in &cands[1..] {
        let n = &nodes[c.0];
        let same_source = match (&first.req.source, &n.req.source) {
            (ReqSource::Git { url: a, .. }, ReqSource::Git { url: b, .. }) => {
                normalize_url(a) == normalize_url(b)
            }
            (ReqSource::Path { dir: a, .. }, ReqSource::Path { dir: b, .. }) => a == b,
            _ => false,
        };
        if !same_source {
            return Err(conflict(
                c,
                "one library is used per name, so point both at the same source",
            ));
        }
        if n.req.include_root != first.req.include_root || n.req.folder != first.req.folder {
            return Err(conflict(
                c,
                "they give different `include_root` or `name` values",
            ));
        }
    }
    // Minimal version selection over tags that read as versions: the
    // highest, and of equal ones the first tag by byte order.
    let versions: Option<Vec<(Version, &str, usize)>> = cands
        .iter()
        .map(|&(i, _)| match &nodes[i].req.source {
            ReqSource::Git {
                reference: GitRef::Tag(t),
                ..
            } => Version::parse(t).map(|v| (v, t.as_str(), i)),
            _ => None,
        })
        .collect();
    if let Some(vs) = versions {
        let best = vs
            .iter()
            .max_by(|a, b| a.0.cmp(&b.0).then_with(|| b.1.cmp(a.1)))
            .expect("at least two candidates");
        return Ok(best.2);
    }
    for &c in &cands[1..] {
        if nodes[c.0].commit != first.commit {
            return Err(conflict(
                c,
                "exact revisions must agree (only version tags like v1.2.3 are reconciled), so make them name the same commit",
            ));
        }
    }
    Ok(first_ix)
}

/// The first 7 characters of a commit id (characters, so that no string
/// can make it panic).
pub fn short(c: &str) -> &str {
    match c.char_indices().nth(7) {
        Some((i, _)) => &c[..i],
        None => c,
    }
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

    /// A graph past the limit stops with a clear error rather than
    /// cloning on: here three path dependencies against a limit of two.
    #[test]
    fn the_graph_has_a_limit() {
        let base = std::env::temp_dir().join(format!("scad-deps-limit-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let mut toml = String::from("[dependencies]\n");
        for k in ["A", "B", "C"] {
            std::fs::create_dir_all(base.join(k)).unwrap();
            toml.push_str(&format!("{k} = {{ path = \"{k}\" }}\n"));
        }
        let manifest = Manifest::parse(&base, &toml).unwrap();
        let store = Store::open(Some(&base.join("store"))).unwrap();
        let update = Update::Nothing;
        let mut r = Resolver::new(&store, true, None, &update, "scad-deps", BTreeSet::new());
        assert_eq!(r.resolve(&manifest).unwrap().packages.len(), 3);
        r.limit = 2;
        let e = r.resolve(&manifest).unwrap_err().0;
        assert!(e.contains("more than 2 packages"), "{e}");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn relative_paths() {
        assert_eq!(relative(Path::new("/a/b"), Path::new("/a/c/d")), "../c/d");
        assert_eq!(relative(Path::new("/a/b"), Path::new("/a/b/x")), "x");
        assert_eq!(relative(Path::new("/a/b"), Path::new("/a/b")), ".");
    }
}
