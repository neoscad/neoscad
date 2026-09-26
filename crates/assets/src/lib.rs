//! The resources OpenSCAD installs next to its executable, compiled in:
//! the Liberation fonts (the default font is Liberation Sans) and the MCAD
//! library. See `assets/README.md` for their sources and licences.
//!
//! A host adds the fonts to its [`text::FontDb`] with [`add_fonts`] and
//! mounts the libraries with [`libraries`], then appends the mount point to
//! its [`lang::loader::LibraryPath`] after every other directory, where
//! OpenSCAD puts `<resources>/libraries` (`parser_init()` in
//! `parsersettings.cc`): a library of the same name in `OPENSCADPATH` or
//! the user library directory wins.

use std::path::Path;
use std::sync::Arc;

use lang::loader::FileSystem;
use lang::vfs::Overlay;

mod tables {
    include!(concat!(env!("OUT_DIR"), "/assets.rs"));
}

/// The bundled font files: path under `assets/fonts`, contents; in the
/// order a font directory is scanned.
pub static FONTS: &[(&str, &[u8])] = tables::FONTS;

/// The bundled library files: path under `assets/libraries` (so starting
/// with `MCAD/`), contents.
pub static LIBRARIES: &[(&str, &[u8])] = tables::LIBRARIES;

/// Add the bundled fonts to `db` (without copying them).
pub fn add_fonts(db: &mut text::FontDb) {
    for (_, data) in FONTS {
        db.add_static(data);
    }
}

/// `base` with the bundled libraries mounted at `root` (the directory to
/// append to the library path).
pub fn libraries(base: Arc<dyn FileSystem + Send + Sync>, root: impl AsRef<Path>) -> Overlay {
    Overlay::new(base, root, LIBRARIES)
}

#[cfg(test)]
mod tests {
    use super::*;
    use lang::loader::{LibraryPath, StdFs, find_valid_path};
    use std::path::PathBuf;

    fn reference() -> Option<PathBuf> {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.reference/openscad");
        if root.join("fonts").is_dir() {
            Some(root)
        } else {
            eprintln!("skipped: no reference checkout");
            None
        }
    }

    /// The bundled files are the reference checkout's, byte for byte, and
    /// every font there is bundled: the conformance runner relies on this
    /// to use the bundled fonts for the suite's expected outputs.
    #[test]
    fn identical_to_the_reference_checkout() {
        let Some(root) = reference() else { return };
        for (name, data) in FONTS {
            let theirs = std::fs::read(root.join("fonts").join(name)).expect(name);
            assert!(theirs == *data, "fonts/{name} differs from the reference");
        }
        let mut theirs = Vec::new();
        let mut stack = vec![root.join("fonts")];
        while let Some(d) = stack.pop() {
            for e in std::fs::read_dir(&d).unwrap() {
                let p = e.unwrap().path();
                if p.is_dir() {
                    stack.push(p);
                } else if p.extension().is_some_and(|e| e == "ttf") {
                    theirs.push(p);
                }
            }
        }
        assert_eq!(theirs.len(), FONTS.len(), "fonts in the reference");
        let mcad = root.join("libraries");
        if mcad.join("MCAD/units.scad").is_file() {
            for (name, data) in LIBRARIES {
                let theirs = std::fs::read(mcad.join(name)).expect(name);
                assert!(
                    theirs == *data,
                    "libraries/{name} differs from the reference"
                );
            }
        }
    }

    #[test]
    fn default_font_and_mcad_resolve() {
        let mut db = text::FontDb::new();
        add_fonts(&mut db);
        let face = db.lookup("").expect("the default font");
        let (_, regular) = FONTS
            .iter()
            .find(|(n, _)| n.ends_with("/LiberationSans-Regular.ttf"))
            .expect("Liberation Sans Regular is bundled");
        assert!(std::ptr::eq(face.data.as_ptr(), regular.as_ptr()));

        let fs = libraries(Arc::new(StdFs), "/nonexistent-neoscad/libraries");
        let libs = LibraryPath(vec![PathBuf::from("/nonexistent-neoscad/libraries")]);
        let p = find_valid_path(
            &fs,
            &libs,
            Path::new("/"),
            Path::new("MCAD/units.scad"),
            &[],
        )
        .expect("MCAD/units.scad");
        let (_, units) = LIBRARIES
            .iter()
            .find(|(n, _)| *n == "MCAD/units.scad")
            .expect("bundled");
        assert_eq!(fs.read(&p).unwrap(), *units);
    }
}
