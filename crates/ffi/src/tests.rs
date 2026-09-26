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
