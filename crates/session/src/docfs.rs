//! The session's file system: open documents' unsaved text over the
//! host's file system. Everything the pipeline reads goes through it, so a
//! document an editor has not saved is what its includes, `use`s and
//! `import()`s of it see, exactly as its own render sees it.

use std::collections::HashMap;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, RwLock};

use lang::loader::{FileSystem, Metadata};

/// `p` with `.` dropped and `..` folded into its parent: how documents are
/// keyed, so `/a/./b.scad` and `/a/c/../b.scad` are one document.
pub fn normal(p: &Path) -> PathBuf {
    let mut out: Vec<Component<'_>> = Vec::new();
    for c in p.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => match out.last() {
                Some(Component::Normal(_)) => {
                    out.pop();
                }
                Some(Component::RootDir | Component::Prefix(_)) => {}
                _ => out.push(c),
            },
            c => out.push(c),
        }
    }
    out.iter().collect()
}

/// An open document's unsaved text.
#[derive(Debug, Clone)]
pub struct Buffer {
    pub text: Arc<[u8]>,
    /// Bumped on every change; reported as the modification time so that
    /// caches keyed by metadata see each version as a new file.
    pub version: u64,
}

pub struct DocFs {
    base: Arc<dyn FileSystem + Send + Sync>,
    buffers: RwLock<HashMap<PathBuf, Buffer>>,
}

impl std::fmt::Debug for DocFs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let n = self.buffers.read().map_or(0, |b| b.len());
        f.debug_struct("DocFs").field("buffers", &n).finish()
    }
}

impl DocFs {
    pub fn new(base: Arc<dyn FileSystem + Send + Sync>) -> DocFs {
        DocFs {
            base,
            buffers: RwLock::new(HashMap::new()),
        }
    }

    pub fn set(&self, path: &Path, text: Arc<[u8]>, version: u64) {
        self.buffers
            .write()
            .expect("buffers")
            .insert(normal(path), Buffer { text, version });
    }

    pub fn remove(&self, path: &Path) -> bool {
        self.buffers
            .write()
            .expect("buffers")
            .remove(&normal(path))
            .is_some()
    }

    pub fn buffer(&self, path: &Path) -> Option<Buffer> {
        self.buffers
            .read()
            .expect("buffers")
            .get(&normal(path))
            .cloned()
    }
}

impl FileSystem for DocFs {
    fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
        match self.buffer(path) {
            Some(b) => Ok(b.text.to_vec()),
            None => self.base.read(path),
        }
    }
    fn exists(&self, path: &Path) -> bool {
        self.buffer(path).is_some() || self.base.exists(path)
    }
    fn is_dir(&self, path: &Path) -> bool {
        self.buffer(path).is_none() && self.base.is_dir(path)
    }
    fn canonicalize(&self, path: &Path) -> Option<PathBuf> {
        // A buffer that was never saved has no file to resolve links in;
        // one that was keeps the disk's answer (symlinked directories).
        self.base
            .canonicalize(path)
            .or_else(|| self.buffer(path).map(|_| normal(path)))
    }
    fn metadata(&self, path: &Path) -> Option<Metadata> {
        match self.buffer(path) {
            // Far below any real time, and different for every version.
            Some(b) => Some(Metadata {
                modified: Some(i128::MIN + i128::from(b.version)),
                len: b.text.len() as u64,
            }),
            None => self.base.metadata(path),
        }
    }
    fn read_dir(&self, path: &Path) -> io::Result<Vec<PathBuf>> {
        self.base.read_dir(path)
    }
}

/// One request's view of the session's files: [`DocFs`], noting every file
/// the request found (read, or asked the metadata of). Parses served from
/// the cache still ask their files' metadata to validate themselves, and
/// imports are keyed by it, so a warm request notes the same files as a
/// cold one. A host watches these for changes on disk ([`crate::Rendered::files`]):
/// the app re-previews a document when a file it includes or imports is
/// saved by another program.
pub struct Recorder {
    fs: Arc<DocFs>,
    seen: std::sync::Mutex<std::collections::BTreeSet<PathBuf>>,
}

impl std::fmt::Debug for Recorder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Recorder").finish_non_exhaustive()
    }
}

impl Recorder {
    pub fn new(fs: Arc<DocFs>) -> Recorder {
        Recorder {
            fs,
            seen: std::sync::Mutex::new(Default::default()),
        }
    }

    fn note(&self, path: &Path) {
        self.seen
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(normal(path));
    }

    /// The files found so far, sorted.
    pub fn files(&self) -> Vec<PathBuf> {
        self.seen
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .cloned()
            .collect()
    }
}

impl FileSystem for Recorder {
    fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
        let r = self.fs.read(path);
        if r.is_ok() {
            self.note(path);
        }
        r
    }
    // Existence probes are the library path search trying each directory
    // in turn: the file it settles on is read or asked its metadata next,
    // so a probe alone notes nothing.
    fn exists(&self, path: &Path) -> bool {
        self.fs.exists(path)
    }
    fn is_dir(&self, path: &Path) -> bool {
        self.fs.is_dir(path)
    }
    fn canonicalize(&self, path: &Path) -> Option<PathBuf> {
        self.fs.canonicalize(path)
    }
    fn metadata(&self, path: &Path) -> Option<Metadata> {
        let m = self.fs.metadata(path);
        if m.is_some() {
            self.note(path);
        }
        m
    }
    fn read_dir(&self, path: &Path) -> io::Result<Vec<PathBuf>> {
        self.fs.read_dir(path)
    }
}
