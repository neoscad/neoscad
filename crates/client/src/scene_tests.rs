//! The preview's scene (`run_scene`): what it costs on the web demo's
//! heavy examples, cold and warm, and that a warm one is the cold one.

use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use lang::loader::{FileSystem, LibraryPath, StdFs};

use super::*;

/// The repository root (this crate is `crates/client`).
fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// A client over the real file system, with the bundled libraries and
/// `.reference` (for BOSL2) on the library path, a clock, and limits that
/// stop a runaway well before it fills the machine.
fn disk_client() -> Client {
    let fs: Arc<dyn FileSystem + Send + Sync> =
        Arc::new(assets::libraries(Arc::new(StdFs), "/neoscad/libraries"));
    let libs = LibraryPath(vec![
        repo().join(".reference"),
        PathBuf::from("/neoscad/libraries"),
    ]);
    let mut cfg = session::Config::new(fs, libs);
    let t0 = Instant::now();
    cfg.clock = Some(Arc::new(move || t0.elapsed().as_secs_f64() * 1000.0));
    cfg.limits = session::Limits {
        memory: Some(2 << 30),
        time: Some(300.0),
        ..session::Limits::NONE
    };
    Client::new(cfg)
}

/// Fails the run once the process passes 2 GB resident (`ps`), so a
/// regression cannot fill the machine's swap.
fn memory_guard(step: &str) {
    let out = std::process::Command::new("ps")
        .args(["-o", "rss=", "-p", &std::process::id().to_string()])
        .output()
        .expect("ps");
    let kib: u64 = String::from_utf8_lossy(&out.stdout)
        .trim()
        .parse()
        .unwrap_or(0);
    assert!(
        kib < 2 << 20,
        "{step}: {kib} KiB resident, over the 2 GB guard"
    );
}

/// One preview: the session's timings, the scene's own time (measured
/// here), and a hash of the packed scene, which is what reaches a viewer.
struct Previewed {
    session_ms: f64,
    scene_ms: f64,
    reported: Timings,
    packed: u64,
}

fn preview(c: &Client, path: &str, overrides: Vec<ParameterOverride>) -> Previewed {
    let req = DocumentRequest {
        mode: RenderMode::Preview,
        overrides,
        parts: false,
        enable: Vec::new(),
    };
    let scheme = render::ColorScheme::cornfield();
    let (run, _, _) = c.document_run(path, &req).unwrap();
    let r = c.session.render(&run, req.mode.into(), &scheme).unwrap();
    assert_eq!(r.exit_code, 0, "{:?}", r.log.diagnostics_json());
    let t = Instant::now();
    let scene = run_scene(&r, &scheme, render::Previewer::OpenCsg)
        .unwrap()
        .expect("a scene");
    let scene_ms = t.elapsed().as_secs_f64() * 1000.0;
    let p = scene.pack();
    let mut h = std::hash::DefaultHasher::new();
    p.faces.hash(&mut h);
    p.edges.hash(&mut h);
    p.meta.to_json().hash(&mut h);
    Previewed {
        session_ms: r.timings.total,
        scene_ms,
        reported: render_result(&r, &scheme).timings,
        packed: h.finish(),
    }
}

/// A model with each kind of product the cache keys: coloured wedges that
/// share one cutter (converted once, `geom::csg`'s shared negatives), a
/// cube minus copies of a subtree with unions of its own (a plan,
/// `geom::shared`), a 2D difference drawn as slabs, a highlighted one,
/// and a cube coloured inside its difference (only the positive's colour
/// tells two of them apart). `hole` is the radius of the 2D difference's
/// hole, which the tests edit to change that product alone, and `green`
/// the colour of the wedges and that cube, which changes only colours.
fn products_model(hole: f64, green: f64) -> String {
    format!(
        "module holes() for (a = [[0, 0, 0], [90, 0, 0], [0, 90, 0]]) \
             rotate(a) cylinder(h = 12, r = 0.8, center = true, $fn = 8);\n\
         for (i = [0 : 5]) color([i / 6, {green}, 1 - i / 6]) difference() {{\n\
             rotate(i * 60) rotate_extrude(angle = 60, $fn = 36) translate([5, 0]) circle(2, $fn = 12);\n\
             rotate_extrude($fn = 36) translate([5, 0]) rotate(45) square(1.2, center = true);\n\
         }}\n\
         translate([0, 20, 0]) difference() {{\n\
             cube(9, center = true);\n\
             for (t = [[0, 0, 0], [3, 0, 0], [-3, 0, 0]]) translate(t) holes();\n\
         }}\n\
         translate([0, -20, 0]) difference() {{ square(5); translate([2, 2]) circle({hole}, $fn = 10); }}\n\
         #translate([20, 0, 0]) difference() {{ sphere(3, $fn = 16); cube(3); }}\n\
         translate([0, 0, 15]) difference() {{ color([{green}, 0.2, 0.2]) cube(4, center = true); sphere(2.5, $fn = 12); }}\n"
    )
}

/// The packed scene of a preview of `path`, as a viewer receives it.
fn packed(scene: &render::Scene) -> Vec<u8> {
    let p = scene.pack();
    let mut out = p.faces;
    out.extend(p.edges);
    out.extend(p.meta.to_json().into_bytes());
    out
}

/// A preview of `path` through `run_scene`, with the session's entries
/// in the geometry cache before and after the scene.
fn cached_preview(c: &Client, path: &str) -> (Vec<u8>, usize, usize) {
    let req = DocumentRequest {
        mode: RenderMode::Preview,
        overrides: Vec::new(),
        parts: false,
        enable: Vec::new(),
    };
    let scheme = render::ColorScheme::cornfield();
    let (run, _, _) = c.document_run(path, &req).unwrap();
    let r = c.session.render(&run, req.mode.into(), &scheme).unwrap();
    assert_eq!(r.exit_code, 0, "{:?}", r.log.diagnostics_json());
    let before = c.session.stats().geometry.entries;
    let scene = run_scene(&r, &scheme, render::Previewer::OpenCsg)
        .unwrap()
        .expect("a scene");
    (packed(&scene), before, c.session.stats().geometry.entries)
}

/// A preview of `text` with no product cache, in a fresh session.
fn uncached_preview(text: String) -> Vec<u8> {
    let c = crate::tests::client();
    c.open(DOC, Some(text)).unwrap();
    let req = DocumentRequest {
        mode: RenderMode::Preview,
        overrides: Vec::new(),
        parts: false,
        enable: Vec::new(),
    };
    let scheme = render::ColorScheme::cornfield();
    let (run, _, _) = c.document_run(DOC, &req).unwrap();
    let r = c.session.render(&run, req.mode.into(), &scheme).unwrap();
    let tree = r.tree.as_ref().expect("a preview");
    let scene =
        render::preview::scene_until(tree, &scheme, render::Previewer::OpenCsg, &r.stop).unwrap();
    packed(&scene)
}

const DOC: &str = "/doc/products.scad";

/// A preview whose products come from the cache is the preview computed
/// without one, byte for byte: cold, unchanged, and after an edit that
/// changes one product (the rest hit), on one thread and on four.
#[test]
fn cached_previews_are_the_uncached_ones() {
    let mut runs = Vec::new();
    for threads in [1, 4] {
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(threads)
            .build()
            .unwrap();
        runs.push(pool.install(|| {
            let models = [
                products_model(1.0, 0.5),
                products_model(1.5, 0.5),
                products_model(1.5, 0.25),
            ];
            let fresh: Vec<Vec<u8>> = models.iter().cloned().map(uncached_preview).collect();
            let c = crate::tests::client();
            c.open(DOC, Some(models[0].clone())).unwrap();
            let (cold, before, after) = cached_preview(&c, DOC);
            assert!(after > before, "the products were kept");
            assert!(
                cold == fresh[0],
                "a cold cached preview differs ({threads} threads)"
            );
            let (warm, before, after) = cached_preview(&c, DOC);
            assert_eq!(after, before, "an unchanged re-preview computed a product");
            assert!(
                warm == fresh[0],
                "a warm preview differs ({threads} threads)"
            );
            c.update(DOC, models[1].clone()).unwrap();
            let (edited, before, after) = cached_preview(&c, DOC);
            assert_eq!(after, before + 1, "the edit changed one product");
            assert!(
                edited == fresh[1],
                "an edited preview differs ({threads} threads)"
            );
            // The wedges' colours: the same meshes, so only the key tells
            // them apart.
            c.update(DOC, models[2].clone()).unwrap();
            let (recoloured, before, after) = cached_preview(&c, DOC);
            assert_eq!(after, before + 7, "the edit recoloured seven products");
            assert!(
                recoloured == fresh[2],
                "a recoloured preview differs ({threads} threads)"
            );
            fresh
        }));
    }
    assert!(runs[0] == runs[1], "the thread count changed a scene");
}

/// `render_result`'s timings count the scene `run_scene` built as
/// geometry, so the apps' "Previewed in" includes the booleans.
#[test]
fn run_timings_include_the_scene() {
    let files: Arc<dyn FileSystem + Send + Sync> = Arc::new(lang::vfs::MemFs::new());
    let mut cfg = session::Config::new(files, LibraryPath(Vec::new()));
    // A clock that moves on with every reading, so any step takes time.
    let ticks = Arc::new(std::sync::atomic::AtomicU64::new(0));
    cfg.clock = Some(Arc::new(move || {
        ticks.fetch_add(1, std::sync::atomic::Ordering::Relaxed) as f64
    }));
    let c = Client::new(cfg);
    c.open(DOC, Some(products_model(1.0, 0.5))).unwrap();
    let req = DocumentRequest {
        mode: RenderMode::Preview,
        overrides: Vec::new(),
        parts: false,
        enable: Vec::new(),
    };
    let scheme = render::ColorScheme::cornfield();
    let (run, _, _) = c.document_run(DOC, &req).unwrap();
    let r = c.session.render(&run, req.mode.into(), &scheme).unwrap();
    let before = render_result(&r, &scheme).timings;
    assert_eq!(before.total_ms, r.timings.total);
    run_scene(&r, &scheme, render::Previewer::OpenCsg).unwrap();
    let ms = r.scene.ms();
    assert!(ms > 0.0);
    let after = render_result(&r, &scheme).timings;
    assert_eq!(after.geometry_ms, r.timings.geometry + ms);
    assert_eq!(after.total_ms, r.timings.total + ms);
    assert_eq!(after.evaluate_ms, before.evaluate_ms);
}

/// The web demo's heavy previews natively, cold, again unchanged, after a
/// text edit that changes one product, and after a parameter that changes
/// every product. Prints a line per step; run with
///
///     cargo test --release -p neoscad-client preview_timings -- --ignored --nocapture
///
/// Needs `.reference/BOSL2` for the gearbox.
#[test]
#[ignore = "a benchmark: seconds per model, and needs .reference"]
fn preview_timings() {
    let cases: [(&str, &str, &str, (&str, f64)); 2] = [
        (
            "threaded-ring.scad",
            // A new product beside the 36 wedges.
            "",
            "\ntranslate([20, 0, 0]) difference() { cube(4, center = true); sphere(2.5); }\n",
            ("turns", 5.0),
        ),
        (
            "gearbox.scad",
            // The carrier's plate only.
            "rounding = 1.2",
            "rounding = 1.4",
            ("thick", 12.0),
        ),
    ];
    for (file, edit_from, edit_to, (param, value)) in cases {
        let src = repo().join("web/examples").join(file);
        if file == "gearbox.scad" && !repo().join(".reference/BOSL2").exists() {
            eprintln!("skip {file}: no .reference/BOSL2");
            continue;
        }
        let text = std::fs::read_to_string(&src).unwrap();
        let c = disk_client();
        let path = src.to_string_lossy().into_owned();
        c.open(&path, Some(text.clone())).unwrap();
        let line = |step: &str, p: &Previewed| {
            println!(
                "{file:20} {step:12} session {:8.1} ms  scene {:8.1} ms  reported total {:8.1} ms \
                 (geometry {:8.1})  packed {:016x}",
                p.session_ms, p.scene_ms, p.reported.total_ms, p.reported.geometry_ms, p.packed
            );
            memory_guard(step);
        };
        let cold = preview(&c, &path, Vec::new());
        line("cold", &cold);
        let again = preview(&c, &path, Vec::new());
        line("again", &again);
        assert_eq!(
            cold.packed, again.packed,
            "{file}: a re-preview drew another scene"
        );
        let edited = if edit_from.is_empty() {
            format!("{text}{edit_to}")
        } else {
            assert!(text.contains(edit_from), "{file}: no '{edit_from}'");
            text.replacen(edit_from, edit_to, 1)
        };
        c.update(&path, edited).unwrap();
        let p = preview(&c, &path, Vec::new());
        line("text edit", &p);
        let o = vec![ParameterOverride {
            name: param.into(),
            value: ParameterValue::Number { value },
        }];
        let p = preview(&c, &path, o);
        line(&format!("{param}={value}"), &p);
    }
}
