//! `use`d libraries: OpenSCAD parses them before running the program
//! (`SourceFile::handleDependencies` and `SourceFileCache::process`), and
//! reports their scanner and parser messages then.
//!
//! Each library is parsed once, from its text plus the same `"\n\x03\n"`
//! and `-D` suffix as the main file, and its own `use`s are followed
//! depth-first, most recently used first. A library that fails to parse
//! is reported and skipped; the program still runs.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::loader::{FileSystem, LibraryPath, find_valid_path, generic};
use crate::{Program, parse_library};

/// One `use`d library, in the order OpenSCAD processes them.
#[derive(Debug)]
pub struct Library {
    pub path: String,
    /// `None` when the file could not be read.
    pub program: Option<Program>,
}

impl Library {
    /// `WARNING: Can't open library file '...'` (printed with a trailing
    /// blank line), when the file could not be read.
    pub fn open_error(&self) -> Option<String> {
        self.program.is_none().then(|| format!("WARNING: Can't open library file '{}'\n", self.path))
    }
}

/// Parse every library `root` uses, transitively. `suffix` is what the
/// command line appended to the main text (`"\n\x03\n"` plus `-D`s).
pub fn load_dependencies(root: &Program, suffix: &[u8], fs: &dyn FileSystem, libs: &LibraryPath) -> Vec<Library> {
    let main = root.sources.path(root.main).to_path_buf();
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    let dir = main.parent().map(Path::to_path_buf).unwrap_or_default();
    visit(&root.ast.uses, &dir, &main, suffix, fs, libs, &mut seen, &mut out);
    out
}

#[allow(clippy::too_many_arguments)]
fn visit(
    uses: &[String],
    dir: &Path,
    main: &Path,
    suffix: &[u8],
    fs: &dyn FileSystem,
    libs: &LibraryPath,
    seen: &mut HashSet<String>,
    out: &mut Vec<Library>,
) {
    for name in uses {
        // Names that were not found while scanning are searched again.
        let path = if Path::new(name).is_absolute() {
            PathBuf::from(name)
        } else {
            match find_valid_path(fs, libs, dir, Path::new(name), &[]) {
                Some(p) => p,
                None => continue,
            }
        };
        let key = generic(&path);
        if !fs.exists(&path) || !seen.insert(key.clone()) {
            continue;
        }
        // A directory (`use </>`) opens as an empty stream in OpenSCAD, so
        // it parses as an empty library rather than failing to open.
        let read = if fs.is_dir(&path) { Ok(Vec::new()) } else { fs.read(&path) };
        let Ok(mut text) = read else {
            out.push(Library { path: key, program: None });
            continue;
        };
        text.extend_from_slice(suffix);
        let program = parse_library(path.clone(), text, main, fs, libs);
        let ok = !program.has_syntax_errors();
        let lib_uses = program.ast.uses.clone();
        out.push(Library { path: key, program: Some(program) });
        if ok {
            let lib_dir = path.parent().map(Path::to_path_buf).unwrap_or_default();
            visit(&lib_uses, &lib_dir, main, suffix, fs, libs, seen, out);
        }
    }
}
