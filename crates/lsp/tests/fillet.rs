//! Fillet calls in the language server (`--enable fillet`;
//! `docs/fillets.md`, section 5.4): a host's rendered run brings each
//! call's selection, and the server offers "Pin count", which writes the
//! number of edges the run selected as `expect`; a wrong `expect` is an
//! error whose quick fix is the same edit.

mod common;

use std::sync::Arc;

use common::{Client, uri};
use serde_json::{Value, json};

const MAIN: &str = "/w/main.scad";

const MODEL: &str = "fillet_edges(r = 1, edges = \"|z\") cube(10);
fillet_edges(r = 1, edges = \">z\", expect = 3) translate([20, 0, 0]) cube(10);
";

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

fn actions_at(c: &mut Client, needle: &str) -> Vec<Value> {
    let at = Client::pos(MODEL, needle, 0);
    let r = c.request(
        "textDocument/codeAction",
        json!({"textDocument": {"uri": uri(MAIN)}, "range": {"start": at, "end": at}, "context": {"diagnostics": []}}),
    );
    r.as_array().unwrap().clone()
}

#[test]
fn pin_count_from_a_hosts_rendered_run() {
    let fs = Arc::new(lang::vfs::MemFs::new());
    fs.insert(MAIN, MODEL.as_bytes().to_vec());
    let cfg = session::Config::new(fs, lang::loader::LibraryPath(Vec::new()));
    let mut c = Client::with_config(cfg, true);
    let on = session::Extensions::NONE.with(session::Extension::Fillet);
    c.server.set_extensions(on);
    c.open(MAIN, MODEL);
    // The host renders (here, a check) and hands the server its log.
    let mut run = session::Run::new(MAIN);
    run.text = Some(Arc::from(MODEL.as_bytes()));
    run.extensions = on;
    let checked = c
        .session
        .check(&session::check::CheckRequest {
            run,
            settings: Default::default(),
        })
        .unwrap();
    let pubs = c.server.supply_log(
        &c.session,
        MAIN.as_ref(),
        Arc::from(MODEL.as_bytes()),
        &checked.log,
    );
    let p: Value = serde_json::from_str(&pubs[0]).unwrap();
    let d = p["params"]["diagnostics"].as_array().unwrap();
    // The wrong count is an error, with the edit as its fix.
    let count = d
        .iter()
        .find(|d| d["code"] == "fillet-count")
        .unwrap_or_else(|| panic!("{d:?}"));
    assert_eq!(count["severity"], 1);
    // "Pin count" on the first call appends `expect`.
    let actions = actions_at(&mut c, "|z");
    let pin = actions
        .iter()
        .find(|a| a["title"] == "Pin count: expect = 4")
        .unwrap_or_else(|| panic!("{actions:?}"));
    let edits = pin["edit"]["changes"][uri(MAIN)].as_array().unwrap();
    assert!(
        apply(MODEL, edits)
            .starts_with("fillet_edges(r = 1, edges = \"|z\", expect = 4) cube(10);\n"),
        "{}",
        apply(MODEL, edits)
    );
    // On the second, it corrects the number.
    let actions = actions_at(&mut c, ">z");
    let pin = actions
        .iter()
        .find(|a| a["title"] == "Pin count: expect = 4")
        .unwrap_or_else(|| panic!("{actions:?}"));
    let edits = pin["edit"]["changes"][uri(MAIN)].as_array().unwrap();
    assert!(apply(MODEL, edits).contains("edges = \">z\", expect = 4)"));
}

/// A client whose session runs with `--enable fillet` (and `query`, for
/// `@name`), over `text` at `MAIN`.
fn fillet_client(text: &str, query: bool) -> Client {
    Client::mem_config(&[(MAIN, text)], |cfg| {
        let mut on = session::Extensions::NONE.with(session::Extension::Fillet);
        if query {
            on = on.with(session::Extension::Query);
        }
        cfg.extensions = on;
    })
}

fn complete_at(c: &mut Client, text: &str, needle: &str, delta: usize) -> Value {
    c.at("textDocument/completion", MAIN, text, needle, delta)
}

const SELECTORS: &str = "fillet_edges(r = 1, edges = \"|z and \") cube(10);
chamfer_edges(1, \"|z \") cube(10);
fillet_edges(r = 1, except = \"%ci\") cube(10);
fillet_edges(r = 1, edges = [\"|z\", \">\"]) cube(10);
fillet_edges(r = 1, edges = \"child(\") cube(10);
fillet_edges(r = 1, edges = \"cnovex\") cube(10);
echo(\"|z and \");
";

/// Completion inside `edges` and `except` strings offers the selector
/// language: atoms where an operand goes, operators after one, nothing
/// inside `child(`, the "did you mean" word for a slip, and nothing in
/// other strings.
#[test]
fn selector_strings_complete() {
    let mut c = fillet_client(SELECTORS, false);
    c.open(MAIN, SELECTORS);
    let r = complete_at(&mut c, SELECTORS, "|z and \"", 7);
    let l = common::labels(&r);
    for want in [
        "all",
        "convex",
        "%circle",
        "|z",
        ">>z[i]",
        "child(i, j)",
        "not",
    ] {
        assert!(l.contains(&want.to_string()), "{want} not in {l:?}");
    }
    assert!(!l.contains(&"and".to_string()), "{l:?}");
    // `@name` needs `--enable query`.
    assert!(!l.contains(&"@name".to_string()), "{l:?}");
    // A snippet for the indexed group.
    let nth = r["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["label"] == ">>z[i]")
        .unwrap();
    assert_eq!(nth["insertText"], ">>${1:z}[${2:-2}]");
    assert_eq!(nth["insertTextFormat"], 2);
    // After an atom: the operators (the positional `edges` of a chamfer).
    let r = complete_at(&mut c, SELECTORS, "|z \"", 3);
    let mut l = common::labels(&r);
    l.sort();
    assert_eq!(l, ["and", "exc", "or"]);
    // `except`, with the atom typed so far as the edit range.
    let r = complete_at(&mut c, SELECTORS, "%ci", 3);
    assert_eq!(common::labels(&r), ["%circle"]);
    assert_eq!(
        r["itemDefaults"]["editRange"]["start"],
        Client::pos(SELECTORS, "%ci", 0)
    );
    // In a list.
    let r = complete_at(&mut c, SELECTORS, "\">\"", 2);
    let l = common::labels(&r);
    assert!(
        l.contains(&">z".to_string()) && l.contains(&">>z[i]".to_string()),
        "{l:?}"
    );
    assert!(!l.contains(&"|z".to_string()), "{l:?}");
    // Inside `child(`: numbers go there.
    let r = complete_at(&mut c, SELECTORS, "child(\"", 6);
    assert_eq!(common::labels(&r), Vec::<String>::new());
    // A slip: the word it is likely for.
    let r = complete_at(&mut c, SELECTORS, "cnovex", 6);
    assert_eq!(common::labels(&r), ["convex"]);
    assert_eq!(r["items"][0]["filterText"], "cnovex");
    // Any other string completes nothing.
    let r = complete_at(&mut c, SELECTORS, "echo(\"|z and ", 13);
    assert_eq!(common::labels(&r), Vec::<String>::new());

    // With `--enable query`, `@name` too.
    let mut c = fillet_client(SELECTORS, true);
    c.open(MAIN, SELECTORS);
    let r = complete_at(&mut c, SELECTORS, "|z and \"", 7);
    assert!(common::labels(&r).contains(&"@name".to_string()));
}

/// Off, or under a program's own `module fillet_edges`, a string is a
/// string; and the builtins are offered only with the extension.
#[test]
fn selector_completion_needs_the_builtin() {
    let mut c = Client::mem(&[(MAIN, SELECTORS)]);
    c.open(MAIN, SELECTORS);
    let r = complete_at(&mut c, SELECTORS, "|z and \"", 7);
    assert_eq!(common::labels(&r), Vec::<String>::new());
    let text = "fil";
    c.open(MAIN, text);
    let r = c.at("textDocument/completion", MAIN, text, "fil", 3);
    assert!(!common::labels(&r).contains(&"fillet_edges".to_string()));
    let mut on = fillet_client(text, false);
    on.open(MAIN, text);
    let r = on.at("textDocument/completion", MAIN, text, "fil", 3);
    assert!(common::labels(&r).contains(&"fillet_edges".to_string()));

    let own = "module fillet_edges(r, edges) children();\nfillet_edges(r = 1, edges = \"|z and \") cube(10);\n";
    let mut c = fillet_client(own, false);
    c.open(MAIN, own);
    let r = complete_at(&mut c, own, "|z and \"", 7);
    assert_eq!(common::labels(&r), Vec::<String>::new());
}

/// What a host's `check` of `text` logged.
fn checked_log(c: &Client, text: &str, on: session::Extensions) -> session::Log {
    let mut run = session::Run::new(MAIN);
    run.text = Some(Arc::from(text.as_bytes()));
    run.extensions = on;
    c.session
        .check(&session::check::CheckRequest {
            run,
            settings: Default::default(),
        })
        .unwrap()
        .log
}

/// A client whose host supplies rendered runs, over `text`.
fn host_client(text: &str) -> (Client, session::Extensions) {
    let fs = Arc::new(lang::vfs::MemFs::new());
    fs.insert(MAIN, text.as_bytes().to_vec());
    let cfg = session::Config::new(fs, lang::loader::LibraryPath(Vec::new()));
    let mut c = Client::with_config(cfg, true);
    let on = session::Extensions::NONE.with(session::Extension::Fillet);
    c.server.set_extensions(on);
    c.open(MAIN, text);
    (c, on)
}

/// Hover: an atom in a selector string, a call's named argument, and the
/// call's last rendered run.
#[test]
fn hover_on_selectors_arguments_and_the_last_run() {
    let text = "fillet_edges(r = 1, edges = \"%circle or |z\", expect = 4) cube(10);\n";
    let mut c = fillet_client(text, false);
    c.open(MAIN, text);
    let h = c.at("textDocument/hover", MAIN, text, "|z", 1);
    let v = h["contents"]["value"].as_str().unwrap();
    assert!(v.starts_with("`|z`: lines parallel to z"), "{v}");
    assert_eq!(h["range"]["start"], Client::pos(text, "|z", 0));
    let h = c.at("textDocument/hover", MAIN, text, "%circle", 3);
    assert!(
        h["contents"]["value"]
            .as_str()
            .unwrap()
            .starts_with("`%circle`: circles and arcs"),
        "{h}"
    );
    let h = c.at("textDocument/hover", MAIN, text, " or", 2);
    assert!(
        h["contents"]["value"]
            .as_str()
            .unwrap()
            .starts_with("`or`:"),
        "{h}"
    );
    // A named argument: its line of the reference.
    let h = c.at("textDocument/hover", MAIN, text, "expect", 2);
    let v = h["contents"]["value"].as_str().unwrap();
    assert!(v.contains("Argument of `fillet_edges("), "{v}");
    assert!(
        v.contains("the number of edges the selection must match"),
        "{v}"
    );
    assert!(v.contains("--enable fillet"), "{v}");
    // Any builtin's: `scale` here is the argument, not the module.
    let text2 = "linear_extrude(10, scale = 2) square(1);\n";
    c.open(MAIN, text2);
    let h = c.at("textDocument/hover", MAIN, text2, "scale", 1);
    let v = h["contents"]["value"].as_str().unwrap();
    assert!(v.contains("Argument of `linear_extrude("), "{v}");

    // The name: the reference, then what a host's rendered run selected.
    let (mut c, on) = host_client(text);
    let log = checked_log(&c, text, on);
    c.server
        .supply_log(&c.session, MAIN.as_ref(), Arc::from(text.as_bytes()), &log);
    let h = c.at("textDocument/hover", MAIN, text, "fillet_edges", 2);
    let v = h["contents"]["value"].as_str().unwrap();
    assert!(v.contains("NeoSCAD extension (`--enable fillet`)"), "{v}");
    assert!(
        v.contains("Last run: `fillet_edges at line 1: 4 edges (4 line, convex, 90°), r 1"),
        "{v}"
    );
    assert!(
        v.contains("\n- 1. line (convex, 90°, translational)"),
        "{v}"
    );
}

/// A rendered run's fillet diagnostics in the editor: a selection
/// problem on the `edges` string, and a size that does not fit on `r`
/// with the size that does as a quick fix. (A mixed vertex is no longer a
/// diagnostic: the call rounds it in two passes, `docs/fillets.md`
/// section 15.6.)
#[test]
fn rendered_diagnostics_sit_on_what_to_change_with_their_fixes() {
    let text = "fillet_edges(r = 1, edges = \"|z\", expect = 3) cube(10);
fillet_edges(r = 3, edges = \"|y\") translate([20, 0, 0]) cube([20, 10, 4]);
fillet_edges(r = 1, edges = \"all\") translate([0, 40, 0]) union() { cube([20, 20, 5]); translate([5, 5, 0]) cube([10, 10, 15]); }
";
    let (mut c, on) = host_client(text);
    let log = checked_log(&c, text, on);
    let pubs = c
        .server
        .supply_log(&c.session, MAIN.as_ref(), Arc::from(text.as_bytes()), &log);
    let p: Value = serde_json::from_str(&pubs[0]).unwrap();
    let d = p["params"]["diagnostics"].as_array().unwrap().clone();
    let find = |code: &str| {
        d.iter()
            .find(|x| x["code"] == code)
            .unwrap_or_else(|| panic!("{code} not in {d:?}"))
            .clone()
    };
    // The count: on the selector.
    let count = find("fillet-count");
    assert_eq!(count["range"]["start"], Client::pos(text, "\"|z\"", 0));
    assert_eq!(count["range"]["end"], Client::pos(text, "\"|z\"", 4));
    // The size: on `3`, fixed by the largest that fits, less 5%.
    let big = find("fillet-overlap");
    assert_eq!(big["range"]["start"], Client::pos(text, "r = 3", 4));
    let fix = &big["data"]["fixes"][0];
    assert_eq!(fix["edits"][0]["newText"], "1.9");
    // Code actions answer from the published diagnostics too.
    let at = Client::pos(text, "r = 3", 4);
    let actions = c.request(
        "textDocument/codeAction",
        json!({"textDocument": {"uri": uri(MAIN)}, "range": {"start": at, "end": at}, "context": {"diagnostics": [], "only": ["quickfix"]}}),
    );
    let a = &actions.as_array().unwrap()[0];
    assert_eq!(a["kind"], "quickfix");
    let fixed = apply(text, a["edit"]["changes"][uri(MAIN)].as_array().unwrap());
    assert!(
        fixed.contains("fillet_edges(r = 1.9, edges = \"|y\")"),
        "{fixed}"
    );
    // The mixed vertex builds in two passes: nothing to fix.
    assert!(
        !d.iter().any(|x| x["code"] == "fillet-unsupported-vertex"),
        "{d:?}"
    );
}
