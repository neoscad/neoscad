//! Leaf geometry, tessellated exactly as OpenSCAD's `createGeometry`
//! methods do (`src/core/primitives.cc`): same vertex order, same face
//! order and winding, same fragment counts. The tier 3 images show
//! tessellation directly, so a sphere with one ring too many is a failure.

use eval::node::Discretizer;
use eval::trig::{cos_degrees, sin_degrees};

use crate::fragments::circular_segments;
use crate::polygon2d::{Outline, Polygon2d};
use crate::polyset::PolySet;

/// `generate_circle` (`primitives.cc:58-65`): `fragments` points at height
/// `z`, starting on +x and going counter-clockwise.
fn circle(out: &mut Vec<[f64; 3]>, r: f64, z: f64, fragments: i32) {
    for i in 0..fragments {
        let phi = (360.0 * f64::from(i)) / f64::from(fragments);
        out.push([r * cos_degrees(phi), r * sin_degrees(phi), z]);
    }
}

fn empty3() -> PolySet {
    PolySet {
        triangular: true,
        ..Default::default()
    }
}

/// `CubeNode::createGeometry` (`primitives.cc:100-135`).
pub fn cube(size: [f64; 3], center: bool) -> PolySet {
    if size.iter().any(|&s| s <= 0.0 || !s.is_finite()) {
        return empty3();
    }
    let (lo, hi) = if center {
        (size.map(|s| -s / 2.0), size.map(|s| s / 2.0))
    } else {
        ([0.0; 3], size)
    };
    let vertices = (0..8)
        .map(|i| {
            [
                if i & 1 != 0 { hi[0] } else { lo[0] },
                if i & 2 != 0 { hi[1] } else { lo[1] },
                if i & 4 != 0 { hi[2] } else { lo[2] },
            ]
        })
        .collect();
    let faces = vec![
        vec![4, 5, 7, 6], // top
        vec![2, 3, 1, 0], // bottom
        vec![0, 1, 5, 4], // front
        vec![1, 3, 7, 5], // right
        vec![3, 2, 6, 7], // back
        vec![2, 0, 4, 6], // left
    ];
    PolySet {
        vertices,
        faces,
        convex: Some(true),
        ..Default::default()
    }
}

/// `SphereNode::createGeometry` (`primitives.cc:177-223`): `(n + 1) / 2`
/// rings of `n` points, each ring at the middle of its latitude band (so
/// there are no poles), capped by one `n`-gon at each end.
pub fn sphere(r: f64, disc: &Discretizer) -> PolySet {
    if r <= 0.0 || !r.is_finite() {
        return empty3();
    }
    let n = circular_segments(disc, r).unwrap_or(3);
    let rings = (n + 1) / 2;
    let mut vertices = Vec::with_capacity((rings * n) as usize);
    for i in 0..rings {
        let phi = (180.0 * (f64::from(i) + 0.5)) / f64::from(rings);
        circle(&mut vertices, r * sin_degrees(phi), r * cos_degrees(phi), n);
    }
    let n = n as u32;
    let rings = rings as u32;
    let mut faces = Vec::with_capacity((rings * n + 2) as usize);
    faces.push((0..n).collect());
    for i in 0..rings - 1 {
        for j in 0..n {
            faces.push(vec![
                i * n + (j + 1) % n,
                i * n + j,
                (i + 1) * n + j,
                (i + 1) * n + (j + 1) % n,
            ]);
        }
    }
    faces.push((0..n).map(|i| rings * n - i - 1).collect());
    PolySet {
        vertices,
        faces,
        convex: Some(true),
        ..Default::default()
    }
}

/// `CylinderNode::createGeometry` (`primitives.cc:251-308`), including
/// cones (`r2 == 0`, one apex vertex) and inverted cones (`r1 == 0`).
pub fn cylinder(h: f64, r1: f64, r2: f64, center: bool, disc: &Discretizer) -> PolySet {
    if h <= 0.0
        || !h.is_finite()
        || r1 < 0.0
        || !r1.is_finite()
        || r2 < 0.0
        || !r2.is_finite()
        || (r1 <= 0.0 && r2 <= 0.0)
    {
        return empty3();
    }
    // `std::fmax`: the larger radius sets the fragment count.
    let n = circular_segments(disc, r1.max(r2)).unwrap_or(3);
    let (z1, z2) = if center {
        (-h / 2.0, h / 2.0)
    } else {
        (0.0, h)
    };
    let cone = r2 == 0.0;
    let inverted = r1 == 0.0;
    let mut vertices = Vec::new();
    if inverted {
        vertices.push([0.0, 0.0, z1]);
    } else {
        circle(&mut vertices, r1, z1, n);
    }
    if cone {
        vertices.push([0.0, 0.0, z2]);
    } else {
        circle(&mut vertices, r2, z2, n);
    }
    let n = n as u32;
    let mut faces = Vec::new();
    for i in 0..n {
        let j = (i + 1) % n;
        if cone {
            faces.push(vec![i, j, n]);
        } else if inverted {
            faces.push(vec![0, j + 1, i + 1]);
        } else {
            faces.push(vec![i, j, j + n, i + n]);
        }
    }
    if !inverted {
        faces.push((0..n).map(|i| n - i - 1).collect());
    }
    if !cone {
        let offset = if inverted { 1 } else { n };
        faces.push((0..n).map(|i| offset + i).collect());
    }
    PolySet {
        vertices,
        faces,
        convex: Some(true),
        ..Default::default()
    }
}

/// `PolyhedronNode::createGeometry` (`primitives.cc:399-414`): the points
/// and faces as given, each face reversed (OpenSCAD's polyhedron faces are
/// clockwise seen from outside; meshes are counter-clockwise). Convexity is
/// unknown. Index validation happened when the node was built.
pub fn polyhedron(points: &[[f64; 3]], faces: &[Vec<usize>]) -> PolySet {
    let faces: Vec<Vec<u32>> = faces
        .iter()
        .map(|f| f.iter().rev().map(|&i| i as u32).collect())
        .collect();
    let triangular = faces.iter().all(|f| f.len() <= 3);
    PolySet {
        vertices: points.to_vec(),
        faces,
        convex: None,
        triangular,
        ..Default::default()
    }
}

/// `SquareNode::createGeometry` (`primitives.cc:492-508`).
pub fn square(size: [f64; 2], center: bool) -> Polygon2d {
    if size.iter().any(|&s| s <= 0.0 || !s.is_finite()) {
        return Polygon2d::default();
    }
    let (v1, v2) = if center {
        (
            [-size[0] / 2.0, -size[1] / 2.0],
            [size[0] / 2.0, size[1] / 2.0],
        )
    } else {
        ([0.0, 0.0], size)
    };
    Polygon2d::from_outline(vec![v1, [v2[0], v1[1]], v2, [v1[0], v2[1]]])
}

/// `CircleNode::createGeometry` (`primitives.cc:550-564`).
pub fn circle2d(r: f64, disc: &Discretizer) -> Polygon2d {
    if r <= 0.0 || !r.is_finite() {
        return Polygon2d::default();
    }
    let n = circular_segments(disc, r).unwrap_or(3);
    let pts = (0..n)
        .map(|i| {
            let phi = (360.0 * f64::from(i)) / f64::from(n);
            [r * cos_degrees(phi), r * sin_degrees(phi)]
        })
        .collect();
    Polygon2d::from_outline(pts)
}

/// `PolygonNode::createGeometry` (`primitives.cc:626-653`): without paths
/// the points form one outline (if there are at least three); otherwise
/// each path is an outline, the first positive and the rest holes. The
/// result is unsanitized: the evaluator runs it through Clipper's even-odd
/// union, which is what makes nested paths holes whatever their winding.
pub fn polygon(points: &[[f64; 2]], paths: &[Vec<usize>]) -> Polygon2d {
    let mut p = Polygon2d::default();
    if paths.is_empty() {
        if points.len() > 2 {
            p.outlines.push(Outline::new(points.to_vec()));
        }
        return p;
    }
    for (i, path) in paths.iter().enumerate() {
        p.outlines.push(Outline {
            vertices: path.iter().map(|&k| points[k]).collect(),
            positive: i == 0,
        });
    }
    p
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(fn_: f64) -> Discretizer {
        Discretizer {
            fn_,
            fa: 12.0,
            fs: 2.0,
        }
    }

    #[test]
    fn sphere_layout_matches_openscad() {
        // sphere(1, $fn=6) as the nightly exports it: 3 rings of 6, 14 faces.
        let s = sphere(1.0, &d(6.0));
        assert_eq!(s.vertices.len(), 18);
        assert_eq!(s.faces.len(), 14);
        assert_eq!(s.faces[0], vec![0, 1, 2, 3, 4, 5]);
        assert_eq!(s.faces[1], vec![1, 0, 6, 7]);
        assert_eq!(s.faces[13], vec![17, 16, 15, 14, 13, 12]);
        assert_eq!(s.vertices[0], [0.5, 0.0, cos_degrees(30.0)]);
        // Default $fa/$fs on r = 10: 30 fragments, 15 rings.
        let s = sphere(10.0, &d(0.0));
        assert_eq!(s.vertices.len(), 30 * 15);
        assert_eq!(s.faces.len(), 30 * 14 + 2);
    }

    #[test]
    fn cylinder_and_cones() {
        let c = cylinder(2.0, 1.0, 1.0, false, &d(8.0));
        assert_eq!((c.vertices.len(), c.faces.len()), (16, 10));
        let cone = cylinder(2.0, 1.0, 0.0, true, &d(8.0));
        assert_eq!((cone.vertices.len(), cone.faces.len()), (9, 9));
        assert_eq!(cone.faces[0], vec![0, 1, 8]);
        assert_eq!(cone.vertices[8], [0.0, 0.0, 1.0]);
        let inv = cylinder(2.0, 0.0, 1.0, false, &d(8.0));
        assert_eq!((inv.vertices.len(), inv.faces.len()), (9, 9));
        assert_eq!(inv.faces[0], vec![0, 2, 1]);
        assert_eq!(inv.faces[8], (1..9).collect::<Vec<u32>>());
        assert!(cylinder(1.0, 0.0, 0.0, false, &d(8.0)).is_empty());
    }

    #[test]
    fn cube_matches_openscad() {
        let c = cube([1.0, 2.0, 3.0], true);
        assert_eq!(c.vertices[0], [-0.5, -1.0, -1.5]);
        assert_eq!(c.vertices[7], [0.5, 1.0, 1.5]);
        assert_eq!(c.faces[0], vec![4, 5, 7, 6]);
        assert!(cube([1.0, 0.0, 1.0], false).is_empty());
    }

    /// Vertex and face counts of `openscad -o x.off` on the 2026.09.23
    /// nightly for each primitive (the second line of the OFF file).
    #[test]
    fn counts_match_the_nightly() {
        let disc = |fn_: f64, fa: f64, fs: f64| Discretizer { fn_, fa, fs };
        let def = disc(0.0, 12.0, 2.0);
        let cases: &[(&str, PolySet, usize, usize)] = &[
            ("sphere(r=1)", sphere(1.0, &def), 15, 12),
            ("sphere(r=10)", sphere(10.0, &def), 450, 422),
            ("sphere(r=5)", sphere(5.0, &def), 128, 114),
            ("sphere(r=0.001)", sphere(0.001, &def), 15, 12),
            (
                "sphere(r=3,$fn=7)",
                sphere(3.0, &disc(7.0, 12.0, 2.0)),
                28,
                23,
            ),
            (
                "sphere(r=3,$fn=2)",
                sphere(3.0, &disc(2.0, 12.0, 2.0)),
                6,
                5,
            ),
            (
                "sphere(r=100,$fa=5,$fs=0.5)",
                sphere(100.0, &disc(0.0, 5.0, 0.5)),
                2592,
                2522,
            ),
            (
                "sphere(r=2,$fs=0.1)",
                sphere(2.0, &disc(0.0, 12.0, 0.1)),
                450,
                422,
            ),
            (
                "cylinder(h=2,r=1)",
                cylinder(2.0, 1.0, 1.0, false, &def),
                10,
                7,
            ),
            (
                "cylinder(h=2,r=10)",
                cylinder(2.0, 10.0, 10.0, false, &def),
                60,
                32,
            ),
            (
                "cylinder(h=2,r1=3,r2=0,$fn=9)",
                cylinder(2.0, 3.0, 0.0, false, &disc(9.0, 12.0, 2.0)),
                10,
                10,
            ),
            (
                "cylinder(h=2,r1=0,r2=4)",
                cylinder(2.0, 0.0, 4.0, false, &def),
                14,
                14,
            ),
            (
                "cylinder(h=1,r1=1,r2=20,$fa=3)",
                cylinder(1.0, 1.0, 20.0, false, &disc(0.0, 3.0, 2.0)),
                126,
                65,
            ),
            (
                "cylinder(h=1,r=5,$fn=4.5)",
                cylinder(1.0, 5.0, 5.0, false, &disc(4.5, 12.0, 2.0)),
                10,
                7,
            ),
            ("cube([1,2,3])", cube([1.0, 2.0, 3.0], false), 8, 6),
        ];
        for (name, ps, v, f) in cases {
            assert_eq!((ps.vertices.len(), ps.faces.len()), (*v, *f), "{name}");
        }
    }

    #[test]
    fn polyhedron_reverses_faces() {
        let p = polyhedron(
            &[[0.0; 3], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
            &[vec![0, 1, 2], vec![0, 3, 1, 2]],
        );
        assert_eq!(p.faces[0], vec![2, 1, 0]);
        assert!(!p.triangular);
    }
}
