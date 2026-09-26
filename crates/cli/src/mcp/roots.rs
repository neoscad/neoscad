//! What `neoscad mcp` may touch: the allowed roots (the working directory
//! and each `--root`) for reading and writing, plus the library path and
//! font directories for reading only.
//!
//! Two layers enforce it. Tool arguments that name files (`path`,
//! `base_dir`, `export`, `output`, `diff_against`) are checked before a
//! request starts, so an agent gets a clear refusal. And the session's
//! file system is a [`RootedFs`], which refuses every read outside the
//! allowed directories: a model's `include`, `use`, `import()` or
//! `surface()` names files too, and without this layer inline source
//! could read any file on the machine (`import("/etc/passwd")` fails to
//! parse, but its error text may quote the file).

use std::io;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use lang::loader::{FileSystem, Metadata};

/// The directories a client may use.
#[derive(Debug, Clone)]
pub struct Roots {
    /// Read and write, canonical. The first is the default base directory.
    write: Vec<PathBuf>,
    /// Read only (libraries, fonts), canonical.
    read: Vec<PathBuf>,
}

impl Roots {
    /// `write` are the working directory and `--root`s; `read` adds the
    /// library path and font directories. Directories that do not exist
    /// are dropped (they cannot contain anything).
    pub fn new(write: &[PathBuf], read: &[PathBuf]) -> Roots {
        let canon = |v: &[PathBuf]| -> Vec<PathBuf> {
            let mut out: Vec<PathBuf> = v.iter().filter_map(|p| p.canonicalize().ok()).collect();
            out.dedup();
            out
        };
        Roots {
            write: canon(write),
            read: canon(read),
        }
    }

    /// The directory relative paths resolve against when no `base_dir`
    /// is given: the first root (the working directory).
    pub fn home(&self) -> &Path {
        self.write.first().map_or(Path::new("/"), PathBuf::as_path)
    }

    pub fn writable(&self) -> &[PathBuf] {
        &self.write
    }

    /// May `p` (absolute) be read?
    pub fn can_read(&self, p: &Path) -> bool {
        let r = resolve(p);
        self.write
            .iter()
            .chain(&self.read)
            .any(|d| r.starts_with(d))
    }

    /// May `p` (absolute) be written?
    pub fn can_write(&self, p: &Path) -> bool {
        let r = resolve(p);
        self.write.iter().any(|d| r.starts_with(d))
    }

    /// Why `p` was refused, for the agent: which roots there are and how
    /// to add one.
    pub fn refusal(&self, what: &str, p: &Path) -> String {
        let roots: Vec<String> = self.write.iter().map(|d| d.display().to_string()).collect();
        format!(
            "{what} '{}' is outside the allowed roots ({}); start `neoscad mcp` with `--root DIR` to allow another directory",
            p.display(),
            roots.join(", ")
        )
    }
}

/// `p` with symlinks resolved as far as it exists and `.`/`..` folded
/// lexically after that, so neither a symlink inside a root nor `..`
/// can lead outside it. A path that does not exist yet (an output file)
/// is judged by its deepest existing ancestor.
fn resolve(p: &Path) -> PathBuf {
    let p = session::normal(p);
    let mut existing = p.as_path();
    let mut rest: Vec<Component<'_>> = Vec::new();
    loop {
        if let Ok(c) = existing.canonicalize() {
            let mut out = c;
            for comp in rest.iter().rev() {
                out.push(comp);
            }
            return session::normal(&out);
        }
        let (Some(parent), Some(last)) = (existing.parent(), existing.components().next_back())
        else {
            return p;
        };
        rest.push(last);
        existing = parent;
    }
}

/// A file system that refuses everything outside the roots' readable
/// directories: reads fail with "permission denied", and outside files
/// do not exist, so the evaluator reports them as it reports any file it
/// cannot open.
pub struct RootedFs {
    base: Arc<dyn FileSystem + Send + Sync>,
    roots: Roots,
}

impl std::fmt::Debug for RootedFs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RootedFs")
            .field("roots", &self.roots)
            .finish()
    }
}

impl RootedFs {
    pub fn new(base: Arc<dyn FileSystem + Send + Sync>, roots: Roots) -> RootedFs {
        RootedFs { base, roots }
    }

    fn ok(&self, p: &Path) -> bool {
        // Relative paths are the working directory's, which is a root.
        if p.is_relative() {
            return self.roots.can_read(&self.roots.home().join(p));
        }
        self.roots.can_read(p)
    }
}

fn denied(p: &Path) -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        format!("'{}' is outside the allowed roots", p.display()),
    )
}

impl FileSystem for RootedFs {
    fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
        if !self.ok(path) {
            return Err(denied(path));
        }
        self.base.read(path)
    }
    fn exists(&self, path: &Path) -> bool {
        self.ok(path) && self.base.exists(path)
    }
    fn is_dir(&self, path: &Path) -> bool {
        self.ok(path) && self.base.is_dir(path)
    }
    fn canonicalize(&self, path: &Path) -> Option<PathBuf> {
        if !self.ok(path) {
            return None;
        }
        self.base.canonicalize(path)
    }
    fn metadata(&self, path: &Path) -> Option<Metadata> {
        if !self.ok(path) {
            return None;
        }
        self.base.metadata(path)
    }
    fn read_dir(&self, path: &Path) -> io::Result<Vec<PathBuf>> {
        if !self.ok(path) {
            return Err(denied(path));
        }
        self.base.read_dir(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roots_refuse_escapes() {
        let tmp = std::env::temp_dir().join(format!("nsroots-{}", std::process::id()));
        let inside = tmp.join("inside");
        let outside = tmp.join("outside");
        std::fs::create_dir_all(&inside).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("secret.scad"), "cube(1);").unwrap();
        let roots = Roots::new(std::slice::from_ref(&inside), &[]);
        let inside = inside.canonicalize().unwrap();
        assert!(roots.can_write(&inside.join("new/deep/model.stl")));
        assert!(!roots.can_read(&inside.join("../outside/secret.scad")));
        assert!(!roots.can_write(Path::new("/etc/x")));
        // A symlink inside a root that points out of it is outside.
        #[cfg(unix)]
        {
            let link = inside.join("link");
            let _ = std::fs::remove_file(&link);
            std::os::unix::fs::symlink(&outside, &link).unwrap();
            assert!(!roots.can_read(&link.join("secret.scad")));
            let fs = RootedFs::new(Arc::new(lang::loader::StdFs), roots.clone());
            assert!(fs.read(&link.join("secret.scad")).is_err());
            assert!(!fs.exists(&link.join("secret.scad")));
        }
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
