//! Files and the program they make: each file parsed and indexed once
//! ([`Analyzed`], cached by content in [`Cache`]), and for a document the
//! files it `include`s (textually part of it) and the libraries it
//! `use`s (their modules and functions only), which is where a name that
//! is not local resolves ([`World::resolve`]).

use std::collections::{HashMap, HashSet};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use docs::UserDoc;
use lang::Program;
use lang::loader::{FileSystem, LibraryPath, Metadata, find_valid_path};
use lang::source::SourceFile;

use crate::index::{Def, DefId, DefKind, FileIndex, Ns, RefKind, ScopeId, ScopeKind};

/// A file parsed alone and indexed.
#[derive(Debug)]
pub struct Analyzed {
    pub path: PathBuf,
    pub program: Program,
    pub index: FileIndex,
    docs: OnceLock<Vec<UserDoc>>,
}

impl Analyzed {
    pub fn new(path: PathBuf, text: Vec<u8>) -> Analyzed {
        let program = lang::parse_file(path.clone(), text);
        let index = crate::index::build(&program);
        Analyzed {
            path,
            program,
            index,
            docs: OnceLock::new(),
        }
    }

    pub fn source(&self) -> &SourceFile {
        self.program.sources.get(self.program.main)
    }

    pub fn text(&self) -> &[u8] {
        &self.source().text
    }

    /// The text of a byte range, lossy.
    pub fn slice(&self, (a, b): (u32, u32)) -> String {
        let t = self.text();
        let a = (a as usize).min(t.len());
        let b = (b as usize).clamp(a, t.len());
        String::from_utf8_lossy(&t[a..b]).into_owned()
    }

    /// The top-level modules and functions with their comment blocks
    /// (`docs::definitions`, BOSL2's structured blocks included), made
    /// when first asked for.
    pub fn docs(&self) -> &[UserDoc] {
        self.docs.get_or_init(|| docs::definitions(&self.program))
    }

    /// The documentation of a top-level module or function.
    pub fn user_doc(&self, d: &Def) -> Option<&UserDoc> {
        let kind = match d.kind {
            DefKind::Module => docs::Kind::Module,
            DefKind::Function => docs::Kind::Function,
            _ => return None,
        };
        let line = self.source().line_of(d.span.0);
        self.docs()
            .iter()
            .find(|u| u.kind == kind && u.name == d.name && u.line == line)
    }
}

/// How many files the cache keeps (BOSL2 is about sixty).
const CACHE_FILES: usize = 1024;

/// Analysed library and included files, by path, keyed by their content:
/// a file whose metadata is unchanged is not read again, and one whose
/// text hashes the same is not parsed again. Shared by every server of a
/// host, so each window of the app indexes BOSL2 once.
#[derive(Debug, Default)]
pub struct Cache {
    files: Mutex<HashMap<PathBuf, Entry>>,
    tick: AtomicU64,
}

#[derive(Debug)]
struct Entry {
    meta: Option<Metadata>,
    hash: u64,
    file: Arc<Analyzed>,
    used: u64,
}

pub fn content_hash(text: &[u8]) -> u64 {
    let mut h = DefaultHasher::new();
    text.hash(&mut h);
    h.finish()
}

impl Cache {
    pub fn new() -> Cache {
        Cache::default()
    }

    /// `path` analysed, or `None` when it cannot be read.
    pub fn file(&self, fs: &dyn FileSystem, path: &Path) -> Option<Arc<Analyzed>> {
        let meta = fs.metadata(path);
        let tick = self.tick.fetch_add(1, Ordering::Relaxed);
        {
            let mut files = self.files.lock().unwrap_or_else(PoisonError::into_inner);
            if let Some(e) = files.get_mut(path)
                && meta.is_some()
                && e.meta == meta
            {
                e.used = tick;
                return Some(e.file.clone());
            }
        }
        let text = fs.read(path).ok()?;
        let hash = content_hash(&text);
        let mut files = self.files.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(e) = files.get_mut(path)
            && e.hash == hash
        {
            e.meta = meta;
            e.used = tick;
            return Some(e.file.clone());
        }
        drop(files);
        // Parsed without the lock: another thread may parse the same file
        // meanwhile, which costs time, never correctness.
        let file = Arc::new(Analyzed::new(path.to_path_buf(), text));
        let mut files = self.files.lock().unwrap_or_else(PoisonError::into_inner);
        if files.len() >= CACHE_FILES
            && let Some(old) = files
                .iter()
                .min_by_key(|(_, e)| e.used)
                .map(|(p, _)| p.clone())
        {
            files.remove(&old);
        }
        files.insert(
            path.to_path_buf(),
            Entry {
                meta,
                hash,
                file: file.clone(),
                used: tick,
            },
        );
        Some(file)
    }

    pub fn len(&self) -> usize {
        self.files
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// A definition in a file.
#[derive(Debug, Clone)]
pub struct Found {
    pub file: Arc<Analyzed>,
    pub def: DefId,
}

impl Found {
    pub fn def(&self) -> &Def {
        &self.file.index.defs[self.def]
    }

    pub fn same(&self, other: &Found) -> bool {
        Arc::ptr_eq(&self.file, &other.file) && self.def == other.def
    }
}

/// What a name refers to.
#[derive(Debug, Clone)]
pub enum Target {
    Def(Found),
    /// OpenSCAD's builtins of that name (a module and a function may
    /// share one).
    Builtin(Vec<&'static docs::Entry>),
    /// The file an `include` or `use` names.
    File(PathBuf),
}

/// Where a visible name comes from, nearest first (completion's order).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Origin {
    Local,
    File,
    Include,
    Library,
    Builtin,
}

/// A name visible at a point.
#[derive(Debug, Clone)]
pub struct Candidate {
    pub name: String,
    pub ns: Ns,
    pub origin: Origin,
    pub found: Option<Found>,
    pub builtin: Option<&'static docs::Entry>,
}

/// A document and the files that make its program.
#[derive(Debug)]
pub struct World {
    /// The document first, then what it includes and what those include
    /// (each file once).
    pub files: Vec<Arc<Analyzed>>,
    /// `(file, directive)` to the file an `include` or `use` resolved to.
    pub targets: HashMap<(usize, usize), PathBuf>,
    /// Each `use`d library with the files it includes, in `use` order.
    pub libs: Vec<Vec<Arc<Analyzed>>>,
}

/// Where the world's files come from: an open document's text first,
/// then the host's files through the cache.
pub struct Loader<'a> {
    pub fs: &'a dyn FileSystem,
    pub libs: &'a LibraryPath,
    pub cache: &'a Cache,
    pub open: &'a dyn Fn(&Path) -> Option<Arc<Analyzed>>,
}

impl std::fmt::Debug for Loader<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Loader")
            .field("libs", self.libs)
            .finish_non_exhaustive()
    }
}

impl Loader<'_> {
    fn load(&self, path: &Path) -> Option<Arc<Analyzed>> {
        (self.open)(path).or_else(|| self.cache.file(self.fs, path))
    }

    /// Where a directive of `from` points, if the file exists.
    pub fn resolve(&self, from: &Path, written: &str) -> Option<PathBuf> {
        if written.is_empty() {
            return None;
        }
        let dir = from.parent().unwrap_or(Path::new("/"));
        find_valid_path(self.fs, self.libs, dir, Path::new(written), &[])
            .map(|p| session::normal(&p))
    }

    /// `main` and the files it includes, depth first. `targets` gets
    /// where each directive of those files points, keyed by the file's
    /// index plus `base`.
    fn closure(
        &self,
        main: Arc<Analyzed>,
        base: usize,
        targets: &mut HashMap<(usize, usize), PathBuf>,
        uses: &mut Vec<PathBuf>,
    ) -> Vec<Arc<Analyzed>> {
        let mut files = vec![main];
        let mut seen: HashSet<PathBuf> = HashSet::from([files[0].path.clone()]);
        let mut order: Vec<usize> = vec![0];
        while let Some(fi) = order.pop() {
            let f = files[fi].clone();
            let mut children = Vec::new();
            for (di, d) in f.index.directives.iter().enumerate() {
                let Some(p) = self.resolve(&f.path, &d.path) else {
                    continue;
                };
                targets.insert((base + fi, di), p.clone());
                if !d.include {
                    if !uses.contains(&p) {
                        uses.push(p);
                    }
                    continue;
                }
                if !seen.insert(p.clone()) {
                    continue;
                }
                if let Some(a) = self.load(&p) {
                    files.push(a);
                    children.push(files.len() - 1);
                }
            }
            order.extend(children.into_iter().rev());
        }
        files
    }
}

impl World {
    pub fn new(main: Arc<Analyzed>, loader: &Loader<'_>) -> World {
        let mut targets = HashMap::new();
        let mut uses = Vec::new();
        let files = loader.closure(main, 0, &mut targets, &mut uses);
        let mut libs = Vec::new();
        for u in uses {
            // A library's own `use`s are not visible through it, and its
            // directives' targets are only needed inside it.
            let Some(lib) = loader.load(&u) else {
                continue;
            };
            let mut t = HashMap::new();
            libs.push(loader.closure(lib, 0, &mut t, &mut Vec::new()));
        }
        World {
            files,
            targets,
            libs,
        }
    }

    pub fn main(&self) -> &Arc<Analyzed> {
        &self.files[0]
    }

    /// The index of `file` in [`World::files`].
    pub fn file_index(&self, file: &Arc<Analyzed>) -> Option<usize> {
        self.files.iter().position(|f| Arc::ptr_eq(f, file))
    }

    /// `name` in namespace `ns`, as seen from `scope` of `file` at byte
    /// `at`: its local scopes outward, the program's top level (the
    /// document and its includes), then the `use`d libraries, then the
    /// builtins.
    pub fn resolve(
        &self,
        file: &Arc<Analyzed>,
        scope: ScopeId,
        at: u32,
        name: &str,
        ns: Ns,
    ) -> Option<Target> {
        let ix = &file.index;
        let mut s = Some(scope);
        while let Some(i) = s {
            if ix.scopes[i].kind == ScopeKind::File {
                break;
            }
            if let Some(d) = pick(ix, i, name, ns, at) {
                return Some(Target::Def(Found {
                    file: file.clone(),
                    def: d,
                }));
            }
            s = ix.scopes[i].parent;
        }
        if let Some(f) = self.top(name, ns, file) {
            return Some(Target::Def(f));
        }
        if ns != Ns::Variable {
            for lib in self.libs.iter().rev() {
                for f in lib {
                    if let Some(d) = pick(&f.index, 0, name, ns, u32::MAX) {
                        return Some(Target::Def(Found {
                            file: f.clone(),
                            def: d,
                        }));
                    }
                }
            }
        }
        let b = builtins(name, ns);
        (!b.is_empty()).then_some(Target::Builtin(b))
    }

    /// A top-level definition of the program: in `prefer` (the file asked
    /// from) first, then the document, then its includes.
    pub fn top(&self, name: &str, ns: Ns, prefer: &Arc<Analyzed>) -> Option<Found> {
        let order = std::iter::once(prefer).chain(self.files.iter());
        for f in order {
            if let Some(d) = pick(&f.index, 0, name, ns, u32::MAX) {
                return Some(Found {
                    file: f.clone(),
                    def: d,
                });
            }
        }
        None
    }

    /// What reference `r` of `file` names: a named argument names the
    /// callee's parameter; a function call a function, or failing one a
    /// variable holding a function literal.
    pub fn resolve_ref(&self, file: &Arc<Analyzed>, r: usize) -> Option<Target> {
        let rf = &file.index.refs[r];
        let at = rf.span.0;
        match rf.kind {
            RefKind::Module => self.resolve(file, rf.scope, at, &rf.name, Ns::Module),
            RefKind::Variable => self.resolve(file, rf.scope, at, &rf.name, Ns::Variable),
            RefKind::Function => self
                .resolve(file, rf.scope, at, &rf.name, Ns::Function)
                .or_else(|| self.resolve(file, rf.scope, at, &rf.name, Ns::Variable)),
            RefKind::NamedArg => {
                let callee = rf.callee?;
                match self.resolve_ref(file, callee)? {
                    Target::Def(f) => {
                        let inner = f.def().inner?;
                        let p = f
                            .file
                            .index
                            .defs_in(inner)
                            .find(|(_, d)| d.kind == DefKind::Parameter && d.name == rf.name)?;
                        Some(Target::Def(Found {
                            file: f.file.clone(),
                            def: p.0,
                        }))
                    }
                    t @ Target::Builtin(_) => Some(t),
                    Target::File(_) => None,
                }
            }
        }
    }

    /// Every name visible from `scope` of `file` at `at`, nearest first,
    /// each name once per namespace.
    pub fn visible(&self, file: &Arc<Analyzed>, scope: ScopeId, at: u32) -> Vec<Candidate> {
        let mut out = Vec::new();
        let mut seen: HashSet<(String, Ns)> = HashSet::new();
        let mut add = |out: &mut Vec<Candidate>, c: Candidate| {
            if seen.insert((c.name.clone(), c.ns)) {
                out.push(c);
            }
        };
        let ix = &file.index;
        let mut s = Some(scope);
        while let Some(i) = s {
            if ix.scopes[i].kind == ScopeKind::File {
                break;
            }
            for (id, d) in ix.defs_in(i) {
                if d.visible_from <= at {
                    add(&mut out, candidate(file, id, Origin::Local));
                }
            }
            s = ix.scopes[i].parent;
        }
        for f in std::iter::once(file).chain(self.files.iter()) {
            let origin = if Arc::ptr_eq(f, file) {
                Origin::File
            } else {
                Origin::Include
            };
            for (id, _) in f.index.defs_in(0) {
                add(&mut out, candidate(f, id, origin));
            }
        }
        for lib in &self.libs {
            for f in lib {
                for (id, d) in f.index.defs_in(0) {
                    if d.kind.ns() != Ns::Variable {
                        add(&mut out, candidate(f, id, Origin::Library));
                    }
                }
            }
        }
        for e in docs::builtins() {
            // The sketch vocabulary exists only inside sketch bodies; until
            // completion knows where it is (stage 4 of
            // docs/language-extensions.md), it is not offered at all, so
            // `on` or `length` are never suggested where they mean nothing.
            if e.extension.as_deref() == Some("sketch") && e.name != "sketch" {
                continue;
            }
            let ns = match e.kind {
                docs::Kind::Module => Ns::Module,
                docs::Kind::Function => Ns::Function,
                docs::Kind::Variable => Ns::Variable,
            };
            add(
                &mut out,
                Candidate {
                    name: e.name.clone(),
                    ns,
                    origin: Origin::Builtin,
                    found: None,
                    builtin: Some(e),
                },
            );
        }
        out
    }
}

fn candidate(file: &Arc<Analyzed>, id: DefId, origin: Origin) -> Candidate {
    let d = &file.index.defs[id];
    Candidate {
        name: d.name.clone(),
        ns: d.kind.ns(),
        origin,
        found: Some(Found {
            file: file.clone(),
            def: id,
        }),
        builtin: None,
    }
}

/// The definition of `name` in `scope` visible at `at`: a variable's
/// first assignment (OpenSCAD keeps the first position), a module's or
/// function's last definition (a later one replaces it).
fn pick(ix: &FileIndex, scope: ScopeId, name: &str, ns: Ns, at: u32) -> Option<DefId> {
    let mut found = ix
        .defs_in(scope)
        .filter(|(_, d)| d.name == name && d.kind.ns() == ns && d.visible_from <= at)
        .map(|(i, _)| i);
    if ns == Ns::Variable {
        found.next()
    } else {
        found.last()
    }
}

/// The builtins called `name` in namespace `ns`.
pub fn builtins(name: &str, ns: Ns) -> Vec<&'static docs::Entry> {
    let kind = match ns {
        Ns::Module => docs::Kind::Module,
        Ns::Function => docs::Kind::Function,
        Ns::Variable => docs::Kind::Variable,
    };
    docs::builtin(name)
        .into_iter()
        .filter(|e| e.kind == kind)
        .collect()
}
