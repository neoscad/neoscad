//! File systems that are not the disk: [`MemFs`], files held in memory
//! (the WASM build's documents, tests), and [`Overlay`], read-only files
//! compiled into the binary and mounted at a directory over another file
//! system (the bundled MCAD library).
//!
//! Both have no symlinks, so a canonical path is the lexically normal one:
//! `.` dropped and `..` folded into its parent.

use std::collections::BTreeMap;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};

use crate::loader::{FileSystem, Metadata};

/// `p` with `.` dropped and `..` folded (a `..` at the root stays there).
fn normal(p: &Path) -> PathBuf {
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

/// Whether some key of `files` lies strictly below `dir`.
fn has_below<V>(files: &BTreeMap<PathBuf, V>, dir: &Path) -> bool {
    files
        .range(dir.to_path_buf()..)
        .take_while(|(k, _)| k.starts_with(dir))
        .any(|(k, _)| k.as_path() != dir)
}

/// The entries directly inside `dir`: files, and the directories implied
/// by deeper files.
fn entries<V>(files: &BTreeMap<PathBuf, V>, dir: &Path) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    for (k, _) in files
        .range(dir.to_path_buf()..)
        .take_while(|(k, _)| k.starts_with(dir))
    {
        if let Ok(rest) = k.strip_prefix(dir)
            && let Some(first) = rest.components().next()
        {
            let e = dir.join(first);
            if out.last() != Some(&e) {
                out.push(e);
            }
        }
    }
    out
}

#[derive(Debug)]
struct MemFile {
    data: Arc<[u8]>,
    /// The write counter when the file was last written, reported as its
    /// modification time so that cache keys change with the contents.
    version: u64,
}

/// Files in memory, keyed by absolute path; directories are implied by the
/// files below them. Files can be written while the file system is shared
/// (a long-lived host updating a document), and every write gives the file
/// a new [`Metadata::modified`], so geometry imported from its old
/// contents is not reused.
#[derive(Debug, Default)]
pub struct MemFs {
    files: RwLock<BTreeMap<PathBuf, MemFile>>,
    writes: AtomicU64,
}

impl MemFs {
    pub fn new() -> MemFs {
        MemFs::default()
    }

    /// Create or replace a file.
    pub fn insert(&self, path: impl AsRef<Path>, data: impl Into<Arc<[u8]>>) {
        let version = self.writes.fetch_add(1, Ordering::Relaxed) + 1;
        self.files.write().expect("MemFs lock").insert(
            normal(path.as_ref()),
            MemFile {
                data: data.into(),
                version,
            },
        );
    }

    /// Delete a file; returns whether it existed.
    pub fn remove(&self, path: impl AsRef<Path>) -> bool {
        self.files
            .write()
            .expect("MemFs lock")
            .remove(&normal(path.as_ref()))
            .is_some()
    }
}

impl FileSystem for MemFs {
    fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
        self.files
            .read()
            .expect("MemFs lock")
            .get(&normal(path))
            .map(|f| f.data.to_vec())
            .ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))
    }
    fn exists(&self, path: &Path) -> bool {
        let p = normal(path);
        let files = self.files.read().expect("MemFs lock");
        files.contains_key(&p) || has_below(&files, &p)
    }
    fn is_dir(&self, path: &Path) -> bool {
        let p = normal(path);
        let files = self.files.read().expect("MemFs lock");
        !files.contains_key(&p) && has_below(&files, &p)
    }
    fn canonicalize(&self, path: &Path) -> Option<PathBuf> {
        self.exists(path).then(|| normal(path))
    }
    fn metadata(&self, path: &Path) -> Option<Metadata> {
        let files = self.files.read().expect("MemFs lock");
        files.get(&normal(path)).map(|f| Metadata {
            modified: Some(i128::from(f.version)),
            len: f.data.len() as u64,
        })
    }
    fn read_dir(&self, path: &Path) -> io::Result<Vec<PathBuf>> {
        let p = normal(path);
        let files = self.files.read().expect("MemFs lock");
        if !has_below(&files, &p) {
            return Err(io::Error::from(io::ErrorKind::NotFound));
        }
        Ok(entries(&files, &p))
    }
}

/// Read-only files compiled into the binary, mounted at `root` over a base
/// file system. A path under `root` that names an overlay file or an
/// implied directory is served from memory; every other path goes to the
/// base, so a real directory at the same place still shows through for
/// files the overlay lacks. This is how the bundled libraries become
/// OpenSCAD's `<resources>/libraries` without anything written to disk.
pub struct Overlay {
    base: Arc<dyn FileSystem + Send + Sync>,
    root: PathBuf,
    /// Keyed by the full path (`root` joined with the relative name).
    files: BTreeMap<PathBuf, &'static [u8]>,
}

impl std::fmt::Debug for Overlay {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Overlay")
            .field("root", &self.root)
            .field("files", &self.files.len())
            .finish_non_exhaustive()
    }
}

impl Overlay {
    /// Mount `files` (paths relative to `root`, `/`-separated) at `root`.
    pub fn new<'f>(
        base: Arc<dyn FileSystem + Send + Sync>,
        root: impl AsRef<Path>,
        files: impl IntoIterator<Item = &'f (&'static str, &'static [u8])>,
    ) -> Overlay {
        let root = normal(root.as_ref());
        let files = files
            .into_iter()
            .map(|(name, data)| (normal(&root.join(name)), *data))
            .collect();
        Overlay { base, root, files }
    }

    /// The directory the files are mounted at.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The overlay's view of `path`: `Some(normal path)` when it lies under
    /// the root and names a file or directory there.
    fn own(&self, path: &Path) -> Option<PathBuf> {
        let p = normal(path);
        (p.starts_with(&self.root) && (self.files.contains_key(&p) || has_below(&self.files, &p)))
            .then_some(p)
    }
}

impl FileSystem for Overlay {
    fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
        match self.own(path) {
            Some(p) => match self.files.get(&p) {
                Some(d) => Ok(d.to_vec()),
                None => Err(io::Error::from(io::ErrorKind::IsADirectory)),
            },
            None => self.base.read(path),
        }
    }
    fn exists(&self, path: &Path) -> bool {
        self.own(path).is_some() || self.base.exists(path)
    }
    fn is_dir(&self, path: &Path) -> bool {
        match self.own(path) {
            Some(p) => !self.files.contains_key(&p),
            None => self.base.is_dir(path),
        }
    }
    fn canonicalize(&self, path: &Path) -> Option<PathBuf> {
        self.own(path).or_else(|| self.base.canonicalize(path))
    }
    fn metadata(&self, path: &Path) -> Option<Metadata> {
        match self.own(path) {
            // Compiled-in contents never change, so no time is needed to
            // tell versions apart.
            Some(p) => self.files.get(&p).map(|d| Metadata {
                modified: None,
                len: d.len() as u64,
            }),
            None => self.base.metadata(path),
        }
    }
    fn read_dir(&self, path: &Path) -> io::Result<Vec<PathBuf>> {
        match self.own(path) {
            Some(p) => Ok(entries(&self.files, &p)),
            None => self.base.read_dir(path),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::loader::{LibraryPath, find_valid_path};

    #[test]
    fn memfs_files_directories_and_versions() {
        let fs = MemFs::new();
        fs.insert("/doc/a.scad", b"cube();".to_vec());
        fs.insert("/doc/lib/b.scad", b"sphere();".to_vec());
        assert!(fs.exists(Path::new("/doc")) && fs.is_dir(Path::new("/doc")));
        assert!(fs.exists(Path::new("/doc/./lib/../a.scad")));
        assert!(!fs.is_dir(Path::new("/doc/a.scad")));
        assert!(!fs.exists(Path::new("/do")));
        assert_eq!(
            fs.canonicalize(Path::new("/doc/lib/../a.scad")),
            Some(PathBuf::from("/doc/a.scad"))
        );
        assert_eq!(
            fs.read_dir(Path::new("/doc")).unwrap(),
            vec![PathBuf::from("/doc/a.scad"), PathBuf::from("/doc/lib")]
        );
        let m1 = fs.metadata(Path::new("/doc/a.scad")).unwrap();
        fs.insert("/doc/a.scad", b"cube(2);".to_vec());
        let m2 = fs.metadata(Path::new("/doc/a.scad")).unwrap();
        assert_ne!(m1.modified, m2.modified);
        assert_eq!(m2.len, 8);
    }

    #[test]
    fn overlay_serves_its_files_and_falls_through() {
        let base = Arc::new(MemFs::new());
        base.insert("/doc/main.scad", b"".to_vec());
        base.insert("/res/libraries/Other/x.scad", b"x".to_vec());
        static FILES: [(&str, &[u8]); 2] = [
            ("MCAD/units.scad", b"mm = 1;"),
            ("MCAD/bitmap/bitmap.scad", b""),
        ];
        let fs = Overlay::new(base, "/res/libraries", &FILES);
        assert_eq!(
            fs.read(Path::new("/res/libraries/MCAD/units.scad"))
                .unwrap(),
            b"mm = 1;"
        );
        assert!(fs.is_dir(Path::new("/res/libraries/MCAD/bitmap")));
        assert!(fs.exists(Path::new("/res/libraries/Other/x.scad")));
        assert!(fs.exists(Path::new("/doc/main.scad")));
        assert!(!fs.exists(Path::new("/res/libraries/MCAD/nope.scad")));
        let libs = LibraryPath(vec![PathBuf::from("/res/libraries")]);
        assert_eq!(
            find_valid_path(
                &fs,
                &libs,
                Path::new("/doc"),
                Path::new("MCAD/units.scad"),
                &[]
            ),
            Some(PathBuf::from("/res/libraries/MCAD/units.scad"))
        );
    }
}
