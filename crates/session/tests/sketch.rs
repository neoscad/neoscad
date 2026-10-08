//! Constrained sketches (`--enable sketch`, docs/language-extensions.md):
//! the golden models in `conformance/extensions/sketch` (their console
//! output and `.csg`, and for the diagnostics models their JSON), the flag
//! off, a program's own `module sketch`, diagnostics and their hints as
//! JSON (every edit a hint carries applied, and the problem gone), the
//! unknowns limit, a large solve stopped by cancellation and by the time
//! limit, and a warm session's export equal to a cold one.
//!
//! The goldens are rewritten with `NEOSCAD_BLESS=1`, for a change meant to
//! alter them.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

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
        // The diagnostics models' JSON: codes, spans and hints with their
        // edits, as `check` and the language server get them.
        if matches!(name, "diagnostics.scad" | "hints.scad") {
            let r = s.evaluate(&run(name, true), false).unwrap();
            let mut json = serde_json::to_string_pretty(&r.log.diagnostics_json()).unwrap();
            json.push('\n');
            compare(&p.with_extension("json"), &json, &mut failures);
        }
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
    assert_eq!(d["hints"][0]["message"], "remove `distance(o, a, 10)`");
    assert_eq!(d["span"]["start"]["column"], 3);
    // A failed sketch is an empty shape; the model still evaluates.
    assert!(!r.aborted);
    assert!(String::from_utf8_lossy(&r.log.stderr).contains("ECHO: \"still evaluating\""));
}

/// Every example in `docs/sketch.md` evaluates with the extension on and
/// gives exactly the diagnostics its fence names (```openscad
/// expect=code,...), none for a plain one; each sketch in it that has no
/// expected error has a profile or is all construction geometry.
#[test]
fn docs_examples_evaluate() {
    let doc =
        std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/sketch.md"))
            .unwrap();
    let mut examples: Vec<(usize, Vec<String>, String)> = Vec::new();
    let mut open: Option<(usize, Vec<String>, String)> = None;
    for (i, line) in doc.lines().enumerate() {
        match (&mut open, line.strip_prefix("```")) {
            (None, Some(info)) if info.starts_with("openscad") => {
                let expect = info
                    .split_whitespace()
                    .find_map(|w| w.strip_prefix("expect="))
                    .map(|c| c.split(',').map(str::to_string).collect())
                    .unwrap_or_default();
                open = Some((i + 1, expect, String::new()));
            }
            (Some(_), Some("")) => examples.extend(open.take()),
            (Some((_, _, text)), _) => {
                text.push_str(line);
                text.push('\n');
            }
            _ => {}
        }
    }
    assert!(examples.len() >= 30, "{} examples", examples.len());
    let mut failures = Vec::new();
    for (line, expect, text) in &examples {
        let (s, _) = session(&[("example.scad", text.as_bytes())]);
        let r = s.evaluate(&run("example.scad", true), false).unwrap();
        let mut got: Vec<String> = r
            .log
            .diagnostics_json()
            .iter()
            .map(|d| d["code"].as_str().unwrap_or("").to_string())
            .collect();
        got.dedup();
        if &got != expect || r.aborted {
            failures.push(format!(
                "docs/sketch.md:{line}: expected {expect:?}, got {got:?}\n{}",
                String::from_utf8_lossy(&r.log.stderr)
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// `check` lists every sketch with its state, degrees of freedom and the
/// codes of its diagnostics (`docs/language-extensions.md`, section 4.8);
/// a model without sketches has no `sketches` at all.
#[test]
fn check_lists_the_sketches() {
    use session::check::{CheckRequest, CheckSettings};
    let text = std::fs::read(goldens().join("diagnostics.scad")).unwrap();
    let (s, _) = session(&[("diagnostics.scad", &text), ("plain.scad", b"cube(1);")]);
    let c = s
        .check(&CheckRequest {
            run: run("diagnostics.scad", true),
            settings: CheckSettings::default(),
        })
        .unwrap();
    let got: Vec<String> = c.summary["sketches"]
        .as_array()
        .unwrap()
        .iter()
        .map(|k| {
            format!(
                "{} line {}: {} dof {} empty {} {}",
                k["name"], k["line"], k["status"], k["dof"], k["empty"], k["codes"]
            )
        })
        .collect();
    assert_eq!(
        got,
        [
            "\"under\" line 13: \"underconstrained\" dof 6 empty false [\"sketch-underconstrained\"]",
            "\"strict\" line 16: \"underconstrained\" dof 2 empty true [\"sketch-underconstrained\"]",
            "\"conflict\" line 19: \"conflict\" dof 0 empty true [\"sketch-conflict\"]",
            "\"redundant\" line 29: \"fully-constrained\" dof 0 empty false [\"sketch-redundant\"]",
            "\"open\" line 39: \"error\" dof 0 empty true [\"sketch-open-profile\"]",
            "\"big\" line 46: \"error\" dof 0 empty true [\"sketch-fillet-too-large\"]",
            "\"misuse\" line 55: \"error\" dof 0 empty true [\"sketch-geometry-in-body\",\"sketch-unknown-entity\"]",
        ]
    );
    // The check's summary has no entities or edits: `measure` has those.
    assert!(c.summary["sketches"][0].get("entities").is_none());
    assert!(
        session::check::text(&c.summary)
            .contains("sketch 'conflict' (line 19): conflicting constraints")
    );
    let c = s
        .check(&CheckRequest {
            run: run("plain.scad", true),
            settings: CheckSettings::default(),
        })
        .unwrap();
    assert!(c.summary.get("sketches").is_none(), "{}", c.summary);
}

/// `measure --sketch NAME`: every entity's solved values.
#[test]
fn measure_gives_a_sketchs_solved_values() {
    use session::measure::MeasureRequest;
    let text = std::fs::read(goldens().join("slot.scad")).unwrap();
    let (s, _) = session(&[("slot.scad", &text)]);
    let mut req = MeasureRequest::new(run("slot.scad", true));
    req.sketch = Some("slot".into());
    let m = s.measure(&req).unwrap();
    assert_eq!(m.exit_code, 0, "{}", m.summary);
    let sk = &m.summary["sketch"];
    assert_eq!(sk["status"], "fully-constrained");
    assert_eq!(sk["instances"], 1);
    let e = |name: &str| {
        sk["entities"]
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["name"] == name)
            .unwrap_or_else(|| panic!("{name}: {sk}"))
            .clone()
    };
    assert_eq!(e("c2")["at"], serde_json::json!([30.0, 0.0]));
    let top = e("top");
    assert_eq!(top["length"], 30.0);
    assert_eq!(top["angle"], 0.0);
    assert_eq!(e("bot")["angle"], 180.0);
    let e1 = e("e1");
    assert_eq!(e1["radius"], 4.0);
    assert_eq!(e1["sweep"], 180.0);
    assert_eq!(e("axis")["construction"], true);
    assert!(sk.get("pin").is_none(), "an editor's, not measure's");
    let t = session::measure::text(&m.summary);
    assert!(
        t.contains("  top line [0, 4]..[30, 4], length 30, angle 0°\n"),
        "{t}"
    );
    assert!(
        t.contains("  e1 arc centre [0, 0], radius 4, from [0, 4] to [0, -4], sweep 180°\n"),
        "{t}"
    );
    // An unknown name lists the names there are; without the extension
    // there are none.
    req.sketch = Some("slit".into());
    let m = s.measure(&req).unwrap();
    assert_eq!(m.exit_code, 1);
    assert_eq!(m.summary["error"], "no sketch 'slit' (sketches: slot)");
    let mut req = MeasureRequest::new(run("slot.scad", false));
    req.sketch = Some("slot".into());
    let m = s.measure(&req).unwrap();
    assert!(
        m.summary["error"]
            .as_str()
            .unwrap()
            .contains("the model has no sketches (they need `--enable sketch`)"),
        "{}",
        m.summary
    );
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

/// The byte offset of a JSON span position (1-based line, 1-based byte
/// column).
fn offset(text: &[u8], pos: &serde_json::Value) -> usize {
    let (line, col) = (
        pos["line"].as_u64().unwrap() as usize,
        pos["column"].as_u64().unwrap() as usize,
    );
    let mut start = 0;
    for _ in 1..line {
        start += text[start..].iter().position(|&c| c == b'\n').unwrap() + 1;
    }
    start + col - 1
}

/// `text` with one hint's edit applied.
fn apply(text: &[u8], hint: &serde_json::Value) -> Vec<u8> {
    let r = &hint["replace"];
    let (a, b) = (
        offset(text, &r["span"]["start"]),
        offset(text, &r["span"]["end"]),
    );
    let mut out = text[..a].to_vec();
    out.extend_from_slice(r["text"].as_str().unwrap().as_bytes());
    out.extend_from_slice(&text[b..]);
    out
}

/// Diagnostics of `text` as (code, sketch name) pairs, and the JSON.
fn diagnose(text: &[u8]) -> (Vec<(String, String)>, Vec<serde_json::Value>) {
    let (s, _) = session(&[("m.scad", text)]);
    let r = s.evaluate(&run("m.scad", true), false).unwrap();
    let d = r.log.diagnostics_json();
    let keys = d
        .iter()
        .map(|x| {
            let m = x["message"].as_str().unwrap_or("");
            let name = m
                .strip_prefix("Sketch '")
                .and_then(|r| r.split('\'').next())
                .unwrap_or("")
                .to_string();
            (x["code"].as_str().unwrap().to_string(), name)
        })
        .collect();
    (keys, d)
}

/// The free degrees of freedom the under-constrained message of `sketch`
/// gives, 0 without one.
fn dof(json: &[serde_json::Value], sketch: &str) -> usize {
    json.iter()
        .filter(|d| d["code"] == "sketch-underconstrained")
        .filter_map(|d| {
            d["message"]
                .as_str()?
                .strip_prefix(&format!("Sketch '{sketch}': "))?
                .split(' ')
                .next()?
                .parse()
                .ok()
        })
        .next()
        .unwrap_or(0)
}

/// Every hint in `hints.scad` that carries an edit is machine-applicable:
/// applying it alone to the source leaves a program in which the problem
/// it was about is gone, and no new error or warning appears in that
/// sketch. (The fillet's edit makes the fillet fit; the conflict's, any one
/// of them, resolve the conflict; the under-constrained "add all" leaves
/// the sketch fully constrained; pinning the drawing leaves no flip.)
#[test]
fn every_hint_edit_fixes_its_problem() {
    // The model has edits for: under-constrained (twice, 4 + 1), redundant,
    // conflict (4), flips (3), the unguessed point, the fillet, and the
    // labelled sketch's suggestions (3).
    let applied = hint_edits_fix_their_problems("hints.scad");
    assert!(applied >= 15, "{applied}");
    // A line–arc fillet too large for its corner (stage 7): the size the
    // bisection found fits.
    assert_eq!(hint_edits_fix_their_problems("fillets.scad"), 1);
    // And at a corner of two arcs.
    assert_eq!(hint_edits_fix_their_problems("arc-corners.scad"), 1);
}

/// Apply each edit of the golden `name`'s hints alone and check that its
/// problem is gone; how many edits there were.
fn hint_edits_fix_their_problems(name: &str) -> usize {
    let text = std::fs::read(goldens().join(name)).unwrap();
    let (before, json) = diagnose(&text);
    let mut applied = 0;
    for (i, d) in json.iter().enumerate() {
        let (code, sketch) = &before[i];
        let hints = d["hints"].as_array().cloned().unwrap_or_default();
        for h in hints.iter().filter(|h| h.get("replace").is_some()) {
            let fixed = apply(&text, h);
            let (after, after_json) = diagnose(&fixed);
            let same: Vec<&(String, String)> = after.iter().filter(|(_, n)| n == sketch).collect();
            let message = h["message"].as_str().unwrap();
            if code == "sketch-underconstrained" && message.starts_with("add `") {
                // One suggested constraint of several removes its share of
                // the freedom.
                assert!(
                    dof(&after_json, sketch) < dof(&json, sketch),
                    "`{message}` left '{sketch}' as free:\n{}",
                    String::from_utf8_lossy(&fixed)
                );
            } else {
                assert!(
                    !same.iter().any(|(c, _)| c == code),
                    "{code} in '{sketch}' is still there after `{message}`:\n{}",
                    String::from_utf8_lossy(&fixed)
                );
            }
            // An added constraint must not be redundant or conflict, and a
            // removal must not leave anything new behind.
            for (c, _) in &same {
                assert!(
                    before.iter().any(|(b, n)| b == c && n == sketch)
                        || c == "sketch-underconstrained",
                    "`{message}` in '{sketch}' gave a new {c}:\n{}",
                    String::from_utf8_lossy(&fixed)
                );
            }
            applied += 1;
        }
    }
    applied
}

/// A suggested constraint's "add all" completes the sketch: no degree of
/// freedom is left, and the shape is the one solved before (the
/// suggestions are measured on the solution).
#[test]
fn adding_all_suggestions_fully_constrains_the_sketch() {
    let text = std::fs::read(goldens().join("hints.scad")).unwrap();
    let (keys, json) = diagnose(&text);
    let i = keys
        .iter()
        .position(|k| k.0 == "sketch-underconstrained" && k.1 == "loose")
        .unwrap();
    let all = &json[i]["hints"][0];
    assert!(
        all["message"].as_str().unwrap().starts_with("add all 3:"),
        "{all}"
    );
    let fixed = apply(&text, all);
    let (s, _) = session(&[("m.scad", &fixed)]);
    let (log, csg) = evaluate(&s, "m.scad", true);
    assert!(!log.contains("'loose'"), "{log}");
    assert!(
        csg.contains("polygon(points = [[0, 0], [20, 0], [20, 10], [0, 10]]"),
        "{csg}"
    );
}

/// `Limits::sketch_unknowns` is a resource limit like the others: past
/// it, the sketch is refused before the solve with a `resource-limit`
/// error naming the limit and the flag that raises it.
#[test]
fn the_unknowns_limit_stops_a_large_sketch() {
    let src =
        b"sketch(name = \"s\") {\n  for (i = [0:9]) fix(point([i, 0]));\n}\necho(\"after\");\n";
    let (s, _) = session(&[("m.scad", src)]);
    let mut r = run("m.scad", true);
    r.limits = Some(session::Limits {
        sketch_unknowns: Some(10),
        ..session::Limits::AGENT
    });
    let out = s.evaluate(&r, false).unwrap();
    let d = out.log.diagnostics_json();
    let limit = d
        .iter()
        .find(|x| x["code"] == "resource-limit")
        .unwrap_or_else(|| panic!("{d:?}"));
    assert_eq!(
        limit["message"],
        "Resource limit exceeded: sketch() would make 20 sketch unknowns, over the sketch_unknowns limit of 10"
    );
    assert!(
        limit["hints"][0]["message"]
            .as_str()
            .unwrap()
            .contains("--limit sketch_unknowns=N"),
        "{limit}"
    );
    assert_eq!(limit["line"], 1);
    // Within the limit (the agent default), the same model solves.
    let (log, _) = evaluate(&s, "m.scad", true);
    assert!(!log.contains("ERROR"), "{log}");
}

/// A regular `n`-gon drawn with a radius of about 10 but dimensioned with
/// sides of 3: equal sides and corner angles, one side's length, one
/// corner fixed. 2n unknowns, all coupled, so every iteration is a dense
/// O(n^3) factorisation, and the drawing is far from any solution (the
/// angles and the perimeter disagree with it), so the solve runs through
/// continuation for many iterations: seconds even optimised.
fn polygon_sketch(n: usize) -> String {
    let mut s = String::from("sketch(name = \"big\") {\n");
    s.push_str(&format!("  n = {n};\n"));
    s.push_str("  p = [for (i = [0:n-1]) point([cos(360*i/n) * (10 + (i % 3) * 0.2), sin(360*i/n) * (10 - (i % 5) * 0.1)])];\n");
    s.push_str("  l = [for (i = [0:n-1]) line(p[i], p[(i+1)%n])];\n");
    s.push_str("  fix(p[0]);\n  length(l[0], 3);\n");
    s.push_str("  for (i = [1:n-1]) equal(l[0], l[i]);\n");
    s.push_str("  for (i = [0:n-3]) angle(l[i], l[i+1], 360/n);\n");
    s.push_str("}\necho(\"after\");\n");
    s
}

/// A large solve stops at a cancellation: the solver polls the request's
/// interrupt between iterations, and the run returns `Cancelled` soon
/// after the flag goes up rather than after the whole solve.
#[test]
fn a_large_solve_stops_when_cancelled() {
    let text = polygon_sketch(150);
    let (s, _) = session(&[("m.scad", text.as_bytes())]);
    let flag = Arc::new(AtomicBool::new(false));
    let mut r = run("m.scad", true);
    r.interrupt = Some(flag.clone());
    let t = Instant::now();
    let stopper = {
        let flag = flag.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(300));
            flag.store(true, Ordering::Relaxed);
            Instant::now()
        })
    };
    let out = s.evaluate(&r, false);
    let stopped_at = stopper.join().unwrap();
    assert!(out.is_err(), "the solve finished before the cancellation");
    // One iteration at 300 unknowns takes at most a few seconds even
    // unoptimised; the whole solve is many.
    let late = Instant::now().saturating_duration_since(stopped_at);
    assert!(
        late < Duration::from_secs(10),
        "{late:?} after the cancel ({:?} in all)",
        t.elapsed()
    );
}

/// The same solve under a request's time limit: stopped between
/// iterations, and reported as the time limit (a `resource-limit` error),
/// not as a cancellation; the run's other output is still there.
#[test]
fn a_large_solve_stops_at_the_time_limit() {
    let text = polygon_sketch(150);
    let fs = Arc::new(MemFs::new());
    fs.insert("/doc/m.scad", text.into_bytes());
    let mut cfg = Config::new(fs, LibraryPath(Vec::new()));
    cfg.work_dir = PathBuf::from("/doc");
    // The time limit needs a clock, which only a host (here the test)
    // reads.
    let start = Instant::now();
    cfg.clock = Some(Arc::new(move || start.elapsed().as_secs_f64() * 1000.0));
    let s = Session::new(cfg);
    let mut r = run("m.scad", true);
    r.limits = Some(session::Limits {
        time: Some(0.3),
        ..session::Limits::AGENT
    });
    let t = Instant::now();
    let out = s.evaluate(&r, false).unwrap();
    assert!(t.elapsed() < Duration::from_secs(15), "{:?}", t.elapsed());
    let d = out.log.diagnostics_json();
    assert!(
        d.iter().any(|x| x["code"] == "resource-limit"
            && x["message"].as_str().unwrap().contains("time limit")),
        "{d:?}"
    );
}

/// `snapshot --sketch` (stage 7): the overlay drawn from a run's sketch
/// facts. Free entities are orange, a conflict's red, construction
/// dashed; every constraint has a glyph in its state's colour; the
/// profile is filled. The sheet itself needs a GPU, so the overlay is
/// checked here and the request's errors through `Session::snapshot`.
#[test]
fn a_sketch_snapshot_shows_entities_and_constraint_states() {
    let text =
        std::fs::read(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/sketch-states.scad"))
            .unwrap();
    let (s, _) = session(&[("m.scad", &text)]);
    let r = s.evaluate(&run("m.scad", true), false).unwrap();
    let all = &r.log.sketches.0;
    let scheme = render::ColorScheme::cornfield();
    let loose = session::sketches::find(all, "loose").unwrap()[0];
    // The facts: per-entity freedom and per-constraint state.
    let free: Vec<&str> = loose["entities"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["free"] == true)
        .filter_map(|e| e["name"].as_str())
        .collect();
    assert_eq!(free, ["b", "c", "l2", "l3", "l4", "diag", "h.center", "h"]);
    let states: Vec<(&str, &str)> = loose["constraints"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| (c["text"].as_str().unwrap(), c["status"].as_str().unwrap()))
        .collect();
    assert!(
        states.contains(&("horizontal(o, a)", "redundant")),
        "{states:?}"
    );
    assert!(
        states.contains(&("length(l1, 20)", "satisfied")),
        "{states:?}"
    );
    let sheet = session::snapshot_sketch::sheet(loose, &scheme).unwrap();
    let o = &sheet.overlay;
    // Four profile lines, the construction diagonal and the circle.
    assert_eq!(o.strokes.len(), 6);
    assert_eq!(o.strokes.iter().filter(|s| s.dashed).count(), 1);
    let orange = [225, 115, 0];
    assert_eq!(o.strokes.iter().filter(|s| s.color == orange).count(), 5);
    let glyphs: Vec<&str> = o
        .labels
        .iter()
        .filter(|l| l.boxed)
        .map(|l| l.text.as_str())
        .collect();
    assert!(
        glyphs.contains(&"L 20") && glyphs.contains(&"R 2"),
        "{glyphs:?}"
    );
    assert_eq!(sheet.summary["constraints"]["redundant"], 1);
    assert_eq!(sheet.summary["free"].as_array().unwrap().len(), 8);
    assert_eq!(sheet.scene.surfaces().len(), 1, "the profile, filled");
    // A conflict: its constraints and entities red, no profile.
    let conflict = session::sketches::find(all, "conflict").unwrap()[0];
    let sheet = session::snapshot_sketch::sheet(conflict, &scheme).unwrap();
    let red = [200, 30, 30];
    assert!(sheet.overlay.strokes.iter().all(|s| s.color == red));
    assert!(
        sheet
            .overlay
            .labels
            .iter()
            .filter(|l| l.text == "L 20" || l.text == "d 25")
            .all(|l| l.color == red)
    );
    assert!(sheet.scene.surfaces().is_empty());
    // The request: an unknown name lists the names; `--sketch` with
    // `--issues` is refused.
    let mut req = session::snapshot::SnapshotRequest::new(run("m.scad", true), "x.png");
    req.sketch = Some("nope".into());
    let e = s.snapshot(&req).unwrap_err().to_string();
    assert_eq!(e, "no sketch 'nope' (sketches: loose, conflict)");
    req.sketch = Some("loose".into());
    req.issues = Some(Default::default());
    let e = s.snapshot(&req).unwrap_err().to_string();
    assert!(e.contains("cannot be combined"), "{e}");
}
