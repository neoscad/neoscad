//! Included files parsed once (`lang::fragment`): a program that takes
//! its includes' parses from a fragment cache must equal the program
//! parsed with every include spliced as tokens, in every part (sources,
//! tokens, syntax tree, AST and its numbering, uses and diagnostics).

use std::cell::Cell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use lang::fragment::{Fragment, FragmentCache, FragmentKey, SpliceStats};
use lang::loader::{FileSystem, LibraryPath, StdFs};
use lang::vfs::MemFs;
use lang::{Caches, Program, parse_program, parse_program_with};

#[derive(Default)]
struct Store(Mutex<HashMap<FragmentKey, Arc<Fragment>>>);

impl FragmentCache for Store {
    fn get(&self, key: &FragmentKey) -> Option<Arc<Fragment>> {
        self.0.lock().unwrap().get(key).cloned()
    }
    fn put(&self, key: FragmentKey, fragment: Arc<Fragment>) {
        self.0.lock().unwrap().insert(key, fragment);
    }
}

/// Where two programs differ, if they do.
fn diff(a: &Program, b: &Program) -> Option<String> {
    let files = |p: &Program| {
        p.sources
            .iter()
            .map(|(_, f)| (f.path.clone(), f.text.clone()))
            .collect::<Vec<_>>()
    };
    if files(a) != files(b) {
        return Some("sources".into());
    }
    if a.cst.tokens() != b.cst.tokens() {
        return Some("tokens".into());
    }
    if a.cst != b.cst {
        return Some(format!(
            "syntax tree:\n{}\n---\n{}",
            a.cst.debug_dump(&a.sources),
            b.cst.debug_dump(&b.sources)
        ));
    }
    if a.ast.names.iter().collect::<Vec<_>>() != b.ast.names.iter().collect::<Vec<_>>() {
        return Some("names".into());
    }
    if a.ast.exprs != b.ast.exprs {
        return Some("expressions".into());
    }
    if a.ast.root != b.ast.root {
        return Some(format!(
            "root scope:\n{:#?}\n---\n{:#?}",
            a.ast.root, b.ast.root
        ));
    }
    if a.ast.uses != b.ast.uses {
        return Some(format!("ast uses: {:?} / {:?}", a.ast.uses, b.ast.uses));
    }
    if a.uses != b.uses {
        return Some(format!("uses: {:?} / {:?}", a.uses, b.uses));
    }
    if a.diags != b.diags {
        return Some(format!("diagnostics:\n{:#?}\n---\n{:#?}", a.diags, b.diags));
    }
    None
}

/// Parse `main` plain, then twice through one fragment cache (building,
/// then reusing), and check all three agree; returns the stats of the
/// cached parses.
fn check(fs: &dyn FileSystem, libs: &LibraryPath, path: &str, main: &[u8]) -> SpliceStats {
    let plain = parse_program(path.into(), main.to_vec(), fs, libs);
    let store = Store::default();
    let stats = Cell::default();
    for round in 0..2 {
        let cached = parse_program_with(
            path.into(),
            main.to_vec(),
            fs,
            libs,
            Caches {
                fragments: Some(&store),
                stats: Some(&stats),
                ..Caches::default()
            },
        );
        if let Some(d) = diff(&plain, &cached) {
            panic!("{path} (round {round}) differs in {d}");
        }
    }
    stats.get()
}

fn mem(files: &[(&str, &str)]) -> MemFs {
    let fs = MemFs::new();
    for (p, t) in files {
        fs.insert(p, t.as_bytes().to_vec());
    }
    fs
}

fn run(files: &[(&str, &str)], main: &str) -> SpliceStats {
    let fs = mem(files);
    let mut text = main.as_bytes().to_vec();
    text.extend_from_slice(b"\n\x03\n");
    check(
        &fs,
        &LibraryPath(vec!["/lib".into()]),
        "/d/main.scad",
        &text,
    )
}

#[test]
fn top_level_includes_become_fragments() {
    let s = run(
        &[
            (
                "/d/f.scad",
                "a = 2; b = \"\\q\"; a = 3;\nmodule m() { x = 1; x = 2; cube(x); }\n\
                 function g(v) = -v + [1:2:3];\n{ c = 1; }\nif (a) cube(); else sphere();\n\
                 use <u.scad>\nuse <>\ninclude <g.scad>\n",
            ),
            (
                "/d/g.scad",
                "a = 4; h = let(q = 1) [for (i = [0:q]) if (i) i];\n",
            ),
            ("/d/u.scad", "module u() {}\n"),
        ],
        "a = 1; // before\ninclude <f.scad>\nb = a; use <>\ninclude <f.scad>\ninclude <g.scad>\ncube(a);",
    );
    // Per round: three at the top, and g.scad inside f.scad's own parse
    // (a fragment of its own, read inside f.scad) the first time.
    assert_eq!((s.fragments, s.built, s.reused), (7, 3, 4), "{s:?}");
    assert_eq!((s.nested, s.after_error, s.unusable), (0, 0, 0), "{s:?}");
}

#[test]
fn includes_not_between_statements_are_spliced_as_tokens() {
    let files = [
        ("/d/e.scad", "1 + 2"),
        ("/d/half.scad", "x = "),
        ("/d/body.scad", "y = 1; cube(y);\n"),
        ("/d/tail.scad", "if (1) cube();\n"),
    ];
    // Inside a module body and a block: not between the includer's
    // statements.
    let s = run(
        &files,
        "module m() { include <body.scad> }\ntranslate() { include <body.scad> }\n\
         include <body.scad>",
    );
    assert_eq!((s.nested, s.fragments), (4, 2), "{s:?}");
    // Between an `if` and its `else`: the includer alone parses (the
    // directive lands inside the `if`), spliced it does not.
    let s = run(&files, "if (1) sphere(); include <body.scad> else cube();");
    assert_eq!((s.nested, s.fragments), (2, 0), "{s:?}");
    // In an expression, and files that only parse once spliced: the
    // includer alone does not parse, so every include is tokens.
    let s = run(
        &files,
        "a = include <e.scad> ;\ninclude <half.scad> 3;\ninclude <tail.scad> else sphere();",
    );
    assert_eq!((s.after_error, s.fragments), (6, 0), "{s:?}");
    // A file ending in an `if` whose `else` the includer supplies, after
    // an `if` of its own: without the file the `else` is the includer's,
    // which pulls the directive inside that `if`.
    let s = run(&files, "if (1) cube();\ninclude <tail.scad> else sphere();");
    assert_eq!((s.nested, s.fragments), (2, 0), "{s:?}");
}

#[test]
fn syntax_errors_before_an_include_splice_it_as_tokens() {
    let files = [("/d/f.scad", "b = 1;\n")];
    // An open bracket that the include's tokens would continue.
    let s = run(&files, "x = [1, \ninclude <f.scad>\ncube();");
    assert_eq!(s.fragments, 0, "{s:?}");
    // An error after the include leaves it a fragment.
    let s = run(&files, "include <f.scad>\ncube(;\n");
    assert_eq!(s.fragments, 2, "{s:?}");
    // An error in the included file makes it tokens.
    let s = run(&[("/d/f.scad", "b = ;\n")], "include <f.scad>\ncube();");
    assert_eq!((s.fragments, s.unusable), (0, 2), "{s:?}");
}

#[test]
fn circular_missing_and_repeated_includes() {
    let s = run(
        &[
            (
                "/d/self.scad",
                "include <self.scad>\nx = 1;\ninclude <nope.scad>\n",
            ),
            ("/lib/l.scad", "z = 1;\nuse <gone.scad>\n"),
        ],
        "include <self.scad>\ninclude <self.scad>\ninclude <l.scad>\ninclude <nope/x.scad>\n\
         include <>\nuse <>\ninclude <l.scad>",
    );
    assert!(s.fragments >= 4, "{s:?}");
}

#[test]
fn defines_after_the_end_marker() {
    let fs = mem(&[("/d/f.scad", "a = 2;\n")]);
    let text = b"a = 1;\ninclude <f.scad>\n\x03\na = 5;\ninclude <f.scad>\n".to_vec();
    let s = check(&fs, &LibraryPath::default(), "/d/main.scad", &text);
    assert_eq!(s.fragments, 4, "{s:?}");
}

#[test]
fn empty_and_comment_only_files() {
    let s = run(
        &[("/d/e.scad", ""), ("/d/c.scad", "// nothing\n/* at all */")],
        "include <e.scad>include <c.scad>\nif (1) cube(); include <c.scad>\nelse sphere();",
    );
    assert!(s.fragments >= 2, "{s:?}");
}

#[test]
fn a_changed_file_is_parsed_again() {
    let fs = mem(&[
        ("/d/f.scad", "a = 2;\n"),
        ("/d/g.scad", "include <f.scad>\ninclude <s.scad>\n"),
        ("/lib/s.scad", "s = 1;\n"),
    ]);
    let libs = LibraryPath(vec!["/lib".into()]);
    let store = Store::default();
    let main = b"include <g.scad>\ncube(a);\n".to_vec();
    let parse = |fs: &MemFs| {
        let stats = Cell::default();
        let p = parse_program_with(
            "/d/main.scad".into(),
            main.clone(),
            fs,
            &libs,
            Caches {
                fragments: Some(&store),
                stats: Some(&stats),
                ..Caches::default()
            },
        );
        let plain = parse_program("/d/main.scad".into(), main.clone(), fs, &libs);
        assert!(diff(&p, &plain).is_none());
        stats.get()
    };
    assert_eq!(parse(&fs).built, 3);
    assert_eq!(parse(&fs).reused, 1);
    // g.scad is parsed again, with f.scad; s.scad comes from the cache.
    fs.insert("/d/f.scad", b"a = 3;\n".to_vec());
    let s = parse(&fs);
    assert_eq!((s.built, s.reused), (2, 1), "{s:?}");
    // A new file next to g.scad shadows the library's s.scad.
    fs.insert("/d/s.scad", b"s = 2;\n".to_vec());
    let s = parse(&fs);
    assert_eq!((s.built, s.reused), (2, 1), "{s:?}");
}

/// Every `.scad` file under `dir`, sorted.
fn scad_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<_> = rd.filter_map(|e| e.ok().map(|e| e.path())).collect();
    entries.sort();
    for p in entries {
        if p.is_dir() {
            scad_files(&p, out);
        } else if p.extension().is_some_and(|e| e == "scad") {
            out.push(p);
        }
    }
}

/// The corpus check: every `.scad` file of BOSL2, OpenSCAD's tests and
/// examples and MCAD in `.reference`, parsed plain and through a fragment
/// cache (twice: building, then reusing), must agree; prints how includes
/// were put in. Slow in a debug build: `cargo test --release -p
/// neoscad-lang --test fragments -- --ignored --nocapture`.
#[test]
#[ignore]
fn corpus_programs_are_unchanged() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.reference");
    let libs = LibraryPath(vec![root.clone()]);
    for set in [
        "BOSL2",
        "openscad/tests/data/scad",
        "openscad/examples",
        "openscad/libraries/MCAD",
    ] {
        let mut files = Vec::new();
        scad_files(&root.join(set), &mut files);
        let stats = Cell::default();
        let mut affected = 0;
        for f in &files {
            let Ok(mut text) = std::fs::read(f) else {
                continue;
            };
            text.extend_from_slice(b"\n\x03\n");
            let path = f.canonicalize().unwrap_or_else(|_| f.clone());
            let plain = parse_program(path.clone(), text.clone(), &StdFs, &libs);
            let before = stats.get();
            // Fragments are keyed by the main file, so a store per program.
            let store = Store::default();
            for round in 0..2 {
                let cached = parse_program_with(
                    path.clone(),
                    text.clone(),
                    &StdFs,
                    &libs,
                    Caches {
                        fragments: Some(&store),
                        stats: Some(&stats),
                        ..Caches::default()
                    },
                );
                if let Some(d) = diff(&plain, &cached) {
                    panic!("{} (round {round}) differs in {d}", f.display());
                }
            }
            let after: SpliceStats = stats.get();
            if after.nested + after.after_error + after.unusable
                > before.nested + before.after_error + before.unusable
            {
                affected += 1;
            }
        }
        println!(
            "{set}: {} files, {affected} with an include spliced as tokens; {:?}",
            files.len(),
            stats.get()
        );
    }
}
