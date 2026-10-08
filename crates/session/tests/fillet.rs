//! `fillet_edges()`/`chamfer_edges()` (`--enable fillet`,
//! `docs/fillets.md`) through a session: off, the console text is
//! OpenSCAD's unknown-module warning and the JSON hint names the flag; on,
//! a selector error's JSON carries its code, its span inside the string
//! and the "did you mean" edit. (The evaluator's side is in
//! `crates/eval/tests/fillet.rs`.)

use std::path::PathBuf;
use std::sync::Arc;

use lang::loader::LibraryPath;
use lang::vfs::MemFs;
use session::{Config, Run, Session};

fn session(src: &[u8]) -> Session {
    let fs = Arc::new(MemFs::new());
    fs.insert("/doc/m.scad", src.to_vec());
    let mut cfg = Config::new(fs, LibraryPath(Vec::new()));
    cfg.work_dir = PathBuf::from("/doc");
    cfg.limits = session::Limits::AGENT;
    Session::new(cfg)
}

fn run(on: bool) -> Run {
    let mut run = Run::new("m.scad");
    if on {
        run.extensions = eval::Extensions::NONE.with(eval::Extension::Fillet);
    }
    run
}

#[test]
fn off_they_are_unknown_and_the_hint_names_the_flag() {
    let s = session(b"fillet_edges(r = 2) cube(10);\nchamfer_edges(d = 1) cube(10);\n");
    let r = s.evaluate(&run(false), false).unwrap();
    assert_eq!(
        String::from_utf8_lossy(&r.log.stderr),
        "WARNING: Ignoring unknown module 'fillet_edges' in file m.scad, line 1\n\
         WARNING: Ignoring unknown module 'chamfer_edges' in file m.scad, line 2\n"
    );
    let d = r.log.diagnostics_json();
    for (i, name) in [(0, "fillet_edges"), (1, "chamfer_edges")] {
        let hint = d[i]["hints"][0]["message"].as_str().unwrap();
        assert!(
            hint.contains("--enable fillet") && hint.contains(name),
            "{hint}"
        );
    }
}

#[test]
fn a_selector_error_in_json() {
    let s = session(b"fillet_edges(r = 2, edges = \"|z and convx\")\n  cube(10);\n");
    let r = s.evaluate(&run(true), false).unwrap();
    assert_eq!(
        String::from_utf8_lossy(&r.log.stderr),
        "ERROR: fillet_edges(): edges = \"|z and convx\", column 8: unknown selector 'convx': \
         did you mean 'convex'? in file m.scad, line 1\n"
    );
    let d = &r.log.diagnostics_json()[0];
    assert_eq!(d["code"], "fillet-selector");
    assert_eq!(d["severity"], "error");
    // 1-based columns: `convx` is columns 37 to 41, and the end is one past it.
    assert_eq!(d["span"]["start"]["column"], 37);
    assert_eq!(d["span"]["end"]["column"], 42);
    assert_eq!(d["hints"][0]["replace"]["text"], "convex");
    assert_eq!(d["hints"][0]["replace"]["span"], d["span"]);
}

#[test]
fn on_a_call_warns_that_it_is_not_built() {
    let s = session(b"fillet_edges(r = 2) cube(10);\n");
    let r = s.evaluate(&run(true), false).unwrap();
    let d = &r.log.diagnostics_json()[0];
    assert_eq!(d["code"], "fillet-not-built");
    assert_eq!(d["severity"], "warning");
}
