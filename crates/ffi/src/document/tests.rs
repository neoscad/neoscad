//! The document loop through the bridge: one run feeding the markers, the
//! console, the watched files and the view; the customizer's parameters,
//! values and parameter sets.

use std::path::PathBuf;
use std::sync::Arc;

use serde_json::{Value, json};

use super::*;
use crate::{CoreConfig, RenderMode};

const DOC: &str = "/NeoSCAD-ffi-document-test/model.scad";

fn core() -> Arc<Core> {
    Core::new(CoreConfig {
        resource_dir: None,
        test_hooks: true,
    })
    .unwrap()
}

fn request(mode: RenderMode) -> DocumentRequest {
    DocumentRequest {
        mode,
        overrides: Vec::new(),
        parts: false,
        enable: Vec::new(),
    }
}

/// A directory of its own under the temporary directory, removed on drop.
struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> TempDir {
        let d = std::env::temp_dir().join(format!("neoscad-ffi-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        // The session names files by their real path.
        TempDir(std::fs::canonicalize(&d).unwrap())
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Keeps what a run's listener was told.
#[derive(Default)]
struct Collect(std::sync::Mutex<Vec<String>>);

impl DocumentListener for Collect {
    fn language(&self, messages: Vec<String>) {
        self.0.lock().unwrap().extend(messages);
    }
}

fn send(ls: &LanguageServer, m: Value) -> Vec<Value> {
    ls.handle(m.to_string())
        .unwrap()
        .iter()
        .map(|s| serde_json::from_str(s).unwrap())
        .collect()
}

fn parsed(msgs: &[String]) -> Vec<Value> {
    msgs.iter()
        .map(|s| serde_json::from_str::<Value>(s).unwrap()["params"].clone())
        .collect()
}

#[test]
fn one_run_feeds_the_markers_and_the_console() {
    let c = core();
    // "é" is two bytes and one UTF-16 unit: positions must be the
    // editor's, not byte columns.
    let text = "x = \"é\"; cub(1);\nunion() { cube(1); square(1); }\n";
    c.open(DOC.into(), Some(text.into())).unwrap();
    let ls = c.clone().language_server(true).unwrap();
    send(
        &ls,
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"capabilities": {}}}),
    );
    send(
        &ls,
        json!({"jsonrpc": "2.0", "method": "textDocument/didOpen", "params": {
            "textDocument": {"uri": format!("file://{DOC}"), "languageId": "openscad", "version": 3, "text": text}}}),
    );
    // The server does not evaluate by itself.
    assert!(!ls.diagnostics_pending().unwrap());
    let early = Arc::new(Collect::default());
    let r = c
        .run_document(
            DOC.into(),
            request(RenderMode::Render),
            None,
            Some(ls.clone()),
            Some(early.clone()),
        )
        .unwrap();
    // The evaluation's diagnostics went out before the geometry stage...
    let first = parsed(&early.0.lock().unwrap());
    assert_eq!(first.len(), 1, "{first:?}");
    assert_eq!(first[0]["version"], 3);
    let d = first[0]["diagnostics"].as_array().unwrap();
    assert!(d.iter().any(|d| d["code"] == "unknown-module"));
    assert!(
        !d.iter()
            .any(|d| d["message"].as_str().unwrap().contains("Mixing"))
    );
    // ... and again with the geometry stage's warning.
    let pubs = parsed(&r.language);
    assert_eq!(pubs.len(), 1, "{pubs:?}");
    assert_eq!(pubs[0]["version"], 3);
    let d = pubs[0]["diagnostics"].as_array().unwrap();
    let cub = d.iter().find(|d| d["code"] == "unknown-module").unwrap();
    assert_eq!(cub["range"]["start"], json!({"line": 0, "character": 9}));
    // The render's own warning reaches the markers too.
    let mixed = d
        .iter()
        .find(|d| d["message"].as_str().unwrap().contains("Mixing 2D and 3D"))
        .expect("the geometry stage's warning");
    assert_eq!(mixed["range"]["start"]["line"], 1);
    // The console: every line, with editor positions to jump to.
    let w = r
        .console
        .iter()
        .find(|l| l.text.contains("cub"))
        .expect("the unknown module's line");
    assert_eq!(w.kind, ConsoleKind::Warning);
    let at = w.location.as_ref().unwrap();
    assert_eq!(at.path, DOC);
    assert_eq!((at.start_line, at.start_character), (0, 9));
    assert!(
        r.console
            .iter()
            .any(|l| l.text.contains("Mixing 2D and 3D"))
    );
}

#[test]
fn a_run_reads_the_text_of_its_moment_and_reports_the_files_it_read() {
    let dir = TempDir::new("files");
    let main = dir.0.join("main.scad");
    std::fs::write(
        dir.0.join("parts.scad"),
        "module peg() cylinder(h = 5, r = 1);\n",
    )
    .unwrap();
    std::fs::write(&main, "include <parts.scad>\npeg();\n").unwrap();
    let c = core();
    let path = main.to_string_lossy().into_owned();
    c.open(path.clone(), Some("include <parts.scad>\npeg();\n".into()))
        .unwrap();
    let r = c
        .run_document(path.clone(), request(RenderMode::Preview), None, None, None)
        .unwrap();
    assert_eq!(r.render.exit_code, 0, "{}", r.render.console);
    // The document itself is not watched (the app saves it), the include
    // is; the bundled libraries exist only in memory.
    assert_eq!(
        r.files,
        vec![dir.0.join("parts.scad").to_string_lossy().into_owned()]
    );
}

#[test]
fn customizer_values_run_as_assignments_after_the_text() {
    let c = core();
    let text = "/* [Size] */\nsize = 10; // [1:20]\nlabel = \"a\\\"b\";\n/* [Hidden] */\nsecret = 1;\ncube(size);\necho(label);\n";
    c.open(DOC.into(), Some(text.into())).unwrap();
    let mut req = request(RenderMode::Render);
    req.overrides = vec![
        ParameterOverride {
            name: "size".into(),
            value: ParameterValue::Number { value: 3.0 },
        },
        ParameterOverride {
            name: "label".into(),
            value: ParameterValue::Text {
                value: "q\"\\z".into(),
            },
        },
        // Not an identifier: dropped, never spliced into the model.
        ParameterOverride {
            name: "x; cube(99)".into(),
            value: ParameterValue::Number { value: 1.0 },
        },
    ];
    let r = c.run_document(DOC.into(), req, None, None, None).unwrap();
    assert_eq!(r.render.exit_code, 0, "{}", r.render.console);
    assert!((r.render.geometry.unwrap().volume.unwrap() - 27.0).abs() < 1e-9);
    // The value arrives intact (echo prints strings without escapes).
    assert_eq!(r.render.echo, vec!["ECHO: \"q\"\\z\"".to_string()]);
    // The text is untouched, and no reassignment warning.
    assert_eq!(c.read_file(DOC.into()).unwrap(), text);
    assert!(
        r.render.diagnostics.is_empty(),
        "{:?}",
        r.render.diagnostics
    );

    let groups = c.parameters(DOC.into()).unwrap();
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0].name, "Size");
    let names: Vec<&str> = groups[0]
        .parameters
        .iter()
        .map(|p| p.name.as_str())
        .collect();
    assert_eq!(names, ["size", "label"]);
    let size = &groups[0].parameters[0];
    assert_eq!(
        size.control,
        ParameterControl::Slider {
            min: 1.0,
            max: 20.0,
            step: None
        }
    );
    assert_eq!(size.default_value, ParameterValue::Number { value: 10.0 });
}

#[test]
fn parameter_sets_round_trip_through_openscads_file() {
    let dir = TempDir::new("sets");
    let json_path = dir.0.join("model.json").to_string_lossy().into_owned();
    let c = core();
    let text = "width = 10; // [1:100]\nshape = \"round\"; // [round, square]\nsolid = true;\ncube(width);\n";
    c.open(DOC.into(), Some(text.into())).unwrap();
    assert!(c.parameter_sets(json_path.clone()).unwrap().is_empty());
    let set = |w: f64| {
        vec![
            ParameterOverride {
                name: "width".into(),
                value: ParameterValue::Number { value: w },
            },
            ParameterOverride {
                name: "solid".into(),
                value: ParameterValue::Bool { value: false },
            },
        ]
    };
    c.save_parameter_set(DOC.into(), json_path.clone(), "wide".into(), set(42.5))
        .unwrap();
    c.save_parameter_set(DOC.into(), json_path.clone(), "narrow".into(), set(2.0))
        .unwrap();
    // Replacing a set keeps its place.
    c.save_parameter_set(DOC.into(), json_path.clone(), "wide".into(), set(43.0))
        .unwrap();
    assert_eq!(
        c.parameter_sets(json_path.clone()).unwrap(),
        ["wide", "narrow"]
    );
    let written = std::fs::read_to_string(&json_path).unwrap();
    // OpenSCAD's layout: string values, the format version, every
    // parameter of the model.
    let v: Value = serde_json::from_str(&written).unwrap();
    assert_eq!(v["fileFormatVersion"], "1");
    assert_eq!(
        v["parameterSets"]["wide"],
        json!({"width": "43", "shape": "round", "solid": "false"})
    );
    let values = c
        .apply_parameter_set(DOC.into(), json_path.clone(), "narrow".into())
        .unwrap();
    assert_eq!(
        values,
        vec![
            ParameterOverride {
                name: "width".into(),
                value: ParameterValue::Number { value: 2.0 }
            },
            ParameterOverride {
                name: "shape".into(),
                value: ParameterValue::Text {
                    value: "round".into()
                }
            },
            ParameterOverride {
                name: "solid".into(),
                value: ParameterValue::Bool { value: false }
            },
        ]
    );
    // A file OpenSCAD wrote: out-of-range values clamp, unknown keys and
    // missing parameters fall back as `-p -P` does.
    let reference = dir.0.join("ref.json");
    std::fs::write(
        &reference,
        r#"{"parameterSets": {"big": {"width": "500", "nope": "1"}}, "fileFormatVersion": "1"}"#,
    )
    .unwrap();
    let values = c
        .apply_parameter_set(
            DOC.into(),
            reference.to_string_lossy().into_owned(),
            "big".into(),
        )
        .unwrap();
    assert_eq!(values[0].value, ParameterValue::Number { value: 100.0 });
    assert_eq!(values[2].value, ParameterValue::Bool { value: true });
}

#[test]
fn the_files_view_moves_the_camera_when_it_changes() {
    let Ok(v) = Viewport::new("Cornfield".into()) else {
        eprintln!("skipped: no GPU");
        return;
    };
    let c = core();
    c.open(
        DOC.into(),
        Some("$vpr = [10, 20, 30];\n$vpd = 77;\ncube(1);\n".into()),
    )
    .unwrap();
    let r = c
        .run_document(
            DOC.into(),
            request(RenderMode::Preview),
            Some(v.clone()),
            None,
            None,
        )
        .unwrap();
    assert!(r.shown);
    let view = r.file_view.expect("the file's view");
    assert_eq!(view.vpd, 77.0);
    let cam = v.camera().unwrap();
    assert_eq!(cam.vpd, 77.0);
    for (a, b) in cam.vpr.iter().zip([10.0, 20.0, 30.0]) {
        assert!((a - b).abs() < 1e-9, "{:?}", cam.vpr);
    }
    // The user orbits; a run with the same `$vp*` keeps their view...
    v.orbit(30.0, 0.0).unwrap();
    let orbited = v.camera().unwrap();
    let r = c
        .run_document(
            DOC.into(),
            request(RenderMode::Preview),
            Some(v.clone()),
            None,
            None,
        )
        .unwrap();
    assert!(r.file_view.is_none());
    assert_eq!(v.camera().unwrap(), orbited);
    // ... and a changed `$vpd` moves it.
    c.update(
        DOC.into(),
        "$vpr = [10, 20, 30];\n$vpd = 99;\ncube(1);\n".into(),
    )
    .unwrap();
    let r = c
        .run_document(
            DOC.into(),
            request(RenderMode::Preview),
            Some(v.clone()),
            None,
            None,
        )
        .unwrap();
    assert_eq!(r.file_view.unwrap().vpd, 99.0);
    // No `$vp*` warning for a GUI's view.
    assert!(
        r.render.diagnostics.is_empty(),
        "{:?}",
        r.render.diagnostics
    );
}

#[test]
fn a_run_superseded_by_a_newer_one_does_not_replace_its_model() {
    let Ok(v) = Viewport::new("Cornfield".into()) else {
        eprintln!("skipped: no GPU");
        return;
    };
    let c = core();
    c.open(DOC.into(), Some("cube(1);\n".into())).unwrap();
    // A later generation is already under way.
    v.requests.fetch_add(5, std::sync::atomic::Ordering::SeqCst);
    let generation_before = v.requests.load(std::sync::atomic::Ordering::SeqCst);
    let r = c
        .run_document(
            DOC.into(),
            request(RenderMode::Preview),
            Some(v.clone()),
            None,
            None,
        )
        .unwrap();
    assert!(r.shown);
    assert_eq!(
        v.requests.load(std::sync::atomic::Ordering::SeqCst),
        generation_before + 1
    );
    assert_eq!(c.running(DOC.into()).unwrap(), 0);
}

#[test]
fn the_parts_toggle_reaches_the_documents_run() {
    let c = Core::new(CoreConfig {
        resource_dir: None,
        test_hooks: true,
    })
    .unwrap();
    let doc = "/NeoSCAD-ffi-document-parts/model.scad".to_string();
    c.open(doc.clone(), Some("part(\"a\") cube(1);".into()))
        .unwrap();
    let unknown = |parts: bool| {
        let mut req = request(RenderMode::Preview);
        req.parts = parts;
        let r = c.run_document(doc.clone(), req, None, None, None).unwrap();
        r.console
            .iter()
            .any(|l| l.text.contains("unknown module 'part'"))
    };
    assert!(unknown(false));
    assert!(!unknown(true));
}

#[test]
fn enabled_features_reach_the_documents_run() {
    // OpenSCAD's experimental features by their `--enable` names: off, a
    // call warns that it is not enabled; on, it works, as in OpenSCAD.
    let c = Core::new(CoreConfig {
        resource_dir: None,
        test_hooks: true,
    })
    .unwrap();
    let doc = "/NeoSCAD-ffi-document-enable/model.scad".to_string();
    c.open(
        doc.clone(),
        Some("echo(object(a = 1), textmetrics(\"x\").advance);".into()),
    )
    .unwrap();
    let console = |enable: &[&str]| {
        let mut req = request(RenderMode::Preview);
        req.enable = enable.iter().map(|s| s.to_string()).collect();
        let r = c.run_document(doc.clone(), req, None, None, None).unwrap();
        r.console.iter().map(|l| l.text.clone()).collect::<Vec<_>>()
    };
    let off = console(&[]);
    assert!(
        off.iter()
            .any(|l| l.contains("Experimental builtin function 'object' is not enabled")),
        "{off:?}"
    );
    let on = console(&["object-function", "textmetrics"]);
    assert!(
        on.iter()
            .any(|l| l.starts_with("ECHO: { a = 1; }, [") && !l.contains("undef")),
        "{on:?}"
    );
}
