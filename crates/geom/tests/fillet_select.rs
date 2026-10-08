//! Fillet edge selection (`docs/fillets.md`, stage F1): the facts of a
//! call's child and which edges its selectors pick, on the design's
//! worked examples (section 12) and on the selector atoms one by one.

use std::path::PathBuf;

use geom::fillet::{self, Class, Plan, Sense, Status};
use geom::{RenderOptions, Renderer};
use lang::diag::DiagCode;

struct Run {
    root: eval::Node,
    keys: eval::dump::Keys,
    log: Vec<(lang::diag::Severity, DiagCode, String)>,
}

fn evaluate(src: &str) -> Run {
    let path = PathBuf::from("/nonexistent/test.scad");
    let mut text = src.as_bytes().to_vec();
    text.extend_from_slice(b"\n\x03\n");
    let program = lang::parse_file(path, text);
    assert!(!program.has_syntax_errors());
    let mut out = eval::Collect::default();
    let opts = eval::Options {
        extensions: eval::Extensions::NONE
            .with(eval::Extension::Fillet)
            .with(eval::Extension::Part)
            .with(eval::Extension::Query),
        ..eval::Options::default()
    };
    let ev = eval::with_stack(eval::DEFAULT_THREAD_STACK, || {
        eval::evaluate(
            &program,
            &[],
            &[],
            PathBuf::from("/nonexistent"),
            &opts,
            &mut out,
        )
    });
    let keys = eval::dump::Keys::new(&ev.root, &lang::loader::StdFs);
    Run {
        root: ev.root,
        keys,
        log: out.lines,
    }
}

/// The fillet nodes of a tree, outermost first.
fn fillets(n: &eval::Node) -> Vec<&eval::Node> {
    let mut out = Vec::new();
    let mut stack = vec![n];
    while let Some(n) = stack.pop() {
        if matches!(n.kind, eval::node::NodeKind::Fillet(_)) {
            out.push(n);
        }
        stack.extend(n.children.iter().rev());
    }
    out
}

/// The plans of every call in `src`, outermost first.
fn plans_with(src: &str, renderer: &Renderer) -> Vec<Plan> {
    let run = evaluate(src);
    renderer
        .render(&run.root, &run.keys, RenderOptions::default())
        .expect("renders");
    fillets(&run.root)
        .into_iter()
        .map(|n| fillet::plan(renderer, n, &run.keys, &RenderOptions::default()).unwrap())
        .collect()
}

fn plans(src: &str) -> Vec<Plan> {
    plans_with(src, &Renderer::new())
}

fn one(src: &str) -> Plan {
    let mut p = plans(src);
    assert_eq!(p.len(), 1, "one call");
    p.remove(0)
}

/// The selected edges as text, one per line.
fn listing(p: &Plan) -> String {
    let f = p.facts.as_ref().expect("facts");
    p.selected
        .iter()
        .map(|&i| {
            let e = &f.edges[i];
            format!("{} {}", fillet::edge_text(e), e.class.name())
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn codes(p: &Plan) -> Vec<DiagCode> {
    p.diags.iter().map(|d| d.code).collect()
}

fn count(src: &str) -> usize {
    one(src).selected.len()
}

// --- The worked examples (`docs/fillets.md`, section 12) -------------------

/// 12.1: `child(0, 1)` is the one concave edge where the legs meet, and
/// the outer call's selector the heel.
#[test]
fn l_bracket() {
    let src = "t = 5; w = 20;
        fillet_edges(r = 4, edges = \"convex and |y and <x and <z\")
        fillet_edges(r = 3, edges = \"child(0, 1)\")
        { cube([40, w, t]); cube([t, w, 30]); }";
    let p = plans(src);
    assert_eq!(
        listing(&p[0]),
        "line (convex, 90°) at [0, 10, 0], 20 long translational"
    );
    assert_eq!(
        listing(&p[1]),
        "line (concave, 270°) at [5, 10, 5], 20 long translational"
    );
    assert!(
        p.iter()
            .all(|p| p.diags.is_empty() && p.status == Status::Built)
    );
    // The same edge at another thickness and width: provenance does not
    // depend on the sizes.
    let p = plans(&src.replace("t = 5; w = 20;", "t = 8; w = 13;"));
    assert_eq!(
        listing(&p[1]),
        "line (concave, 270°) at [8, 6.5, 8], 13 long translational"
    );
}

/// 12.2, the box: the vertical edges, then the top outline. The inner
/// call's blends are built (stage F2), so the outer call selects on the
/// rounded box: the chain of four lines and four quarter circles, whose
/// arcs are stage F3's (so the outer call is not built yet).
#[test]
fn rounded_box() {
    let p = plans(
        "L = 40; W = 30; H = 20; R = 5;
         fillet_edges(r = 2, edges = \">z\") fillet_edges(r = R, edges = \"|z\") cube([L, W, H]);",
    );
    assert_eq!(
        listing(&p[0]),
        "line (convex, 90°) at [0, 15, 20], 20 long translational\n\
         line (convex, 90°) at [20, 0, 20], 30 long translational\n\
         line (convex, 90°) at [20, 30, 20], 30 long translational\n\
         line (convex, 90°) at [40, 15, 20], 20 long translational\n\
         circle (convex, 90°) at [1.8169, 1.8169, 20], 7.854 long rotational\n\
         circle (convex, 90°) at [1.8169, 28.1831, 20], 7.854 long rotational\n\
         circle (convex, 90°) at [38.1831, 1.8169, 20], 7.854 long rotational\n\
         circle (convex, 90°) at [38.1831, 28.1831, 20], 7.854 long rotational"
    );
    assert_eq!(
        (p[0].status, p[1].status),
        (Status::NotBuilt, Status::Built)
    );
    assert_eq!(
        listing(&p[1]),
        "line (convex, 90°) at [0, 0, 10], 20 long translational\n\
         line (convex, 90°) at [0, 30, 10], 20 long translational\n\
         line (convex, 90°) at [40, 0, 10], 20 long translational\n\
         line (convex, 90°) at [40, 30, 10], 20 long translational"
    );
}

/// 12.2, the lid: where the lip meets the lid. `child(0, 1)` is both of
/// the lip's walls where they stand on the lid, the outer outline the
/// design describes and the inner one inside the lip: with the lip's own
/// corner fillets built, each a chain of four lines and four arcs.
#[test]
fn lid_lip() {
    let p = plans(
        "L = 40; W = 30; R = 5;
         fillet_edges(r = 1, edges = \"child(0, 1)\") {
           translate([-2, -2, 0]) fillet_edges(r = 7, edges = \"|z\") cube([44, 34, 3]);
           translate([0, 0, 3]) difference() {
             fillet_edges(r = R, edges = \"|z\") cube([L, W, 4]);
             translate([1.5, 1.5, -1]) fillet_edges(r = R - 1.5, edges = \"|z\") cube([L - 3, W - 3, 6]);
           }
         }",
    );
    let lid = &p[0];
    assert_eq!(lid.selected.len(), 16);
    let f = lid.facts.as_ref().unwrap();
    for &i in &lid.selected {
        let e = &f.edges[i];
        assert_eq!((e.sense, e.center[2]), (Sense::Concave, 3.0));
    }
    let lines = lid
        .selected
        .iter()
        .filter(|&&i| f.edges[i].class == Class::Translational)
        .count();
    assert_eq!(lines, 8);
    // The outer outline alone: the lip's outside, at its full size.
    let outer = one(
        "fillet_edges(r = 1, edges = \"child(0, 1) and not box(1, 1, 0, 39, 29, 10)\") {
           translate([-2, -2, 0]) cube([44, 34, 3]);
           translate([0, 0, 3]) difference() { cube([40, 30, 4]); translate([1.5, 1.5, -1]) cube([37, 27, 6]); }
         }",
    );
    assert_eq!(outer.selected.len(), 4);
}

/// 12.3: the circle where the boss meets the plate, rotational.
#[test]
fn boss_on_plate() {
    let p = one("fillet_edges(r = 2, edges = \"child(0, 1)\") {
           translate([-20, -20, 0]) cube([40, 40, 4]);
           cylinder(r = 6, h = 14);
         }");
    assert_eq!(
        listing(&p),
        "circle (concave, 270°) at [0, 0, 4], 37.6991 long rotational"
    );
}

/// 12.4: the hole's top rim, not its bottom rim or the cube's lines.
#[test]
fn chamfered_hole() {
    let p = one(
        "chamfer_edges(d = 1, edges = \"%circle and >z\") difference() {
           cube([20, 20, 10]);
           translate([10, 10, -1]) cylinder(d = 6, h = 12);
         }",
    );
    assert_eq!(
        listing(&p),
        "circle (convex, 90°) at [10, 10, 10], 18.8496 long rotational"
    );
}

// --- Atoms and operators -------------------------------------------------

const PLATE_WITH_BOSS: &str =
    "{ translate([-20, -20, 0]) cube([40, 40, 4]); cylinder(r = 6, h = 14); }";

#[test]
fn atoms_on_a_box() {
    let cube = |sel: &str| {
        count(&format!(
            "fillet_edges(r = 1, edges = {sel}) cube([40, 30, 20]);"
        ))
    };
    assert_eq!(cube("\"all\""), 12);
    assert_eq!(cube("\"convex\""), 12);
    assert_eq!(cube("\"concave\""), 0);
    assert_eq!(cube("\"%line\""), 12);
    assert_eq!(cube("\"|z\""), 4);
    assert_eq!(cube("\"z\""), 4);
    assert_eq!(cube("\"#z\""), 8);
    assert_eq!(cube("\">z\""), 4);
    assert_eq!(cube("\"<x\""), 4);
    assert_eq!(cube("\"|x and >y\""), 2);
    assert_eq!(cube("\"|z and >x\""), 2);
    assert_eq!(cube("\"all exc <z\""), 8);
    assert_eq!(cube("\"not |z\""), 8);
    assert_eq!(cube("\">z or <z\""), 8);
    assert_eq!(cube("\"box(-1, -1, -1, 41, 31, 0.5)\""), 4);
    assert_eq!(cube("\"box(0, 0, 0, 40, 30, 20)\""), 12);
    assert_eq!(cube("\">(1, 1, 0)\""), 1);
    // BOSL2 vectors on the bounding box: a face, an edge, a corner.
    assert_eq!(cube("[0, 0, 1]"), 4);
    assert_eq!(cube("[1, 0, 1]"), 1);
    assert_eq!(cube("[1, 1, 1]"), 3);
    assert_eq!(cube("[[0, -1, 1], [1, 0, 1]]"), 2);
    assert_eq!(cube("\"Z\""), 4);
    assert_eq!(cube("\"NONE\""), 0);
    // `except` removes what it matches.
    assert_eq!(
        count("fillet_edges(r = 1, edges = \"|z\", except = \">x\") cube([40, 30, 20]);"),
        2
    );
}

/// `>>d[i]` as CadQuery's `CenterNthSelector` orders it: the groups
/// ascending along the direction, `>>` from the bottom, `<<` from the top,
/// negative indices from the far end.
#[test]
fn nth_groups_follow_cadquery() {
    let src = |sel: &str| format!("fillet_edges(r = 1, edges = \"{sel}\") cube([40, 30, 20]);");
    let centre_z = |sel: &str| {
        let p = one(&src(sel));
        let f = p.facts.as_ref().unwrap();
        let mut z: Vec<f64> = p.selected.iter().map(|&i| f.edges[i].center[2]).collect();
        z.dedup();
        z
    };
    assert_eq!(centre_z(">>z[0]"), vec![0.0]);
    assert_eq!(centre_z(">>z[1]"), vec![10.0]);
    assert_eq!(centre_z(">>z[-1]"), vec![20.0]);
    assert_eq!(centre_z(">>z"), vec![20.0]);
    assert_eq!(centre_z("<<z[0]"), vec![20.0]);
    assert_eq!(centre_z("<<z[-1]"), vec![0.0]);
    assert_eq!(count(&src(">>z[3]")), 0);
}

#[test]
fn provenance_atoms() {
    let boss = |sel: &str| {
        count(&format!(
            "fillet_edges(r = 1, edges = \"{sel}\") {PLATE_WITH_BOSS}"
        ))
    };
    // The plate's 12 lines, the boss's top rim and the circle where they
    // meet.
    assert_eq!(boss("all"), 14);
    assert_eq!(boss("child(0)"), 13);
    assert_eq!(boss("child(1)"), 2);
    assert_eq!(boss("child(0, 1)"), 1);
    assert_eq!(boss("child(1, 0)"), 1);
    assert_eq!(boss("child(1, 1)"), 1);
    assert_eq!(boss("new"), 1);
    assert_eq!(boss("%circle"), 2);
    assert_eq!(boss("#z and %circle"), 2);
    assert_eq!(boss("concave"), 1);
    // Parts, by full dotted name, and the parts inside a part.
    let parts = |sel: &str| {
        count(&format!(
            "fillet_edges(r = 1, edges = \"{sel}\") {{
               part(\"plate\") translate([-20, -20, 0]) cube([40, 40, 4]);
               part(\"plate.boss\") cylinder(r = 6, h = 14);
             }}"
        ))
    };
    assert_eq!(parts("part(plate.boss)"), 2);
    assert_eq!(parts("part(plate)"), 14);
    assert_eq!(parts("part(plat)"), 0);
}

#[test]
fn anchors_select_through_their_point() {
    let src = "fillet_edges(r = 1, edges = \"@rim\") {
                 cube([20, 20, 10]);
                 translate([20, 0, 10]) anchor(\"rim\", [0, 0, 0], [0, 1, 0]);
               }";
    let p = one(src);
    assert_eq!(
        listing(&p),
        "line (convex, 90°) at [20, 10, 10], 20 long translational"
    );
    // Without a direction, every edge through the point: three at a corner.
    assert_eq!(count(&src.replace(", [0, 1, 0]", "")), 3);
}

// --- What is never selected, and the diagnostics ---------------------------

#[test]
fn polygon_seams_are_skipped_and_said() {
    let p = one("fillet_edges(r = 1, edges = \"|z\") cylinder(r = 5, h = 3, $fn = 12);");
    assert_eq!(p.selected.len(), 0);
    assert_eq!(p.skipped.len(), 12);
    assert_eq!(
        codes(&p),
        vec![DiagCode::FilletSkipped, DiagCode::FilletNoEdges]
    );
    assert!(
        p.diags[0]
            .message
            .contains("12 polygon seams of cylinder() at line 1")
    );
    // `all` names no seam, so it skips quietly; the rims stay real edges
    // of the 12-sided prism the user asked for.
    let p = one("fillet_edges(r = 1) cylinder(r = 5, h = 3, $fn = 12);");
    assert_eq!((p.selected.len(), p.skipped.len()), (24, 0));
    assert!(p.diags.is_empty());
    // A $fn sphere is all seams; a $fn circle swept by linear_extrude too.
    assert_eq!(count("fillet_edges(r = 1) sphere(5, $fn = 8);"), 0);
    let p = one("fillet_edges(r = 1, edges = \"|z\") linear_extrude(5) circle(4, $fn = 8);");
    assert_eq!((p.selected.len(), p.skipped.len()), (0, 8));
    // Without $fn the side is one exact cylinder: no seams, two rims.
    assert_eq!(count("fillet_edges(r = 1) cylinder(r = 5, h = 3);"), 2);
}

#[test]
fn faceted_regions_and_tangent_edges_are_skipped() {
    let p = one(
        "fillet_edges(r = 1, edges = \"|z\") hull() { cube(10); translate([20, 0, 0]) cube(5); }",
    );
    assert_eq!(p.selected.len(), 0);
    assert!(p.skipped.len() >= 4);
    assert!(
        p.diags[0]
            .message
            .contains("faceted region of hull() at line 1")
    );
    // A rounded rectangle's sides meet its arcs tangentially.
    let p = one("fillet_edges(r = 1, edges = \"|z\") linear_extrude(5) offset(r = 2) square(10);");
    assert_eq!(p.selected.len(), 0);
    assert_eq!(p.skipped.len(), 8);
    assert!(p.diags[0].message.contains("8 tangent edges"));
}

#[test]
fn expect_pins_the_count() {
    let p = one("fillet_edges(r = 1, edges = \"|z\", expect = 4) cube(10);");
    assert_eq!((p.status, codes(&p)), (Status::Built, vec![]));
    let p = one("fillet_edges(r = 1, edges = \"|z\", expect = 3) cube(10);");
    assert_eq!(
        (p.status, codes(&p)),
        (Status::Count, vec![DiagCode::FilletCount])
    );
    assert!(p.diags[0].message.starts_with(
        "fillet_edges(): edges = \"|z\" matched 4 edges, expect = 3: 1. line (convex, 90°) at [0, 0, 5], 10 long;"
    ));
    assert!(p.diags[0].hints[0].contains("expect = 4"));
    // expect = 0 is a deliberate "nothing", not a warning.
    let p = one("fillet_edges(r = 1, edges = \"concave\", expect = 0) cube(10);");
    assert!(p.diags.is_empty());
    let p = one("fillet_edges(r = 1, edges = \"concave\") cube(10);");
    assert_eq!(
        (p.status, codes(&p)),
        (Status::NoEdges, vec![DiagCode::FilletNoEdges])
    );
}

#[test]
fn unsupported_edges_warn_under_all_and_fail_when_named() {
    // A plane cutting a cylinder obliquely: an ellipse.
    let body = "difference() { cylinder(r = 5, h = 10); translate([0, 0, 6]) rotate([30, 0, 0]) translate([-10, -10, 0]) cube(20); }";
    let p = one(&format!("fillet_edges(r = 1) {body}"));
    // The ellipse is left sharp with a warning; the rest of "all"
    // includes the cylinder's rim, a circle, so nothing is built yet.
    assert_eq!(p.status, Status::NotBuilt);
    let d = p
        .diags
        .iter()
        .find(|d| d.code == DiagCode::FilletUnsupportedEdge)
        .unwrap();
    assert_eq!(d.severity, lang::diag::Severity::Warning);
    assert!(d.message.contains("ellipse between a"), "{}", d.message);
    let p = one(&format!("fillet_edges(r = 1, edges = \"%ellipse\") {body}"));
    let d = p
        .diags
        .iter()
        .find(|d| d.code == DiagCode::FilletUnsupportedEdge)
        .unwrap();
    assert_eq!(d.severity, lang::diag::Severity::Error);
    // A tee of two cylinders: their intersection is no circle.
    let p = one(
        "fillet_edges(r = 1, edges = \"child(0, 1)\") { cylinder(r = 5, h = 20); translate([0, 0, 10]) rotate([0, 90, 0]) cylinder(r = 3, h = 20); }",
    );
    assert_eq!(p.selected.len(), p.unsupported.len());
    assert!(!p.selected.is_empty());
}

#[test]
fn two_d_and_empty_children() {
    let p = one("fillet_edges(r = 1) square(10);");
    assert_eq!(
        (p.status, codes(&p)),
        (Status::TwoD, vec![DiagCode::Fillet2d])
    );
    let p = one("fillet_edges(r = 1) { }");
    assert_eq!((p.status, codes(&p)), (Status::Empty, vec![]));
}

#[test]
fn bad_child_indices_and_anchors_are_evaluation_errors() {
    let run = evaluate("fillet_edges(r = 1, edges = \"child(0, 2)\") { cube(1); cube(2); }");
    assert!(fillets(&run.root).is_empty(), "the call becomes a group");
    let (_, code, text) = run
        .log
        .iter()
        .find(|l| l.1 == DiagCode::FilletSelector)
        .expect("an error");
    assert_eq!(*code, DiagCode::FilletSelector);
    assert!(
        text.contains(
            "child(0, 2) names child 2, but the call has 2 children, child(0) to child(1)"
        ),
        "{text}"
    );
    let run =
        evaluate("fillet_edges(r = 1, edges = \"@lid\") { cube(1); anchor(\"lip\", [0, 0, 1]); }");
    let text = &run
        .log
        .iter()
        .find(|l| l.1 == DiagCode::FilletSelector)
        .unwrap()
        .2;
    assert!(
        text.contains("no anchor named 'lid' among the children; the children's anchors are @lip"),
        "{text}"
    );
}

#[test]
fn anchors_are_in_the_key() {
    let key = |x: f64| {
        let run = evaluate(&format!(
            "fillet_edges(r = 1, edges = \"@a\") {{ cube(10); anchor(\"a\", [{x}, 0, 0]); }}"
        ));
        run.keys.get(fillets(&run.root)[0])
    };
    assert_ne!(key(0.0), key(10.0));
    assert_eq!(key(10.0), key(10.0));
}

// --- Determinism -----------------------------------------------------------

/// Every fact and selection of a few models, as text.
fn fingerprint(renderer: &Renderer) -> String {
    let mut out = String::new();
    for src in [
        "fillet_edges(r = 1, edges = \"child(0, 1)\") { translate([-20, -20, 0]) cube([40, 40, 4]); cylinder(r = 6, h = 14); }",
        "chamfer_edges(d = 1) difference() { cube([20, 20, 10]); translate([10, 10, -1]) cylinder(d = 6, h = 12); }",
        "fillet_edges(r = 1) for (i = [0:3]) translate([i * 8, 0, 0]) rotate([0, 0, i * 15]) cube([6, 4, 3 + i]);",
    ] {
        for p in plans_with(src, renderer) {
            out.push_str(&format!("{:?}\n{:?}\n{:?}\n", p.facts, p.selected, p.diags));
        }
    }
    out
}

#[test]
fn selection_is_the_same_at_any_thread_count_and_warm_or_cold() {
    let cold = fingerprint(&Renderer::new());
    let warm_renderer = Renderer::new();
    fingerprint(&warm_renderer);
    assert_eq!(fingerprint(&warm_renderer), cold, "warm differs from cold");
    for threads in [1, 2, 8] {
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(threads)
            .stack_size(eval::DEFAULT_THREAD_STACK)
            .build()
            .unwrap();
        for _ in 0..2 {
            assert!(
                pool.install(|| fingerprint(&Renderer::new())) == cold,
                "selection differs on {threads} threads"
            );
        }
    }
}

#[test]
fn a_cancelled_request_is_interrupted_and_not_cached() {
    let run = evaluate("fillet_edges(r = 1) cube(10);");
    let renderer = Renderer::new();
    let flag = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
    let opts = RenderOptions {
        interrupt: Some(flag.clone()),
        ..RenderOptions::default()
    };
    let node = fillets(&run.root)[0];
    let p = fillet::plan(&renderer, node, &run.keys, &opts).unwrap();
    assert_eq!(p.status, Status::Interrupted);
    flag.store(false, std::sync::atomic::Ordering::Relaxed);
    let p = fillet::plan(&renderer, node, &run.keys, &opts).unwrap();
    assert_eq!(p.selected.len(), 12);
}
