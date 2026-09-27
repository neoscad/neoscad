//! Parsed files, kept between requests.
//!
//! A program is its main file with every `include` spliced in (OpenSCAD's
//! includes are textual: an include can sit in the middle of an
//! expression), so the main file and its includes are one parse; each
//! `use`d library is a parse of its own. Both are cached:
//!
//! - the main program by its path and full text (the `-D` suffix
//!   included), validated by the metadata (modification time and size,
//!   through `FileSystem::metadata`) of every included file;
//! - a library by its path, the main file's path (its reassignment
//!   warnings name it) and the suffix, validated by the metadata of the
//!   library and its includes.
//!
//! So a one-line edit to the main file parses the main file again, while
//! every `use`d library comes from the cache; an edit on disk to any file
//! a parse read invalidates that parse. Its includes are part of that
//! parse, but an include between top-level statements (a library's
//! `include <BOSL2/std.scad>`) comes from the [`FragmentStore`], parsed
//! and lowered once (`lang::fragment`), and the rest from the
//! [`LexStore`], read and lexed once. A parse that could not find an include is never cached:
//! the file may appear before the next request, and nothing would notice.
//!
//! Entries are evicted least recently used first once their estimated
//! size passes the budget.

use std::collections::{HashMap, HashSet};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use lang::Program;
use lang::diag::DiagCode;
use lang::fragment::{Fragment, FragmentCache, FragmentKey};
use lang::loader::{
    FileSystem, LexCache, LexedFile, LibraryPath, Metadata, find_valid_path, generic,
};
use lang::source::FileId;

/// Included files read and lexed, by path (see `lang::loader::LexCache`):
/// after an edit to a main file that includes a large library, the parse
/// starts from the library's tokens instead of reading and lexing it
/// again. Bounded by `budget` bytes of text (tokens take about as much
/// again), dropping the least recently used.
/// A lexed file: the metadata it was read with, the file, its last use.
type LexEntry = (Metadata, Arc<LexedFile>, u64);

#[derive(Debug)]
pub struct LexStore {
    files: std::sync::Mutex<HashMap<PathBuf, LexEntry>>,
    clock: std::sync::atomic::AtomicU64,
    budget: usize,
}

impl LexStore {
    pub fn new(budget: usize) -> LexStore {
        LexStore {
            files: Default::default(),
            clock: Default::default(),
            budget,
        }
    }

    /// Entries and bytes of text held.
    pub fn size(&self) -> (usize, usize) {
        let f = self
            .files
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        (f.len(), f.values().map(|(_, l, _)| cost(l)).sum())
    }

    pub fn clear(&self) {
        self.files
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clear();
    }
}

fn cost(f: &LexedFile) -> usize {
    f.text.len() + f.tokens.len() * std::mem::size_of::<lang::syntax::lexer::Token>()
}

impl LexCache for LexStore {
    fn get(&self, path: &Path, meta: &Metadata) -> Option<Arc<LexedFile>> {
        let mut files = self
            .files
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let e = files.get_mut(path).filter(|(m, _, _)| m == meta)?;
        e.2 = self
            .clock
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Some(e.1.clone())
    }

    fn put(&self, path: &Path, meta: Metadata, file: Arc<LexedFile>) {
        let mut files = self
            .files
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let stamp = self
            .clock
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        files.insert(path.to_path_buf(), (meta, file, stamp));
        let mut total: usize = files.values().map(|(_, l, _)| cost(l)).sum();
        while total > self.budget && files.len() > 1 {
            let Some(oldest) = files
                .iter()
                .min_by_key(|(_, (_, _, s))| *s)
                .map(|(p, _)| p.clone())
            else {
                break;
            };
            if let Some((_, l, _)) = files.remove(&oldest) {
                total -= cost(&l);
            }
        }
    }
}

/// Included files parsed and lowered (`lang::fragment`): after an edit to
/// a main file that includes a large library between its statements, the
/// parse takes the library's statements as they are instead of parsing
/// them again, which is most of a re-parse. Checked by the loader against
/// the files and paths they depend on; bounded by `budget` estimated
/// bytes, dropping the least recently used.
#[derive(Debug)]
pub struct FragmentStore {
    frags: std::sync::Mutex<HashMap<FragmentKey, (Arc<Fragment>, u64)>>,
    clock: std::sync::atomic::AtomicU64,
    budget: usize,
}

impl FragmentStore {
    pub fn new(budget: usize) -> FragmentStore {
        FragmentStore {
            frags: Default::default(),
            clock: Default::default(),
            budget,
        }
    }

    /// Entries and estimated bytes held.
    pub fn size(&self) -> (usize, usize) {
        let f = self
            .frags
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        (f.len(), f.values().map(|(x, _)| x.cost()).sum())
    }

    pub fn clear(&self) {
        self.frags
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clear();
    }
}

impl FragmentCache for FragmentStore {
    fn get(&self, key: &FragmentKey) -> Option<Arc<Fragment>> {
        let mut frags = self
            .frags
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let e = frags.get_mut(key)?;
        e.1 = self
            .clock
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Some(e.0.clone())
    }

    fn put(&self, key: FragmentKey, fragment: Arc<Fragment>) {
        let mut frags = self
            .frags
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let stamp = self
            .clock
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        frags.insert(key, (fragment, stamp));
        let mut total: usize = frags.values().map(|(f, _)| f.cost()).sum();
        while total > self.budget && frags.len() > 1 {
            let Some(oldest) = frags
                .iter()
                .min_by_key(|(_, (_, s))| *s)
                .map(|(k, _)| k.clone())
            else {
                break;
            };
            if let Some((f, _)) = frags.remove(&oldest) {
                total -= f.cost();
            }
        }
    }
}

/// The caches a parse draws on.
#[derive(Debug, Clone, Copy)]
pub struct Stores<'a> {
    pub lexed: &'a LexStore,
    pub fragments: &'a FragmentStore,
}

impl Stores<'_> {
    pub fn caches(&self) -> lang::Caches<'_> {
        lang::Caches {
            lex: Some(self.lexed),
            fragments: Some(self.fragments),
            stats: None,
        }
    }
}

/// Main programs kept per path (see `ParseCache::mains`).
const MAINS_PER_PATH: usize = 2;

/// Default budget: parsed programs are far smaller than geometry.
pub const PARSE_BUDGET: usize = 256 << 20;

#[derive(Debug)]
struct Entry {
    program: Arc<Program>,
    /// Every file the parse read whose text is not in the key, with its
    /// metadata then.
    deps: Vec<(PathBuf, Metadata)>,
    cost: usize,
    stamp: u64,
}

/// What the parse cache holds.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ParseStats {
    pub entries: usize,
    pub bytes: usize,
    pub budget: usize,
    pub hits: u64,
    pub misses: u64,
    pub evictions: u64,
}

#[derive(Debug)]
pub struct ParseCache {
    entries: HashMap<u64, Entry>,
    /// The main-program keys of each path, newest last. Every edit makes a
    /// new text and so a new key; only the last few (an undo, a
    /// snapshot after a render) can come back, so older ones go at once
    /// rather than filling the budget with a large library's parses.
    mains: HashMap<PathBuf, Vec<u64>>,
    bytes: usize,
    budget: usize,
    clock: u64,
    hits: u64,
    misses: u64,
    evictions: u64,
}

/// One `use`d library, as `lang::deps::Library` describes it, with a
/// shared parse.
#[derive(Debug, Clone)]
pub struct Lib {
    pub path: String,
    /// `None` when the file could not be read.
    pub program: Option<Arc<Program>>,
    pub uses: Vec<String>,
}

impl Lib {
    /// `lang::deps::Library::open_error`.
    pub fn open_error(&self) -> Option<String> {
        self.program
            .is_none()
            .then(|| format!("WARNING: Can't open library file '{}'\n", self.path))
    }
}

fn key(parts: &[&[u8]]) -> u64 {
    let mut h = DefaultHasher::new();
    for p in parts {
        p.hash(&mut h);
    }
    h.finish()
}

/// Whether a parse depends on a file that was not there.
fn missed_a_file(p: &Program) -> bool {
    p.diags.iter().any(|d| {
        matches!(
            d.code,
            DiagCode::IncludeNotFound | DiagCode::LibraryNotFound
        )
    })
}

impl ParseCache {
    pub fn new(budget: usize) -> ParseCache {
        ParseCache {
            entries: HashMap::new(),
            mains: HashMap::new(),
            bytes: 0,
            budget,
            clock: 0,
            hits: 0,
            misses: 0,
            evictions: 0,
        }
    }

    pub fn stats(&self) -> ParseStats {
        ParseStats {
            entries: self.entries.len(),
            bytes: self.bytes,
            budget: self.budget,
            hits: self.hits,
            misses: self.misses,
            evictions: self.evictions,
        }
    }

    pub fn set_budget(&mut self, budget: usize) {
        self.budget = budget;
        self.shrink();
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        self.mains.clear();
        self.bytes = 0;
    }

    /// Record `k` as `path`'s newest main program, dropping all but the
    /// last [`MAINS_PER_PATH`].
    fn main_key(&mut self, path: &Path, k: u64) {
        let keys = self.mains.entry(path.to_path_buf()).or_default();
        keys.retain(|&x| x != k);
        keys.push(k);
        while keys.len() > MAINS_PER_PATH {
            let old = keys.remove(0);
            if let Some(e) = self.entries.remove(&old) {
                self.bytes -= e.cost;
                self.evictions += 1;
            }
        }
    }

    fn get(&mut self, k: u64, fs: &dyn FileSystem) -> Option<Arc<Program>> {
        let fresh = self
            .entries
            .get(&k)
            .map(|e| e.deps.iter().all(|(p, m)| fs.metadata(p) == Some(*m)));
        match fresh {
            Some(true) => {
                self.clock += 1;
                let e = self.entries.get_mut(&k).expect("checked");
                e.stamp = self.clock;
                self.hits += 1;
                Some(e.program.clone())
            }
            Some(false) => {
                if let Some(e) = self.entries.remove(&k) {
                    self.bytes -= e.cost;
                }
                self.misses += 1;
                None
            }
            None => {
                self.misses += 1;
                None
            }
        }
    }

    /// Keep `program` under `k` if every file it read beyond the key can
    /// be checked later.
    fn put(&mut self, k: u64, program: &Arc<Program>, skip: usize, fs: &dyn FileSystem) {
        if missed_a_file(program) {
            return;
        }
        let mut deps = Vec::new();
        for (id, f) in program.sources.iter() {
            if (id.0 as usize) < skip {
                continue;
            }
            match fs.metadata(&f.path) {
                Some(m) => deps.push((f.path.clone(), m)),
                // A file system that cannot tell versions apart: nothing
                // would notice a change, so do not keep it.
                None => return,
            }
        }
        let text: usize = program.sources.iter().map(|(_, f)| f.text.len()).sum();
        // Tokens, syntax tree and AST: about sixteen bytes per source byte.
        let cost = text * 16 + 1024;
        self.clock += 1;
        if let Some(old) = self.entries.insert(
            k,
            Entry {
                program: program.clone(),
                deps,
                cost,
                stamp: self.clock,
            },
        ) {
            self.bytes -= old.cost;
        }
        self.bytes += cost;
        self.shrink();
    }

    fn shrink(&mut self) {
        while self.bytes > self.budget && self.entries.len() > 1 {
            let Some((&k, _)) = self.entries.iter().min_by_key(|(_, e)| e.stamp) else {
                break;
            };
            if let Some(e) = self.entries.remove(&k) {
                self.bytes -= e.cost;
                self.evictions += 1;
            }
        }
    }
}

/// The main program: `text` (with the suffix already appended) parsed as
/// `path`, from the cache when its includes are unchanged.
pub fn main_program(
    cache: &std::sync::Mutex<ParseCache>,
    stores: Stores<'_>,
    path: &Path,
    text: Vec<u8>,
    fs: &dyn FileSystem,
    libs: &LibraryPath,
) -> Arc<Program> {
    let k = key(&[b"main", generic(path).as_bytes(), &text]);
    if let Some(p) = cache
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(k, fs)
    {
        return p;
    }
    let program = Arc::new(lang::parse_program_with(
        path.to_path_buf(),
        text,
        fs,
        libs,
        stores.caches(),
    ));
    // The main file's text is in the key; its includes are checked by
    // their metadata.
    let mut c = cache
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    c.put(k, &program, FileId(1).0 as usize, fs);
    if c.entries.contains_key(&k) {
        c.main_key(path, k);
    }
    program
}

/// `SourceFile::handleDependencies`: a used name as a library key, if the
/// file exists (`lang::deps`).
fn resolve(name: &str, dir: &Path, fs: &dyn FileSystem, libs: &LibraryPath) -> Option<String> {
    let path = if Path::new(name).is_absolute() {
        PathBuf::from(name)
    } else {
        find_valid_path(fs, libs, dir, Path::new(name), &[])?
    };
    fs.exists(&path).then(|| generic(&path))
}

/// `lang::deps::load_dependencies` with each library's parse from the
/// cache: every library `root` uses, transitively, in the order OpenSCAD
/// processes them.
pub fn libraries(
    cache: &std::sync::Mutex<ParseCache>,
    stores: Stores<'_>,
    root: &Program,
    suffix: &[u8],
    fs: &dyn FileSystem,
    libs: &LibraryPath,
) -> Vec<Lib> {
    let main = root.sources.path(root.main).to_path_buf();
    let dir = main.parent().map(Path::to_path_buf).unwrap_or_default();
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    let mut ctx = Visit {
        cache,
        stores,
        main: &main,
        suffix,
        fs,
        libs,
    };
    ctx.visit(&root.ast.uses, &dir, &mut seen, &mut out);
    out
}

struct Visit<'a> {
    cache: &'a std::sync::Mutex<ParseCache>,
    stores: Stores<'a>,
    main: &'a Path,
    suffix: &'a [u8],
    fs: &'a dyn FileSystem,
    libs: &'a LibraryPath,
}

impl Visit<'_> {
    fn visit(
        &mut self,
        uses: &[String],
        dir: &Path,
        seen: &mut HashSet<String>,
        out: &mut Vec<Lib>,
    ) {
        for name in uses {
            let Some(key_path) = resolve(name, dir, self.fs, self.libs) else {
                continue;
            };
            if !seen.insert(key_path.clone()) {
                continue;
            }
            let path = PathBuf::from(&key_path);
            let Some(program) = self.library(&path) else {
                out.push(Lib {
                    path: key_path,
                    program: None,
                    uses: Vec::new(),
                });
                continue;
            };
            let ok = !program.has_syntax_errors();
            let lib_uses = program.ast.uses.clone();
            let lib_dir = path.parent().map(Path::to_path_buf).unwrap_or_default();
            let resolved = lib_uses
                .iter()
                .filter_map(|n| resolve(n, &lib_dir, self.fs, self.libs))
                .collect();
            out.push(Lib {
                path: key_path,
                program: Some(program),
                uses: resolved,
            });
            if ok {
                self.visit(&lib_uses, &lib_dir, seen, out);
            }
        }
    }

    /// One library's parse, `None` when it cannot be read.
    fn library(&mut self, path: &Path) -> Option<Arc<Program>> {
        let k = key(&[
            b"lib",
            generic(path).as_bytes(),
            generic(self.main).as_bytes(),
            self.suffix,
        ]);
        if let Some(p) = self
            .cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(k, self.fs)
        {
            return Some(p);
        }
        // A directory (`use </>`) opens as an empty stream in OpenSCAD.
        let mut text = if self.fs.is_dir(path) {
            Vec::new()
        } else {
            self.fs.read(path).ok()?
        };
        text.extend_from_slice(self.suffix);
        let program = Arc::new(lang::parse_library_with(
            path.to_path_buf(),
            text,
            self.main,
            self.fs,
            self.libs,
            self.stores.caches(),
        ));
        self.cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .put(k, &program, 0, self.fs);
        Some(program)
    }
}
