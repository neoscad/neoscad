//! The bridge's Rust side: each export's result and its errors. The Swift
//! tests (`apple/Tests`) repeat the essentials through the generated
//! bindings.

use super::*;

fn core() -> Arc<Core> {
    Core::new(CoreConfig {
        resource_dir: None,
        test_hooks: true,
    })
    .unwrap()
}

/// A document path that exists nowhere on disk: its text lives only in
/// the session's buffer.
const DOC: &str = "/NeoSCAD-ffi-test/model.scad";

fn with_text(c: &Core, text: &str) {
    c.open(DOC.into(), Some(text.into())).unwrap();
}

#[test]
fn renders_a_cube_with_its_statistics() {
    let c = core();
    with_text(&c, "cube(10);");
    let r = c.render(DOC.into(), RenderMode::Render).unwrap();
    assert_eq!(r.exit_code, 0, "{}", r.console);
    let g = r.geometry.unwrap();
    assert_eq!(g.dimensions, 3);
    assert!((g.volume.unwrap() - 1000.0).abs() < 1e-9);
    assert_eq!(g.bbox_max, vec![10.0, 10.0, 10.0]);
    assert_eq!(g.triangles, Some(12));
    assert_eq!(g.manifold, Some(true));
}

#[test]
fn a_syntax_error_has_a_line_and_column() {
    let c = core();
    with_text(&c, "cube(10);\ncube(;\n");
    let r = c.evaluate(DOC.into()).unwrap();
    assert_ne!(r.exit_code, 0);
    let d = &r.diagnostics[0];
    assert_eq!(d.code, "syntax-error");
    assert_eq!(d.severity, Severity::Error);
    assert_eq!(d.line, Some(2));
    let span = d.span.unwrap();
    assert_eq!((span.start_line, span.start_column), (2, 6));
    assert!(r.console.contains("Parser error"), "{}", r.console);
}

#[test]
fn edits_apply_to_the_buffer() {
    let c = core();
    with_text(&c, "cube(10);");
    let d = c
        .edit(
            DOC.into(),
            vec![TextEdit {
                start: 5,
                end: 7,
                text: "2".into(),
            }],
        )
        .unwrap();
    assert_eq!(d.version, 2);
    let r = c.render(DOC.into(), RenderMode::Render).unwrap();
    assert!((r.geometry.unwrap().volume.unwrap() - 8.0).abs() < 1e-9);
    let bad = c.edit(
        DOC.into(),
        vec![TextEdit {
            start: 3,
            end: 99,
            text: String::new(),
        }],
    );
    assert!(matches!(bad, Err(CoreError::InvalidArgument { .. })));
    assert!(c.close(DOC.into()).unwrap());
}

#[test]
fn relative_paths_are_refused() {
    let c = core();
    assert!(matches!(
        c.open("model.scad".into(), Some("cube(1);".into())),
        Err(CoreError::InvalidArgument { .. })
    ));
}

#[test]
fn a_panic_is_an_error_and_the_core_survives() {
    let c = core();
    match c.debug_panic() {
        Err(CoreError::Panicked { message }) => assert!(message.contains("debug_panic")),
        other => panic!("expected Panicked, got {other:?}"),
    }
    with_text(&c, "cube(1);");
    assert_eq!(
        c.render(DOC.into(), RenderMode::Render).unwrap().exit_code,
        0
    );
    let plain = Core::new(CoreConfig::default()).unwrap();
    assert!(matches!(
        plain.debug_panic(),
        Err(CoreError::InvalidArgument { .. })
    ));
}

#[test]
fn limits_start_at_the_agent_defaults_and_can_be_lowered() {
    let c = core();
    assert_eq!(c.limits().unwrap(), default_limits().unwrap());
    // The agent defaults already refuse a runaway `$fn`.
    with_text(&c, "sphere(r=1, $fn=1e6);");
    let r = c.render(DOC.into(), RenderMode::Render).unwrap();
    assert_ne!(r.exit_code, 0);
    assert_eq!(r.diagnostics[0].code, "resource-limit", "{}", r.console);
    // A model within them fails once the limit is lowered below it.
    with_text(&c, "sphere(r=1, $fn=64);");
    assert_eq!(
        c.render(DOC.into(), RenderMode::Render).unwrap().exit_code,
        0
    );
    let mut low = c.limits().unwrap();
    low.fragments = Some(32);
    c.set_limits(low).unwrap();
    // A new model, not the one just rendered: cached geometry is reused
    // without re-checking limits (it already exists).
    with_text(&c, "sphere(r=1, $fn=48);");
    let r = c.render(DOC.into(), RenderMode::Render).unwrap();
    assert_eq!(r.diagnostics[0].code, "resource-limit", "{}", r.console);
    low.time_seconds = Some(0.0);
    assert!(matches!(
        c.set_limits(low),
        Err(CoreError::InvalidArgument { .. })
    ));
}

/// The record carries the counted depth limit: `None` is the default, a
/// number stops a recursion that deep with OpenSCAD's error, 0 is refused.
#[test]
fn the_depth_limit_reaches_the_evaluator() {
    let c = core();
    assert_eq!(c.limits().unwrap().depth, None);
    with_text(
        &c,
        "module m(n) { if (n > 0) m(n - 1); else cube(1); }\nm(50);\n",
    );
    let r = c.render(DOC.into(), RenderMode::Render).unwrap();
    assert!(!r.console.contains("Recursion detected"), "{}", r.console);
    let mut low = c.limits().unwrap();
    low.depth = Some(20);
    c.set_limits(low).unwrap();
    assert_eq!(c.limits().unwrap().depth, Some(20));
    with_text(
        &c,
        "module m(n) { if (n > 0) m(n - 1); else sphere(1); }\nm(50);\n",
    );
    let r = c.render(DOC.into(), RenderMode::Render).unwrap();
    assert!(r.console.contains("Recursion detected"), "{}", r.console);
    low.depth = Some(0);
    assert!(matches!(
        c.set_limits(low),
        Err(CoreError::InvalidArgument { .. })
    ));
}

#[test]
fn snapshots_are_png() {
    let c = core();
    with_text(&c, "cube(10);");
    let s = match c.snapshot(
        DOC.into(),
        SnapshotOptions {
            width: 256,
            height: 256,
            views: vec![],
            dims: false,
            preview: false,
        },
    ) {
        Ok(s) => s,
        // A machine without a GPU (CI) cannot draw; everything else is
        // covered by the other tests.
        Err(CoreError::Failed { message }) => {
            eprintln!("skipped: {message}");
            return;
        }
        Err(e) => panic!("{e}"),
    };
    assert_eq!(s.exit_code, 0);
    assert!(s.png.unwrap().starts_with(b"\x89PNG\r\n\x1a\n"));
    let summary: serde_json::Value = serde_json::from_str(&s.summary_json).unwrap();
    assert!(summary.is_object());
}

#[test]
fn exports_write_the_file() {
    let c = core();
    with_text(&c, "cube(10);");
    let dir = std::env::temp_dir().join(format!("neoscad-ffi-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let out = dir.join("cube.stl");
    let r = c
        .export(DOC.into(), out.to_string_lossy().into_owned(), None)
        .unwrap();
    assert_eq!(r.exit_code, 0, "{}", r.console);
    assert_eq!(r.format, "stl");
    let written = std::fs::read(&out).unwrap();
    assert_eq!(written.len() as u64, r.bytes);
    assert!(written.starts_with(b"solid"));
    assert!(matches!(
        c.export(
            DOC.into(),
            out.to_string_lossy().into_owned(),
            Some("xyz".into())
        ),
        Err(CoreError::InvalidArgument { .. })
    ));
    std::fs::remove_dir_all(&dir).ok();
}

/// The editor's language server over the core: its own copy of the
/// document (the session's buffer is the app's), diagnostics of the exact
/// version it was sent, and the bundled MCAD reachable for hover and
/// definition although it exists only in memory.
#[test]
fn language_server_over_the_core() {
    let c = core();
    // The app's copy says one thing; the server's client another.
    with_text(&c, "cube(1);\n");
    let ls = c.clone().language_server(false).unwrap();
    let send = |m: serde_json::Value| -> Vec<serde_json::Value> {
        ls.handle(m.to_string())
            .unwrap()
            .iter()
            .map(|s| serde_json::from_str(s).unwrap())
            .collect()
    };
    let uri = format!("file://{DOC}");
    let r = send(
        serde_json::json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"capabilities": {}}}),
    );
    assert!(
        r[0]["result"]["capabilities"]["hoverProvider"]
            .as_bool()
            .unwrap()
    );
    let text = "include <MCAD/units.scad>\nsphre(r = mm);\n";
    send(
        serde_json::json!({"jsonrpc": "2.0", "method": "textDocument/didOpen", "params": {
        "textDocument": {"uri": uri, "languageId": "openscad", "version": 7, "text": text}}}),
    );
    // The app's copy is untouched.
    assert_eq!(c.read_file(DOC.into()).unwrap(), "cube(1);\n");
    assert!(ls.diagnostics_pending().unwrap());
    let pubs: Vec<serde_json::Value> = ls
        .publish_diagnostics()
        .unwrap()
        .iter()
        .map(|s| serde_json::from_str(s).unwrap())
        .collect();
    let p = &pubs[0]["params"];
    assert_eq!(p["version"], 7);
    assert_eq!(p["diagnostics"][0]["code"], "unknown-module");
    assert_eq!(
        p["diagnostics"][0]["data"]["fixes"][0]["edits"][0]["newText"],
        "sphere"
    );
    // Definition into MCAD, whose text the core serves for the viewer.
    let r = send(
        serde_json::json!({"jsonrpc": "2.0", "id": 2, "method": "textDocument/definition", "params": {
        "textDocument": {"uri": uri}, "position": {"line": 1, "character": 11}}}),
    );
    let target = r[0]["result"]["uri"].as_str().unwrap().to_string();
    assert!(target.ends_with("/libraries/MCAD/units.scad"), "{target}");
    let path = target.strip_prefix("file://").unwrap();
    assert!(c.read_file(path.into()).unwrap().contains("mm = 1;"));
    assert!(
        c.library_dirs()
            .unwrap()
            .iter()
            .any(|d| path.starts_with(d.as_str()))
    );
}

/// Every entry point that parses runs on a thread with the evaluator's
/// stack, so the app may call it from a dispatch queue (512 KiB) with a
/// document nested as deep as the parser allows: the customizer, the
/// document run with its markers, and the language server's handling and
/// publishing; and freeing the language servers and the core, which hold
/// the parsed document ([`FreedDeep`]). Each of these overflowed the
/// caller's stack in a test build before it had a thread of its own.
#[test]
fn deep_documents_on_a_dispatch_queues_stack() {
    let n = lang::syntax::parser::NESTING_LIMIT as usize - 10;
    let text = format!(
        "w = 2; // [1:10]\n{}cube(w);\n",
        "translate([0, 0, 1]) ".repeat(n)
    );
    let c = core();
    with_text(&c, &text);
    let uri = format!("file://{DOC}");
    let open = |ls: &LanguageServer| {
        for m in [
            serde_json::json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"capabilities": {}}}),
            serde_json::json!({"jsonrpc": "2.0", "method": "textDocument/didOpen", "params": {
                "textDocument": {"uri": uri, "languageId": "openscad", "version": 1, "text": text}}}),
        ] {
            ls.handle(m.to_string()).unwrap();
        }
    };
    fn small<T: Send>(f: impl FnOnce() -> T + Send) -> T {
        std::thread::scope(|s| {
            std::thread::Builder::new()
                .stack_size(512 << 10)
                .spawn_scoped(s, f)
                .unwrap()
                .join()
                .unwrap()
        })
    }
    small(|| {
        let groups = c.parameters(DOC.into()).unwrap();
        assert_eq!(groups[0].parameters[0].name, "w");
    });
    // The server evaluates for its markers.
    let ls = c.clone().language_server(false).unwrap();
    small(|| {
        open(&ls);
        let pubs = ls.publish_diagnostics().unwrap();
        assert!(pubs[0].contains("\"version\":1"), "{pubs:?}");
        drop(ls);
    });
    // The document run hands its diagnostics to the server.
    let ls = c.clone().language_server(true).unwrap();
    small(|| {
        open(&ls);
        let request = DocumentRequest {
            mode: RenderMode::Preview,
            overrides: Vec::new(),
            parts: false,
            enable: Vec::new(),
        };
        let r = c
            .run_document(DOC.into(), request, None, Some(ls.clone()), None)
            .unwrap();
        assert_eq!(r.render.exit_code, 0, "{:?}", r.console);
        assert!(r.language[0].contains("\"version\":1"), "{:?}", r.language);
        drop(ls);
    });
    small(move || drop(c));
}
