//! What only a host may use: the real file system ([`StdFs`]) and the
//! library path the process environment names ([`LibraryPath::from_env`],
//! [`LibraryPath::user_dir`]).
//!
//! This module is the one place in the library crates that reaches the
//! disk or the environment, and it is compiled only with the `host`
//! feature. The command line, the app cores and the test suites turn the
//! feature on; no library crate does in its `[dependencies]`, so library
//! code cannot fall back on the disk by itself (a default that did would
//! read the host's disk from a WASM page or an in-memory session, and a
//! wasm32 build of the library crates would not see the mistake, since
//! there the calls fail quietly). `wasm-check` builds without it, which
//! catches a library crate that starts using it, and
//! `tests/host_boundary.rs` checks the manifests and sources.

use std::io;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use crate::loader::{FileSystem, LibraryPath, Metadata};

/// The real file system.
#[derive(Debug, Default, Clone, Copy)]
pub struct StdFs;

impl FileSystem for StdFs {
    fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
        std::fs::read(path)
    }
    fn exists(&self, path: &Path) -> bool {
        path.exists()
    }
    fn is_dir(&self, path: &Path) -> bool {
        path.is_dir()
    }
    fn canonicalize(&self, path: &Path) -> Option<PathBuf> {
        // Without `\\?\` on Windows, so found includes compare equal to
        // (and print like) every other path (see `crate::paths`). A
        // relative path resolves against the process's working directory,
        // which is how message paths learn it (`diag::relative_path`).
        path.canonicalize().ok().map(crate::paths::plain)
    }
    fn metadata(&self, path: &Path) -> Option<Metadata> {
        let m = std::fs::metadata(path).ok()?;
        let modified = m
            .modified()
            .ok()
            .map(|t| match t.duration_since(UNIX_EPOCH) {
                Ok(d) => d.as_nanos() as i128,
                Err(e) => -(e.duration().as_nanos() as i128),
            });
        Some(Metadata {
            modified,
            len: m.len(),
        })
    }
    fn read_dir(&self, path: &Path) -> io::Result<Vec<PathBuf>> {
        std::fs::read_dir(path)?
            .map(|e| e.map(|e| e.path()))
            .collect()
    }
}

impl LibraryPath {
    /// `OPENSCADPATH` entries, then the per-user library directory, as
    /// `parser_init()` in parsersettings.cc builds it (the resource
    /// directory it adds last is the host's to add). A WASM host builds
    /// its path directly.
    pub fn from_env() -> Self {
        let mut dirs = Vec::new();
        let cwd = std::env::current_dir().unwrap_or_default();
        if let Some(paths) = std::env::var_os("OPENSCADPATH") {
            let sep = if cfg!(windows) { ';' } else { ':' };
            for p in paths.to_string_lossy().split(sep) {
                dirs.push(if p.is_empty() {
                    cwd.clone()
                } else {
                    cwd.join(p)
                });
            }
        }
        if let Some(user) = Self::user_dir() {
            dirs.push(user);
        }
        Self(dirs)
    }

    /// The per-user library directory, `PlatformUtils::userLibraryPath()`
    /// (`PlatformUtils.cc`, `userPath`): `<documents>/OpenSCAD/libraries`,
    /// where the documents directory is `~/Documents` on macOS
    /// (`PlatformUtils-mac.mm`), `$HOME/.local/share` on other Unix systems
    /// (`PlatformUtils-posix.cc`, `documentsPath`), and the shell's
    /// Documents folder on Windows (`PlatformUtils-win.cc`). `None` when
    /// that base cannot be found (no `HOME`, or no known folder).
    pub fn user_dir() -> Option<PathBuf> {
        let base = if cfg!(windows) {
            windows_documents()?
        } else {
            let home = PathBuf::from(std::env::var_os("HOME")?);
            if cfg!(target_os = "macos") {
                home.join("Documents")
            } else {
                home.join(".local/share")
            }
        };
        Some(base.join("OpenSCAD").join("libraries"))
    }
}

/// The Windows Documents folder. OpenSCAD asks the shell for
/// `CSIDL_PERSONAL` (`PlatformUtils-win.cc`, `documentsPath`), whose
/// current name is `FOLDERID_Documents`; `dirs` makes that known-folder
/// call without `unsafe` in this crate. `%USERPROFILE%\Documents` is not
/// equivalent: it is wrong whenever the folder is redirected (OneDrive's
/// Documents backup does this by default on Windows 11), and libraries a
/// user installed for OpenSCAD would then not be found.
#[cfg(windows)]
fn windows_documents() -> Option<PathBuf> {
    dirs::document_dir()
}

#[cfg(not(windows))]
fn windows_documents() -> Option<PathBuf> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `PlatformUtils::userLibraryPath()`: `<documents>/OpenSCAD/libraries`,
    /// with documents at `~/Documents` on macOS and `~/.local/share` on
    /// other Unix systems. (Windows' known folder is checked by CI's
    /// Windows job against PowerShell's `MyDocuments`.)
    #[test]
    fn the_user_library_dir_is_openscads() {
        let Some(dir) = LibraryPath::user_dir() else {
            return;
        };
        assert!(dir.ends_with("OpenSCAD/libraries"), "{}", dir.display());
        if cfg!(unix)
            && let Some(home) = std::env::var_os("HOME")
        {
            let docs = if cfg!(target_os = "macos") {
                "Documents"
            } else {
                ".local/share"
            };
            assert_eq!(
                dir,
                PathBuf::from(home).join(docs).join("OpenSCAD/libraries")
            );
        }
        assert_eq!(LibraryPath::from_env().0.last(), Some(&dir));
    }
}
