//! Rendering small programs end to end and checking the exported meshes
//! against what the 2026.09.23 nightly exports (`openscad x.scad -o x.off`).

use std::path::PathBuf;

use geom::{Geometry, RenderOptions, Renderer};

fn tree(src: &str) -> eval::Evaluation {
    let path = PathBuf::from("/nonexistent/test.scad");
    let mut text = src.as_bytes().to_vec();
    text.extend_from_slice(b"\n\x03\n");
    let program = lang::parse_file(path, text);
    assert!(!program.has_syntax_errors(), "syntax error in test program");
    let mut out = eval::Collect::default();
    eval::with_stack(eval::DEFAULT_THREAD_STACK, || {
        eval::evaluate(&program, &[], &[], PathBuf::from("/nonexistent"), &eval::Options::default(), &mut out)
    })
}

fn render_with(r: &Renderer, src: &str, force: bool) -> (Option<Geometry>, Vec<String>) {
    let ev = tree(src);
    let keys = eval::dump::Keys::new(&ev.root);
    let out = r.render(&ev.root, &keys, RenderOptions { force, ..Default::default() }).expect("supported");
    (out.geometry, out.messages.iter().map(|m| format!("{:?}: {} @{}", m.severity.expect("prefixed message"), m.text, m.loc.as_ref().map_or(0, |l| l.line))).collect())
}

fn off(src: &str) -> String {
    let (g, _) = render_with(&Renderer::new(), src, false);
    let ps = geom::export::as_polyset(&g.expect("geometry"), &geom::color::CORNFIELD).expect("3D");
    String::from_utf8(geom::export::off(&ps, &mut Vec::new())).unwrap()
}

/// Face colour counts, as `awk '{print $5,$6,$7,$8}' | sort | uniq -c`.
fn colour_counts(off: &str) -> Vec<(String, usize)> {
    let mut m = std::collections::BTreeMap::new();
    let mut lines = off.lines();
    lines.next();
    let counts: Vec<usize> = lines.next().unwrap().split(' ').map(|n| n.parse().unwrap()).collect();
    for l in off.lines().skip(2 + counts[0]) {
        let w: Vec<&str> = l.split(' ').collect();
        let n: usize = w[0].parse().unwrap();
        *m.entry(w[n + 1..].join(" ")).or_insert(0) += 1;
    }
    m.into_iter().collect()
}

#[test]
fn difference_paints_cut_faces_with_the_back_colour() {
    let text = off("difference() { cube(10, center=true); sphere(6, $fn=12); }");
    let colours = colour_counts(&text);
    // Front colour on the cube, back colour on the spherical cut.
    assert_eq!(colours.iter().map(|(c, _)| c.as_str()).collect::<Vec<_>>(), ["157 203 81", "249 215 44"]);
}

#[test]
fn a_mesh_used_twice_gets_separate_ids() {
    // The sphere is subtracted on the right but added on the left; only the
    // right one may carry the back colour.
    let text = off("union() { translate([-20,0,0]) union() { cube(10, center=true); sphere(6); } difference() { cube(10, center=true); sphere(6); } }");
    let green: usize = colour_counts(&text).iter().filter(|(c, _)| c == "157 203 81").map(|(_, n)| *n).sum();
    let text_cut = off("difference() { cube(10, center=true); sphere(6); }");
    let green_cut: usize = colour_counts(&text_cut).iter().filter(|(c, _)| c == "157 203 81").map(|(_, n)| *n).sum();
    assert_eq!(green, green_cut);
}

#[test]
fn colour_survives_booleans() {
    let text = off("union() { color(\"red\") cube(1); translate([0.5,0.5,0.5]) cube(1); }");
    let colours: Vec<String> = colour_counts(&text).into_iter().map(|(c, _)| c).collect();
    assert_eq!(colours, ["249 215 44", "255 0 0"]);
}

#[test]
fn mixing_dimensions_warns_like_openscad() {
    let (_, msgs) = render_with(&Renderer::new(), "union() {\ncube(1);\nsquare(1);\n}", false);
    assert_eq!(
        msgs,
        ["Warning: Mixing 2D and 3D objects is not supported @3", "Warning: Ignoring 2D child object for 3D operation @3"]
    );
}

#[test]
fn render_force_converts_a_lone_mesh() {
    let (g, _) = render_with(&Renderer::new(), "cube(1);", true);
    assert!(matches!(g, Some(Geometry::Manifold(_))));
    let (g, _) = render_with(&Renderer::new(), "cube(1);", false);
    assert!(matches!(g, Some(Geometry::PolySet(_))));
}

#[test]
fn background_is_skipped_and_empty_is_none() {
    let (g, _) = render_with(&Renderer::new(), "%cube(1);", false);
    assert!(g.is_none());
    let (g, _) = render_with(&Renderer::new(), "difference() { cube(1); cube(2); }", false);
    assert!(g.is_none_or(|g| g.is_empty()));
}

#[test]
fn output_is_deterministic_and_cache_hits_are_silent() {
    let src = "for (i = [0:7]) translate([i*3,0,0]) difference() { cube(2, center=true); sphere(1.2, $fn=16); }";
    let a = off(src);
    let b = off(src);
    assert_eq!(a, b);
    let r = Renderer::new();
    let (_, first) = render_with(&r, "union() { cube(1); square(1); }", false);
    let (_, second) = render_with(&r, "union() { cube(1); square(1); }", false);
    assert_eq!(first.len(), 2);
    assert!(second.is_empty(), "{second:?}");
}

/// Copies of one cached solid, unioned: the case whose output order
/// depended on thread scheduling. Whether two sibling copies share mesh
/// IDs depended on which thread computed first (a cache hit shares them),
/// and manifold-rust's union of disjoint parts merged copies that shared
/// IDs into one run. At 736bcc1 about one render in six came out in a
/// different order.
#[test]
fn copies_of_a_cached_solid_export_identically_every_time() {
    let src = "module m() difference() { cube(2, center=true); sphere(1.2, $fn=16); }
module row() for (i = [0:3]) translate([i*2,0,0]) m();
for (j = [0:3]) translate([0,j*2,0]) row();";
    let first = off(src);
    for _ in 0..20 {
        assert!(off(src) == first, "export differs between renders");
    }
}

#[test]
fn flipped_polyhedron_face_is_repaired() {
    // polyhedron-tests.scad's "one face flipped" octahedron, unioned so it
    // has to become a solid.
    let src = "union() { polyhedron(points = [[1,0,0],[-1,0,0],[0,1,0],[0,-1,0],[0,0,1],[0,0,-1]], faces = [[0,4,2],[0,2,5],[0,3,4],[0,5,3],[1,2,4],[1,5,2],[1,3,4], [1,3,5]]); translate([5,0,0]) cube(1); }";
    let (g, msgs) = render_with(&Renderer::new(), src, false);
    assert_eq!(msgs, ["Warning: PolySet -> Manifold conversion failed: NotManifold\nTrying to repair and reconstruct mesh.. @0"]);
    let Some(Geometry::Manifold(m)) = g else { panic!("expected a solid") };
    assert_eq!(m.manifold.num_tri(), 8 + 12);
}

/// `hull()` of 2D children keeps CGAL's output order: counter-clockwise
/// from the lowest of the leftmost points. The nightly's SVG for this
/// program lists (0,0), (2,0), (5.5,0.133975), (6,1), (5.5,1.86603),
/// (2,2), (0,2).
#[test]
fn hull_2d_keeps_cgals_point_order() {
    let (g, msgs) = render_with(&Renderer::new(), "hull() { translate([5,1]) circle(1,$fn=6); square(2); }", false);
    assert!(msgs.is_empty(), "{msgs:?}");
    let Some(Geometry::Polygon2d(p)) = g else { panic!("expected 2D") };
    assert_eq!(p.outlines.len(), 1);
    let want = [[0.0, 0.0], [2.0, 0.0], [5.5, 0.133975], [6.0, 1.0], [5.5, 1.86603], [2.0, 2.0], [0.0, 2.0]];
    let got = &p.outlines[0].vertices;
    assert_eq!(got.len(), want.len());
    for (g, w) in got.iter().zip(want) {
        assert!((g[0] - w[0]).abs() < 1e-5 && (g[1] - w[1]).abs() < 1e-5, "{got:?}");
    }
}

/// A hull is a new solid with no original IDs (`ManifoldGeometry(Hull(...))`),
/// so subtracting it leaves front-coloured faces; a minkowski result is made
/// one original (`toOriginal`), so its cut is drawn in the back colour. The
/// nightly shows both colours for this program, and a hull that drops
/// the child's red.
#[test]
fn hull_and_minkowski_cut_faces_follow_openscad() {
    let text = off("difference(){ cube(4,center=true); translate([1.5,1.5,1.5]) hull() color(\"red\") cube(1); translate([0,0,-2]) minkowski(){cube(1,center=true); sphere(0.5,$fn=8);} }");
    let colours: Vec<String> = colour_counts(&text).into_iter().map(|(c, _)| c).collect();
    assert_eq!(colours, ["157 203 81", "249 215 44"]);
    let hull_only = off("difference(){ cube(4,center=true); translate([1.5,1.5,1.5]) hull() color(\"red\") cube(1); }");
    let colours: Vec<String> = colour_counts(&hull_only).into_iter().map(|(c, _)| c).collect();
    assert_eq!(colours, ["249 215 44"]);
}

/// OpenSCAD's minkowski (CGAL decomposition plus hulls) sums the operands
/// wherever they are: a unit cube and one 5 units away give a single cube
/// of side 2 (8 vertices in the nightly's OFF). Manifold's own
/// `MinkowskiSum` would also keep the first cube at the origin.
#[test]
fn minkowski_of_disjoint_cubes_is_one_cube() {
    let (g, msgs) = render_with(&Renderer::new(), "minkowski(){cube(1); translate([5,0,0]) cube(1);}", false);
    assert!(msgs.is_empty(), "{msgs:?}");
    let Some(Geometry::Manifold(m)) = g else { panic!("expected a solid") };
    assert_eq!(m.manifold.num_vert(), 8);
    assert_eq!(m.bounds(), Some(([5.0, 0.0, 0.0], [7.0, 2.0, 2.0])));
}

/// `issue1671.scad`: flat operands leave nothing, after OpenSCAD's warning.
#[test]
fn minkowski_of_flat_operands_warns_and_is_empty() {
    let (g, msgs) = render_with(&Renderer::new(), "minkowski() { scale([0,0,1]) cube(1); scale([0,1,0]) cube(1); scale([1,0,0]) cube(1); }", false);
    assert_eq!(msgs, ["Warning: [manifold] Minkowski hard-crashed, falling back to Nef operation. @0"]);
    assert!(g.is_none_or(|g| g.is_empty()));
}

/// The rounded box renders the same every time, including through the
/// cache of a long-lived renderer.
#[test]
fn minkowski_output_is_deterministic() {
    let src = "minkowski(){ union(){cube([20,5,5]); cube([5,20,5]);} sphere(2,$fn=16);}
translate([30,0,0]) minkowski(){ difference(){cube(10); translate([5,5,5]) sphere(3,$fn=16);} cube(1);}";
    let first = off(src);
    for _ in 0..5 {
        assert!(off(src) == first, "export differs between renders");
    }
}

/// `resize()` with `auto`: the auto axes take the largest requested scale
/// (the nightly's OFF for this cube spans 4 x 8 x 8).
#[test]
fn resize_auto_scales_like_openscad() {
    let (g, _) = render_with(&Renderer::new(), "resize([4,0,0], auto=[false,true,false]) cube([2,4,8]);", false);
    let Some(Geometry::PolySet(ps)) = g else { panic!("expected a mesh") };
    assert_eq!(ps.bounds(), Some(([0.0, 0.0, 0.0], [4.0, 8.0, 8.0])));
    let (g, _) = render_with(&Renderer::new(), "resize([10,0], auto=true) square([2,4]);", false);
    let Some(Geometry::Polygon2d(p)) = g else { panic!("expected 2D") };
    assert_eq!(p.bounds(), Some(([0.0, 0.0], [10.0, 20.0])));
}
