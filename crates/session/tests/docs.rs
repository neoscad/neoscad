//! `neoscad docs`: the builtin reference covers every builtin the
//! evaluator registers, and each example runs; user definitions are found
//! with their comment blocks; unknown names get "did you mean".

use std::path::PathBuf;
use std::sync::Arc;

use lang::loader::LibraryPath;
use lang::vfs::MemFs;
use session::docs::DocsRequest;
use session::{Config, Run, Session};

fn session(files: &[(&str, &[u8])]) -> Session {
    let fs = Arc::new(MemFs::new());
    for (p, t) in files {
        fs.insert(format!("/doc/{p}"), t.to_vec());
    }
    let mut cfg = Config::new(fs, LibraryPath(Vec::new()));
    cfg.work_dir = PathBuf::from("/doc");
    Session::new(cfg)
}

#[test]
fn every_stable_builtin_has_an_entry() {
    let mut missing = Vec::new();
    for b in eval::builtins() {
        if b.status == eval::BuiltinStatus::Experimental {
            continue;
        }
        let kind = match b.kind {
            eval::BuiltinKind::Module => docs::Kind::Module,
            eval::BuiltinKind::Function => docs::Kind::Function,
            eval::BuiltinKind::Variable => docs::Kind::Variable,
        };
        if !docs::builtin(b.name).iter().any(|e| e.kind == kind) {
            missing.push(format!("{} {}", kind.name(), b.name));
        }
    }
    assert!(missing.is_empty(), "no docs entry for: {missing:?}");
    // And nothing documented that the evaluator does not have.
    let known: Vec<&str> = eval::builtins().iter().map(|b| b.name).collect();
    for e in docs::builtins() {
        assert!(known.contains(&e.name.as_str()), "stale entry {}", e.name);
    }
}

/// A DXF drawing with a dimension named `width` and two lines crossing on
/// layer `center`, for the `dxf_dim` and `dxf_cross` examples.
const DXF: &str = "0\nSECTION\n2\nENTITIES\n\
0\nLINE\n8\ncenter\n10\n0\n20\n0\n11\n10\n21\n10\n\
0\nLINE\n8\ncenter\n10\n0\n20\n10\n11\n10\n21\n0\n\
0\nDIMENSION\n8\n0\n1\nwidth\n10\n10\n20\n0\n13\n0\n23\n0\n14\n10\n24\n0\n70\n0\n\
0\nENDSEC\n0\nEOF\n";

#[test]
fn every_example_parses_and_evaluates() {
    let mut failures = Vec::new();
    for e in docs::builtins() {
        let s = session(&[
            ("ex.scad", e.example.as_bytes()),
            ("drawing.dxf", DXF.as_bytes()),
            ("heights.dat", b"1 2\n3 4\n"),
        ]);
        let r = s.evaluate(&Run::new("ex.scad"), false).unwrap();
        let errors = r.log.count(lang::diag::Severity::Error);
        if r.exit_code != 0 || errors > 0 {
            failures.push(format!(
                "{} {}: {}",
                e.kind.name(),
                e.name,
                String::from_utf8_lossy(&r.log.stderr)
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn user_definitions_and_hints() {
    let lib =
        b"// Library: a rounded plate.\n// r: corner radius\nmodule plate(w, r = 1) cube(w);\n";
    let main = b"use <lib.scad>\n\n// Doubles.\nfunction twice(x) = 2 * x;\n";
    let s = session(&[("lib.scad", lib), ("main.scad", main)]);
    let r = s.docs(&DocsRequest {
        name: Some("plate".into()),
        file: Some("main.scad".into()),
        ..DocsRequest::default()
    });
    assert_eq!(r.exit_code, 0, "{}", r.text);
    assert_eq!(
        r.text,
        "module plate(w, r = 1)  (lib.scad:3)\n  Library: a rounded plate.\n  r: corner radius\n"
    );
    let r = s.docs(&DocsRequest {
        name: Some("twice".into()),
        file: Some("main.scad".into()),
        ..DocsRequest::default()
    });
    assert_eq!(r.json["entries"][0]["signature"], "function twice(x)");
    let r = s.docs(&DocsRequest {
        name: Some("twise".into()),
        file: Some("main.scad".into()),
        ..DocsRequest::default()
    });
    assert_eq!(r.exit_code, 1);
    assert_eq!(r.json["did_you_mean"], "twice");
    let r = s.docs(&DocsRequest {
        name: Some("cylindr".into()),
        ..DocsRequest::default()
    });
    assert_eq!(
        r.text,
        "neoscad docs: no builtin named 'cylindr'; did you mean 'cylinder'?\n"
    );
    let r = s.docs(&DocsRequest::default());
    assert!(
        r.text.starts_with("modules (37): cube sphere"),
        "{}",
        r.text
    );
}
