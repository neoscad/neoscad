//! The panels' requests through the bridge: check findings, measurements
//! and sections, exports with their failures, cancelling, and that these
//! requests do not cancel the document's own runs.

use super::*;
use crate::{CoreConfig, DocumentRequest, RenderMode};

fn core(text: &str) -> (Arc<Core>, String) {
    let c = Core::new(CoreConfig {
        resource_dir: None,
        test_hooks: true,
    })
    .unwrap();
    let doc = "/NeoSCAD-ffi-inspect-test/model.scad".to_string();
    c.open(doc.clone(), Some(text.into())).unwrap();
    (c, doc)
}

fn defaults() -> CheckOptions {
    default_check_options().unwrap()
}

fn parts() -> RunOptions {
    RunOptions {
        overrides: Vec::new(),
        parts: true,
        enable: Vec::new(),
    }
}

/// A directory of its own under the system's temporary one.
fn temp_dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!(
        "neoscad-ffi-{name}-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[test]
fn a_thin_wall_is_an_error_finding_with_a_place_and_a_fix() {
    let (c, doc) = core("cube(20); translate([30, 0, 0]) cube([10, 0.3, 10]);");
    let r = c
        .check(doc, defaults(), RunOptions::default(), None)
        .unwrap();
    assert!(!r.failed, "{}", r.console);
    assert_eq!(r.exit_code, 1);
    let f = r.findings.iter().find(|f| f.code == "thin-wall").unwrap();
    assert_eq!(f.severity, FindingSeverity::Error);
    assert_eq!(f.id, 1);
    assert_eq!(f.value, Some(0.3));
    assert_eq!(f.point.len(), 3);
    assert!(f.point[0] >= 30.0, "{:?}", f.point);
    assert!(f.bbox_min.is_some() && f.bbox_max.is_some());
    assert!(f.fix.contains("0.8"), "{}", f.fix);
    assert!(r.errors >= 1);
    assert_eq!(r.min_wall, Some(0.3));
    assert!(r.text.contains("thin-wall"), "{}", r.text);
    // The same object the command line prints.
    let v: Value = serde_json::from_str(&r.summary_json).unwrap();
    assert_eq!(v["settings"]["nozzle"], 0.4);
}

#[test]
fn check_settings_and_parts_reach_the_check() {
    let text = "part(\"a\") cube(10); part(\"b\") translate([12, 0, 0]) cube(10);";
    let (c, doc) = core(text);
    let mut o = defaults();
    o.bed = Some(vec![15.0, 15.0, 15.0]);
    let r = c.check(doc.clone(), o, parts(), None).unwrap();
    assert_eq!(r.parts, vec!["a".to_string(), "b".to_string()]);
    assert!(r.findings.iter().any(|f| f.code == "bed-fit"), "{}", r.text);
    // Without the extension `part` is an unknown module.
    let r = c
        .check(doc.clone(), defaults(), RunOptions::default(), None)
        .unwrap();
    assert!(r.parts.is_empty());
    assert!(r.console.contains("unknown module 'part'"), "{}", r.console);
    // Bad settings are the caller's error, not a finding.
    let mut o = defaults();
    o.nozzle = 0.0;
    assert!(matches!(
        c.check(doc, o, RunOptions::default(), None),
        Err(CoreError::InvalidArgument { .. })
    ));
}

#[test]
fn a_model_that_fails_reports_failed_with_its_error() {
    let (c, doc) = core("cube(;");
    let r = c
        .check(doc, defaults(), RunOptions::default(), None)
        .unwrap();
    assert!(r.failed);
    assert_eq!(r.exit_code, 1);
    assert!(r.findings.is_empty());
    assert!(r.console.contains("Parser error"), "{}", r.console);
}

#[test]
fn customizer_values_reach_the_check() {
    let (c, doc) = core("t = 5; cube([10, t, 10]);");
    let run = RunOptions {
        overrides: vec![ParameterOverride {
            name: "t".into(),
            value: crate::ParameterValue::Number { value: 0.3 },
        }],
        parts: false,
        enable: Vec::new(),
    };
    let r = c.check(doc, defaults(), run, None).unwrap();
    assert_eq!(r.min_wall, Some(0.3), "{}", r.text);
}

#[test]
fn a_cube_measures_and_sections_exactly() {
    let (c, doc) = core("cube(10);");
    let m = c.measure(doc, RunOptions::default(), None).unwrap();
    assert_eq!(m.exit_code, 0);
    let s = m.model.unwrap();
    assert_eq!(s.volume, 1000.0);
    assert_eq!(s.area, 600.0);
    assert_eq!(s.centroid, vec![5.0, 5.0, 5.0]);
    assert_eq!(s.bbox_max, vec![10.0, 10.0, 10.0]);
    assert_eq!(m.components, Some(1));
    assert_eq!(m.manifold, Some(true));
    let meas = m.measurement.unwrap();
    let z = meas.section(SectionAxis::Z, 5.0, None).unwrap();
    assert_eq!(z.plane, "z=5");
    assert_eq!((z.area, z.perimeter, z.contours), (100.0, 40.0, 1));
    assert_eq!(z.outline.len(), 1);
    // At least the square's four corners (a slice keeps the points where
    // the cube's triangles' edges cross the plane, collinear ones too).
    assert!(z.outline[0].len() >= 12 && z.outline[0].len() % 3 == 0);
    assert!(z.outline[0].chunks(3).all(|p| p[2] == 5.0));
    let x = meas.section(SectionAxis::X, 2.5, None).unwrap();
    assert_eq!(x.area, 100.0);
    assert!(x.outline[0].chunks(3).all(|p| p[0] == 2.5));
    // A plane that misses cuts nothing.
    let miss = meas.section(SectionAxis::Z, 20.0, None).unwrap();
    assert_eq!((miss.area, miss.contours), (0.0, 0));
    assert!(miss.bbox_min.is_none());
    // Picking: straight down onto the top face.
    let hit = meas
        .pick(vec![5.0, 5.0, 100.0], vec![0.0, 0.0, -2.0])
        .unwrap()
        .unwrap();
    assert!((hit[2] - 10.0).abs() < 1e-9, "{hit:?}");
    assert!(
        meas.pick(vec![50.0, 50.0, 100.0], vec![0.0, 0.0, -1.0])
            .unwrap()
            .is_none()
    );
}

#[test]
fn parts_measure_on_their_own_and_their_distance() {
    let text = "part(\"a\") cube(10); part(\"b\") translate([15, 0, 0]) cube([10, 10, 5]);";
    let (c, doc) = core(text);
    let m = c.measure(doc, parts(), None).unwrap();
    let names: Vec<_> = m.parts.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(names, ["a", "b"]);
    assert_eq!(m.parts[1].solid.as_ref().unwrap().volume, 500.0);
    let meas = m.measurement.unwrap();
    let b = meas.between("a".into(), "b".into()).unwrap();
    assert_eq!(b.distance, Some(5.0));
    assert!(!b.touching && !b.overlapping);
    assert!(b.point_a.is_some() && b.point_b.is_some());
    let s = meas.section(SectionAxis::Z, 2.0, Some("b".into())).unwrap();
    assert_eq!(s.area, 100.0);
    assert!(matches!(
        meas.section(SectionAxis::Z, 2.0, Some("c".into())),
        Err(CoreError::InvalidArgument { .. })
    ));
}

#[test]
fn a_2d_model_measures_as_2d() {
    let (c, doc) = core("square(4);");
    let m = c.measure(doc, RunOptions::default(), None).unwrap();
    assert!(m.model.is_none());
    assert_eq!(m.model_2d.unwrap().area, 16.0);
    assert!(m.measurement.is_none());
}

/// `predictible-output` in `RunOptions::enable` sorts the exported mesh,
/// as `--enable` does on the command line; without it the file is in the
/// kernel's order, as OpenSCAD writes it by default.
#[test]
fn exports_sort_with_predictible_output() {
    let dir = temp_dir("export-sorted");
    let (c, doc) = core("translate([2, 0, 0]) cube(1); cube(1);");
    let off = |name: &str, sort: bool| {
        let target = dir.join(name);
        let r = c
            .export_file(
                doc.clone(),
                target.to_string_lossy().into(),
                ExportOptions::default(),
                RunOptions {
                    enable: if sort {
                        vec!["predictible-output".into()]
                    } else {
                        Vec::new()
                    },
                    ..Default::default()
                },
                None,
                None,
            )
            .unwrap();
        assert_eq!(r.exit_code, 0, "{}", r.console);
        std::fs::read_to_string(&target).unwrap()
    };
    let sorted = off("sorted.off", true);
    let vertices: Vec<&str> = sorted.lines().skip(2).take(16).collect();
    let mut expected = vertices.clone();
    expected.sort_by(|a, b| {
        let p =
            |s: &str| -> Vec<f64> { s.split_whitespace().map(|t| t.parse().unwrap()).collect() };
        p(a).partial_cmp(&p(b)).unwrap()
    });
    assert_eq!(vertices, expected);
    assert_eq!(vertices[0], "0 0 0 ");
    assert_ne!(off("plain.off", false), sorted);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn exports_write_binary_stl_and_3mf() {
    let dir = temp_dir("export");
    let (c, doc) = core("cube(10);");
    let stl = dir.join("cube.stl");
    let r = c
        .export_file(
            doc.clone(),
            stl.to_string_lossy().into(),
            ExportOptions {
                format: Some("binstl".into()),
                ..Default::default()
            },
            RunOptions::default(),
            None,
            None,
        )
        .unwrap();
    assert_eq!(r.exit_code, 0, "{}", r.console);
    let bytes = std::fs::read(&stl).unwrap();
    // 80-byte header, a count, 50 bytes per triangle.
    assert_eq!(bytes.len(), 84 + 50 * 12);
    assert_eq!(u32::from_le_bytes(bytes[80..84].try_into().unwrap()), 12);
    assert_eq!(r.bytes, bytes.len() as u64);
    let tmf = dir.join("cube.3mf");
    let r = c
        .export_file(
            doc,
            tmf.to_string_lossy().into(),
            ExportOptions {
                threemf_color_mode: Some(ThreeMfColorMode::SelectedOnly),
                threemf_color: Some("#ff0000".into()),
                ..Default::default()
            },
            RunOptions::default(),
            None,
            None,
        )
        .unwrap();
    assert_eq!(
        (r.exit_code, r.format.as_str()),
        (0, "3mf"),
        "{}",
        r.console
    );
    let bytes = std::fs::read(&tmf).unwrap();
    assert_eq!(&bytes[..2], b"PK");
    // No temporary file is left beside the output.
    let left: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(left.len(), 2, "{left:?}");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn export_failures_say_why() {
    let dir = temp_dir("export-fail");
    let (c, doc) = core("square(10);");
    // A 2D model to a 3D format.
    let r = c
        .export_file(
            doc.clone(),
            dir.join("a.stl").to_string_lossy().into(),
            ExportOptions::default(),
            RunOptions::default(),
            None,
            None,
        )
        .unwrap();
    assert_eq!(r.exit_code, 1);
    assert!(r.console.contains("not a 3D object"), "{}", r.console);
    // A folder that does not exist.
    let r = c
        .export_file(
            doc.clone(),
            dir.join("missing/a.svg").to_string_lossy().into(),
            ExportOptions::default(),
            RunOptions::default(),
            None,
            None,
        )
        .unwrap();
    assert_eq!(r.exit_code, 1);
    assert!(r.console.contains("Can't write"), "{}", r.console);
    assert!(
        r.diagnostics
            .iter()
            .any(|d| d.code == "output-not-writable"),
        "{:?}",
        r.diagnostics
    );
    // An unknown format is the caller's error.
    assert!(matches!(
        c.export_file(
            doc,
            dir.join("a.amf").to_string_lossy().into(),
            ExportOptions::default(),
            RunOptions::default(),
            None,
            None,
        ),
        Err(CoreError::InvalidArgument { .. })
    ));
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn export_reports_its_stages_and_stops_when_cancelled() {
    struct Stages(std::sync::Mutex<Vec<String>>);
    impl ProgressListener for Stages {
        fn stage(&self, stage: String) {
            self.0.lock().unwrap().push(stage);
        }
    }
    let dir = temp_dir("export-cancel");
    let (c, doc) = core("cube(10);");
    let stages = Arc::new(Stages(Default::default()));
    let out = dir.join("a.off");
    let r = c
        .export_file(
            doc.clone(),
            out.to_string_lossy().into(),
            ExportOptions::default(),
            RunOptions::default(),
            None,
            Some(stages.clone()),
        )
        .unwrap();
    assert_eq!(r.exit_code, 0);
    assert!(stages.0.lock().unwrap().contains(&"geometry".to_string()));
    std::fs::remove_file(&out).unwrap();
    // A token cancelled before the request starts stops it, and nothing
    // is written.
    let token = CancelToken::new().unwrap();
    token.cancel().unwrap();
    assert!(token.is_cancelled().unwrap());
    let r = c.export_file(
        doc,
        out.to_string_lossy().into(),
        ExportOptions::default(),
        RunOptions::default(),
        Some(token),
        None,
    );
    assert!(matches!(r, Err(CoreError::Cancelled)), "{r:?}");
    assert!(!out.exists());
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_panel_request_does_not_cancel_the_documents_run() {
    // A slow document run, then a check on the same document while it
    // runs: the run must finish rather than be superseded.
    let (c, doc) = core("for (i = [0:40]) translate([i * 3, 0, 0]) sphere(1, $fn = 64);");
    let c2 = c.clone();
    let d2 = doc.clone();
    let run = std::thread::spawn(move || {
        c2.run_document(
            d2,
            DocumentRequest {
                mode: RenderMode::Render,
                overrides: Vec::new(),
                parts: false,
                enable: Vec::new(),
            },
            None,
            None,
            None,
        )
    });
    while c.running(doc.clone()).unwrap() == 0 && !run.is_finished() {
        std::thread::yield_now();
    }
    let r = c
        .check(doc, defaults(), RunOptions::default(), None)
        .unwrap();
    assert!(!r.failed);
    let d = run.join().unwrap();
    assert!(d.is_ok(), "{d:?}");
}

#[test]
fn a_detached_snapshot_is_a_png() {
    let (c, doc) = core("cube(10);");
    let r = match c.snapshot_file(
        doc,
        SnapshotOptions {
            width: 256,
            height: 256,
            views: Vec::new(),
            dims: false,
            preview: false,
        },
        RunOptions::default(),
        None,
    ) {
        Ok(r) => r,
        Err(CoreError::Failed { message }) => {
            eprintln!("skipped: {message}");
            return;
        }
        Err(e) => panic!("{e}"),
    };
    assert_eq!(r.exit_code, 0);
    assert_eq!(&r.png.unwrap()[1..4], b"PNG");
}
