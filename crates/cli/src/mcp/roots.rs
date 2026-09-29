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
            // Plain (no `\\?\` on Windows): the paths judged against a root
            // are built by joining onto it or come from a client, and a
            // verbatim root would contain none of them (lang::paths).
            let mut out: Vec<PathBuf> = v
                .iter()
                .filter_map(|p| p.canonicalize().ok().map(lang::paths::plain))
                .collect();
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
        !r.as_os_str().is_empty()
            && self
                .write
                .iter()
                .chain(&self.read)
                .any(|d| r.starts_with(d))
    }

    /// May `p` (absolute) be written? Judged by where it resolves
    /// ([`resolve`]), dangling links included.
    pub fn can_write(&self, p: &Path) -> bool {
        let r = resolve(p);
        !r.as_os_str().is_empty() && self.write.iter().any(|d| r.starts_with(d))
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

/// Symlinks followed before a path is judged unresolvable (Linux's
/// `MAXSYMLINKS`); a loop is refused like a path outside the roots.
const MAX_LINKS: u32 = 40;

/// Where `p` really leads: `.`/`..` folded lexically (the tools use the
/// folded path), then every symlink along it followed, the last one
/// included **even when it dangles**. A path that does not exist yet (an
/// output file) keeps its missing tail as written.
///
/// Following a dangling link matters for writes: `canonicalize` fails on
/// one, and judging it by its parent (inside the root) let
/// `out.stl -> ~/Library/LaunchAgents/x.plist` pass while the kernel
/// followed the link out of the root on write (the agent-surface audit's
/// finding 2). A link loop resolves to an empty path, which no root
/// contains.
pub fn resolve(p: &Path) -> PathBuf {
    let p = session::normal(p);
    let mut todo: std::collections::VecDeque<std::ffi::OsString> =
        std::collections::VecDeque::new();
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::Prefix(_) | Component::RootDir => out.push(c.as_os_str()),
            _ => todo.push_back(c.as_os_str().to_os_string()),
        }
    }
    let mut hops = 0;
    while let Some(name) = todo.pop_front() {
        if name == "." {
            continue;
        }
        if name == ".." {
            out.pop();
            continue;
        }
        let next = out.join(&name);
        match std::fs::symlink_metadata(&next) {
            Ok(m) if m.file_type().is_symlink() => {
                hops += 1;
                let Ok(target) = std::fs::read_link(&next) else {
                    return PathBuf::new();
                };
                if hops > MAX_LINKS {
                    return PathBuf::new();
                }
                // The target's components replace the link's; a relative
                // target is relative to the link's directory (`out`).
                if target.is_absolute() {
                    out = PathBuf::new();
                }
                for c in target.components().rev() {
                    match c {
                        Component::Prefix(_) | Component::RootDir => {}
                        _ => todo.push_front(c.as_os_str().to_os_string()),
                    }
                }
                if target.is_absolute() {
                    for c in target.components() {
                        match c {
                            Component::Prefix(_) | Component::RootDir => out.push(c.as_os_str()),
                            _ => break,
                        }
                    }
                }
            }
            _ => out = next,
        }
    }
    out
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
            // A dangling link out of the root: its parent is inside, but a
            // write would follow it (the audit's repro).
            let dangling = inside.join("dangling.stl");
            let _ = std::fs::remove_file(&dangling);
            std::os::unix::fs::symlink(outside.join("newfile.stl"), &dangling).unwrap();
            assert!(!roots.can_write(&dangling));
            assert!(!roots.can_read(&dangling));
            // A relative dangling link that climbs out, and one through a
            // link to a directory.
            let rel = inside.join("rel.png");
            let _ = std::fs::remove_file(&rel);
            std::os::unix::fs::symlink("../outside/dangle.png", &rel).unwrap();
            assert!(!roots.can_write(&rel));
            // A dangling link that stays inside is fine, and resolves to
            // its target.
            let ok = inside.join("ok.stl");
            let _ = std::fs::remove_file(&ok);
            std::os::unix::fs::symlink("new/target.stl", &ok).unwrap();
            assert!(roots.can_write(&ok));
            assert_eq!(resolve(&ok), inside.join("new/target.stl"));
            // A loop is refused.
            let (a, b) = (inside.join("loop-a"), inside.join("loop-b"));
            let _ = std::fs::remove_file(&a);
            let _ = std::fs::remove_file(&b);
            std::os::unix::fs::symlink(&b, &a).unwrap();
            std::os::unix::fs::symlink(&a, &b).unwrap();
            assert!(!roots.can_write(&a.join("x.stl")));
            assert!(!outside.join("newfile.stl").exists());
        }
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
