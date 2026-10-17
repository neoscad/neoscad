//! What a project adds to a main file's library path: a host calls
//! [`project_for`] for each file it evaluates (it reads a few small files
//! and links, and runs no git) and searches [`ProjectPaths::dirs`] after
//! the including file's own directory and before its own library path
//! (`OPENSCADPATH` and the rest).

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

use crate::lock::Lock;
use crate::manifest::{LIBRARIES, LOCKFILE, MANIFEST, Manifest};

/// A project's part of the search path for one main file.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProjectPaths {
    /// The project root (the directory holding `scad.toml`).
    pub root: PathBuf,
    /// Directories searched before the host's library path, in order:
    /// the managed `<root>/.scad/libraries`, then the manifest's own
    /// `[paths] libraries`.
    pub dirs: Vec<PathBuf>,
    /// The dependencies the manifest (and the lockfile) declare, by the
    /// folder name an `include <Name/…>` uses.
    pub declared: Vec<Declared>,
    /// Changes whenever a link in the managed directory points somewhere
    /// else (after a fetch or an update), so a host that caches parses
    /// keyed by file can tell that an include may now resolve to another
    /// copy.
    pub stamp: u64,
}

/// One declared dependency.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Declared {
    /// The folder name: `include <BOSL2/std.scad>` names `BOSL2`.
    pub folder: String,
    /// Whether its link exists and resolves.
    pub fetched: bool,
}

/// The nearest directory at or above `start` holding a `scad.toml`.
pub fn find_root(start: &Path) -> Option<PathBuf> {
    let start = std::path::absolute(start).ok()?;
    start
        .ancestors()
        .find(|d| d.join(MANIFEST).is_file())
        .map(Path::to_path_buf)
}

/// The project of the file `main` (the nearest `scad.toml` above it):
/// `<root>/.scad/libraries`, then the manifest's `[paths] libraries`, and
/// its declared dependencies with whether each is fetched. None outside a
/// project.
pub fn project_for(main: &Path) -> Option<ProjectPaths> {
    let dir = main.parent()?;
    let root = find_root(if dir.as_os_str().is_empty() {
        Path::new(".")
    } else {
        dir
    })?;
    let libs = root.join(LIBRARIES);
    let mut dirs = vec![libs.clone()];
    let mut declared: Vec<Declared> = Vec::new();
    let mut add = |folder: &str| {
        if !declared.iter().any(|d| d.folder == folder) {
            declared.push(Declared {
                folder: folder.to_string(),
                fetched: libs.join(folder).is_dir(),
            });
        }
    };
    // A manifest that does not parse still gives the managed directory;
    // a fetch reports the error.
    if let Ok(m) = Manifest::read(&root) {
        for d in &m.deps {
            add(&d.folder);
        }
        dirs.extend(m.paths.iter().map(|p| root.join(p)));
    }
    if let Ok(Some(lock)) = Lock::read(&root.join(LOCKFILE)) {
        for p in &lock.packages {
            add(&p.folder);
        }
    }
    let mut h = DefaultHasher::new();
    if let Ok(rd) = std::fs::read_dir(&libs) {
        let mut entries: Vec<(String, Option<PathBuf>)> = rd
            .flatten()
            .map(|e| {
                (
                    e.file_name().to_string_lossy().into_owned(),
                    std::fs::read_link(e.path()).ok(),
                )
            })
            .collect();
        entries.sort();
        entries.hash(&mut h);
    }
    Some(ProjectPaths {
        root,
        dirs,
        declared,
        stamp: h.finish(),
    })
}
