//! STEP export with exact surfaces (`geom::exact`, `--enable exact`): the
//! `$fn` rule, transforms, determinism at any thread count and with a warm
//! or cold cache, and pinned bytes for a few models.

use std::path::{Path, PathBuf};

use geom::exact::{ExactExport, ExactOptions, SubstitutionKind, meshbrep};
use geom::{RenderOptions, Renderer};
use sha2::{Digest, Sha256};

fn cases() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../conformance/extensions/exact")
}

fn tree(src: &str) -> eval::Evaluation {
    let path = PathBuf::from("/nonexistent/test.scad");
    let program = lang::parse_file(path, src.as_bytes().to_vec());
    assert!(!program.has_syntax_errors(), "syntax error in test program");
    let mut out = eval::Collect::default();
    eval::with_stack(eval::DEFAULT_THREAD_STACK, || {
        eval::evaluate(
            &program,
            &[],
            &[],
            PathBuf::from("/nonexistent"),
            &eval::Options::default(),
            &mut out,
        )
    })
}

fn step_options() -> meshbrep::StepOptions {
    meshbrep::StepOptions {
        product_name: "test".into(),
        file_name: "test.step".into(),
        originating_system: "NeoSCAD".into(),
        ..Default::default()
    }
}

/// The normal render and the exact export of `src` with `renderer`.
fn export_with(renderer: &Renderer, src: &str) -> Result<ExactExport, String> {
    let ev = tree(src);
    let keys = eval::dump::Keys::new(&ev.root, &lang::loader::StdFs);
    let opts = RenderOptions::default();
    let normal = renderer
        .render(&ev.root, &keys, opts.clone())
        .expect("supported")
        .geometry
        .expect("geometry");
    let x = ExactOptions {
        step: step_options(),
        clock: None,
    };
    geom::exact::export_step(renderer, &ev.root, &keys, &opts, &normal, &x).map_err(|f| f.message)
}

fn export(src: &str) -> ExactExport {
    export_with(&Renderer::new(), src).unwrap_or_else(|e| panic!("{src}: {e}"))
}

fn count(step: &str, entity: &str) -> usize {
    step.matches(&format!("={entity}(")).count()
}

fn sha(s: &str) -> String {
    Sha256::digest(s.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// The owner's rule: a curve is exact unless `$fn` is set. `$fa`/`$fs`
/// fragments become a true cylinder, cone or sphere (reported as an
/// exact substitution); an explicit `$fn` keeps OpenSCAD's polygon, which
/// is planar and so exact as it is (reported as kept).
#[test]
fn fn_set_keeps_the_polygon_and_fa_fs_curves_become_exact() {
    use std::f64::consts::PI;
    let e = export("cylinder(r=5, h=10);");
    assert_eq!(count(&e.step, "CYLINDRICAL_SURFACE"), 1);
    assert_eq!(e.substitutions.len(), 1);
    assert_eq!(e.substitutions[0].kind, SubstitutionKind::Exact);
    assert!(
        e.substitutions[0].detail.contains("16-sided"),
        "{:?}",
        e.substitutions
    );
    assert!((e.stats.volume - 250.0 * PI).abs() < 1e-9 * e.stats.volume);

    let e = export("$fa = 6; $fs = 0.5; sphere(5);");
    assert_eq!(count(&e.step, "SPHERICAL_SURFACE"), 1);
    assert!((e.stats.volume - 500.0 * PI / 3.0).abs() < 1e-9 * e.stats.volume);

    let e = export("cylinder(r=5, h=10, $fn=6);");
    assert_eq!(count(&e.step, "CYLINDRICAL_SURFACE"), 0);
    assert_eq!(e.stats.faces, 8);
    assert_eq!(e.stats.exact_faces, 8);
    assert_eq!(e.substitutions[0].kind, SubstitutionKind::Polygon);
    // The hexagonal prism's own volume, not the circle's.
    let hex = 1.5 * 3f64.sqrt() * 25.0 * 10.0;
    assert!((e.stats.volume - hex).abs() < 1e-9 * hex);

    let e = export("sphere(5, $fn=12);");
    assert_eq!(count(&e.step, "SPHERICAL_SURFACE"), 0);
    assert_eq!(e.substitutions[0].kind, SubstitutionKind::Polygon);

    // A cone, and a mixture: the substitutions are per source location.
    let e = export("cylinder(r1=5, r2=2, h=10); translate([20,0,0]) cylinder(r=3, h=4, $fn=8);");
    assert_eq!(count(&e.step, "CONICAL_SURFACE"), 1);
    let kinds: Vec<_> = e.substitutions.iter().map(|s| s.kind).collect();
    assert_eq!(kinds, [SubstitutionKind::Exact, SubstitutionKind::Polygon]);
}

/// Transforms that keep circles circles (rotations, mirrors, uniform
/// scales) keep the surfaces exact; a non-uniform scale makes an ellipse,
/// which falls back to facets with a report. Planes stay exact under any
/// transform.
#[test]
fn similarities_stay_exact_and_other_scales_fall_back() {
    use std::f64::consts::PI;
    let e = export("scale(2) rotate([30, 40, 50]) mirror([1, 0, 0]) sphere(5);");
    assert_eq!(count(&e.step, "SPHERICAL_SURFACE"), 1);
    assert!((e.stats.volume - 4000.0 * PI / 3.0).abs() < 1e-9 * e.stats.volume);
    assert!(
        e.substitutions
            .iter()
            .all(|s| s.kind == SubstitutionKind::Exact)
    );

    let e = export("scale([1, 2, 1]) sphere(5);");
    assert_eq!(count(&e.step, "SPHERICAL_SURFACE"), 0);
    assert_eq!(e.substitutions[0].kind, SubstitutionKind::Faceted);
    assert!(e.substitutions[0].detail.contains("ellipsoid"));
    assert_eq!(e.stats.exact_faces, 0);

    let e = export("multmatrix([[1, 0.5, 0, 0], [0, 1, 0, 0], [0, 0, 2, 0]]) cube(10);");
    assert_eq!(e.stats.faces, 6);
    assert_eq!(e.stats.exact_faces, 6);
    assert!((e.stats.volume - 2000.0).abs() < 1e-9 * 2000.0);
    assert!(e.substitutions.is_empty());
}

/// `color()`, `render()` and booleans do not lose a face's surface, and
/// mesh-only constructs mix with exact faces in one solid.
#[test]
fn surfaces_survive_color_render_and_mix_with_facets() {
    let e = export(
        "color(\"red\") render() difference() { cube(20); translate([10, 10, -1]) color(\"blue\") cylinder(r=4, h=22); }",
    );
    assert_eq!(count(&e.step, "CYLINDRICAL_SURFACE"), 1);
    assert_eq!(e.stats.faces, 7);
    let e = export(
        "union() { hull() { cube(10); translate([5, 5, 5]) cube(10); } translate([5, 5, 0]) cylinder(r=3, h=30); }",
    );
    assert_eq!(count(&e.step, "CYLINDRICAL_SURFACE"), 1);
    let faceted: Vec<_> = e
        .substitutions
        .iter()
        .filter(|s| s.kind == SubstitutionKind::Faceted)
        .map(|s| s.module)
        .collect();
    assert_eq!(faceted, ["hull"]);
}

/// Mesh exports and the export render share nothing that the export
/// could change: rendering the same tree normally before and after an
/// exact export gives the same mesh.
#[test]
fn the_normal_render_is_untouched() {
    let src = "difference() { cube(20, center=true); sphere(12); }";
    let ev = tree(src);
    let keys = eval::dump::Keys::new(&ev.root, &lang::loader::StdFs);
    let stl = |r: &Renderer| {
        let g = r
            .render(&ev.root, &keys, RenderOptions::default())
            .unwrap()
            .geometry
            .unwrap();
        let ps = geom::export::as_polyset(&g, &geom::color::CORNFIELD).unwrap();
        geom::export::stl(&ps, true, false, &mut Vec::new())
    };
    let r = Renderer::new();
    let before = stl(&r);
    export_with(&r, src).unwrap();
    assert!(stl(&r) == before);
    assert!(stl(&Renderer::new()) == before);
}

fn audit_case(name: &str) -> String {
    std::fs::read_to_string(cases().join(format!("{name}.scad"))).unwrap()
}

/// The same STEP bytes at 1, 2 and 8 threads, and with a cold cache, a
/// warm one, and one warmed by a different model first.
#[test]
fn step_bytes_are_the_same_at_any_thread_count_and_cache_state() {
    let models: Vec<String> = ["c01", "c14", "x02", "x07", "f02"]
        .iter()
        .map(|n| audit_case(n))
        .collect();
    let cold = |m: &String| export(m).step;
    let first: Vec<String> = models.iter().map(cold).collect();
    for threads in [1, 2, 8] {
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(threads)
            .stack_size(eval::DEFAULT_THREAD_STACK)
            .build()
            .unwrap();
        for _ in 0..2 {
            let again: Vec<String> = pool.install(|| models.iter().map(cold).collect());
            assert!(again == first, "STEP differs on {threads} threads");
        }
    }
    // Warm: one renderer for every model, twice over, in reverse order the
    // second time.
    let r = Renderer::new();
    for (m, want) in models.iter().zip(&first) {
        assert!(&export_with(&r, m).unwrap().step == want);
    }
    for (m, want) in models.iter().zip(&first).rev() {
        assert!(&export_with(&r, m).unwrap().step == want);
    }
}

/// Pinned bytes. A change to reconstruction or the writer that alters
/// them must be deliberate: check the new files (OCCT read-back,
/// `conformance exact`) and update the hashes.
#[test]
fn golden_step_hashes() {
    let pinned = [
        (
            "b01",
            "eb613a0e82645cc55b998cd8ef2f29840917024fd0d9de23bb9cc6a6a7d5783a",
        ),
        (
            "c14",
            "0692a3f082d5de193a81aeae35d0cdaa1d72db801138838b681842860dbf67b2",
        ),
        (
            "x02",
            "082e91e4bf44dff7df9d5cce9ee5d2edf467b427ca49e1ae0f9536ec60d7fd4a",
        ),
        (
            "x07",
            "aa023e903d443cc12b5cc26c650d38b3ae4cfff9a5849aaaa40946482d9a76a9",
        ),
    ];
    let mut wrong = Vec::new();
    for (name, want) in pinned {
        let got = sha(&export(&audit_case(name)).step);
        if got != want {
            wrong.push(format!("(\"{name}\", \"{got}\"),"));
        }
    }
    assert!(wrong.is_empty(), "new hashes:\n{}", wrong.join("\n"));
}

/// Every audit case at OpenSCAD's defaults: valid, exact where it can be,
/// and within 1e-6 of its closed-form volume (gate 3).
#[test]
fn audit_cases_match_their_closed_forms() {
    let mut names: Vec<PathBuf> = std::fs::read_dir(cases())
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|x| x == "scad"))
        .collect();
    names.sort();
    assert_eq!(names.len(), 28);
    for p in names {
        let src = std::fs::read_to_string(&p).unwrap();
        let e = export(&src);
        if let Some(v) = src
            .lines()
            .find_map(|l| l.strip_prefix("// volume: "))
            .and_then(|v| v.trim().parse::<f64>().ok())
        {
            let rel = (e.stats.volume - v).abs() / v;
            assert!(
                rel < 1e-6,
                "{}: {} vs {v} ({rel:.1e})",
                p.display(),
                e.stats.volume
            );
        }
    }
}

/// Bodies that touch along an edge or at a corner stay separate solids
/// (Manifold keeps them apart), and an inside-out body is refused rather
/// than written as a cavity with nothing around it.
#[test]
fn touching_bodies_export_and_inside_out_bodies_are_refused() {
    let e = export("cube(10); translate([10, 10, 0]) cube(10);");
    assert_eq!(e.step.matches("=MANIFOLD_SOLID_BREP(").count(), 2);
    let e = export_with(
        &Renderer::new(),
        "polyhedron(points = [[1,0,0],[-1,0,0],[0,1,0],[0,-1,0],[0,0,1],[0,0,-1]], faces = [[0,2,4],[0,5,2],[0,4,3],[0,3,5],[1,4,2],[1,2,5],[1,3,4],[1,5,3]]);",
    )
    .unwrap_err();
    assert!(e.contains("inside out"), "{e}");
}

/// A tree as deep as recursive modules make them exports on a small
/// stack: the export render walks it iteratively, as the normal render
/// does, so a model the normal render handles cannot overflow it.
#[test]
fn a_deep_tree_exports_on_a_small_stack() {
    let ev = tree(
        "module r(n) if (n > 0) translate([0, 0, 0.001]) r(n - 1); else cylinder(r = 2, h = 3);\nr(5000);",
    );
    let keys = eval::dump::Keys::new(&ev.root, &lang::loader::StdFs);
    let step = std::thread::Builder::new()
        .stack_size(1 << 20)
        .spawn(move || {
            let r = Renderer::new();
            let opts = RenderOptions::default();
            let normal = r
                .render(&ev.root, &keys, opts.clone())
                .unwrap()
                .geometry
                .unwrap();
            let x = ExactOptions {
                step: step_options(),
                clock: None,
            };
            let e = geom::exact::export_step(&r, &ev.root, &keys, &opts, &normal, &x)
                .map_err(|f| f.message)
                .unwrap();
            drop(ev);
            e.step
        })
        .unwrap()
        .join()
        .unwrap();
    assert!(step.contains("CYLINDRICAL_SURFACE"));
}
