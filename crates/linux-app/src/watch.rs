//! Which files a window watches, without GIO: the files its last run read
//! that another program could change (`Client::run_files`: includes, used
//! libraries, imports and fonts on disk, not the document itself, which
//! the window writes, and not what exists only in memory, the bundled
//! MCAD), grouped by directory.
//!
//! The window watches the directories, not the files (`GFileMonitor` per
//! directory, `app/window.rs`), as the macOS app's `FileWatcher.swift`
//! watches with FSEvents: most editors save by writing a temporary file
//! and renaming it over the original, which replaces the file, and a
//! monitor of the old file can miss the new one; the directory sees the
//! new file arrive under the same name. A change to any other file in
//! those directories is ignored here.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// The watched files, by directory.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WatchSet {
    dirs: BTreeMap<PathBuf, BTreeSet<OsString>>,
}

impl WatchSet {
    /// Watch exactly `files` from now on. Whether the directories changed
    /// (the host then replaces its monitors; the same directories with
    /// other files in them need no new monitor).
    pub fn set(&mut self, files: impl IntoIterator<Item = PathBuf>) -> bool {
        let mut dirs: BTreeMap<PathBuf, BTreeSet<OsString>> = BTreeMap::new();
        for f in files {
            if let (Some(dir), Some(name)) = (f.parent(), f.file_name()) {
                dirs.entry(dir.to_path_buf())
                    .or_default()
                    .insert(name.to_os_string());
            }
        }
        let changed = !dirs.keys().eq(self.dirs.keys());
        self.dirs = dirs;
        changed
    }

    /// The directories to monitor, in order.
    pub fn directories(&self) -> Vec<PathBuf> {
        self.dirs.keys().cloned().collect()
    }

    /// Whether a change to `path` (as a directory monitor reports it: the
    /// directory joined with the file's name) is a change to a watched
    /// file.
    pub fn concerns(&self, path: &Path) -> bool {
        match (path.parent(), path.file_name()) {
            (Some(dir), Some(name)) => self.dirs.get(dir).is_some_and(|n| n.contains(name)),
            _ => false,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.dirs.is_empty()
    }

    /// Watch nothing (the window closed).
    pub fn clear(&mut self) {
        self.dirs.clear();
    }
}

/// The files of a finished run worth watching: those `Client::run_files`
/// names that are files on this disk.
pub fn on_disk<'a>(files: impl IntoIterator<Item = &'a PathBuf>) -> Vec<PathBuf> {
    files.into_iter().filter(|f| f.is_file()).cloned().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn files_are_watched_through_their_directories() {
        let mut w = WatchSet::default();
        assert!(w.is_empty());
        assert!(w.set([
            PathBuf::from("/a/inc.scad"),
            PathBuf::from("/a/other.scad"),
            PathBuf::from("/b/lib/x.scad"),
        ]));
        assert_eq!(
            w.directories(),
            [PathBuf::from("/a"), PathBuf::from("/b/lib")]
        );
        assert!(w.concerns(Path::new("/a/inc.scad")));
        assert!(w.concerns(Path::new("/b/lib/x.scad")));
        // Another file in a watched directory, a temporary file an editor
        // writes before its rename, and a file elsewhere.
        assert!(!w.concerns(Path::new("/a/model.scad")));
        assert!(!w.concerns(Path::new("/a/.inc.scad.swp")));
        assert!(!w.concerns(Path::new("/c/inc.scad")));
        // Other files in the same directories: no new monitors.
        assert!(!w.set([PathBuf::from("/a/inc.scad"), PathBuf::from("/b/lib/y.scad")]));
        assert!(!w.concerns(Path::new("/a/other.scad")));
        assert!(w.concerns(Path::new("/b/lib/y.scad")));
        assert!(w.set([]));
        assert!(w.is_empty());
    }

    #[test]
    fn only_files_on_disk_are_watched() {
        let dir =
            std::env::temp_dir().join(format!("neoscad-linux-app-watch-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let real = dir.join("inc.scad");
        std::fs::write(&real, "x = 1;").unwrap();
        // The bundled MCAD exists only in the core's memory.
        let memory = PathBuf::from("/nonexistent-neoscad-mcad/regular_shapes.scad");
        assert_eq!(on_disk([&real, &memory, &dir]), std::slice::from_ref(&real));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
