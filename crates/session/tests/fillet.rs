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

/// Stage F1's reports: `check`'s `fillets`, `measure --fillet`, the
/// overlay `snapshot --fillet` draws, the diagnostics and the "Pin count"
/// edit, and that a warm session answers as a cold one.
const MODEL: &[u8] = b"chamfer_edges(d = 1, edges = \"%circle and >z\")
  difference() {
    cube([20, 20, 10]);
    translate([10, 10, -1]) cylinder(d = 6, h = 12);
  }
fillet_edges(r = 1, edges = \"|z\", expect = 3) translate([30, 0, 0]) cube(10);
fillet_edges(r = 1, edges = \"|z\") translate([50, 0, 0]) cylinder(r = 5, h = 3, $fn = 12);
";

fn check(s: &Session, on: bool) -> serde_json::Value {
    let req = session::check::CheckRequest {
        run: run(on),
        settings: Default::default(),
    };
    let mut v = s.check(&req).unwrap().summary;
    v.as_object_mut().unwrap().remove("timings_ms");
    v
}

#[test]
fn check_reports_each_call() {
    let s = session(MODEL);
    let v = check(&s, true);
    let f = v["fillets"].as_array().expect("fillets");
    assert_eq!(f.len(), 3);
    // The chamfered hole: its top rim.
    assert_eq!(f[0]["module"], "chamfer_edges");
    assert_eq!(f[0]["d"], 1.0);
    assert_eq!(f[0]["selector"], "\"%circle and >z\"");
    // A circle: its blend is stage F3's.
    assert_eq!(f[0]["status"], "not-built");
    assert_eq!(f[0]["codes"], serde_json::json!(["fillet-not-built"]));
    assert_eq!(f[0]["line"], 1);
    let e = &f[0]["edges"][0];
    assert_eq!(
        (&e["curve"], &e["sense"], &e["class"], &e["angle"]),
        (
            &serde_json::json!("circle"),
            &serde_json::json!("convex"),
            &serde_json::json!("rotational"),
            &serde_json::json!(90.0)
        )
    );
    assert_eq!(e["center"], serde_json::json!([10.0, 10.0, 10.0]));
    assert_eq!(e["faces"], serde_json::json!(["plane", "cylinder"]));
    // The pinned count is wrong: an error with the edit that fixes it.
    assert_eq!(f[1]["status"], "count");
    assert_eq!(f[1]["matched"], 4);
    assert_eq!(f[1]["codes"], serde_json::json!(["fillet-count"]));
    let d = v["diagnostics"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d["code"] == "fillet-count")
        .expect("fillet-count");
    assert_eq!(d["severity"], "error");
    assert_eq!(d["hints"][0]["replace"]["text"], "4");
    // The $fn cylinder: its sides are seams, so nothing is selected.
    assert_eq!(f[2]["status"], "no-edges");
    assert_eq!(
        f[2]["skipped"],
        serde_json::json!([{"reason": "polygon seam", "count": 12, "module": "cylinder", "line": 7}])
    );
    // The human summary has a line per call.
    let text = session::check::text(&v);
    assert!(
        text.contains("chamfer_edges at line 1: 1 edge (1 circle, convex, 90°), d 1, edges = \"%circle and >z\""),
        "{text}"
    );
    // "Pin count": the first call has no `expect`; the edit appends it.
    assert_eq!(
        f[0]["pin"]["text"],
        "edges = \"%circle and >z\", expect = 1"
    );
    assert_eq!(f[1]["pin"]["text"], "4");
    assert!(f[2].get("pin").is_none(), "nothing to pin");
}

#[test]
fn off_or_without_calls_check_has_no_fillets() {
    let s = session(MODEL);
    assert!(check(&s, false).get("fillets").is_none());
    let s = session(b"cube(10);\n");
    assert!(check(&s, true).get("fillets").is_none());
}

#[test]
fn a_warm_session_reports_as_a_cold_one() {
    let warm = session(MODEL);
    let first = check(&warm, true);
    assert_eq!(check(&warm, true), first);
    assert_eq!(check(&session(MODEL), true), first);
}

#[test]
fn measure_and_snapshot_name_a_call() {
    let s = session(MODEL);
    let mut req = session::measure::MeasureRequest::new(run(true));
    req.fillet = Some("2".into());
    let m = s.measure(&req).unwrap();
    assert_eq!(m.summary["fillet"]["matched"], 4);
    let text = session::measure::text(&m.summary);
    assert!(
        text.contains(
            "  1. line (convex, 90°, translational) at [30, 0, 5], 10 long, plane | plane"
        ),
        "{text}"
    );
    // By selector, quoted or not.
    req.fillet = Some("%circle and >z".into());
    assert_eq!(s.measure(&req).unwrap().summary["fillet"]["index"], 1);
    req.fillet = Some("9".into());
    let m = s.measure(&req).unwrap();
    assert_eq!(m.exit_code, 1);
    assert!(
        m.summary["error"]
            .as_str()
            .unwrap()
            .starts_with("no fillet call '9'; the calls are: 1 ("),
        "{}",
        m.summary["error"]
    );
    // The overlay: four bold numbered edges, eight thin ones.
    let r = s
        .check(&session::check::CheckRequest {
            run: run(true),
            settings: Default::default(),
        })
        .unwrap();
    let plan = &r.log.fillets.plans[1];
    let (o, header, legend) = session::fillets::overlay(plan);
    assert_eq!(o.strokes.iter().filter(|s| s.bold).count(), 4);
    assert_eq!(o.strokes.iter().filter(|s| !s.bold).count(), 8);
    let numbers: Vec<&str> = o.labels.iter().map(|l| l.text.as_str()).collect();
    assert_eq!(numbers, ["1", "2", "3", "4"]);
    assert!(header.starts_with("fillet_edges: 4 edges"), "{header}");
    assert_eq!(legend.len(), 2);
    // The same model gives the same overlay.
    assert_eq!(session::fillets::overlay(plan).0, o);
    // The seams are dashed.
    let (o, _, _) = session::fillets::overlay(&r.log.fillets.plans[2]);
    assert_eq!(o.strokes.iter().filter(|s| s.dashed).count(), 12);
    // An unknown call is the snapshot's error; --fillet with --issues is
    // refused.
    let mut req = session::snapshot::SnapshotRequest::new(run(true), "x.png");
    req.fillet = Some("7".into());
    let e = s.snapshot(&req).unwrap_err().to_string();
    assert!(e.starts_with("no fillet call '7'"), "{e}");
    req.issues = Some(Default::default());
    let e = s.snapshot(&req).unwrap_err().to_string();
    assert!(e.contains("cannot be combined"), "{e}");
}

/// Built: no diagnostics, the status says so, and the blends are in the
/// model `check` measures.
#[test]
fn on_a_call_builds_its_blends() {
    let s = session(b"fillet_edges(r = 2) cube(10);\n");
    let v = check(&s, true);
    assert_eq!(v["fillets"][0]["status"], "built");
    assert_eq!(v["diagnostics"]["items"], serde_json::json!([]));
    assert_eq!(v["exit_code"], 0);
    // 1000 less twelve spandrels and eight sphere corners of r 2, in the
    // mesh's polygonal arcs: under the exact 907.70.
    let vol = v["model"]["volume"].as_f64().unwrap();
    assert!(vol > 850.0 && vol < 907.71, "{vol}");
}

/// A failed call (decision 2): an error with the fix as an edit, the
/// child left sharp, and `check` failing with the error counted.
#[test]
fn a_failed_call_fails_check_with_its_fix() {
    let src = "fillet_edges(r = 3, edges = \"|y\") cube([20, 10, 4]);\n";
    let v = check(&session(src.as_bytes()), true);
    assert_eq!(v["exit_code"], 1);
    assert_eq!(v["ok"], false);
    assert_eq!(v["counts"]["fillet_errors"], 1);
    assert_eq!(v["counts"]["errors"], 1);
    assert_eq!(v["fillets"][0]["status"], "overlap");
    let d = &v["diagnostics"]["items"][0];
    assert_eq!(d["code"], "fillet-overlap");
    assert_eq!(d["hints"][0]["message"], "the largest r that fits is 1.99");
    // The edit replaces the `3`.
    let rep = &d["hints"][0]["replace"];
    assert_eq!(rep["text"], "1.99");
    assert_eq!(rep["span"]["start"]["column"], 18);
    assert_eq!(rep["span"]["end"]["column"], 19);
    // The child is unchanged.
    assert_eq!(v["model"]["volume"], 800.0);
    // The edit applied, the call builds and the check passes.
    let fixed = src.replacen("r = 3", "r = 1.99", 1);
    let v = check(&session(fixed.as_bytes()), true);
    assert_eq!(v["exit_code"], 0, "{}", v["diagnostics"]);
    assert_eq!(v["fillets"][0]["status"], "built");
}

/// Convex and concave edges at a vertex: refused, and the edit rewrites
/// the call as two nested ones, the concave edges first.
#[test]
fn a_mixed_vertex_offers_the_nested_rewrite() {
    let head = "fillet_edges(r = 1, edges = \"all\")";
    let src = format!(
        "{head} union() {{ cube([20, 20, 5]); translate([5, 5, 0]) cube([10, 10, 15]); }}\n"
    );
    let v = check(&session(src.as_bytes()), true);
    assert_eq!(v["exit_code"], 1);
    let d = v["diagnostics"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d["code"] == "fillet-unsupported-vertex")
        .expect("unsupported vertex")
        .clone();
    let text = d["hints"][0]["replace"]["text"].as_str().unwrap();
    assert_eq!(
        text,
        "fillet_edges(r = 1, edges = \"(all) and convex\") fillet_edges(r = 1, edges = \"(all) and concave\")"
    );
    let fixed = src.replacen(head, text, 1);
    let v = check(&session(fixed.as_bytes()), true);
    let f = v["fillets"].as_array().unwrap();
    assert_eq!(f.len(), 2);
    assert_eq!(f[1]["status"], "built", "{}", v["diagnostics"]);
}
