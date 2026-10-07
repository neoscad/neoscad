//! Queries (`--enable query`, docs/language-extensions.md sections 5.2 to
//! 5.4): the golden models in `conformance/extensions/query` (their
//! console output and `.csg`), the names with the flag off and their JSON
//! hints, the `.csg` export as plain OpenSCAD, and a warm session's export
//! equal to a cold one. For the queries that render (`child_bounds()`,
//! `child_measure()`, through `session::oracle`): an export byte-identical
//! to the model with the answers written in, warm and cold; the child's
//! warnings printed once; the same answers in preview and render; 1, 2 and
//! 8 threads; and the limits. (That anchors and queries change no output
//! is in `crates/eval/tests/query.rs`; the render-free models' thread
//! counts in `crates/geom/tests/query.rs`.)
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
    for name in [
        "plate.scad",
        "sketch.scad",
        "reuse.scad",
        "plate-bounds.scad",
        "bounds.scad",
    ] {
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

// --- Geometry queries (`child_bounds()`, `child_measure()`) ---------------

/// Flag off, the geometry queries are OpenSCAD's unknown functions too,
/// and their JSON hints name the flag.
#[test]
fn off_geometry_queries_are_unknown() {
    let src = b"module m() { echo(child_bounds(0), child_measure(0)); children(); }\n\
                m() cube(1);\n";
    let (s, _) = session(&[("m.scad", src)]);
    let (log, _) = evaluate(&s, "m.scad", false);
    assert_eq!(
        log,
        "WARNING: Ignoring unknown function 'child_bounds' in file m.scad, line 1\n\
         WARNING: Ignoring unknown function 'child_measure' in file m.scad, line 1\n\
         ECHO: undef, undef\n"
    );
    let r = s.evaluate(&run("m.scad", false), false).unwrap();
    let d = r.log.diagnostics_json();
    for (i, name) in [(0, "child_bounds"), (1, "child_measure")] {
        let hint = d[i]["hints"][0]["message"].as_str().unwrap();
        assert!(
            hint.contains("--enable query") && hint.contains(name),
            "{hint}"
        );
    }
}

/// The bounds `child_bounds(0)` gives in `plate-bounds.scad`, found by
/// rendering its child on its own, written so they parse back to the same
/// numbers.
fn gear_bounds() -> String {
    let text =
        String::from_utf8(std::fs::read(goldens().join("plate-bounds.scad")).unwrap()).unwrap();
    let gear = text.split("module plate_for").next().unwrap().to_string()
        + "translate([10, 5, 0]) gear(teeth = 17);\n";
    let (s, _) = session(&[("gear.scad", gear.as_bytes())]);
    let scheme = render::ColorScheme::cornfield();
    let r = s
        .render(&run("gear.scad", true), Mode::Render, &scheme)
        .unwrap();
    match session::oracle::facts(r.geometry.as_ref()) {
        eval::Facts::Solid { min, max, .. } => format!("{:?}", [min, max]),
        f => panic!("{f:?}"),
    }
}

/// The model of `plate-bounds.scad` with its query replaced by the
/// numbers it answers: no query renders anything.
fn plate_without_query() -> String {
    let text =
        String::from_utf8(std::fs::read(goldens().join("plate-bounds.scad")).unwrap()).unwrap();
    let with = text.replace("child_bounds(0);", &format!("{};", gear_bounds()));
    assert_ne!(with, text);
    with
}

/// A query's render shares the geometry cache with the final render, and
/// changes no byte of the export: the model exports the same STL as its
/// twin with the answers written in (which renders no query), cold, warm
/// after other renders, and warm after the twin itself. (Original IDs
/// decide the order of a solid's triangles; a cached entry's are rebased
/// onto each render's blocks, which is what this proves holds for query
/// renders too, so the scratch-renderer fallback of the design is not
/// needed.)
#[test]
fn a_query_changes_no_exported_byte() {
    let text = std::fs::read(goldens().join("plate-bounds.scad")).unwrap();
    let twin = plate_without_query();
    let (cold, _) = session(&[("m.scad", &text)]);
    let c = export(&cold, run("m.scad", true), "stl");
    let (plain, _) = session(&[("m.scad", twin.as_bytes())]);
    let p = export(&plain, run("m.scad", false), "stl");
    assert!(c == p, "the query changed the export");
    // Warm: the twin first, then the query model in the same session,
    // and the other way round.
    let (warm, fs) = session(&[("m.scad", twin.as_bytes())]);
    export(&warm, run("m.scad", false), "stl");
    fs.insert("/doc/m.scad", text.clone());
    assert!(export(&warm, run("m.scad", true), "stl") == c);
    fs.insert("/doc/m.scad", twin.into_bytes());
    assert!(export(&warm, run("m.scad", false), "stl") == c);
}

/// Warm equals cold for a model whose queries render: a session that
/// rendered other gears (so other queries) first exports the same bytes
/// as a fresh one, twice.
#[test]
fn a_warm_query_export_equals_a_cold_one() {
    let text =
        String::from_utf8(std::fs::read(goldens().join("plate-bounds.scad")).unwrap()).unwrap();
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

/// The queried child's geometry warnings are printed once, by the final
/// render, where the same model without the query prints them: a query
/// render's messages are dropped, and the final render replays those of
/// the cached nodes it reuses.
#[test]
fn a_queried_childs_warnings_print_once() {
    let model = |query: bool| {
        format!(
            "module show() {{ {} children(); }}\n\
             show() {{ cube(1); square(5); }}\n\
             show() translate([3, 0, 0]) {{ cube(1); square(2); }}\n",
            if query { "b = child_bounds();" } else { "" }
        )
    };
    let (s, _) = session(&[("q.scad", model(true).as_bytes())]);
    let (p, _) = session(&[("p.scad", model(false).as_bytes())]);
    let scheme = render::ColorScheme::cornfield();
    let with = s
        .render(&run("q.scad", true), Mode::Render, &scheme)
        .unwrap();
    let without = p
        .render(&run("p.scad", true), Mode::Render, &scheme)
        .unwrap();
    let with = String::from_utf8_lossy(&with.log.stderr).replace("q.scad", "p.scad");
    let without = String::from_utf8_lossy(&without.log.stderr).into_owned();
    assert!(with.contains("Mixing 2D and 3D"), "{with}");
    assert_eq!(with, without);
    // The same request again, warm, prints the same.
    let again = s
        .render(&run("q.scad", true), Mode::Render, &scheme)
        .unwrap();
    let again = String::from_utf8_lossy(&again.log.stderr).replace("q.scad", "p.scad");
    assert_eq!(again, without);
}

/// The answers are the same in a preview (`Session::evaluate` evaluates
/// as a preview) as in an export, which renders: a query measures what a
/// render makes, `%` children left out and `#` ones kept.
#[test]
fn preview_and_render_answer_alike() {
    for name in ["bounds.scad", "plate-bounds.scad"] {
        let text = std::fs::read(goldens().join(name)).unwrap();
        let (s, _) = session(&[(name, &text)]);
        let (preview, _) = evaluate(&s, name, true);
        let echoes = |log: &str| -> Vec<String> {
            log.lines()
                .filter(|l| l.starts_with("ECHO:"))
                .map(String::from)
                .collect()
        };
        let mut r = run(name, true);
        r.limits = Some(session::Limits::AGENT);
        let scheme = render::ColorScheme::cornfield();
        let rendered = s.render(&r, Mode::Render, &scheme).unwrap();
        let rendered = String::from_utf8_lossy(&rendered.log.stderr).into_owned();
        assert_eq!(echoes(&preview), echoes(&rendered), "{name}");
        assert!(!echoes(&preview).is_empty());
    }
}

/// Query models export the same bytes at 1, 2 and 8 threads.
#[test]
fn query_exports_are_the_same_at_any_thread_count() {
    let names = ["plate-bounds.scad", "bounds.scad"];
    let all = || -> Vec<Vec<u8>> {
        names
            .iter()
            .map(|n| {
                let text = std::fs::read(goldens().join(n)).unwrap();
                let (s, _) = session(&[(n, &text)]);
                export(&s, run(n, true), "stl")
            })
            .collect()
    };
    let first = all();
    for threads in [1, 2, 8] {
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(threads)
            .stack_size(eval::DEFAULT_THREAD_STACK)
            .build()
            .unwrap();
        for _ in 0..2 {
            assert!(
                pool.install(all) == first,
                "a query model exports differently on {threads} threads"
            );
        }
    }
}

/// The limits stop a query: `queries` counts the renders, and a query
/// render that passes a count limit stops the request with that limit's
/// error at the node that passed it.
#[test]
fn limits_stop_queries() {
    let text = std::fs::read(goldens().join("bounds.scad")).unwrap();
    let (s, _) = session(&[("b.scad", &text)]);
    let mut r = run("b.scad", true);
    r.limits = Some(session::Limits {
        queries: Some(5),
        ..session::Limits::AGENT
    });
    let e = s.evaluate(&r, false).unwrap();
    let log = String::from_utf8_lossy(&e.log.stderr).into_owned();
    assert!(
        log.contains(
            "ERROR: Resource limit exceeded: child_measure() would make 6 geometry queries, over the queries limit of 5 in file b.scad, line 8"
        ),
        "{log}"
    );
    assert_ne!(e.exit_code, 0);
    let src = b"module m() { b = child_bounds(0); echo(b); children(0); }\n\
                m() union() {\n  cube(1);\n  sphere(5, $fn = 200);\n}\n";
    let (s, _) = session(&[("t.scad", src)]);
    let mut r = run("t.scad", true);
    r.limits = Some(session::Limits {
        triangles: Some(1_000),
        ..session::Limits::AGENT
    });
    let e = s.evaluate(&r, false).unwrap();
    let log = String::from_utf8_lossy(&e.log.stderr).into_owned();
    assert!(
        log.starts_with("ERROR: Resource limit exceeded: sphere() would make")
            && log.contains("triangles limit of 1,000 in file t.scad, line 4"),
        "{log}"
    );
    assert!(!log.contains("ECHO"), "{log}");
}

/// Every `openscad` example in `docs/geometry-queries.md` evaluates with
/// the queries on, without an error, to exactly the console output in the
/// `text` block after it (when the block shows output rather than a
/// command line).
#[test]
fn docs_examples_evaluate() {
    let doc = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/geometry-queries.md"),
    )
    .unwrap();
    // (first line, source, expected output)
    let mut examples: Vec<(usize, String, Option<String>)> = Vec::new();
    let mut open: Option<(usize, bool, String)> = None;
    for (i, line) in doc.lines().enumerate() {
        match (&mut open, line.strip_prefix("```")) {
            (None, Some(info)) if info == "openscad" || info == "text" => {
                open = Some((i + 1, info == "openscad", String::new()));
            }
            (Some(_), Some("")) => {
                let (at, is_source, text) = open.take().unwrap();
                if is_source {
                    examples.push((at, text, None));
                } else if text.starts_with("ECHO") || text.starts_with("WARNING") {
                    examples.last_mut().expect("a source before its output").2 = Some(text);
                }
            }
            (Some((_, _, text)), _) => {
                text.push_str(line);
                text.push('\n');
            }
            _ => {}
        }
    }
    assert!(examples.len() >= 5, "{} examples", examples.len());
    let shown = examples.iter().filter(|e| e.2.is_some()).count();
    assert!(shown >= 5, "{shown} examples show their output");
    let mut failures = Vec::new();
    for (line, text, expect) in &examples {
        let (s, _) = session(&[("example.scad", text.as_bytes())]);
        let r = s.evaluate(&run("example.scad", true), false).unwrap();
        let got = String::from_utf8_lossy(&r.log.stderr).into_owned();
        let errors = r
            .log
            .diagnostics_json()
            .iter()
            .any(|d| d["severity"] == "error");
        if r.aborted || errors || expect.as_ref().is_some_and(|e| *e != got) {
            failures.push(format!(
                "docs/geometry-queries.md:{line}: expected\n{}\ngot\n{got}",
                expect.as_deref().unwrap_or("(no error)")
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
