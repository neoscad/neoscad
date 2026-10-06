//! The protocol's handlers, natively: the requests of
//! `docs/web-protocol.md` and their replies, as the worker sees them.
//! `test/run.mjs` runs the built module in node for the same.

use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::{Value, json};

use super::*;

const DOC: &str = "/doc/main.scad";

/// A clock that moves 1 ms per reading, so timings are deterministic.
fn clock() -> Clock {
    let t = Arc::new(AtomicU64::new(0));
    Arc::new(move || t.fetch_add(1, Ordering::Relaxed) as f64)
}

/// A request's reply: its envelope and buffers.
fn call(w: &mut Worker, request: Value, buffers: Vec<Vec<u8>>) -> (Value, Vec<Vec<u8>>) {
    let r = w.handle(&request.to_string(), buffers);
    (serde_json::from_str(&r.json).unwrap(), r.buffers)
}

/// A request that must succeed: its result.
fn ok(w: &mut Worker, request: Value) -> Value {
    let (v, _) = call(w, request, Vec::new());
    assert_eq!(v["ok"], true, "{v}");
    v["result"].clone()
}

/// A request that must fail: its error's kind.
fn err(w: &mut Worker, request: Value) -> String {
    let (v, _) = call(w, request, Vec::new());
    assert_eq!(v["ok"], false, "{v}");
    v["error"]["kind"].as_str().unwrap().to_string()
}

fn worker() -> Worker {
    let mut w = Worker::new(Some(clock()));
    let r = ok(&mut w, json!({ "id": 1, "type": "init", "seed": 7 }));
    assert_eq!(r["libraryDirs"], json!([LIBRARY_DIR]));
    assert_eq!(r["limits"]["memoryBytes"], 1u64 << 30);
    w
}

#[test]
fn linked_stack_matches_the_evaluator() {
    assert_eq!(
        env!("NEOSCAD_WEB_STACK_SIZE").parse::<usize>().unwrap(),
        eval::recursion::WASM_STACK_SIZE
    );
}

#[test]
fn requests_before_init_and_unknown_ones_fail() {
    let mut w = Worker::new(None);
    assert_eq!(
        err(&mut w, json!({ "id": 1, "type": "open", "path": DOC })),
        "invalidArgument"
    );
    let (v, _) = call(&mut w, json!({ "id": 9, "type": "init" }), Vec::new());
    assert_eq!(v["id"], 9);
    assert_eq!(
        err(&mut w, json!({ "id": 2, "type": "init" })),
        "invalidArgument"
    );
    assert_eq!(
        err(&mut w, json!({ "id": 3, "type": "nope" })),
        "invalidArgument"
    );
    assert_eq!(
        err(
            &mut w,
            json!({ "id": 4, "type": "open", "path": "main.scad" })
        ),
        "invalidArgument"
    );
    let r = w.handle("not json", Vec::new());
    assert!(r.json.contains("\"ok\":false"), "{}", r.json);
}

/// A preview's scene arrives as `render::packed`'s wire form: two
/// buffers of whole vertices and segments, and the metadata as JSON text,
/// which the viewer's `PackedScene::from_parts` takes back.
#[test]
fn a_run_packs_its_scene_into_buffers() {
    let mut w = worker();
    ok(
        &mut w,
        json!({ "id": 1, "type": "open", "path": DOC,
                "text": "difference() { cube(10); #sphere(6); }\necho(\"hi\");\n" }),
    );
    let (v, buffers) = call(
        &mut w,
        json!({ "id": 2, "type": "run", "path": DOC, "mode": "preview" }),
        Vec::new(),
    );
    assert_eq!(v["ok"], true, "{v}");
    let r = &v["result"];
    assert_eq!(r["render"]["exitCode"], 0);
    assert_eq!(r["render"]["echo"], json!(["ECHO: \"hi\""]));
    let s = &r["scene"];
    assert_eq!(s["faces"], json!({ "$buffer": 0 }));
    assert_eq!(s["edges"], json!({ "$buffer": 1 }));
    let packed = render::packed::PackedScene::from_parts(
        buffers[0].clone(),
        buffers[1].clone(),
        s["meta"].as_str().unwrap(),
    )
    .unwrap();
    assert!(packed.face_vertex_count() > 0);
    assert_eq!(packed.edge_segment_count(), 0);
    assert!(!packed.meta.draws.is_empty());
    assert_eq!(packed.meta.bbox.unwrap().1[2], 10.0);
    // The same scene the desktop draws: packing it here gives the bytes.
    let meta: Value = serde_json::from_str(s["meta"].as_str().unwrap()).unwrap();
    assert!(meta["image_csg"].is_array() && meta["edge_color"].is_array());
    let echo = r["console"]
        .as_array()
        .unwrap()
        .iter()
        .find(|l| l["kind"] == "echo")
        .unwrap();
    assert_eq!(echo["text"], "ECHO: \"hi\"");
    assert!(r["render"]["timings"]["totalMs"].as_f64().unwrap() > 0.0);

    // Render mode, a 2D model: outlines as edge segments, and no scene
    // when asked for none.
    ok(
        &mut w,
        json!({ "id": 3, "type": "update", "path": DOC, "text": "square(2);" }),
    );
    let (v, buffers) = call(
        &mut w,
        json!({ "id": 4, "type": "run", "path": DOC, "mode": "render" }),
        Vec::new(),
    );
    assert_eq!(v["result"]["scene"]["edges"], json!({ "$buffer": 1 }));
    assert_eq!(buffers[1].len(), 4 * render::scene::EDGE_SEGMENT_SIZE);
    let r = ok(
        &mut w,
        json!({ "id": 5, "type": "run", "path": DOC, "mode": "render", "scene": false }),
    );
    assert!(r["scene"].is_null());
    assert_eq!(r["render"]["geometry"]["dimensions"], 2);
}

#[test]
fn a_failed_run_is_a_result_with_its_console() {
    let mut w = worker();
    ok(
        &mut w,
        json!({ "id": 1, "type": "open", "path": DOC, "text": "cube(;" }),
    );
    let r = ok(
        &mut w,
        json!({ "id": 2, "type": "run", "path": DOC, "mode": "render" }),
    );
    assert_eq!(r["render"]["exitCode"], 1);
    assert_eq!(r["render"]["diagnostics"][0]["code"], "syntax-error");
    assert!(r["scene"].is_null());
    let line = &r["console"][0];
    assert_eq!(line["kind"], "error");
    assert_eq!(line["location"]["startLine"], 0);
}

/// Edits in editor positions (UTF-16 columns): `é` is two bytes but one
/// unit, so the edit after it lands where the editor meant.
#[test]
fn edits_convert_utf16_positions() {
    let mut w = worker();
    ok(
        &mut w,
        json!({ "id": 1, "type": "open", "path": DOC, "text": "// é\ncube(1);\n" }),
    );
    let d = ok(
        &mut w,
        json!({ "id": 2, "type": "edit", "path": DOC, "edits": [
            { "start": { "line": 0, "character": 4 }, "end": { "line": 0, "character": 4 },
              "text": "!" },
            { "start": { "line": 1, "character": 5 }, "end": { "line": 1, "character": 6 },
              "text": "3" },
        ] }),
    );
    assert_eq!(d["path"], DOC);
    let t = ok(&mut w, json!({ "id": 3, "type": "readFile", "path": DOC }));
    assert_eq!(t["text"], "// é!\ncube(3);\n");
}

#[test]
fn camera_and_file_view() {
    let mut w = worker();
    ok(
        &mut w,
        json!({ "id": 1, "type": "open", "path": DOC,
                "text": "echo($vpd);\n$vpr = [10, 20, 30];\ncube(1);\n" }),
    );
    let r = ok(
        &mut w,
        json!({ "id": 2, "type": "run", "path": DOC, "mode": "preview",
                "camera": { "vpt": [0, 0, 0], "vpr": [55, 0, 25], "vpd": 123, "vpf": 22.5 } }),
    );
    assert_eq!(r["render"]["echo"], json!(["ECHO: 123"]));
    assert_eq!(r["fileView"], json!({ "vpr": [10.0, 20.0, 30.0] }));
}

#[test]
fn parameters_and_overrides() {
    let mut w = worker();
    ok(
        &mut w,
        json!({ "id": 1, "type": "open", "path": DOC,
                "text": "/* [Size] */\nw = 2; // [1:10]\ncube(w);\n" }),
    );
    let p = ok(
        &mut w,
        json!({ "id": 2, "type": "parameters", "path": DOC }),
    );
    assert_eq!(p["groups"][0]["name"], "Size");
    assert_eq!(p["groups"][0]["parameters"][0]["control"]["kind"], "slider");
    let r = ok(
        &mut w,
        json!({ "id": 3, "type": "run", "path": DOC, "mode": "render",
                "overrides": [{ "name": "w", "value": { "kind": "number", "value": 3 } }] }),
    );
    assert!((r["render"]["geometry"]["volume"].as_f64().unwrap() - 27.0).abs() < 1e-9);
}

#[test]
fn check_measure_section_between_and_pick() {
    let mut w = worker();
    ok(
        &mut w,
        json!({ "id": 1, "type": "open", "path": DOC,
                "text": "part(\"a\") cube(10);\npart(\"b\") translate([15, 0, 0]) cube(0.3);\n" }),
    );
    let d = ok(&mut w, json!({ "id": 2, "type": "defaults" }));
    assert_eq!(d["checkOptions"]["nozzle"], 0.4);
    let c = ok(
        &mut w,
        json!({ "id": 3, "type": "check", "path": DOC, "run": { "parts": true } }),
    );
    assert_eq!(c["parts"], json!(["a", "b"]));
    assert!(!c["findings"].as_array().unwrap().is_empty(), "{c}");
    assert_eq!(
        err(
            &mut w,
            json!({ "id": 4, "type": "check", "path": DOC, "options": {
                "nozzle": -1, "minWall": 0.8, "maxOverhang": 45, "bed": null,
                "bedTolerance": 0, "maxFindings": 10 } })
        ),
        "invalidArgument"
    );
    let m = ok(
        &mut w,
        json!({ "id": 5, "type": "measure", "path": DOC, "run": { "parts": true } }),
    );
    let h = m["measurement"].as_u64().unwrap();
    assert_eq!(m["parts"][0]["name"], "a");
    let s = ok(
        &mut w,
        json!({ "id": 6, "type": "section", "measurement": h, "axis": "z", "offset": 5,
                "part": "a" }),
    );
    assert!((s["area"].as_f64().unwrap() - 100.0).abs() < 1e-6);
    let b = ok(
        &mut w,
        json!({ "id": 7, "type": "between", "measurement": h, "a": "a", "b": "b" }),
    );
    assert!((b["distance"].as_f64().unwrap() - 5.0).abs() < 1e-6);
    let p = ok(
        &mut w,
        json!({ "id": 8, "type": "pick", "measurement": h,
                "origin": [5, 5, 50], "direction": [0, 0, -1] }),
    );
    assert!((p["point"][2].as_f64().unwrap() - 10.0).abs() < 1e-9);
    // A new measurement replaces the old one.
    ok(&mut w, json!({ "id": 9, "type": "measure", "path": DOC }));
    assert_eq!(
        err(
            &mut w,
            json!({ "id": 10, "type": "pick", "measurement": h,
                    "origin": [5, 5, 50], "direction": [0, 0, -1] })
        ),
        "invalidArgument"
    );
}

#[test]
fn exports_come_back_as_a_buffer() {
    let mut w = worker();
    ok(
        &mut w,
        json!({ "id": 1, "type": "open", "path": DOC, "text": "cube(1);" }),
    );
    let (v, buffers) = call(
        &mut w,
        json!({ "id": 2, "type": "export", "path": DOC, "format": "binstl" }),
        Vec::new(),
    );
    let r = &v["result"];
    assert_eq!(r["exitCode"], 0, "{v}");
    assert_eq!(r["mime"], "model/stl");
    assert_eq!(r["data"], json!({ "$buffer": 0 }));
    // 80-byte header, a count, 12 triangles of 50 bytes.
    assert_eq!(buffers[0].len(), 84 + 12 * 50);
    assert_eq!(r["bytes"], 684);
    let (v, _) = call(
        &mut w,
        json!({ "id": 3, "type": "export", "path": DOC, "format": "3mf",
                "creationDate": "2026-09-29T12:00:00Z" }),
        Vec::new(),
    );
    assert_eq!(v["result"]["mime"], "model/3mf");
    assert_eq!(
        err(
            &mut w,
            json!({ "id": 4, "type": "export", "path": DOC, "format": "gif" })
        ),
        "invalidArgument"
    );
    // A 2D model to a 3D format fails with the reason, and no data.
    ok(
        &mut w,
        json!({ "id": 5, "type": "update", "path": DOC, "text": "square(1);" }),
    );
    let (v, buffers) = call(
        &mut w,
        json!({ "id": 6, "type": "export", "path": DOC, "format": "stl" }),
        Vec::new(),
    );
    assert_eq!(v["result"]["exitCode"], 1, "{v}");
    assert!(v["result"]["data"].is_null());
    assert!(buffers.is_empty());
}

/// Libraries added as files and as a tar archive are found by `include`,
/// and the bundled MCAD reads back.
#[test]
fn added_files_and_archives_are_libraries() {
    let mut w = worker();
    let mut t = Vec::new();
    let body = b"module thing() cube(2);\n";
    let mut h = vec![0u8; 512];
    h[..14].copy_from_slice(b"LIB/thing.scad");
    h[124..136].copy_from_slice(format!("{:011o}\0", body.len()).as_bytes());
    h[156] = b'0';
    t.extend(h);
    t.extend_from_slice(body);
    t.resize(1024 + 1024, 0);
    let (v, _) = call(
        &mut w,
        json!({ "id": 1, "type": "addFiles", "tar": { "$buffer": 0 },
                "files": [{ "path": "/doc/part.scad", "data": "sphere(1);" }] }),
        vec![t],
    );
    assert_eq!(v["result"]["added"], 2, "{v}");
    ok(
        &mut w,
        json!({ "id": 2, "type": "open", "path": DOC,
                "text": "use <LIB/thing.scad>\ninclude <part.scad>\nthing();\n" }),
    );
    let r = ok(
        &mut w,
        json!({ "id": 3, "type": "run", "path": DOC, "mode": "render", "scene": false }),
    );
    assert_eq!(r["render"]["exitCode"], 0, "{r}");
    assert_eq!(r["render"]["geometry"]["bboxMax"][0], 2.0);
    assert!(
        r["files"]
            .as_array()
            .unwrap()
            .contains(&json!("/neoscad/libraries/LIB/thing.scad")),
        "{r}"
    );
    let m = ok(
        &mut w,
        json!({ "id": 4, "type": "readFile", "path": "/neoscad/libraries/MCAD/units.scad" }),
    );
    assert!(m["text"].as_str().unwrap().contains("mm"));
    assert_eq!(
        err(
            &mut w,
            json!({ "id": 5, "type": "addFiles", "tar": { "$buffer": 3 } })
        ),
        "invalidArgument"
    );
}

/// The language server takes the client's messages, and a run hands it
/// the diagnostics for the text the client sent.
#[test]
fn the_language_server_gets_diagnostics_from_runs() {
    let mut w = worker();
    let lsp = |w: &mut Worker, id: u64, m: Value| -> Vec<Value> {
        let r = ok(
            w,
            json!({ "id": id, "type": "lsp", "message": m.to_string() }),
        );
        r["messages"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| serde_json::from_str(s.as_str().unwrap()).unwrap())
            .collect()
    };
    let init = lsp(
        &mut w,
        1,
        json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize",
                "params": { "capabilities": {} } }),
    );
    assert!(init[0]["result"]["capabilities"].is_object());
    let text = "cube(1);\nfoo();\n";
    ok(
        &mut w,
        json!({ "id": 2, "type": "open", "path": DOC, "text": text }),
    );
    lsp(
        &mut w,
        3,
        json!({ "jsonrpc": "2.0", "method": "textDocument/didOpen", "params": {
            "textDocument": { "uri": "file:///doc/main.scad", "languageId": "openscad",
                              "version": 1, "text": text } } }),
    );
    let r = ok(
        &mut w,
        json!({ "id": 4, "type": "run", "path": DOC, "mode": "preview" }),
    );
    let notes: Vec<Value> = r["language"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| serde_json::from_str(s.as_str().unwrap()).unwrap())
        .collect();
    assert_eq!(notes[0]["method"], "textDocument/publishDiagnostics", "{r}");
    let d = &notes[0]["params"]["diagnostics"][0];
    assert_eq!(d["range"]["start"]["line"], 1, "{d}");
}

#[test]
fn limits_are_set_and_checked() {
    let mut w = worker();
    let mut l = ok(&mut w, json!({ "id": 1, "type": "defaults" }))["limits"].clone();
    l["fragments"] = json!(5);
    let r = ok(&mut w, json!({ "id": 2, "type": "setLimits", "limits": l }));
    assert_eq!(r["limits"]["fragments"], 5);
    l["timeSeconds"] = json!(-1);
    assert_eq!(
        err(&mut w, json!({ "id": 3, "type": "setLimits", "limits": l })),
        "invalidArgument"
    );
    let s = ok(&mut w, json!({ "id": 4, "type": "stats" }));
    assert!(s["memoryBytes"].is_u64());
}

/// The fonts are the page's to add: a run that draws text without them
/// says so (`fontsWanted`), and once they are added under `FONT_DIR` the
/// next run draws the text instead of reusing the result made without it.
/// A model without text never wants them.
#[test]
fn text_without_the_fonts_asks_for_them() {
    let mut w = worker();
    let run = |w: &mut Worker, text: &str| {
        ok(
            w,
            json!({ "id": 2, "type": "open", "path": DOC, "text": text }),
        );
        ok(
            w,
            json!({ "id": 3, "type": "run", "path": DOC, "mode": "render", "scene": false }),
        )
    };
    let plain = run(&mut w, "cube(1);");
    assert_eq!(plain["fontsWanted"], false, "{plain}");
    let bare = run(&mut w, "linear_extrude(1) text(\"NeoSCAD\");");
    assert_eq!(bare["fontsWanted"], true, "{bare}");
    assert_eq!(bare["render"]["geometry"], Value::Null, "{bare}");

    let files: Vec<Value> = assets::FONTS
        .iter()
        .enumerate()
        .map(|(i, (name, _))| json!({ "path": format!("{FONT_DIR}/{name}"), "data": { "$buffer": i } }))
        .collect();
    let buffers = assets::FONTS.iter().map(|(_, d)| d.to_vec()).collect();
    let (v, _) = call(
        &mut w,
        json!({ "id": 4, "type": "addFiles", "files": files }),
        buffers,
    );
    assert_eq!(v["result"]["added"], assets::FONTS.len(), "{v}");

    let drawn = run(&mut w, "linear_extrude(1) text(\"NeoSCAD\");");
    assert_eq!(drawn["fontsWanted"], false, "{drawn}");
    assert!(
        drawn["render"]["geometry"]["triangles"].as_u64().unwrap() > 100,
        "{drawn}"
    );
}
