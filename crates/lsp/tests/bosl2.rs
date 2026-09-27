//! BOSL2 through the server, from `.reference/BOSL2` (skipped when it is
//! not checked out): completion after `include <BOSL2/std.scad>`, hover
//! and definition of `cuboid`, and the warm latency of completion and
//! hover on a BOSL2 model.

mod common;

use std::sync::Arc;
use std::time::Instant;

use common::{Client, bosl2_root, labels, uri};
use lang::loader::{LibraryPath, StdFs};
use serde_json::json;

/// A document that exists only in the client (as an unsaved one would),
/// with the checkout as the library directory.
const MODEL: &str = "/virtual/model.scad";

fn client() -> Option<Client> {
    let Some(root) = bosl2_root() else {
        eprintln!("skipped: .reference/BOSL2 is not checked out");
        return None;
    };
    Some(Client::over(Arc::new(StdFs), LibraryPath(vec![root])))
}

fn at_end(t: &str) -> serde_json::Value {
    let lines: Vec<&str> = t.split('\n').collect();
    json!({"line": lines.len() - 1, "character": lines.last().unwrap().len()})
}

fn complete(c: &mut Client, text: &str) -> serde_json::Value {
    c.notify(
        "textDocument/didChange",
        json!({"textDocument": {"uri": uri(MODEL), "version": 2}, "contentChanges": [{"text": text}]}),
    );
    c.request(
        "textDocument/completion",
        json!({"textDocument": {"uri": uri(MODEL)}, "position": at_end(text)}),
    )
}

#[test]
fn completion_after_include_std() {
    let Some(mut c) = client() else {
        return;
    };
    c.open(MODEL, "");
    let r = complete(&mut c, "include <BOSL2/std.scad>\ncub");
    let item = r["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["label"] == "cuboid")
        .expect("cuboid is offered")
        .clone();
    assert_eq!(item["kind"], 9);
    assert!(
        item["detail"]
            .as_str()
            .unwrap()
            .starts_with("module cuboid(")
    );
    let doc = item["documentation"]["value"].as_str().unwrap();
    assert!(
        doc.contains("Creates a cube with chamfering and roundovers.")
            && doc.contains("BOSL2/shapes3d.scad"),
        "{doc}"
    );
    // Private helpers only when asked for.
    let r = complete(&mut c, "include <BOSL2/std.scad>\nx = _");
    assert!(labels(&r).iter().any(|l| l.starts_with('_')));
    let r = complete(&mut c, "include <BOSL2/std.scad>\nx = a");
    assert!(!labels(&r).iter().any(|l| l.starts_with('_')));
    // Functions in expressions, BOSL2's constants too.
    let r = complete(&mut c, "include <BOSL2/std.scad>\nx = cubo");
    let l = labels(&r);
    assert!(l.contains(&"cuboid".to_string()), "{l:?}");
    let r = complete(&mut c, "include <BOSL2/std.scad>\nx = TO");
    assert!(labels(&r).contains(&"TOP".to_string()));
    // Named parameters of a BOSL2 module, with its argument docs.
    let r = complete(&mut c, "include <BOSL2/std.scad>\ncuboid(10, roun");
    let item = r["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["label"] == "rounding=")
        .expect("rounding= is offered")
        .clone();
    assert!(
        item["detail"].as_str().is_some_and(|d| !d.is_empty()),
        "{item}"
    );
}

#[test]
fn hover_and_definition_of_cuboid() {
    let Some(mut c) = client() else {
        return;
    };
    let text = "include <BOSL2/std.scad>\ncuboid([20, 10, 5], rounding = 1);\n";
    c.open(MODEL, text);
    let h = c.at("textDocument/hover", MODEL, text, "cuboid(", 2);
    let v = h["contents"]["value"].as_str().unwrap();
    assert!(v.contains("module cuboid("), "{v}");
    assert!(v.contains("BOSL2/shapes3d.scad:"), "{v}");
    assert!(
        v.contains("Creates a cube with chamfering and roundovers."),
        "{v}"
    );
    assert!(v.contains("usage:") && v.contains("arguments:"), "{v}");
    // Compact: a BOSL2 block's examples are left out.
    assert!(v.len() < 6000, "{} bytes", v.len());
    let d = c.at("textDocument/definition", MODEL, text, "cuboid(", 0);
    assert!(
        d["uri"].as_str().unwrap().ends_with("/BOSL2/shapes3d.scad"),
        "{d}"
    );
    let line = d["range"]["start"]["line"].as_u64().unwrap();
    assert_eq!(line, 203);
    // Signature help from BOSL2's own `Arguments:` lines.
    let s = c.at("textDocument/signatureHelp", MODEL, text, "rounding", 0);
    let sig = &s["signatures"][0];
    assert!(
        sig["label"].as_str().unwrap().starts_with("cuboid(size"),
        "{s}"
    );
    let active = s["activeParameter"].as_u64().unwrap() as usize;
    assert!(
        sig["parameters"][active]["documentation"]
            .as_str()
            .is_some(),
        "{s}"
    );
}

#[test]
fn diagnostics_of_a_bosl2_model() {
    let Some(mut c) = client() else {
        return;
    };
    let text = "include <BOSL2/std.scad>\ncuboid([20, 10, 5], rounding = 1);\ncuboidd(3);\n";
    c.open(MODEL, text);
    let pubs = c.diagnostics();
    let mine = pubs.iter().find(|p| p["uri"] == uri(MODEL)).unwrap();
    let list = mine["diagnostics"].as_array().unwrap();
    assert_eq!(list.len(), 1, "{list:?}");
    assert_eq!(list[0]["code"], "unknown-module");
    assert_eq!(list[0]["data"]["fixes"][0]["edits"][0]["newText"], "cuboid");
}

fn percentile(v: &mut [f64], q: f64) -> f64 {
    v.sort_by(f64::total_cmp);
    v[((v.len() as f64 * q) as usize).min(v.len() - 1)]
}

/// Warm completion and hover on a BOSL2 model: p95 under 20 ms in an
/// optimised build (checked there; a debug build only reports).
#[test]
fn latency_on_a_bosl2_model() {
    let Some(mut c) = client() else {
        return;
    };
    let text = "include <BOSL2/std.scad>\ncuboid([20, 10, 5], rounding = 1);\nx = c";
    c.open(MODEL, text);
    let complete_params = json!({"textDocument": {"uri": uri(MODEL)}, "position": at_end(text)});
    let hover_params =
        json!({"textDocument": {"uri": uri(MODEL)}, "position": Client::pos(text, "cuboid(", 2)});
    // Warm: the library indexed, the document's program built. The first
    // request pays for that (reported as the cold time).
    let t0 = Instant::now();
    c.request("textDocument/completion", complete_params.clone());
    let cold = t0.elapsed().as_secs_f64() * 1000.0;
    c.request("textDocument/hover", hover_params.clone());
    let (mut comp, mut hov) = (Vec::new(), Vec::new());
    for i in 0..100 {
        // An edit between requests, as typing makes: the document is
        // parsed again, the libraries are not.
        let t = format!("{text}{}", if i % 2 == 0 { "" } else { " " });
        c.notify(
            "textDocument/didChange",
            json!({"textDocument": {"uri": uri(MODEL), "version": 3 + i}, "contentChanges": [{"text": t}]}),
        );
        let t0 = Instant::now();
        let r = c.request("textDocument/completion", complete_params.clone());
        comp.push(t0.elapsed().as_secs_f64() * 1000.0);
        assert!(!r["items"].as_array().unwrap().is_empty());
        let t0 = Instant::now();
        let h = c.request("textDocument/hover", hover_params.clone());
        hov.push(t0.elapsed().as_secs_f64() * 1000.0);
        assert!(h["contents"].is_object());
    }
    let (c50, c95) = (percentile(&mut comp, 0.5), percentile(&mut comp, 0.95));
    let (h50, h95) = (percentile(&mut hov, 0.5), percentile(&mut hov, 0.95));
    eprintln!(
        "BOSL2 latency (warm, {} files cached): completion p50 {c50:.2} ms p95 {c95:.2} ms; hover p50 {h50:.2} ms p95 {h95:.2} ms; cold first completion {cold:.1} ms",
        c.server.cached_files()
    );
    if !cfg!(debug_assertions) {
        assert!(c95 < 20.0 && h95 < 20.0, "p95 over 20 ms");
    }
}
