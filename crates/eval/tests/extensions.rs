//! NeoSCAD's `--enable` extensions against OpenSCAD: their names must
//! never be OpenSCAD experiment names, and turning them on must not change
//! a file that defines the same names itself (`docs/language-extensions.md`,
//! sections 1, 2 and 9).

use std::path::{Path, PathBuf};

use eval::{Extension, Extensions, Options};

fn reference() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.reference/openscad")
}

/// The `--enable` names `Feature.cc` declares: the first string literal of
/// each `const Feature Feature::X(...)` definition, the ones behind an
/// `#ifdef` (`python-engine`) included.
fn openscad_feature_names(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for def in text.split("const Feature Feature::").skip(1) {
        if let Some(open) = def.find('"')
            && let Some(len) = def[open + 1..].find('"')
        {
            out.push(def[open + 1..open + 1 + len].to_string());
        }
    }
    out
}

/// The owner chose unprefixed names (`sketch`, not `neoscad-sketch`), on
/// the condition that they are checked against OpenSCAD's own list at
/// every reference update. If upstream adds an experiment called `sketch`,
/// `--enable sketch` would mean two things; this fails first, and the
/// extension has to be renamed before the reference moves.
#[test]
fn no_extension_name_is_an_openscad_feature() {
    let Ok(text) = std::fs::read_to_string(reference().join("src/Feature.cc")) else {
        eprintln!("skipped: no reference checkout");
        return;
    };
    let names = openscad_feature_names(&text);
    // The parse must find what the evaluator mirrors, or a format change
    // in Feature.cc would make the check below vacuous.
    for f in eval::Feature::ALL {
        assert!(
            names.iter().any(|n| n == f.name()),
            "Feature.cc parse missed '{}': {names:?}",
            f.name()
        );
    }
    for e in Extension::ALL {
        assert!(
            !names.iter().any(|n| n == e.name()),
            "OpenSCAD now has an experiment named '{}': rename NeoSCAD's extension",
            e.name()
        );
        assert_ne!(e.name(), "all");
    }
}

struct Lines(Vec<String>);

impl eval::Output for Lines {
    fn message(&mut self, m: &eval::Message<'_>) {
        self.0.push(format!(
            "{}: {} @{}",
            m.diag.severity.openscad_label(),
            String::from_utf8_lossy(m.text),
            m.diag.line
        ));
    }
}

fn evaluate(path: &Path, text: Vec<u8>, extensions: Extensions) -> (Vec<String>, String) {
    let program = lang::parse_file(path.to_path_buf(), text);
    let opts = Options {
        extensions,
        ..Options::default()
    };
    let mut out = Lines(Vec::new());
    let dir = path.parent().unwrap_or(Path::new("/")).to_path_buf();
    let ev = eval::with_stack(eval::DEFAULT_THREAD_STACK, || {
        eval::evaluate(&program, &[], &[], dir.clone(), &opts, &mut out)
    });
    let csg = eval::dump::csg(&ev.root, &dir, &lang::loader::StdFs);
    (out.0, csg)
}

/// `examples/Basics/roof.scad` defines its own `module sketch()`. Its
/// manifest cases are all skipped (they need `roof`), so this evaluates
/// the file, plus a direct call of its `sketch()`, with every extension
/// off and on: a program's own definition shadows the builtin, so the
/// messages and the tree must be the same.
#[test]
fn a_programs_own_sketch_module_wins_over_the_extension() {
    let path = reference().join("examples/Basics/roof.scad");
    let Ok(mut text) = std::fs::read(&path) else {
        eprintln!("skipped: no reference checkout");
        return;
    };
    text.extend_from_slice(b"\nsketch();\n");
    let all = Extension::ALL
        .into_iter()
        .fold(Extensions::NONE, Extensions::with);
    let off = evaluate(&path, text.clone(), Extensions::NONE);
    let on = evaluate(&path, text, all);
    assert!(off.1.contains("polygon("), "{}", off.1);
    assert_eq!(off, on);
}
