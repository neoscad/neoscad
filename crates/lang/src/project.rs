//! What a project adds to the library path of one main file, and the hint
//! an include it cannot resolve gets (`docs/dependencies.md`).
//!
//! A host finds the project (the nearest `scad.toml` above the main file)
//! and reads its links; this module only holds the answer, so library
//! crates never look for a project on the disk themselves. The project's
//! directories go after the including file's own directory and before the
//! host's library path (`OPENSCADPATH`, the user library folder, the
//! bundled libraries): [`ProjectLibs::library_path`].

use std::path::PathBuf;

use crate::diag::{DiagCode, Diagnostic, Hint};
use crate::loader::LibraryPath;

/// A project's part of the search path for one main file.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProjectLibs {
    /// The project root (the directory holding `scad.toml`).
    pub root: PathBuf,
    /// Directories searched before the host's library path, in order:
    /// the managed `<root>/.scad/libraries`, then the manifest's own
    /// `[paths]` entries.
    pub dirs: Vec<PathBuf>,
    /// The dependencies the manifest declares, by the folder name an
    /// `include <Name/…>` uses.
    pub declared: Vec<Declared>,
    /// Changes whenever a link in the managed directory points somewhere
    /// else (after `neoscad fetch` or `update`), so a host that caches
    /// parses keyed by file can tell that an include may now resolve to
    /// another copy.
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

impl ProjectLibs {
    /// `base` (the host's library path) with this project's directories
    /// in front.
    pub fn library_path(&self, base: &LibraryPath) -> LibraryPath {
        LibraryPath(self.dirs.iter().chain(base.0.iter()).cloned().collect())
    }

    /// The hint for an `include`/`use` of `local` (as written) that did
    /// not resolve: a declared dependency that is not fetched yet names
    /// the command that fetches it, and a library-like path (`Name/…`)
    /// the manifest does not declare names the command that adds it.
    pub fn missing_hint(&self, local: &str) -> Option<String> {
        let local = local.replace('\\', "/");
        let (first, _) = local.split_once('/')?;
        if first.is_empty() || first == "." || first == ".." {
            return None;
        }
        match self.declared.iter().find(|d| d.folder == first) {
            Some(d) if !d.fetched => Some(format!(
                "`{first}` is declared in scad.toml but not fetched: run `neoscad fetch`"
            )),
            Some(_) => Some(format!(
                "`{first}` is fetched, but has no '{}': check the path, and the dependency's `include_root`",
                &local[first.len() + 1..]
            )),
            None => Some(format!(
                "`{first}` is not a dependency in scad.toml: add it with `neoscad add {first} --git URL --tag TAG` and run `neoscad fetch`"
            )),
        }
    }
}

impl ProjectLibs {
    /// `d` with [`ProjectLibs::missing_hint`] added, when it is an
    /// include or `use` that did not resolve and the project has a hint
    /// for it; None leaves `d` as it is.
    pub fn annotate(&self, d: &Diagnostic) -> Option<Diagnostic> {
        if !matches!(
            d.code,
            DiagCode::IncludeNotFound | DiagCode::LibraryNotFound
        ) {
            return None;
        }
        // "Can't open include file 'X'.", "Can't open library 'X'." (the
        // last quoted name: "Can't" has a quote too).
        let end = d.message.rfind('\'')?;
        let start = d.message[..end].rfind('\'')? + 1;
        let hint = self.missing_hint(&d.message[start..end])?;
        let mut d = d.clone();
        d.hints.push(Hint {
            message: hint,
            replacement: None,
        });
        Some(d)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project() -> ProjectLibs {
        ProjectLibs {
            root: "/p".into(),
            dirs: vec!["/p/.scad/libraries".into()],
            declared: vec![
                Declared {
                    folder: "BOSL2".into(),
                    fetched: false,
                },
                Declared {
                    folder: "Mine".into(),
                    fetched: true,
                },
            ],
            stamp: 0,
        }
    }

    #[test]
    fn the_project_goes_before_the_host_path() {
        let base = LibraryPath(vec!["/env".into(), "/user".into()]);
        assert_eq!(
            project().library_path(&base).0,
            vec![
                PathBuf::from("/p/.scad/libraries"),
                "/env".into(),
                "/user".into()
            ]
        );
    }

    #[test]
    fn hints_name_the_command() {
        let p = project();
        assert_eq!(
            p.missing_hint("BOSL2/std.scad").unwrap(),
            "`BOSL2` is declared in scad.toml but not fetched: run `neoscad fetch`"
        );
        assert!(
            p.missing_hint("Other/x.scad")
                .unwrap()
                .contains("neoscad add Other")
        );
        assert!(
            p.missing_hint("Mine/nope.scad")
                .unwrap()
                .contains("include_root")
        );
        assert_eq!(p.missing_hint("plain.scad"), None);
        assert_eq!(p.missing_hint("../up.scad"), None);
        let d = Diagnostic::new(
            DiagCode::IncludeNotFound,
            crate::diag::Severity::Warning,
            "Can't open include file 'BOSL2/std.scad'.",
        );
        assert_eq!(
            p.annotate(&d).unwrap().hints[0].message,
            p.missing_hint("BOSL2/std.scad").unwrap()
        );
    }
}
