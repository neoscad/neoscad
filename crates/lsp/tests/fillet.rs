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
