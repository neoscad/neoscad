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
    tree_with(src, eval::Options::default())
}

fn tree_with(src: &str, options: eval::Options) -> eval::Evaluation {
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
            &options,
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
    export_tree(renderer, tree(src))
}

fn export_tree(renderer: &Renderer, ev: eval::Evaluation) -> Result<ExactExport, String> {
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
    let models: Vec<String> = ["c01", "c14", "x02", "x07", "f02", "e02", "e04", "e09"]
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
        (
            "e02",
            "6f075cdc292f6c098508b85c2fd955e8ca65bba0761c490327a8c882d6905e72",
        ),
        (
            "e04",
            "a8c154194cdbeceaa05cb3321a8fa95dcc8cbb791b1ab7b10245e1560c5d07bb",
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
    // The audit's 28, stage 2's extrusions e01-e10, and the stroke joint
    // j01.
    assert_eq!(names.len(), 39);
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

// Regressions of the stop-rule failure classes (`docs/followups.md`,
// "Exact geometry"). Each failed before the fix it names.

/// Two equal cones whose axes cross at their base centres are tangent at
/// a point near the bases, where their intersection crosses itself (the
/// BOSL2 `distributors` examples are six of them). The mesh's crossing
/// sits up to 0.9 mm off the exact one and used to be solved onto the
/// intersection there, so the curves on either side folded back
/// ("boundary crosses itself"); it now gets the exact tangent point.
#[test]
fn cones_crossing_at_a_tangent_point_export() {
    for a in [60, 90, 120] {
        let e = export(&format!(
            "for (i = [0:1]) rotate([{a} * i, 0, 0]) cylinder(h = 20, r1 = 5, r2 = 0);"
        ));
        assert!(e.stats.exact_faces == e.stats.faces, "{a}");
    }
}

/// A sphere cut by three planes through its centre: its boundary arcs
/// pass through the poles of every circle normal, which the frame used to
/// take as its axis (`rotate-parameters.scad`; "boundary crosses itself"
/// or a volume off by a sixth). The axis now keeps clear of the boundary.
#[test]
fn a_sphere_cut_through_its_centre_exports() {
    for src in [
        "union() { cube([1, 2, 3]); sphere(1); }",
        "union() { cube([1, 2, 3]); rotate(45) cube([1, 2, 3]); sphere(1); }",
    ] {
        let e = export(src);
        assert!(e.step.contains("SPHERICAL_SURFACE"), "{src}");
    }
}

/// A transform that flattens its child leaves no volume; the normal
/// render drops it (`issue4522.scad`), and so does the export, instead of
/// failing on a mesh that is not a closed solid.
#[test]
fn a_flattening_scale_is_left_out_as_the_render_leaves_it() {
    let e = export("cube(); scale([1, 0, 0]) cube();");
    assert_eq!(e.stats.faces, 6);
    assert!((e.stats.volume - 1.0).abs() < 1e-12);
}

/// A cube standing on a rotated prism, both under one rotation: rounding
/// leaves the mesh a tunnel of no thickness under the cube (genus 1),
/// which the exact faces close (BOSL2 `attachments__079`). Fewer handles
/// than the mesh is a note, not an error.
#[test]
fn a_tunnel_of_no_thickness_in_the_mesh_is_a_note() {
    let e = export(
        "multmatrix([[cos(20), 0, sin(20), 15], [0, 1, 0, 0], [-sin(20), 0, cos(20), 0]]) {\n  cylinder(d = 10, h = 10, center = true, $fn = 16);\n  translate([0, 0, 6.5]) cube(3, center = true);\n}",
    );
    assert!(
        e.stats
            .notes
            .iter()
            .any(|n| n.contains("less than the input mesh's")),
        "{:?}",
        e.stats.notes
    );
}

/// A rotated Menger sponge: bodies that touch along edges, which Manifold
/// keeps apart and rounding then joins on the wrong side, so a corner of
/// a face lies on another edge of the same face. The file used to pass
/// our checks and read back from OCCT with a face split and a solid that
/// would not close (`example024.scad`: 60 free edges). It is refused.
#[test]
fn a_boundary_touching_itself_is_refused() {
    let src = "module m(s, l) { cube([30, s / 3, s / 3], center = true);\n  if (l > 1) for (i = [-1:1], j = [-1:1]) if (i || j) translate([0, i * s / 3, j * s / 3]) m(s / 3, l - 1); }\nrotate([45, atan(1 / sqrt(2)), 0]) difference() { cube(27, center = true); for (v = [[0, 0, 0], [0, 0, 90], [0, 90, 0]]) rotate(v) m(27, 2); }";
    let err = export_with(&Renderer::new(), src).unwrap_err();
    assert!(err.contains("touches itself"), "{err}");
}

/// A cube cut by two others whose faces miss each other by 1e-10: the
/// mesh keeps a sliver triangle between them, which becomes a face of two
/// straight edges along one line once its short edge collapses. It has
/// no area and no orientation; it is removed and its neighbours share
/// the edge (`issue1165.scad`). The fin of 8.8e-11 between the cuts goes
/// too (the mesh's edges shorter than 1e-7 are collapsed first), leaving
/// the cube's cut side as two faces of one plane: seven, not the nine
/// the fin had.
#[test]
fn a_face_with_no_area_is_removed() {
    let e = export(
        "translate([0, 10, 0]) difference() {\n  cube(10, center = true);\n  translate([6, 5.5, 0]) cube(11, center = true);\n  translate([6, -5.500000000088, 0]) cube(11, center = true);\n}",
    );
    assert!(
        e.stats.notes.iter().any(|n| n.contains("of no area")),
        "{:?}",
        e.stats.notes
    );
    assert_eq!(e.stats.faces, 7);
}

// Stage 2: extrusions.

fn kinds(e: &ExactExport, kind: SubstitutionKind) -> Vec<&'static str> {
    e.substitutions
        .iter()
        .filter(|s| s.kind == kind)
        .map(|s| s.module)
        .collect()
}

/// Extruded and revolved profiles carry their curves: arcs become
/// cylinders, cones and tori, lines planes and cones, and the volume is
/// the closed form's.
#[test]
fn extrusions_are_exact() {
    use std::f64::consts::PI;
    let cases: [(&str, &str, f64); 5] = [
        (
            "linear_extrude(3) difference() { circle(10); circle(4); }",
            "CYLINDRICAL_SURFACE",
            252.0 * PI,
        ),
        (
            "rotate_extrude() translate([10, 0]) circle(3);",
            "TOROIDAL_SURFACE",
            180.0 * PI * PI,
        ),
        (
            "linear_extrude(10, scale = 0.5) circle(5);",
            "CONICAL_SURFACE",
            437.5 * PI / 3.0,
        ),
        // A sphere from a half disc on the axis.
        (
            "rotate_extrude() intersection() { circle(5); translate([0, -5]) square([5, 10]); }",
            "SPHERICAL_SURFACE",
            500.0 * PI / 3.0,
        ),
        // Rotated, mirrored and scaled uniformly: still exact.
        (
            "rotate([30, 40, 50]) mirror([1, 0, 0]) scale(2) rotate_extrude(angle = 120) translate([10, 0]) circle(3);",
            "TOROIDAL_SURFACE",
            8.0 * 60.0 * PI * PI,
        ),
    ];
    for (src, entity, volume) in cases {
        let e = export(src);
        assert!(count(&e.step, entity) >= 1, "{src}: no {entity}");
        assert_eq!(e.stats.exact_faces, e.stats.faces, "{src}");
        let rel = (e.stats.volume - volume).abs() / volume;
        assert!(
            rel < 1e-9,
            "{src}: {} vs {volume} ({rel:.1e})",
            e.stats.volume
        );
        assert!(
            kinds(&e, SubstitutionKind::Faceted).is_empty(),
            "{src}: {:?}",
            e.substitutions
        );
    }
}

/// The 2D tree is followed through booleans, transforms and offsets: a
/// mirrored union keeps its circle, a negative offset of a disc with a
/// square hole rounds the hole's corners with exact arcs and shrinks the
/// disc's circle, and a chamfered offset is all planes.
#[test]
fn profiles_keep_their_curves_through_2d_operations() {
    use std::f64::consts::PI;
    let e = export(
        "linear_extrude(4) mirror([1, 0]) union() { circle(5); translate([4, 0]) square([8, 3]); }",
    );
    assert_eq!(count(&e.step, "CYLINDRICAL_SURFACE"), 1);
    assert_eq!(e.stats.exact_faces, e.stats.faces);
    // offset(r = -1) of a radius-10 disc less a 4 mm square: a radius-9
    // disc less the square grown by 1 with round corners.
    let e = export(
        "linear_extrude(3) offset(r = -1) difference() { circle(10); square(4, center = true); }",
    );
    assert_eq!(count(&e.step, "CYLINDRICAL_SURFACE"), 5);
    let hole = 16.0 + 4.0 * 4.0 * 1.0 + PI;
    let want = 3.0 * (81.0 * PI - hole);
    assert!(
        (e.stats.volume - want).abs() < 1e-9 * want,
        "{} vs {want}",
        e.stats.volume
    );
    let e = export("linear_extrude(3) offset(delta = 2, chamfer = true) square(10);");
    assert_eq!(e.stats.faces, 10);
    assert_eq!(count(&e.step, "CYLINDRICAL_SURFACE"), 0);
    // Clipper squares a corner off at delta from the vertex.
    let cut = 2.0 * 2f64.sqrt() - 2.0;
    let want = 3.0 * (196.0 - 4.0 * cut * cut);
    assert!(
        (e.stats.volume - want).abs() < 1e-9 * want,
        "{} vs {want}",
        e.stats.volume
    );
}

/// What has no exact surface yet is built by the normal render and
/// reported where it comes from: a twist, a non-uniform scale, a
/// projection.
#[test]
fn twists_scales_and_text_fall_back_with_a_report() {
    let e = export("linear_extrude(10, twist = 90) square(5, center = true);");
    assert_eq!(kinds(&e, SubstitutionKind::Faceted), ["linear_extrude"]);
    let e = export("linear_extrude(10, scale = [1, 2]) circle(5);");
    assert_eq!(kinds(&e, SubstitutionKind::Faceted), ["linear_extrude"]);
    // A 2D shape only the normal render builds (text and imports too).
    let e = export("linear_extrude(2) projection() sphere(5);");
    assert_eq!(kinds(&e, SubstitutionKind::Faceted), ["projection"]);
    // An arc scaled toward a point off its centre sweeps an oblique cone.
    let e = export("linear_extrude(10, scale = 0.5) translate([8, 0]) circle(2);");
    assert_eq!(kinds(&e, SubstitutionKind::Faceted), ["linear_extrude"]);
    // An explicit $fn keeps the polygon: planes, exact as they are.
    let e = export("rotate_extrude($fn = 8) translate([8, 0]) circle(2);");
    assert!(kinds(&e, SubstitutionKind::Faceted).is_empty());
    assert_eq!(count(&e.step, "TOROIDAL_SURFACE"), 0);
}

/// A sketch's solved arcs are exact circles: the slot of
/// `docs/language-extensions.md` section 6.2, cut from a plate.
#[test]
fn a_sketch_slot_extrudes_with_exact_ends() {
    use std::f64::consts::PI;
    let src = r#"slot_len = 30;
slot_w = 8;
linear_extrude(3) difference() {
  square([50, 20], center = true);
  translate([-slot_len / 2, 0]) sketch(name = "slot") {
    c1 = point([0, 0]);
    c2 = point([slot_len, 0]);
    axis = line(c1, c2, construction = true);
    top = line([0, slot_w / 2], [slot_len, slot_w / 2]);
    bot = line([slot_len, -slot_w / 2], [0, -slot_w / 2]);
    e1 = arc(c1, top.start, bot.end);
    e2 = arc(c2, bot.start, top.end);
    fix(c1);
    horizontal(axis); length(axis, slot_len);
    tangent(e1, top); tangent(e1, bot);
    tangent(e2, top); tangent(e2, bot);
    diameter(e1, slot_w); equal(e1, e2);
  }
}"#;
    let options = eval::Options {
        extensions: eval::extensions::Extensions::default()
            .with(eval::extensions::Extension::Sketch),
        ..Default::default()
    };
    let e = export_tree(&Renderer::new(), tree_with(src, options)).unwrap();
    assert_eq!(count(&e.step, "CYLINDRICAL_SURFACE"), 2);
    let want = 3.0 * (1000.0 - 240.0 - 16.0 * PI);
    assert!(
        (e.stats.volume - want).abs() < 1e-7 * want,
        "{} vs {want}",
        e.stats.volume
    );
}

// Stage 2's corpus hardening. Each failed before the fix it names.

/// A cylinder poking 0.01 mm through a face (the overlap BOSL2 gives
/// every mask) meets the face at 2.6°. The mesh's crossing then stands a
/// quarter of a millimetre along the face from the exact one, and the
/// cross-check's bound, made for steep intersections, refused a correct
/// B-rep. The bound now divides by the angle.
#[test]
fn a_cylinder_grazing_a_face_exports() {
    let e = export(
        "difference() { cube([60, 60, 30]); translate([50, 10, 19.99]) cylinder(r = 10.01, h = 10.02); }",
    );
    // The cube less the cylinder's part inside it: two circular
    // segments of the r = 10.01 disc stand outside at x = 60 and y = 0.
    let (r, d) = (10.01f64, 10.0f64);
    let segment = r * r * (d / r).acos() - d * (r * r - d * d).sqrt();
    let want = 108000.0 - 10.01 * (std::f64::consts::PI * r * r - 2.0 * segment);
    assert!(
        (e.stats.volume - want).abs() < 1e-9 * want,
        "{} vs {want}",
        e.stats.volume
    );
}

/// One profile extruded along an edge and revolved round a corner (an
/// edge mask meeting its corner mask): the extruded lines' planes are the
/// revolved lines' cones' tangent planes along a generator where they
/// meet. That contact is now known, so the edge is that line.
#[test]
fn an_extruded_and_a_revolved_mask_meet_exactly() {
    let profile = "polygon([[10, -0.01], [-0.01, -0.01], [-0.01, 10], [-1.77636e-15, 10], [0.192147, 8.0491], [0.761205, 6.17317], [1.6853, 4.4443], [2.92893, 2.92893], [4.4443, 1.6853], [6.17317, 0.761205], [8.0491, 0.192147], [10, 0]]);";
    let e = export(&format!(
        "difference() {{ cube([60, 60, 30]); translate([10, 0, 30]) rotate([90, 0, 90]) linear_extrude(40.01) rotate(180) {profile} translate([50, 10, 20]) rotate_extrude(angle = 90, start = -90) translate([10, 0]) mirror([1, 0]) {profile} }}"
    ));
    // The normal render's volume at $fn 2048 and 8192 for the revolve,
    // extrapolated (its error falls as 1/n²): 107726.871417327.
    let want = 107726.871417327;
    assert!(
        (e.stats.volume - want).abs() < 1e-8 * want,
        "{} vs {want}",
        e.stats.volume
    );
    assert!(e.stats.notes.iter().all(|n| !n.contains("B-spline")));
}

/// The corner patch of BOSL2's masks (the profile extruded upwards and
/// revolved, intersected) is smaller than the export render's
/// tessellation: its mesh's curves stand ten sagittas off the exact
/// edges, and the B-rep it made passed every other check 0.4% off the
/// model. Such a mesh is refused at every resolution. The finest one is
/// then written with the regions around the failure as facets, or, if
/// that fails too, with the extrusions as facets.
///
/// The normal render is far coarser than the model here: its volume is
/// 1.5615 at OpenSCAD's defaults but 6.3847 at `$fn = 200` and 6.5050 at
/// `$fn = 1000` (about 6.510 extrapolated, as 1/n²), so a file near 6.5
/// is the model, and one at 1.56 the coarse render of it.
#[test]
fn a_mesh_coarser_than_its_features_is_not_trusted() {
    let arc = "[0.192147, 8.0491], [0.761205, 6.17317], [1.6853, 4.4443], [2.92893, 2.92893], [4.4443, 1.6853], [6.17317, 0.761205], [8.0491, 0.192147], [10, 0]";
    let src = format!(
        "intersection() {{ linear_extrude(height = 10.01) polygon([[-0.01, -0.01], [-0.01, 10], [0, 10], {arc}, [10, -0.01]]); translate([10, 10, 0]) rotate([0, 0, 180]) rotate_extrude(angle = 90) translate([10, 0]) mirror([1, 0]) polygon([[10, -0.01], [-0.01, -0.01], [-0.01, 10], [-1.77636e-15, 10], {arc}]); }}"
    );
    match export_with(&Renderer::new(), &src) {
        Ok(e) if e.stats.partial.is_some() => {
            // The finest mesh, faceted where it failed: inscribed in the
            // model, and short of it by no more than its facets' caps.
            assert!(
                e.stats.volume > 6.0 && e.stats.volume < 6.511,
                "{:?}",
                e.stats
            );
        }
        Ok(e) => {
            // Written only as the normal render's facets, which are the
            // model as rendered: the volume is the mesh's.
            assert!(e.stats.fallback.is_some(), "{:?}", e.stats);
            assert!((e.stats.volume - e.stats.normal_volume).abs() < 1e-9);
        }
        Err(msg) => assert!(msg.contains("topology"), "{msg}"),
    }
}

/// Extrusions that do not reconstruct exact fall back to facets, so a
/// model stage 1 exported still exports (with the extrusions reported):
/// the corner patch above, unioned instead of intersected, fails exact
/// at both resolutions. The regions around the failure go to facets
/// first (a partial fallback), and the whole extrusions only if that
/// fails; either is reported at the extrusions.
#[test]
fn extrusions_fall_back_to_facets_when_they_do_not_reconstruct() {
    let arc = "[0.192147, 8.0491], [0.761205, 6.17317], [1.6853, 4.4443], [2.92893, 2.92893], [4.4443, 1.6853], [6.17317, 0.761205], [8.0491, 0.192147], [10, 0]";
    let e = export(&format!(
        "linear_extrude(height = 10.01) polygon([[-0.01, -0.01], [-0.01, 10], [0, 10], {arc}, [10, -0.01]]); translate([10, 10, 0]) rotate([0, 0, 180]) rotate_extrude(angle = 90) translate([10, 0]) mirror([1, 0]) polygon([[10, -0.01], [-0.01, -0.01], [-0.01, 10], [-1.77636e-15, 10], {arc}]);"
    ));
    assert!(e.stats.fallback.is_some() || e.stats.partial.is_some());
    assert_eq!(e.stats.exact_attempt_faceted, Vec::<&str>::new());
    let faceted = kinds(&e, SubstitutionKind::Faceted);
    assert!(
        !faceted.is_empty()
            && faceted
                .iter()
                .all(|k| ["linear_extrude", "rotate_extrude"].contains(k)),
        "{faceted:?}"
    );
}

/// The tripods of `example017.scad` stand in slots of a disc, the slots
/// turned in 2D and the tripods in 3D, so their flush faces differ in
/// the last bits: Manifold leaves a closed sliver of no volume between
/// them, which reconstruction now drops. (Skipped without the reference
/// checkout.)
#[test]
fn flush_tabs_in_rotated_slots_export() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../.reference/openscad/examples/Old/example017.scad");
    let Ok(text) = std::fs::read_to_string(&path) else {
        eprintln!("skipped: no reference checkout");
        return;
    };
    let lib = &text[..text.find("module parts()").expect("example017's modules")];
    let src = format!(
        "{lib}\nlinear_extrude(height = thickness) shape_outer_disc();\nrotate(120) translate([0, thickness * 2 + locklen1 + inner1_to_inner2 + boltlen + midhole, 0]) rotate([90, 0, -90]) linear_extrude(height = thickness, center = true) shape_tripod();\n"
    );
    let e = export(&src);
    assert!(
        e.stats.notes.iter().any(|n| n.contains("of no area")),
        "{:?}",
        e.stats.notes
    );
}

/// BOSL2's `stroke()` joint: cylinders ending on great circles of a
/// sphere of their own radius, which meet where all three surfaces touch.
/// The cylinders' caps lie on the sphere's equators exactly, but the
/// mesh's caps poke out between the sphere's polygon and their own in
/// slivers whose two edges are the same circle; those faces of no area
/// are removed, and the joint is exact at any angle between the
/// cylinders. It was refused before ("no outer loop").
#[test]
fn a_stroke_joint_exports_exact() {
    for turn in ["[0, 90, 0]", "[0, 60, 0]", "[0, 60, 30]"] {
        let e = export(&format!(
            "sphere(d = 1); cylinder(d = 1, h = 3); rotate({turn}) cylinder(d = 1, h = 3);"
        ));
        assert!(e.stats.partial.is_none(), "{turn}: {:?}", e.stats.partial);
        assert_eq!(e.stats.faces, 5, "{turn}");
        assert_eq!(e.stats.exact_faces, 5, "{turn}");
    }
}

/// A cut that stops 2.1e-9 short of a face leaves a wall thinner than
/// anything a STEP file can hold (BOSL2 `hinges__015` has one between
/// flush faces from two chains of transforms). Manifold's mesh has needle
/// triangles along it, whose corners lie on the edges they face, and the
/// boundary of the face beside them touched itself. The needles are
/// flipped into their neighbours, and the model exports exact.
#[test]
fn a_wall_thinner_than_the_tolerance_is_cleaned_up() {
    let e = export(
        "difference() { cube([20, 2.1, 7]); translate([4, -1, 3.5]) cube([5, 3.0999999979, 5]); }",
    );
    assert_eq!(e.stats.exact_faces, e.stats.faces);
    assert!(
        e.stats.notes.iter().any(|n| n.contains("needle")),
        "{:?}",
        e.stats.notes
    );
    // The wall's volume (3.7e-8) is all the file loses.
    assert!((e.stats.volume - 257.25).abs() < 1e-7, "{}", e.stats.volume);
}

/// Where part of a model does not reconstruct (three cones whose pairs
/// touch on the third's base plane: BOSL2 `distributors`), the faces
/// around the failure are written as facets and the rest stays exact,
/// with the substitution reported at the module and line it came from;
/// before, the whole file was refused. The result is held to every
/// check an exact export is.
#[test]
fn a_region_that_does_not_reconstruct_is_written_as_facets() {
    let src = "for (i = [0:2]) rotate([60 * i, 0, 0]) cylinder(h = 20, r1 = 5, r2 = 0);\ntranslate([100, 0, 0]) cylinder(r = 5, h = 10);";
    let e = export(src);
    let p = e.stats.partial.as_ref().expect("a partial fallback");
    assert!(p.regions > 0 && p.triangles < p.exact_triangles, "{p:?}");
    // The separate cylinder keeps its three faces exact.
    assert!(e.stats.exact_faces >= 3, "{:?}", e.stats);
    assert_eq!(count(&e.step, "CYLINDRICAL_SURFACE"), 1);
    // Judged by the exact attempts, nothing in the model is mesh-only.
    assert_eq!(e.stats.exact_attempt_faceted, Vec::<&str>::new());
    let partly: Vec<_> = e
        .substitutions
        .iter()
        .filter(|s| s.kind == SubstitutionKind::Faceted)
        .collect();
    assert_eq!(partly.len(), 1, "{partly:?}");
    assert_eq!(partly[0].module, "cylinder");
    assert!(partly[0].detail.contains("partly as planar facets"));
    assert_eq!(partly[0].loc.as_ref().map(|l| l.line), Some(1));
    // Within what its facets and curves account for of the render (the
    // render's 16-sided cones are 2% short of the round ones).
    assert!(
        (e.stats.volume - e.stats.normal_volume).abs() < 3e-2 * e.stats.volume,
        "{:?}",
        e.stats
    );
    // The same bytes again.
    assert!(export(src).step == e.step);
}

/// A subtree that recurs is built once by the export render and placed
/// again (`walk::memo`): a module instantiated under rotations, a mirror
/// and a uniform scale exports as the same solid, with the same report,
/// as the same instances written out at separate lines (which the memo
/// keeps apart, so they are each built in place). An instance under a
/// non-uniform scale is built in place, its cylinders as facets. The
/// fine `$fa`/`$fs` make the subtree large enough to be kept
/// (`memo::MIN_TRIANGLES`).
#[test]
fn a_recurring_subtree_is_built_once_and_placed() {
    let body = "difference() { cylinder(r = 3, h = 5); translate([0, 0, -1]) cylinder(r = 1, h = 7); cube([8, 1, 2], center = true); }";
    let module = format!("$fa = 0.5; $fs = 0.01;\nmodule m() {body}\n");
    let placements = [
        "translate([10, 0, 0])",
        "rotate(90) translate([10, 0, 0])",
        "rotate([30, 0, 180]) translate([10, 0, 0])",
        "mirror([0, 1, 0]) translate([10, 0, 20])",
        "translate([0, -20, 0]) scale(1.5)",
        "translate([0, 20, 0]) scale([1, 1.5, 1])",
    ];
    let looped = format!(
        "{module}for (i = [0:{}]) {}\n",
        placements.len() - 1,
        placements
            .iter()
            .enumerate()
            .map(|(i, p)| format!("if (i == {i}) {p} m();"))
            .collect::<Vec<_>>()
            .join(" else ")
    );
    // The same instances, each with its own copy of the module on a line
    // of its own: subtrees whose substitutions are reported at different
    // places, which the memo builds apart.
    let apart: String = std::iter::once("$fa = 0.5; $fs = 0.01;\n".to_string())
        .chain(
            placements
                .iter()
                .enumerate()
                .map(|(i, p)| format!("module m{i}() {body}\n{p} m{i}();\n")),
        )
        .collect();
    let placed = |src: &str| {
        let ev = tree(src);
        let keys = eval::dump::Keys::new(&ev.root, &lang::loader::StdFs);
        geom::exact::walk::export_render(
            &Renderer::new(),
            &ev.root,
            &keys,
            &RenderOptions::default(),
            1,
        )
        .unwrap_or_else(|_| panic!("export render"))
        .placed_copies
    };
    // The loop's instances share one source location (the similar ones
    // after the first are placed); the second model's do not.
    assert_eq!(placed(&looped), 4);
    assert_eq!(placed(&apart), 0);

    let (a, b) = (export(&looped), export(&apart));
    assert!(a.stats.partial.is_none() && b.stats.partial.is_none());
    assert_eq!(
        (a.stats.faces, a.stats.exact_faces, a.stats.edges),
        (b.stats.faces, b.stats.exact_faces, b.stats.edges)
    );
    assert!(
        (a.stats.volume - b.stats.volume).abs() < 1e-12 * b.stats.volume,
        "{} {}",
        a.stats.volume,
        b.stats.volume
    );
    // The same substitutions, as many times (at one line each in the
    // loop, spread over the copies' lines apart).
    let report = |e: &ExactExport| {
        let mut s: std::collections::BTreeMap<String, u32> = Default::default();
        for x in &e.substitutions {
            *s.entry(format!("{:?} {} {}", x.kind, x.module, x.detail))
                .or_default() += x.count;
        }
        s
    };
    assert_eq!(report(&a), report(&b));
    // Each of the two cylinders exact in the five similar instances, the
    // placed ones counted, and as facets in the stretched one.
    let counts = |kind: SubstitutionKind| -> Vec<u32> {
        a.substitutions
            .iter()
            .filter(|s| s.kind == kind && s.module == "cylinder")
            .map(|s| s.count)
            .collect()
    };
    assert_eq!(
        counts(SubstitutionKind::Exact),
        [5, 5],
        "{:?}",
        a.substitutions
    );
    assert_eq!(
        counts(SubstitutionKind::Faceted),
        [1, 1],
        "{:?}",
        a.substitutions
    );
    // The same bytes again.
    assert!(export(&looped).step == a.step);
}
