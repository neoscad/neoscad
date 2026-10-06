//! `use <font.ttf>` (`SourceFile::registerUse`): a font file is registered
//! as a font, never parsed as a library, and a missing one prints
//! OpenSCAD's error after the usual warning.

use std::path::{Path, PathBuf};

use lang::deps::{load_dependencies, resolve_uses};
use lang::diag::{DiagCode, Severity};
use lang::loader::{LibraryPath, is_font_path};
use lang::vfs::MemFs;
use lang::{Program, parse_program};

fn parse(fs: &MemFs, src: &str) -> Program {
    parse_program(
        PathBuf::from("/doc/m.scad"),
        src.as_bytes().to_vec(),
        fs,
        &LibraryPath(Vec::new()),
    )
}

fn lines(p: &Program) -> Vec<String> {
    p.openscad_diags()
        .map(|d| {
            d.render_openscad(
                &p.sources,
                Path::new("/doc"),
                Path::new("/doc"),
                &MemFs::new(),
            )
        })
        .collect()
}

#[test]
fn font_names_are_recognised_in_any_case() {
    assert!(is_font_path("a.ttf"));
    assert!(is_font_path("dir/A.OTF"));
    assert!(is_font_path("x.TtF"));
    assert!(!is_font_path("a.scad"));
    assert!(!is_font_path("ttf"));
    assert!(!is_font_path("a.ttf.scad"));
}

#[test]
fn a_missing_font_prints_openscads_error_after_the_warning() {
    let fs = MemFs::new();
    let p = parse(&fs, "use <missing.ttf>\ncube(1);\n");
    assert_eq!(
        lines(&p),
        [
            "WARNING: Can't open library 'missing.ttf'. in file m.scad, line 1",
            "ERROR: Can't read font with path 'missing.ttf'",
        ]
    );
    let font = p
        .diags
        .iter()
        .find(|d| d.code == DiagCode::FontNotFound)
        .unwrap();
    assert_eq!(font.severity, Severity::Error);
    // It is not a syntax error: the program still runs, as OpenSCAD's does.
    assert!(!p.has_syntax_errors());
    // A missing library is only the warning.
    let q = parse(&fs, "use <missing.scad>\n");
    assert_eq!(
        lines(&q),
        ["WARNING: Can't open library 'missing.scad'. in file m.scad, line 1"]
    );
}

#[test]
fn a_font_that_exists_is_not_parsed_as_a_library() {
    let fs = MemFs::new();
    // Bytes that would be syntax errors as OpenSCAD source.
    fs.insert("/doc/f.ttf", b"\x00\x01\x00\x00 ((( not scad".to_vec());
    let p = parse(&fs, "use <f.ttf>\ncube(1);\n");
    assert!(lines(&p).is_empty(), "{:?}", lines(&p));
    // Still listed for the host to register as a font.
    assert!(
        p.ast.uses.iter().any(|u| u.ends_with("f.ttf")),
        "{:?}",
        p.ast.uses
    );
    assert!(load_dependencies(&p, b"", &fs, &LibraryPath(Vec::new())).is_empty());
    assert!(resolve_uses(&p, &fs, &LibraryPath(Vec::new())).is_empty());
}
