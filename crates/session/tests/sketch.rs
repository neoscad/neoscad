//! Constrained sketches (`--enable sketch`, docs/language-extensions.md):
//! the golden models in `conformance/extensions/sketch` (their console
//! output and `.csg`), the flag off, a program's own `module sketch`,
//! diagnostics as JSON, and a warm session's export equal to a cold one.
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
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../conformance/extensions/sketch")
}

fn session(files: &[(&str, &[u8])]) -> (Session, Arc<MemFs>) {
    let fs = Arc::new(MemFs::new());
    for (p, t) in files {
        fs.insert(format!("/doc/{p}"), t.to_vec());
    }
    let mut cfg = Config::new(fs.clone(), LibraryPath(Vec::new()));
    cfg.work_dir = PathBuf::from("/doc");
    // Bounded like an agent's calls: a sketch here that ran away would
    // stop with a diagnostic, not fill the machine's memory.
    cfg.limits = session::Limits::AGENT;
    (Session::new(cfg), fs)
}

fn run(path: &str, sketch: bool) -> Run {
    let mut run = Run::new(path);
    if sketch {
        run.extensions = eval::Extensions::NONE.with(eval::Extension::Sketch);
    }
    run
}

/// What the command line prints and the `.csg` it writes.
fn evaluate(s: &Session, path: &str, sketch: bool) -> (String, String) {
    let r = s.evaluate(&run(path, sketch), true).unwrap();
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

/// Every golden model's console output and `.csg`, with the flag on.
#[test]
fn golden_models() {
    let mut names: Vec<PathBuf> = std::fs::read_dir(goldens())
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "scad"))
        .collect();
    names.sort();
    assert!(names.len() >= 6, "{names:?}");
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

/// The worked examples (sections 6.1 and 6.2) render to the shapes they
/// describe: the gusset's area (the L, the gusset triangle, and the two
/// concave fillets' material), and the plate less the slot.
#[test]
fn worked_examples_render_their_shapes() {
    let scheme = render::ColorScheme::cornfield();
    let volume = |name: &str| {
        let text = std::fs::read(goldens().join(name)).unwrap();
        let (s, _) = session(&[(name, &text)]);
        let r = s.render(&run(name, true), Mode::Render, &scheme).unwrap();
        assert_eq!(r.exit_code, 0, "{}", String::from_utf8_lossy(&r.log.stderr));
        r.geometry_json(&scheme.geometry_scheme())["volume"]
            .as_f64()
            .unwrap()
    };
    // The gusset: 40 x 4 and 4 x 26 legs, an 18 x 18 / 2 gusset, and per
    // fillet the corner it fills, r^2 / tan(67.5 deg) - (pi / 4) r^2 / 2,
    // plus the 6 chords' segments (the arc is concave, so the chords add
    // material): 6 r^2 (a - sin a) / 2 with a = 7.5 degrees.
    let r: f64 = 3.0;
    let a = 7.5f64.to_radians();
    let fillet = r * r / 67.5f64.to_radians().tan() - std::f64::consts::FRAC_PI_4 * r * r / 2.0
        + 6.0 * r * r * (a - a.sin()) / 2.0;
    let gusset = (160.0 + 104.0 + 162.0 + 2.0 * fillet) * 12.0;
    let v = volume("gusset.scad");
    assert!((v - gusset).abs() < 1e-6 * gusset, "{v} vs {gusset}");
    // The slot: 30 x 8 and two half circles of radius 4 in 7 segments
    // each (12 for the circle: $fa 12, $fs 2 give 13 for 360 degrees, so
    // ceil(6.5) = 7 for 180), cut from a 50 x 20 plate, 3 thick.
    let half = 7.0 * 16.0 * (180f64 / 7.0).to_radians().sin() / 2.0;
    let slot = (1000.0 - (240.0 + 2.0 * half)) * 3.0;
    let v = volume("slot.scad");
    assert!((v - slot).abs() < 1e-6 * slot, "{v} vs {slot}");
}

/// The `.csg` export of a sketch model is plain OpenSCAD: it evaluates
/// without the flag and without a message, to the same shape. (Its
/// coordinates are printed to 6 digits, as every `.csg` number is, so the
/// shape is the same to that precision; `geom`'s tests show the sketch
/// node renders exactly as its polygon.)
#[test]
fn csg_export_is_plain_openscad() {
    let scheme = render::ColorScheme::cornfield();
    for name in ["gusset.scad", "slot.scad", "holes.scad", "scoping.scad"] {
        let text = std::fs::read(goldens().join(name)).unwrap();
        let (s, fs) = session(&[(name, &text)]);
        let (_, csg) = evaluate(&s, name, true);
        assert!(!csg.contains("sketch"), "{name}: {csg}");
        fs.insert("/doc/export.csg", csg.into_bytes());
        let volume = |run: Run| {
            let r = s.render(&run, Mode::Render, &scheme).unwrap();
            let g = r.geometry_json(&scheme.geometry_scheme());
            (r.log.stderr, g["volume"].as_f64(), g["area"].as_f64())
        };
        let from_source = volume(run(name, true));
        // The `.csg` needs no flag: it is OpenSCAD.
        let (log, v, a) = volume(run("export.csg", false));
        assert!(
            !String::from_utf8_lossy(&log).contains("WARNING"),
            "{name}: {}",
            String::from_utf8_lossy(&log)
        );
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

/// Flag off: `sketch` is OpenSCAD's unknown module with its exact warning
/// (and the JSON hint names the flag), and nothing in the body runs.
#[test]
fn off_it_is_an_unknown_module() {
    let src = b"sketch(name = \"s\") { p = point([0, 0]); echo(\"body\"); fix(p); }\n\
                p = point([1, 2]);\n";
    let (s, _) = session(&[("m.scad", src)]);
    let (log, csg) = evaluate(&s, "m.scad", false);
    assert_eq!(
        log,
        "WARNING: Ignoring unknown function 'point' in file m.scad, line 2\n\
         WARNING: Ignoring unknown module 'sketch' in file m.scad, line 1\n"
    );
    assert_eq!(csg, "\n");
    let r = s.evaluate(&run("m.scad", false), false).unwrap();
    let d = r.log.diagnostics_json();
    let hint = d[1]["hints"][0]["message"].as_str().unwrap();
    assert!(hint.contains("--enable sketch"), "{hint}");
    // On, the vocabulary is still not global: `point` outside a body is
    // the same unknown function.
    let (log, _) = evaluate(&s, "m.scad", true);
    assert_eq!(
        log,
        "WARNING: Ignoring unknown function 'point' in file m.scad, line 2\n\
         ECHO: \"body\"\n"
    );
}

/// OpenSCAD's `examples/Basics/roof.scad` defines its own `module
/// sketch()`. With the flag on, it is still that module: a program's own
/// definitions shadow the extension's builtins, as they shadow every
/// builtin (section 1). The manifest's cases of the file are all skipped
/// (they need `roof`), so its module is called here directly.
#[test]
fn a_programs_own_sketch_module_wins() {
    // roof.scad's definition (lines 17-20), as of the reference checkout.
    let own = "module sketch() {\n    polygon(points=[[-5,-1],[-0.15,-1],[0,0],[0.15,-1],[5,-1],\n    [5,-0.1],[4,0],[5,0.1],[5,1],[-5,1]]);\n}\n";
    let roof = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../.reference/openscad/examples/Basics/roof.scad");
    let mut programs = vec![format!("{own}sketch();\nlinear_extrude(1) sketch();\n")];
    // With the reference checkout present, the file itself, its own
    // module called as well as through `roof()`.
    if let Ok(text) = std::fs::read_to_string(&roof) {
        assert!(text.contains(own), "roof.scad's module sketch changed");
        programs.push(format!("{text}\nsketch();\n"));
    }
    for p in programs {
        let (s, _) = session(&[("roof.scad", p.as_bytes())]);
        let off = evaluate(&s, "roof.scad", false);
        let on = evaluate(&s, "roof.scad", true);
        assert_eq!(off, on);
        assert!(on.1.contains("polygon(points = [[-5, -1]"), "{}", on.1);
    }
}

/// The real libraries, not their shapes (`scoping.scad` has those): MCAD
/// (vendored in `assets/`) defines `function distance`, `function angle`
/// and `module chamfer`; BOSL2 (from `.reference/BOSL2`, when it is
/// checked out) `function arc`, `module arc`, `function circle`, `module
/// circle` and `module fillet`. Outside a sketch body each keeps its
/// library meaning; inside, the sketch's.
#[test]
fn library_names_keep_their_meaning_outside_sketch_bodies() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let fs = Arc::new(MemFs::new());
    let mount = |dir: &Path, as_: &str| {
        let mut n = 0;
        for e in std::fs::read_dir(dir).into_iter().flatten() {
            let p = e.unwrap().path();
            if p.extension().is_some_and(|x| x == "scad") {
                let name = p.file_name().unwrap().to_str().unwrap().to_string();
                fs.insert(format!("/lib/{as_}/{name}"), std::fs::read(&p).unwrap());
                n += 1;
            }
        }
        n > 0
    };
    assert!(mount(&root.join("assets/libraries/MCAD"), "MCAD"));
    let bosl2 = mount(&root.join(".reference/BOSL2"), "BOSL2");
    if !bosl2 {
        eprintln!("BOSL2 part skipped: .reference/BOSL2 is not checked out");
    }
    let mut src = String::from(
        "use <MCAD/utilities.scad>\n\
         use <MCAD/metric_fastners.scad>\n\
         echo(distance([0, 0, 0], [3, 4, 0]), angle([1, 0, 0]));\n\
         chamfer(1, 2);\n",
    );
    if bosl2 {
        src = format!(
            "include <BOSL2/std.scad>\n{src}\
             echo(len(arc(n = 5, r = 10, angle = 90)), len(circle(r = 2, $fn = 6)));\n\
             circle(r = 2, $fn = 6);\n\
             translate([0, 0, 5]) fillet(l = 10, r = 2);\n"
        );
    }
    src.push_str(
        "translate([20, 0]) sketch(name = \"s\", $fn = 12) {\n\
         \x20 c = circle([0, 0], r = 5);\n\
         \x20 fix(c.center);\n\
         \x20 a = arc(c.center, [3, 0], [0, 3], construction = true);\n\
         \x20 fix(a.start); angle(a, 90);\n\
         \x20 echo(c, a);\n\
         }\n",
    );
    fs.insert("/doc/m.scad", src.into_bytes());
    let mut cfg = Config::new(fs, LibraryPath(vec![PathBuf::from("/lib")]));
    cfg.work_dir = PathBuf::from("/doc");
    cfg.limits = session::Limits::AGENT;
    let s = Session::new(cfg);
    let r = s.evaluate(&run("m.scad", true), true).unwrap();
    let log = String::from_utf8_lossy(&r.log.stderr).into_owned();
    assert!(!log.contains("WARNING") && !log.contains("ERROR"), "{log}");
    let echo = r.log.echo();
    assert_eq!(echo[0], "ECHO: 5, [0, 0, 0]", "{log}");
    if bosl2 {
        assert_eq!(echo[1], "ECHO: 5, 6", "{log}");
    }
    assert_eq!(
        echo.last().unwrap(),
        "ECHO: <sketch circle \"c\">, <sketch arc \"a\">"
    );
    let csg = r.csg.unwrap();
    // MCAD's chamfer and, with BOSL2, its circle and fillet made geometry;
    // the sketch is a 12-gon.
    assert!(
        csg.contains("polygon(points = [[5, 0], [4.33013, 2.5]"),
        "{csg}"
    );
}

/// Diagnostics as JSON: stable codes, severities and the span of the
/// statement each is about.
#[test]
fn diagnostics_have_codes_and_spans() {
    let text = std::fs::read(goldens().join("diagnostics.scad")).unwrap();
    let (s, _) = session(&[("diagnostics.scad", &text)]);
    let r = s.evaluate(&run("diagnostics.scad", true), false).unwrap();
    let got: Vec<String> = r
        .log
        .diagnostics_json()
        .iter()
        .map(|d| format!("{} {} {}", d["code"], d["severity"], d["line"]))
        .collect();
    assert_eq!(
        got,
        [
            "\"sketch-underconstrained\" \"info\" 13",
            "\"sketch-underconstrained\" \"error\" 16",
            "\"sketch-conflict\" \"error\" 25",
            "\"sketch-redundant\" \"warning\" 35",
            "\"sketch-open-profile\" \"error\" 41",
            "\"sketch-open-profile\" \"error\" 41",
            "\"sketch-fillet-too-large\" \"error\" 51",
            "\"sketch-geometry-in-body\" \"error\" 57",
            "\"sketch-unknown-entity\" \"error\" 58",
            "\"sketch-geometry-in-body\" \"error\" 59",
            "\"unknown-module\" \"warning\" 63",
        ]
    );
    // The redundant constraint's hint, and the span: the statement itself.
    let d = &r.log.diagnostics_json()[3];
    assert_eq!(d["hints"][0]["message"], "remove it");
    assert_eq!(d["span"]["start"]["column"], 3);
    // A failed sketch is an empty shape; the model still evaluates.
    assert!(!r.aborted);
    assert!(String::from_utf8_lossy(&r.log.stderr).contains("ECHO: \"still evaluating\""));
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

/// A warm session exports the same bytes as a cold one: the solver never
/// starts from an earlier solve, so rendering other sizes of the gusset
/// first changes nothing (section 4.6, "No history").
#[test]
fn a_warm_export_equals_a_cold_one() {
    let text = String::from_utf8(std::fs::read(goldens().join("gusset.scad")).unwrap()).unwrap();
    let sized = |leg_a: u32, r: u32| {
        text.replace("leg_a  = 40;", &format!("leg_a  = {leg_a};"))
            .replace("r      = 3;", &format!("r      = {r};"))
    };
    let (warm, fs) = session(&[("m.scad", sized(60, 2).as_bytes())]);
    export(&warm, run("m.scad", true), "stl");
    fs.insert("/doc/m.scad", sized(25, 5).into_bytes());
    export(&warm, run("m.scad", true), "stl");
    fs.insert("/doc/m.scad", sized(40, 3).into_bytes());
    let w = export(&warm, run("m.scad", true), "stl");
    let (cold, _) = session(&[("m.scad", sized(40, 3).as_bytes())]);
    let c = export(&cold, run("m.scad", true), "stl");
    assert!(w == c, "the warm export differs from the cold one");
}
