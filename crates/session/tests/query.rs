//! Render-free queries (`--enable query`, docs/language-extensions.md
//! section 5.3): the golden models in `conformance/extensions/query`
//! (their console output and `.csg`), the names with the flag off and
//! their JSON hints, the `.csg` export as plain OpenSCAD, and a warm
//! session's export equal to a cold one. (That anchors and queries change
//! no output is in `crates/eval/tests/query.rs`; the thread counts in
//! `crates/geom/tests/query.rs`.)
//!
//! The goldens are rewritten with `NEOSCAD_BLESS=1`, for a change meant to
//! alter them.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use lang::loader::LibraryPath;
use lang::vfs::MemFs;
use session::export::{Format, Settings};
use session::{Config, ExportRequest, ExportSink, Mode, Run, Session};

fn goldens() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../conformance/extensions/query")
}

fn session(files: &[(&str, &[u8])]) -> (Session, Arc<MemFs>) {
    let fs = Arc::new(MemFs::new());
    for (p, t) in files {
        fs.insert(format!("/doc/{p}"), t.to_vec());
    }
    let mut cfg = Config::new(fs.clone(), LibraryPath(Vec::new()));
    cfg.work_dir = PathBuf::from("/doc");
    // Bounded like an agent's calls.
    cfg.limits = session::Limits::AGENT;
    (Session::new(cfg), fs)
}

/// A run with the queries on (and sketches, for `sketch.scad`), or with
/// no extension at all.
fn run(path: &str, on: bool) -> Run {
    let mut run = Run::new(path);
    if on {
        run.extensions = eval::Extensions::NONE
            .with(eval::Extension::Query)
            .with(eval::Extension::Sketch);
    }
    run
}

/// What the command line prints and the `.csg` it writes.
fn evaluate(s: &Session, path: &str, on: bool) -> (String, String) {
    let r = s.evaluate(&run(path, on), true).unwrap();
    (
        String::from_utf8_lossy(&r.log.stderr).into_owned(),
        r.csg.unwrap_or_default(),
    )
}

fn compare(path: &Path, got: &str, failures: &mut Vec<String>) {
    if std::env::var_os("NEOSCAD_BLESS").is_some() {
        std::fs::write(path, got).unwrap();
        return;
    }
    let want = std::fs::read_to_string(path).unwrap_or_default();
    if want != got {
        failures.push(format!(
            "{} differs:\n--- expected\n{want}\n--- got\n{got}",
            path.display()
        ));
    }
}

fn models() -> Vec<PathBuf> {
    let mut names: Vec<PathBuf> = std::fs::read_dir(goldens())
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "scad"))
        .collect();
    names.sort();
    names
}

/// Every golden model's console output and `.csg`, with the flag on.
#[test]
fn golden_models() {
    let names = models();
    assert!(names.len() >= 7, "{names:?}");
    let mut failures = Vec::new();
    for p in &names {
        let name = p.file_name().unwrap().to_str().unwrap();
        let text = std::fs::read(p).unwrap();
        let (s, _) = session(&[(name, &text)]);
        let (log, csg) = evaluate(&s, name, true);
        compare(&p.with_extension("echo"), &log, &mut failures);
        compare(&p.with_extension("csg"), &csg, &mut failures);
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// Flag off: `anchor` and `child_anchors` are OpenSCAD's unknown module and
/// function, with its exact warnings, and the JSON hints name the flag.
#[test]
fn off_they_are_unknown() {
    let src = b"module m() { a = child_anchors(0); echo(a); children(); }\n\
                m() { cube(1); anchor(\"top\", [0, 0, 1]); }\n";
    let (s, _) = session(&[("m.scad", src)]);
    let (log, csg) = evaluate(&s, "m.scad", false);
    assert_eq!(
        log,
        "WARNING: Ignoring unknown function 'child_anchors' in file m.scad, line 1\n\
         ECHO: undef\n\
         WARNING: Ignoring unknown module 'anchor' in file m.scad, line 2\n"
    );
    // The tree is the same with the flag on: an anchor makes no node.
    let (_, on) = evaluate(&s, "m.scad", true);
    assert_eq!(csg, on);
    let r = s.evaluate(&run("m.scad", false), false).unwrap();
    let d = r.log.diagnostics_json();
    for i in [0, 1] {
        let hint = d[i]["hints"][0]["message"].as_str().unwrap();
        assert!(hint.contains("--enable query"), "{hint}");
    }
}

/// The `.csg` export of a model that uses queries is plain OpenSCAD: it
/// evaluates without the flag and without a message, to the same shape
/// (to the 6 digits a `.csg` prints its numbers with).
#[test]
fn csg_export_is_plain_openscad() {
    let scheme = render::ColorScheme::cornfield();
    // (Not `anchors.scad`: its NaN transform prints as `nan`, which no
    // `.csg` reads back, in OpenSCAD either.)
    for name in ["plate.scad", "sketch.scad", "reuse.scad"] {
        let text = std::fs::read(goldens().join(name)).unwrap();
        let (s, fs) = session(&[(name, &text)]);
        let (_, csg) = evaluate(&s, name, true);
        assert!(!csg.contains("anchor"), "{name}: {csg}");
        fs.insert("/doc/export.csg", csg.into_bytes());
        let measure = |run: Run| {
            let r = s.render(&run, Mode::Render, &scheme).unwrap();
            let g = r.geometry_json(&scheme.geometry_scheme());
            (r.log.stderr, g["volume"].as_f64(), g["area"].as_f64())
        };
        let from_source = measure(run(name, true));
        let (log, v, a) = measure(run("export.csg", false));
        let log = String::from_utf8_lossy(&log);
        // Nothing in it is unknown to OpenSCAD. (`sketch.scad` mixes 2D
        // and 3D, whose geometry warnings the source prints too.)
        assert!(!log.contains("unknown"), "{name}: {log}");
        let near = |x: Option<f64>, y: Option<f64>| match (x, y) {
            (Some(x), Some(y)) => (x - y).abs() <= 1e-4 * x.abs().max(1.0),
            (x, y) => x == y,
        };
        assert!(
            near(v, from_source.1) && near(a, from_source.2),
            "{name}: {v:?} {a:?} vs {:?} {:?}",
            from_source.1,
            from_source.2
        );
    }
}

struct Sink(Vec<u8>);

impl ExportSink for Sink {
    fn write(&mut self, _: &str, data: &[u8]) -> Result<(), String> {
        self.0 = data.to_vec();
        Ok(())
    }
    fn summary(&mut self, _: &session::SummaryFacts<'_>, _: &mut eval::Console<Vec<u8>>) -> bool {
        true
    }
}

fn export(s: &Session, mut run: Run, format: &str) -> Vec<u8> {
    let scheme = render::ColorScheme::cornfield();
    run.limits = Some(session::Limits::AGENT);
    let path = run.input.clone();
    let req = ExportRequest {
        run,
        outputs: vec![(
            format!("out.{format}"),
            Format::from_id(format).expect("a format"),
        )],
        force: false,
        scheme: scheme.clone(),
        settings: Settings {
            scheme: scheme.geometry_scheme(),
            svg: io::svg::SvgStyle::default(),
            pdf: io::pdf::PdfOptions::default(),
            pdf_warnings: Vec::new(),
            threemf: io::threemf::Options::default(),
            threemf_warning: None,
            title: path.clone(),
            source_path: path,
            creation_date: "2026-01-01T00:00:00Z".to_string(),
            pov_camera: None,
            predictible_output: false,
        },
    };
    let mut sink = Sink(Vec::new());
    let r = s.export(&req, &mut sink).expect("not cancelled");
    assert_eq!(r.exit_code, 0, "{}", String::from_utf8_lossy(&r.log.stderr));
    assert!(!sink.0.is_empty());
    sink.0
}

/// A warm session exports the same bytes as a cold one: a query renders
/// nothing (its answers come from anchors in the tree), so what earlier
/// renders left in the cache cannot reach it. The warm session first
/// renders other gears, then this one with the queries and without them.
#[test]
fn a_warm_export_equals_a_cold_one() {
    let text = String::from_utf8(std::fs::read(goldens().join("plate.scad")).unwrap()).unwrap();
    let sized = |teeth: u32| text.replace("gear(teeth = 17)", &format!("gear(teeth = {teeth})"));
    let (warm, fs) = session(&[("m.scad", sized(11).as_bytes())]);
    export(&warm, run("m.scad", true), "stl");
    fs.insert("/doc/m.scad", sized(23).into_bytes());
    export(&warm, run("m.scad", true), "stl");
    fs.insert("/doc/m.scad", sized(17).into_bytes());
    let w = export(&warm, run("m.scad", true), "stl");
    let w2 = export(&warm, run("m.scad", true), "stl");
    let (cold, _) = session(&[("m.scad", sized(17).as_bytes())]);
    let c = export(&cold, run("m.scad", true), "stl");
    assert!(w == c, "the warm export differs from the cold one");
    assert!(w2 == c, "a repeated warm export differs from the cold one");
}
