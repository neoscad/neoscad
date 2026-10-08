//! NeoSCAD's `fillet_edges()` and `chamfer_edges()` (`--enable fillet`,
//! `docs/fillets.md`) at the evaluator, stage F0:
//!
//! - off, both are OpenSCAD's unknown modules, and on, a program's own
//!   definitions (and the libraries' `fillet` and `chamfer` modules,
//!   whose names were avoided for this reason) behave as they do off;
//! - on, a call is a node the `.csg` prints as the call, with its
//!   selectors in canonical form, so equal selectors share a cache key;
//! - wrong arguments are errors at the argument (at the column inside a
//!   selector string), with a "did you mean" edit where one is clear, and
//!   leave the children as a plain group.

use std::path::{Path, PathBuf};

use eval::{Extension, Extensions, Options};
use lang::diag::{DiagCode, Diagnostic, Severity};
use lang::loader::{LibraryPath, StdFs};

fn reference() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.reference")
}

struct Diags(Vec<Diagnostic>);

impl eval::Output for Diags {
    fn message(&mut self, m: &eval::Message<'_>) {
        self.0.push(m.diag.clone());
    }
}

struct Out {
    diags: Vec<Diagnostic>,
    csg: String,
    key: String,
    text: Vec<u8>,
}

impl Out {
    fn lines(&self) -> Vec<String> {
        self.diags
            .iter()
            .map(|d| format!("{}: {} @{}", d.severity.openscad_label(), d.message, d.line))
            .collect()
    }

    /// The source text a diagnostic's span covers.
    fn at(&self, d: &Diagnostic) -> String {
        let s = d.span.expect("a span");
        String::from_utf8_lossy(&self.text[s.start as usize..s.end as usize]).into_owned()
    }

    fn only(&self, severity: Severity) -> Vec<&Diagnostic> {
        self.diags
            .iter()
            .filter(|d| d.severity == severity)
            .collect()
    }
}

fn run_at(path: &Path, src: &str, extensions: Extensions) -> Out {
    let text = src.as_bytes().to_vec();
    let program = lang::parse_program(
        path.to_path_buf(),
        text.clone(),
        &StdFs,
        &LibraryPath(Vec::new()),
    );
    assert!(!program.has_syntax_errors(), "{src}");
    let opts = Options {
        extensions,
        ..Options::default()
    };
    let mut out = Diags(Vec::new());
    let dir = path.parent().unwrap_or(Path::new("/")).to_path_buf();
    let ev = eval::with_stack(eval::DEFAULT_THREAD_STACK, || {
        eval::evaluate(&program, &[], &[], dir.clone(), &opts, &mut out)
    });
    let csg = eval::dump::csg(&ev.root, &dir, &StdFs);
    let keys = eval::dump::Keys::new(&ev.root, &StdFs);
    let key = format!("{:?}", keys.get(&ev.root));
    Out {
        diags: out.0,
        csg,
        key,
        text,
    }
}

fn run(src: &str, extensions: Extensions) -> Out {
    run_at(Path::new("/nonexistent/t.scad"), src, extensions)
}

fn on() -> Extensions {
    Extensions::NONE.with(Extension::Fillet)
}

fn every() -> Extensions {
    Extension::ALL
        .into_iter()
        .fold(Extensions::NONE, Extensions::with)
}

#[test]
fn off_both_are_unknown_modules_and_all_does_not_turn_them_on() {
    let src = "fillet_edges(r = 2) cube(10);\nchamfer_edges(d = 1) cube(10);\n";
    for ext in [Extensions::NONE, Extensions::from_names(&["all"])] {
        let o = run(src, ext);
        assert_eq!(
            o.lines(),
            [
                "WARNING: Ignoring unknown module 'fillet_edges' @1",
                "WARNING: Ignoring unknown module 'chamfer_edges' @2",
            ]
        );
        assert_eq!(o.csg, "\n");
    }
}

#[test]
fn on_a_call_is_a_node_printed_as_the_call() {
    let o = run(
        "fillet_edges(r = 2, edges = \"|Z  AND >x\") cube(10);",
        on(),
    );
    assert_eq!(
        o.csg,
        "fillet_edges(r = 2, edges = \"|z and >x\", except = undef, expect = undef, $fn = 0, $fa = 12, $fs = 2) {\n\tcube(size = [10, 10, 10], center = false);\n}\n\n"
    );
    // The geometry is not built yet, and the call says so.
    let w = o.only(Severity::Warning);
    assert_eq!(w.len(), 1, "{:?}", o.lines());
    assert_eq!(w[0].code, DiagCode::FilletNotBuilt);
    assert_eq!(
        o.at(w[0]),
        "fillet_edges(r = 2, edges = \"|Z  AND >x\") cube(10);"
    );

    let o = run(
        "chamfer_edges(1, [[0, 0, 1], \"%circle\"], except = [1, 0, 1], expect = 4, $fn = 8) cube(10);",
        on(),
    );
    assert!(o.only(Severity::Error).is_empty(), "{:?}", o.lines());
    assert!(
        o.csg.starts_with(
            "chamfer_edges(d = 1, edges = [[0, 0, 1], \"%circle\"], except = [1, 0, 1], expect = 4, $fn = 8, $fa = 12, $fs = 2) {"
        ),
        "{}",
        o.csg
    );
    // `r` is another name for a chamfer's `d`.
    let o = run("chamfer_edges(r = 1) cube(10);", on());
    assert!(
        o.csg.starts_with("chamfer_edges(d = 1, edges = \"all\","),
        "{}",
        o.csg
    );
}

/// The key is the `.csg` label's: two spellings of one selector share it,
/// and every argument that will change the result changes it.
#[test]
fn keys_follow_the_canonical_selector() {
    let key = |s: &str| run(&format!("{s} cube(10);"), on()).key;
    let base = key("fillet_edges(r = 2, edges = \"|z\")");
    assert_eq!(base, key("fillet_edges(2, \"|Z\")"));
    assert_eq!(base, key("fillet_edges(r = 2, edges = \"( |z )\")"));
    for other in [
        "fillet_edges(r = 3, edges = \"|z\")",
        "fillet_edges(r = 2, edges = \"|y\")",
        "fillet_edges(r = 2, edges = \"|z\", except = \">z\")",
        "fillet_edges(r = 2, edges = \"|z\", expect = 4)",
        "fillet_edges(r = 2, edges = \"|z\", $fn = 8)",
        "chamfer_edges(d = 2, edges = \"|z\")",
        "group()",
    ] {
        assert_ne!(base, key(other), "{other}");
    }
}

/// A `.csg` export of a fillet runs again (with the flag) to the same
/// `.csg`.
#[test]
fn the_csg_reads_back_to_itself() {
    let src = "fillet_edges(r = 2, edges = [\"child(0, 1) or new\", [0, -1, 1]], except = \"not (|z and convex)\") { cube(10); translate([5, 5, 5]) cube(10); }\n\
               chamfer_edges(d = 0.5, edges = \">>z[-2] exc box(0, 0, 0, 1, 1, 1)\") cube(3);";
    let a = run(src, on());
    assert!(a.only(Severity::Error).is_empty(), "{:?}", a.lines());
    let b = run(&a.csg, on());
    assert!(b.only(Severity::Error).is_empty(), "{:?}", b.lines());
    assert_eq!(a.csg, b.csg);
}

#[test]
fn selector_errors_point_into_the_string() {
    let src = "fillet_edges(r = 2, edges = \"|z and convx\") cube(10);";
    let o = run(src, on());
    let e = o.only(Severity::Error);
    assert_eq!(e.len(), 1, "{:?}", o.lines());
    assert_eq!(e[0].code, DiagCode::FilletSelector);
    assert_eq!(o.at(e[0]), "convx");
    assert_eq!(
        e[0].message,
        "fillet_edges(): edges = \"|z and convx\", column 8: unknown selector 'convx': did you mean 'convex'?"
    );
    let (span, text) = e[0].hints[0].replacement.clone().expect("an edit");
    assert_eq!(text, "convex");
    assert_eq!(span, e[0].span.unwrap());
    // A failed call is a plain group: the children render, sharp, and
    // nothing says the geometry is unbuilt.
    assert!(o.csg.starts_with("group() {"), "{}", o.csg);
    assert!(o.only(Severity::Warning).is_empty(), "{:?}", o.lines());

    // In a list, at the item.
    let o = run("fillet_edges(2, [\"|z\", \"%circel\"]) cube(10);", on());
    let e = o.only(Severity::Error);
    assert_eq!(o.at(e[0]), "circel");

    // A string with an escape is not the source's bytes: the whole
    // string, and no edit.
    let o = run("fillet_edges(2, \"\\u0041 or convx\") cube(10);", on());
    let e = o.only(Severity::Error);
    assert_eq!(o.at(e[0]), "\"\\u0041 or convx\"");
    assert!(e[0].hints.iter().all(|h| h.replacement.is_none()));

    // A computed string: the expression.
    let o = run(
        "s = \"|q\";\nfillet_edges(2, edges = str(s)) cube(10);",
        on(),
    );
    let e = o.only(Severity::Error);
    assert_eq!(o.at(e[0]), "str(s)");
    assert_eq!(e[0].line, 2);
}

#[test]
fn argument_errors() {
    let err = |src: &str| {
        let o = run(src, on());
        let e = o.only(Severity::Error);
        assert_eq!(e.len(), 1, "{src}: {:?}", o.lines());
        (e[0].message.clone(), o.at(e[0]))
    };
    let (m, at) = err("fillet_edges() cube(10);");
    assert_eq!(
        m,
        "fillet_edges(): r is required: the fillet radius, a positive number"
    );
    assert_eq!(at, "fillet_edges() cube(10);");
    let (m, at) = err("fillet_edges(r = -1) cube(10);");
    assert_eq!(m, "fillet_edges(): r must be a positive number, found -1");
    assert_eq!(at, "-1");
    let (m, at) = err("chamfer_edges(\"1\") cube(10);");
    assert_eq!(
        m,
        "chamfer_edges(): d must be a positive number, found \"1\""
    );
    assert_eq!(at, "\"1\"");
    let (m, at) = err("chamfer_edges(d = 1, r = 1) cube(10);");
    assert_eq!(m, "chamfer_edges(): give d or r, not both");
    assert_eq!(at, "1");
    let (m, _) = err("fillet_edges(1, edges = 3) cube(10);");
    assert!(
        m.starts_with("fillet_edges(): edges must be a selector string"),
        "{m}"
    );
    let (m, at) = err("fillet_edges(1, edges = [0, 2, 1]) cube(10);");
    assert!(m.contains("each -1, 0 or 1"), "{m}");
    assert_eq!(at, "[0, 2, 1]");
    let (m, at) = err("fillet_edges(1, expect = 2.5) cube(10);");
    assert_eq!(
        m,
        "fillet_edges(): expect must be a whole number of edges, found 2.5"
    );
    assert_eq!(at, "2.5");
    let (m, at) = err("fillet_edges(1, except = \"\") cube(10);");
    assert!(m.contains("empty"), "{m}");
    assert_eq!(at, "\"\"");
    // `edges = undef` is the default.
    let o = run("fillet_edges(1, edges = undef) cube(10);", on());
    assert!(o.csg.contains("edges = \"all\""), "{}", o.csg);
}

/// `part(name)` and `@anchor` in a selector need their own extensions.
#[test]
fn selector_atoms_of_other_extensions_need_their_flags() {
    let src = "fillet_edges(1, \"part(lid) or @tip\") cube(10);";
    let o = run(src, on());
    let e = o.only(Severity::Error);
    assert_eq!(o.at(e[0]), "part(lid)");
    assert!(e[0].message.contains("--enable part"), "{}", e[0].message);
    let o = run(src, on().with(Extension::Part));
    let e = o.only(Severity::Error);
    assert_eq!(o.at(e[0]), "@tip");
    assert!(e[0].message.contains("--enable query"), "{}", e[0].message);
    let o = run(src, on().with(Extension::Part).with(Extension::Query));
    assert!(o.only(Severity::Error).is_empty(), "{:?}", o.lines());
}

/// A program's own `fillet_edges` wins over the builtin, as for every
/// builtin.
#[test]
fn a_programs_own_definitions_win() {
    let src = "module fillet_edges(r) { echo(\"mine\", r); children(); }\n\
               module chamfer_edges() cube(1);\n\
               fillet_edges(2) sphere(1);\nchamfer_edges(d = 1);\n";
    let off = run(src, Extensions::NONE);
    let on = run(src, every());
    assert_eq!(off.lines(), on.lines());
    assert_eq!(off.csg, on.csg);
    assert!(off.lines()[0].contains("\"mine\", 2"), "{:?}", off.lines());
}

/// The names avoid the libraries' own: BOSL2's `fillet` mask, MCAD's
/// `chamfer`, and the `fillet(r, l)` NeoSCAD's MCP recipes teach agents
/// (`docs/fillets.md`, section 2). With every extension on they behave as
/// with none.
#[test]
fn library_fillet_and_chamfer_modules_are_untouched() {
    let recipes = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../cli/src/mcp/recipes.scad");
    let recipes = std::fs::read_to_string(&recipes).expect("the MCP recipes");
    let src = format!("{recipes}\nfillet(2, 10);\ntranslate([20, 0, 0]) fillet(r = 1, l = 5);\n");
    let path = Path::new("/nonexistent/recipes.scad");
    let (off, all) = (
        run_at(path, &src, Extensions::NONE),
        run_at(path, &src, every()),
    );
    assert!(off.csg.contains("difference()"), "{}", off.csg);
    assert_eq!(off.lines(), all.lines());
    assert_eq!(off.csg, all.csg);

    let bosl2 = reference().join("BOSL2/std.scad");
    let mcad = reference().join("openscad/libraries/MCAD/metric_fastners.scad");
    if !bosl2.exists() || !mcad.exists() {
        eprintln!("skipped the libraries: no reference checkout");
        return;
    }
    let main = reference().join("main.scad");
    for src in [
        format!(
            "include <{}>\nfillet(l = 10, r = 2);\ncuboid(10, rounding = 1, edges = \"Z\");\n",
            bosl2.display()
        ),
        format!("include <{}>\nchamfer(len = 10, r = 2);\n", mcad.display()),
    ] {
        let off = run_at(&main, &src, Extensions::NONE);
        let all = run_at(&main, &src, every());
        assert!(off.only(Severity::Error).is_empty(), "{:?}", off.lines());
        assert!(!off.csg.trim().is_empty());
        assert!(!all.csg.contains("fillet_edges") && !all.csg.contains("chamfer_edges"));
        assert_eq!(off.lines(), all.lines());
        assert_eq!(off.csg, all.csg);
    }
}
