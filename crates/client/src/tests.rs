//! The glue on its own, over an in-memory session. The app's behaviour is
//! tested through it in `crates/ffi` (and the Swift tests), the worker's
//! in `crates/web`.

use std::path::PathBuf;
use std::sync::Arc;

use lang::loader::{FileSystem, LibraryPath};
use lang::vfs::MemFs;
use serde_json::json;

use super::*;

const DOC: &str = "/doc/main.scad";

fn client() -> Client {
    let files: Arc<dyn FileSystem + Send + Sync> = Arc::new(MemFs::new());
    let fs: Arc<dyn FileSystem + Send + Sync> =
        Arc::new(assets::libraries(files, "/neoscad/libraries"));
    let mut cfg = session::Config::new(fs, LibraryPath(vec![PathBuf::from("/neoscad/libraries")]));
    cfg.limits = session::Limits::AGENT;
    Client::new(cfg)
}

#[test]
fn numbers_print_as_cpp_streams_do() {
    assert_eq!(g(43.0, 16), "43");
    assert_eq!(g(0.1, 16), "0.1");
    assert_eq!(g(1.0 / 3.0, 16), "0.3333333333333333");
    assert_eq!(g(1.0 / 3.0, 6), "0.333333");
    assert_eq!(g(1e21, 16), "1e+21");
    assert_eq!(g(-2.5e-7, 6), "-2.5e-07");
    assert_eq!(
        literal(&ParameterValue::Number { value: 0.1 }).unwrap(),
        "0.1"
    );
    assert_eq!(
        literal(&ParameterValue::Vector {
            value: vec![1.0, 2.5]
        })
        .unwrap(),
        "[1, 2.5]"
    );
    assert!(literal(&ParameterValue::Number { value: f64::NAN }).is_none());
}

#[test]
fn relative_paths_are_refused() {
    let c = client();
    assert!(matches!(
        c.open("main.scad", Some("cube();".into())),
        Err(CoreError::InvalidArgument { .. })
    ));
}

/// A warning's console line points at its place in UTF-16 columns: the
/// `é` before it is two bytes but one unit.
#[test]
fn console_lines_point_in_editor_positions() {
    let c = client();
    c.open(DOC, Some("x = \"é\"; echo(x);\ncube(y);\n".into()))
        .unwrap();
    let req = DocumentRequest {
        mode: RenderMode::Render,
        overrides: Vec::new(),
        parts: false,
        enable: Vec::new(),
    };
    let (run, doc, text) = c.document_run(DOC, &req).unwrap();
    let scheme = render::ColorScheme::cornfield();
    let r = c.session.render(&run, req.mode.into(), &scheme).unwrap();
    let lines = c.console_lines(&r.log, &doc, text);
    let echo = lines.iter().find(|l| l.kind == ConsoleKind::Echo).unwrap();
    assert_eq!(echo.text, "ECHO: \"é\"");
    let warning = lines
        .iter()
        .find(|l| l.kind == ConsoleKind::Warning)
        .expect("an unknown variable warns");
    let at = warning.location.as_ref().expect("located");
    assert_eq!(at.path, DOC);
    assert_eq!(at.start_line, 1);
}

#[test]
fn overrides_run_as_assignments_and_bad_ones_are_dropped() {
    let c = client();
    c.open(DOC, Some("w = 1; // [1:10]\ncube(w);\n".into()))
        .unwrap();
    let req = DocumentRequest {
        mode: RenderMode::Render,
        overrides: vec![
            ParameterOverride {
                name: "w".into(),
                value: ParameterValue::Number { value: 3.0 },
            },
            ParameterOverride {
                name: "w; sphere()".into(),
                value: ParameterValue::Number { value: 1.0 },
            },
        ],
        parts: false,
        enable: Vec::new(),
    };
    let (run, _, _) = c.document_run(DOC, &req).unwrap();
    assert_eq!(run.defines, vec!["w=3".to_string()]);
    let scheme = render::ColorScheme::cornfield();
    let r = c.session.render(&run, req.mode.into(), &scheme).unwrap();
    let g = render_result(&r, &scheme).geometry.unwrap();
    assert!((g.volume.unwrap() - 27.0).abs() < 1e-9);
}

/// The JSON shapes `docs/web-protocol.md` promises.
#[test]
fn records_serialise_as_the_web_protocol_says() {
    let c = client();
    c.open(
        DOC,
        Some("/* [Size] */\nw = 2; // [1:10]\nlabel = \"a\";\ncube(w);\n".into()),
    )
    .unwrap();
    let groups = serde_json::to_value(c.parameters(DOC).unwrap()).unwrap();
    assert_eq!(
        groups[0]["parameters"][0],
        json!({
            "name": "w",
            "description": "",
            "control": { "kind": "slider", "min": 1.0, "max": 10.0, "step": null },
            "defaultValue": { "kind": "number", "value": 2.0 },
        })
    );
    assert_eq!(
        groups[0]["parameters"][1]["control"],
        json!({ "kind": "text", "maxLength": null })
    );
    let r = serde_json::to_value(c.render(DOC, RenderMode::Render).unwrap()).unwrap();
    assert_eq!(r["exitCode"], 0);
    assert_eq!(r["geometry"]["bboxMax"], json!([2.0, 2.0, 2.0]));
    assert!(r["timings"]["totalMs"].is_number());
    let e = serde_json::to_value(CoreError::InvalidArgument {
        message: "x".into(),
    })
    .unwrap();
    assert_eq!(e, json!({ "kind": "invalidArgument", "message": "x" }));
    let o: RunOptions = serde_json::from_value(json!({ "parts": true })).unwrap();
    assert!(o.parts && o.overrides.is_empty());
    let limits = serde_json::to_value(c.limits()).unwrap();
    assert_eq!(limits["fragments"], 10_000);
}

#[test]
fn measure_sections_and_picks_a_cube() {
    let c = client();
    c.open(DOC, Some("cube(10);".into())).unwrap();
    let run = c.detached(DOC, &RunOptions::default(), None, None).unwrap();
    let (report, m) = c.measure(run).unwrap();
    assert!((report.model.unwrap().volume - 1000.0).abs() < 1e-6);
    let m = m.unwrap();
    let s = m.section(SectionAxis::Z, 5.0, None).unwrap();
    assert!((s.area - 100.0).abs() < 1e-6);
    let p = m
        .pick(&[5.0, 5.0, 50.0], &[0.0, 0.0, -1.0])
        .unwrap()
        .unwrap();
    assert!((p[2] - 10.0).abs() < 1e-9);
}

/// An export's bytes go to the host's sink; `bytes` is the host's to fill.
#[test]
fn exports_go_to_the_sink() {
    struct Keep(Vec<u8>);
    impl session::ExportSink for Keep {
        fn write(&mut self, _: &str, data: &[u8]) -> Result<(), String> {
            self.0 = data.to_vec();
            Ok(())
        }
        fn summary(
            &mut self,
            _: &session::SummaryFacts<'_>,
            _: &mut eval::Console<Vec<u8>>,
        ) -> bool {
            true
        }
    }
    let c = client();
    c.open(DOC, Some("cube(1);".into())).unwrap();
    let run = c.detached(DOC, &RunOptions::default(), None, None).unwrap();
    let fmt = export_format(None, "/doc/main.stl").unwrap();
    let mut sink = Keep(Vec::new());
    let r = c
        .export(
            run,
            "/doc/main.stl",
            fmt,
            &ExportOptions::default(),
            "2026-01-01T00:00:00Z".into(),
            &mut sink,
        )
        .unwrap();
    assert_eq!(r.exit_code, 0, "{}", r.console);
    assert!(sink.0.starts_with(b"solid "));
    assert!(export_format(Some("nope"), "x").is_err());
}
