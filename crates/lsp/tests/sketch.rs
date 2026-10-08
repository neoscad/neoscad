//! Constrained sketches in the language server (`--enable sketch`;
//! `docs/language-extensions.md`, section 4.8): the vocabulary only
//! inside sketch bodies, info markers, the hints' edits as quick fixes,
//! "Pin drawing", solved values on hover, and navigation from a handle's
//! members.

mod common;

use common::{Client, labels, uri};
use serde_json::{Value, json};

const MAIN: &str = "/w/main.scad";

/// A plate drawn a little off (`a` at x = 20.5 where the length says
/// 20), with its height free: one degree of freedom. The file defines
/// its own `arc` module and `point` function, as BOSL2 and MCAD define
/// names the vocabulary uses.
const PLATE: &str = "module arc(r) { circle(r); }
function point(x) = x * 2;
y = point(3);
linear_extrude(2)
sketch(name = \"plate\") {
  o = point([0, 0]);
  a = point([20.5, 0]);
  b = point([20, 10]);
  c = point([0, 10]);
  l1 = line(o, a); l2 = line(a, b); l3 = line(b, c); l4 = line(c, o);
  fix(o); horizontal(l1); vertical(l2); horizontal(l3); vertical(l4);
  length(l1, 20);
}
arc(2);
";

fn sketch_on() -> impl FnOnce(&mut session::Config) {
    |c: &mut session::Config| {
        c.extensions = session::Extensions::NONE.with(session::Extension::Sketch)
    }
}

fn hover_text(c: &mut Client, text: &str, needle: &str, delta: usize) -> String {
    let h = c.at("textDocument/hover", MAIN, text, needle, delta);
    h["contents"]["value"].as_str().unwrap_or("").to_string()
}

#[test]
fn the_vocabulary_is_offered_and_resolved_only_in_sketch_bodies() {
    let mut c = Client::mem_config(&[(MAIN, PLATE)], sketch_on());
    c.open(MAIN, PLATE);
    // At a statement in the body: the constraint statements, with
    // snippets.
    let r = c.at("textDocument/completion", MAIN, PLATE, "horizontal(l1)", 3);
    let items = r["items"].as_array().unwrap();
    let h = items
        .iter()
        .find(|i| i["label"] == "horizontal")
        .expect("horizontal offered");
    assert_eq!(h["insertText"], "horizontal(${1:line});");
    assert!(
        h["detail"].as_str().unwrap().contains("--enable sketch"),
        "{h}"
    );
    // In an expression in the body: the entities, before the file's own
    // `point` function.
    let r = c.at("textDocument/completion", MAIN, PLATE, "point([0, 0])", 2);
    let p = r["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["label"] == "point")
        .unwrap()
        .clone();
    assert_eq!(p["insertText"], "point([${1:0}, ${2:0}])", "{p}");
    // Outside the body: neither (the file's `point` is a function of its
    // own; `horizontal` does not exist).
    let r = c.at("textDocument/completion", MAIN, PLATE, "arc(2)", 2);
    let l = labels(&r);
    assert!(l.contains(&"arc".to_string()));
    assert!(!l.contains(&"horizontal".to_string()), "{l:?}");
    // Hover: the sketch's `point` and `line` inside, labelled; the file's
    // own `point` outside.
    let inside = hover_text(&mut c, PLATE, "point([0, 0])", 1);
    assert!(inside.contains("A sketch point"), "{inside}");
    assert!(
        inside.contains("NeoSCAD extension (`--enable sketch`)"),
        "{inside}"
    );
    let outside = hover_text(&mut c, PLATE, "point(3)", 1);
    assert!(!outside.contains("sketch"), "{outside}");
    let l = hover_text(&mut c, PLATE, "line(o, a)", 1);
    assert!(l.contains("line(p, q, construction = false)"), "{l}");
    // Going to the definition of a vocabulary name inside goes nowhere
    // (it is a builtin), not to the file's own `point`.
    let d = c.at("textDocument/definition", MAIN, PLATE, "point([0, 0])", 1);
    assert!(d.is_null(), "{d}");
    let d = c.at("textDocument/definition", MAIN, PLATE, "point(3)", 1);
    assert_eq!(d["range"]["start"]["line"], 1, "{d}");
}

#[test]
fn without_the_extension_a_sketch_body_is_ordinary_code() {
    let mut c = Client::mem(&[(MAIN, PLATE)]);
    c.open(MAIN, PLATE);
    let r = c.at("textDocument/completion", MAIN, PLATE, "horizontal(l1)", 3);
    assert!(!labels(&r).contains(&"horizontal".to_string()));
    let inside = hover_text(&mut c, PLATE, "point([0, 0])", 1);
    assert!(!inside.contains("A sketch point"), "{inside}");
}

#[test]
fn a_programs_own_sketch_module_gets_no_vocabulary() {
    // roof.scad defines `module sketch()`: its children are ordinary.
    let text =
        "module sketch() { children(); }\nsketch() { horizontal(1); }\nmodule horizontal(x) {}\n";
    let mut c = Client::mem_config(&[(MAIN, text)], sketch_on());
    c.open(MAIN, text);
    let d = c.at("textDocument/definition", MAIN, text, "horizontal(1)", 1);
    assert_eq!(d["range"]["start"]["line"], 2, "{d}");
}

/// The published diagnostics of the plate, evaluated by the server.
fn plate_diagnostics(c: &mut Client) -> Vec<Value> {
    let pubs = c.diagnostics();
    let p = pubs
        .iter()
        .find(|p| p["uri"] == uri(MAIN))
        .expect("published");
    p["diagnostics"].as_array().unwrap().clone()
}

fn apply(text: &str, edits: &[Value]) -> String {
    let src = lang::source::SourceFile::new("/x".into(), text.as_bytes().to_vec());
    let mut edits: Vec<(u32, u32, String)> = edits
        .iter()
        .map(|e| {
            let (a, b) = lsp::proto::offsets(&src, &e["range"]).unwrap();
            (a, b, e["newText"].as_str().unwrap().to_string())
        })
        .collect();
    edits.sort_by_key(|e| std::cmp::Reverse(e.0));
    let mut out = text.to_string();
    for (a, b, t) in edits {
        out.replace_range(a as usize..b as usize, &t);
    }
    out
}

#[test]
fn info_markers_quick_fixes_and_pin_drawing() {
    let mut c = Client::mem_config(&[(MAIN, PLATE)], sketch_on());
    c.open(MAIN, PLATE);
    let diags = plate_diagnostics(&mut c);
    let under = diags
        .iter()
        .find(|d| d["code"] == "sketch-underconstrained")
        .unwrap_or_else(|| panic!("{diags:?}"));
    // Information, not a warning or an error.
    assert_eq!(under["severity"], 3, "{under}");
    let fixes = under["data"]["fixes"].as_array().unwrap();
    // The hint that adds the missing constraint is a fix that inserts it,
    // and "Pin drawing" is offered on the sketch's own diagnostic.
    let add = fixes
        .iter()
        .find(|f| {
            f["edits"][0]["newText"]
                .as_str()
                .unwrap()
                .contains("length(l2, 10);")
        })
        .unwrap_or_else(|| panic!("{fixes:?}"));
    let fixed = apply(PLATE, add["edits"].as_array().unwrap());
    assert!(fixed.contains("  length(l2, 10);\n}"), "{fixed}");
    let pin = fixes
        .iter()
        .find(|f| f["title"].as_str().unwrap().starts_with("Pin drawing"))
        .unwrap_or_else(|| panic!("{fixes:?}"));
    let pinned = apply(PLATE, pin["edits"].as_array().unwrap());
    assert!(pinned.contains("a = point([20, 0]);"), "{pinned}");
    assert!(pinned.contains("b = point([20, 10]);"), "{pinned}");

    // As code actions: the quick fixes of the diagnostic, and "Pin
    // drawing" anywhere in the sketch as a refactoring.
    let at = Client::pos(PLATE, "fix(o)", 0);
    let r = c.request(
        "textDocument/codeAction",
        json!({"textDocument": {"uri": uri(MAIN)}, "range": {"start": at, "end": at}, "context": {"diagnostics": []}}),
    );
    let actions = r.as_array().unwrap();
    let pin = actions
        .iter()
        .find(|a| a["kind"] == "refactor.rewrite")
        .unwrap_or_else(|| panic!("{actions:?}"));
    assert_eq!(
        pin["title"],
        format!(
            "{} ('plate')",
            "Pin drawing: rewrite the guesses to the solved coordinates"
        )
    );
    let edits = pin["edit"]["changes"][uri(MAIN)].as_array().unwrap();
    assert_eq!(apply(PLATE, edits), pinned);
    // Only quick fixes asked for: no refactoring.
    let r = c.request(
        "textDocument/codeAction",
        json!({"textDocument": {"uri": uri(MAIN)}, "range": {"start": at, "end": at}, "context": {"diagnostics": [], "only": ["quickfix"]}}),
    );
    assert!(
        r.as_array()
            .unwrap()
            .iter()
            .all(|a| a["kind"] == "quickfix")
    );
    // Outside the sketch, no "Pin drawing".
    let at = Client::pos(PLATE, "arc(2)", 0);
    let r = c.request(
        "textDocument/codeAction",
        json!({"textDocument": {"uri": uri(MAIN)}, "range": {"start": at, "end": at}, "context": {"diagnostics": []}}),
    );
    assert!(r.as_array().unwrap().is_empty(), "{r}");
}

#[test]
fn hover_shows_the_last_runs_solved_values() {
    let mut c = Client::mem_config(&[(MAIN, PLATE)], sketch_on());
    c.open(MAIN, PLATE);
    plate_diagnostics(&mut c);
    let a = hover_text(&mut c, PLATE, "a = point", 0);
    assert!(a.contains("Solved: `point [20, 0]`"), "{a}");
    assert!(
        a.contains("sketch 'plate': 1 free degree of freedom"),
        "{a}"
    );
    let l = hover_text(&mut c, PLATE, "l1 = line", 0);
    assert!(l.contains("length 20, angle 0°"), "{l}");
    // On a use of the variable too.
    let u = hover_text(&mut c, PLATE, "l1); vertical", 0);
    assert!(u.contains("length 20"), "{u}");
    let s = hover_text(&mut c, PLATE, "sketch(name", 1);
    assert!(
        s.contains("Last run: sketch 'plate' (line 5): 1 free degree of freedom"),
        "{s}"
    );
    // Once the text changes, the old run's values are not shown.
    let edited = PLATE.replace("length(l1, 20)", "length(l1, 25)");
    c.notify(
        "textDocument/didChange",
        json!({"textDocument": {"uri": uri(MAIN), "version": 2}, "contentChanges": [{"text": edited}]}),
    );
    let a = hover_text(&mut c, &edited, "a = point", 0);
    assert!(!a.contains("Solved"), "{a}");
}

/// Hover on a constraint statement shows what the last run made of it
/// (stage 7): satisfied, or redundant beside the statement it repeats.
#[test]
fn hover_shows_a_constraints_state() {
    let text = PLATE.replace("length(l1, 20);", "length(l1, 20); horizontal(o, a);");
    let mut c = Client::mem_config(&[(MAIN, &text)], sketch_on());
    c.open(MAIN, &text);
    plate_diagnostics(&mut c);
    let h = hover_text(&mut c, &text, "horizontal(l1)", 1);
    assert!(
        h.contains("Last run: satisfied (sketch 'plate': 1 free degree of freedom)"),
        "{h}"
    );
    let r = hover_text(&mut c, &text, "horizontal(o, a)", 1);
    assert!(r.contains("Last run: redundant"), "{r}");
    // Outside a sketch, or before any run, a builtin's hover is its
    // documentation alone.
    let l = hover_text(&mut c, &text, "linear_extrude", 1);
    assert!(!l.contains("Last run"), "{l}");
}

#[test]
fn a_hosts_run_brings_its_sketches() {
    use std::sync::Arc;
    let fs = Arc::new(lang::vfs::MemFs::new());
    fs.insert(MAIN, PLATE.as_bytes().to_vec());
    let cfg = session::Config::new(fs, lang::loader::LibraryPath(Vec::new()));
    let mut c = Client::with_config(cfg, true);
    c.server
        .set_extensions(session::Extensions::NONE.with(session::Extension::Sketch));
    c.open(MAIN, PLATE);
    let mut run = session::Run::new(MAIN);
    run.text = Some(Arc::from(PLATE.as_bytes()));
    run.extensions = session::Extensions::NONE.with(session::Extension::Sketch);
    let ev = c.session.evaluate(&run, false).unwrap();
    let pubs = c.server.supply_log(
        &c.session,
        MAIN.as_ref(),
        Arc::from(PLATE.as_bytes()),
        &ev.log,
    );
    let p: Value = serde_json::from_str(&pubs[0]).unwrap();
    let d = p["params"]["diagnostics"].as_array().unwrap();
    assert!(d.iter().any(|d| d["severity"] == 3));
    let a = hover_text(&mut c, PLATE, "a = point", 0);
    assert!(a.contains("Solved: `point [20, 0]`"), "{a}");
    // The host's extensions bind the vocabulary.
    let r = c.at("textDocument/completion", MAIN, PLATE, "horizontal(l1)", 3);
    assert!(labels(&r).contains(&"horizontal".to_string()));
}

#[test]
fn definition_follows_a_handles_members() {
    let text = "sketch() {
  c1 = point([0, 0]);
  top = line([0, 4], [30, 4]);
  e1 = arc(c1, top.start, [0, -4]);
  fix(e1.center);
}
";
    let mut c = Client::mem_config(&[(MAIN, text)], sketch_on());
    c.open(MAIN, text);
    let loc = |c: &mut Client, needle: &str, delta: usize| {
        let d = c.at("textDocument/definition", MAIN, text, needle, delta);
        let r = &d["range"];
        (
            r["start"]["line"].as_u64().unwrap(),
            r["start"]["character"].as_u64().unwrap(),
            r["end"]["character"].as_u64().unwrap(),
        )
    };
    // `top.start` is the `[0, 4]` written as the line's first point.
    assert_eq!(loc(&mut c, "start, [0", 1), (2, 13, 19));
    // `e1.center` is `c1`, so its definition.
    assert_eq!(loc(&mut c, "center)", 1), (1, 2, 4));
    // The handle itself goes to its assignment, as any variable.
    assert_eq!(loc(&mut c, "e1.center", 0), (3, 2, 4));
}
