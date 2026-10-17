//! What a project adds to a main file's library path: hosts call
//! [`project_for`] for each file they evaluate (it reads a few small files
//! and links, and runs no git), and put the answer in front of their
//! library path (`lang::project::ProjectLibs::library_path`).

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

use lang::project::{Declared, ProjectLibs};

use crate::lock::Lock;
use crate::manifest::{LIBRARIES, LOCKFILE, MANIFEST, Manifest};

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
pub fn project_for(main: &Path) -> Option<ProjectLibs> {
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
    // `neoscad fetch` reports the error.
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
    Some(ProjectLibs {
        root,
        dirs,
        declared,
        stamp: h.finish(),
    })
}
