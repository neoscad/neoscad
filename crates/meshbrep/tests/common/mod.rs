//! The audit's test models as CSG trees, evaluated with Manifold into
//! tagged meshes.
//!
//! The cases are the exact-geometry audit's: the 15 boolean cases b01,
//! b03, c01–c03, c06–c15, the idioms x01–x06, the faceted fallbacks
//! x07–x08 and the CSG fillets f01–f02, plus a void (x09), a lone sphere
//! (x10) and a pointed cone (x11). Each comment gives the OpenSCAD source.

#![allow(dead_code)]

use manifold_rust::manifold::Manifold;
use manifold_rust::types::{MeshGL64, OpType};
use meshbrep::primitives::{self, Transform};
use meshbrep::{Surface, TaggedMesh};
use std::f64::consts::PI;

/// The attribution mesh's resolution: OpenSCAD's fragment rule (`$fa`,
/// `$fs`) or a fixed `$fn`, then rounded up to a multiple of 4.
#[derive(Clone, Copy, Debug)]
pub enum Res {
    Rule { fa: f64, fs: f64 },
    Fn(u32),
}

/// The six resolutions of the audit's section 3.4.
pub const RESOLUTIONS: [Res; 6] = [
    Res::Rule { fa: 12.0, fs: 2.0 },
    Res::Rule { fa: 12.0, fs: 0.5 },
    Res::Fn(5),
    Res::Fn(7),
    Res::Fn(13),
    Res::Fn(64),
];

impl Res {
    pub fn segments(&self, r: f64) -> u32 {
        let n = match *self {
            Res::Fn(n) => n.max(3),
            Res::Rule { fa, fs } => ((360.0 / fa).min(r * 2.0 * PI / fs).max(5.0)).ceil() as u32,
        };
        primitives::aligned_segments(n)
    }
    /// Twice as fine.
    pub fn finer(&self) -> Res {
        match *self {
            Res::Fn(n) => Res::Fn(2 * primitives::aligned_segments(n)),
            Res::Rule { fa, fs } => Res::Rule {
                fa: fa / 2.0,
                fs: fs / 2.0,
            },
        }
    }
    pub fn name(&self) -> String {
        match *self {
            Res::Rule { fs, .. } => format!("fs={fs}"),
            Res::Fn(n) => format!("fn={n}"),
        }
    }
}

pub enum Node {
    /// cube(size) at the origin corner, then the transform.
    Cube([f64; 3], Transform),
    /// cylinder(h, r1, r2) from z = 0.
    Cyl(f64, f64, f64, Transform),
    Sphere(f64, Transform),
    /// rotate_extrude(angle) translate([major, 0]) circle(minor): a torus
    /// about z, swept from the xz plane.
    Torus(f64, f64, f64, Transform),
    /// linear_extrude(h) polygon(points): planes only (an explicit $fn).
    Prism(Vec<[f64; 2]>, f64, Transform),
    /// 'U'nion, 'D'ifference, 'I'ntersection.
    Op(char, Vec<Node>),
    /// A mesh-only child: its triangles are tagged faceted.
    Faceted(Box<Node>, u32),
}
use Node::*;

fn t(x: f64, y: f64, z: f64) -> Transform {
    Transform::translate([x, y, z])
}
fn rot(a: f64, b: f64, c: f64) -> Transform {
    Transform::rotate([a, b, c])
}
fn cube(x: f64, y: f64, z: f64, center: bool, m: Transform) -> Node {
    let m = if center {
        m.then_after(&t(-x / 2.0, -y / 2.0, -z / 2.0))
    } else {
        m
    };
    Cube([x, y, z], m)
}
fn cyl(h: f64, r1: f64, r2: f64, center: bool, m: Transform) -> Node {
    let m = if center {
        m.then_after(&t(0.0, 0.0, -h / 2.0))
    } else {
        m
    };
    Cyl(h, r1, r2, m)
}
fn ngon(h: f64, r: f64, n: u32, m: Transform) -> Node {
    let pts = (0..n)
        .map(|i| {
            let a = 2.0 * PI * i as f64 / n as f64;
            [r * a.cos(), r * a.sin()]
        })
        .collect();
    Prism(pts, h, m)
}
fn op(c: char, k: Vec<Node>) -> Node {
    Op(c, k)
}
const ID: Transform = Transform::IDENTITY;

fn plate(nx: usize, ny: usize) -> Node {
    let th = 5.0;
    let mut kids = vec![cube(15.0 * nx as f64, 15.0 * ny as f64, th, false, ID)];
    for i in 0..nx {
        for j in 0..ny {
            let (x, y) = (7.5 + 15.0 * i as f64, 7.5 + 15.0 * j as f64);
            kids.push(cyl(th + 2.0, 2.25, 2.25, false, t(x, y, -1.0)));
            kids.push(cyl(4.51, 0.0, 4.51, false, t(x, y, th - 4.5)));
        }
    }
    op('D', kids)
}

/// A case's tree and its reference volume (closed form, or `None` where
/// only the mesh can be compared).
pub fn case(name: &str) -> (Node, Option<f64>) {
    match name {
        // union(){cube(10); translate([5,5,5]) cube(10);}
        "b01" => (
            op(
                'U',
                vec![
                    cube(10.0, 10.0, 10.0, false, ID),
                    cube(10.0, 10.0, 10.0, false, t(5.0, 5.0, 5.0)),
                ],
            ),
            Some(1875.0),
        ),
        // difference(){cube(10); translate([5,5,-1]) cylinder(r=2.5,h=12);}
        "b03" => (
            op(
                'D',
                vec![
                    cube(10.0, 10.0, 10.0, false, ID),
                    cyl(12.0, 2.5, 2.5, false, t(5.0, 5.0, -1.0)),
                ],
            ),
            Some(1000.0 - 62.5 * PI),
        ),
        // difference(){cube(15,center=true); sphere(10);}
        "c01" => (
            op(
                'D',
                vec![cube(15.0, 15.0, 15.0, true, ID), Sphere(10.0, ID)],
            ),
            Some(3375.0 - sphere_in_cube()),
        ),
        // intersection(){cube(15,center=true); sphere(10);}
        "c02" => (
            op(
                'I',
                vec![cube(15.0, 15.0, 15.0, true, ID), Sphere(10.0, ID)],
            ),
            Some(sphere_in_cube()),
        ),
        // union(){cube(10); translate([10,5,0]) cylinder(r=3,h=15);}
        "c03" => (
            op(
                'U',
                vec![
                    cube(10.0, 10.0, 10.0, false, ID),
                    cyl(15.0, 3.0, 3.0, false, t(10.0, 5.0, 0.0)),
                ],
            ),
            Some(1000.0 + 90.0 * PI),
        ),
        // The countersunk plate of the B-rep audit, 2×2 and 6×4 holes.
        "c06" => (plate(2, 2), Some(4500.0 - 4.0 * 40.5 * PI)),
        "c07" => (plate(6, 4), Some(27000.0 - 24.0 * 40.5 * PI)),
        // The 20×20 plate, for timing (not in the default lists).
        "c07b" => (plate(20, 20), Some(450000.0 - 400.0 * 40.5 * PI)),
        // union() for(i=[0:9]) rotate(36*i) translate([3,0,0]) cylinder(r=2,h=10);
        "c08" => (
            op(
                'U',
                (0..10)
                    .map(|i| {
                        cyl(
                            10.0,
                            2.0,
                            2.0,
                            false,
                            rot(0.0, 0.0, 36.0 * i as f64).then_after(&t(3.0, 0.0, 0.0)),
                        )
                    })
                    .collect(),
            ),
            Some(10.0 * disc_union_area(10, 3.0, 2.0)),
        ),
        // union(){cube(10); translate([10,0,0]) cube(10);}
        "c09" => (
            op(
                'U',
                vec![
                    cube(10.0, 10.0, 10.0, false, ID),
                    cube(10.0, 10.0, 10.0, false, t(10.0, 0.0, 0.0)),
                ],
            ),
            Some(2000.0),
        ),
        // difference(){cube([20,20,10]); translate([5,5,5]) cube([10,10,5]);}
        "c10" => (
            op(
                'D',
                vec![
                    cube(20.0, 20.0, 10.0, false, ID),
                    cube(10.0, 10.0, 5.0, false, t(5.0, 5.0, 5.0)),
                ],
            ),
            Some(3500.0),
        ),
        // difference(){cylinder(r=10,h=5,$fn=6); translate([0,0,-1]) cylinder(r=4,h=7);}
        "c11" => (
            op(
                'D',
                vec![
                    ngon(5.0, 10.0, 6, ID),
                    cyl(7.0, 4.0, 4.0, false, t(0.0, 0.0, -1.0)),
                ],
            ),
            Some(5.0 * (1.5 * 3f64.sqrt() * 100.0) - 80.0 * PI),
        ),
        // difference(){cylinder(r=5,h=30); translate([0,0,20]) rotate([90,0,0]) cylinder(r=1.5,h=12,center=true);}
        "c12" => (
            op(
                'D',
                vec![
                    cyl(30.0, 5.0, 5.0, false, ID),
                    cyl(
                        12.0,
                        1.5,
                        1.5,
                        true,
                        t(0.0, 0.0, 20.0).then_after(&rot(90.0, 0.0, 0.0)),
                    ),
                ],
            ),
            Some(2286.313079308426),
        ),
        // difference(){sphere(10); translate([8,0,0]) sphere(6);}
        "c13" => (
            op('D', vec![Sphere(10.0, ID), Sphere(6.0, t(8.0, 0.0, 0.0))]),
            Some(4000.0 * PI / 3.0 - PI * 64.0 * 272.0 / 96.0),
        ),
        // union(){rotate([0,90,0]) cylinder(r=5,h=40,center=true); cylinder(r=5,h=20);}
        "c14" => (
            op(
                'U',
                vec![
                    cyl(40.0, 5.0, 5.0, true, rot(0.0, 90.0, 0.0)),
                    cyl(20.0, 5.0, 5.0, false, ID),
                ],
            ),
            Some(4379.055636634691),
        ),
        // difference(){cube([20,20,10]); translate([5,10,-1]) cylinder(r=5,h=12);}
        "c15" => (
            op(
                'D',
                vec![
                    cube(20.0, 20.0, 10.0, false, ID),
                    cyl(12.0, 5.0, 5.0, false, t(5.0, 10.0, -1.0)),
                ],
            ),
            Some(4000.0 - 250.0 * PI),
        ),
        // union(){cylinder(r=5,h=10); translate([0,0,10]) cylinder(r=5,h=10);}
        "x01" => (
            op(
                'U',
                vec![
                    cyl(10.0, 5.0, 5.0, false, ID),
                    cyl(10.0, 5.0, 5.0, false, t(0.0, 0.0, 10.0)),
                ],
            ),
            Some(500.0 * PI),
        ),
        // union(){cylinder(r=5,h=20); sphere(5); translate([0,0,20]) sphere(5);}
        "x02" => (
            op(
                'U',
                vec![
                    cyl(20.0, 5.0, 5.0, false, ID),
                    Sphere(5.0, ID),
                    Sphere(5.0, t(0.0, 0.0, 20.0)),
                ],
            ),
            Some(500.0 * PI + 500.0 * PI / 3.0),
        ),
        // Rounded plate: two cubes and four corner cylinders tangent to them.
        "x03" => {
            let mut k = vec![
                cube(20.0, 30.0, 5.0, false, t(5.0, 0.0, 0.0)),
                cube(30.0, 20.0, 5.0, false, t(0.0, 5.0, 0.0)),
            ];
            for (x, y) in [(5.0, 5.0), (25.0, 5.0), (5.0, 25.0), (25.0, 25.0)] {
                k.push(cyl(5.0, 5.0, 5.0, false, t(x, y, 0.0)));
            }
            (op('U', k), Some(5.0 * (800.0 + 25.0 * PI)))
        }
        // difference(){cube(20); translate([-1,10,20]) rotate([0,90,0]) cylinder(r=3,h=22);}
        "x04" => (
            op(
                'D',
                vec![
                    cube(20.0, 20.0, 20.0, false, ID),
                    cyl(
                        22.0,
                        3.0,
                        3.0,
                        false,
                        t(-1.0, 10.0, 20.0).then_after(&rot(0.0, 90.0, 0.0)),
                    ),
                ],
            ),
            Some(8000.0 - 90.0 * PI),
        ),
        // A counterbore flush with the top.
        "x05" => (
            op(
                'D',
                vec![
                    cube(20.0, 20.0, 10.0, false, ID),
                    cyl(12.0, 2.0, 2.0, false, t(10.0, 10.0, -1.0)),
                    cyl(3.0, 4.0, 4.0, false, t(10.0, 10.0, 7.0)),
                ],
            ),
            Some(4000.0 - 76.0 * PI),
        ),
        // union(){cylinder(r=3,h=10); translate([0,0,10]) sphere(5);}
        "x06" => (
            op(
                'U',
                vec![
                    cyl(10.0, 3.0, 3.0, false, ID),
                    Sphere(5.0, t(0.0, 0.0, 10.0)),
                ],
            ),
            Some(216.0 * PI),
        ),
        // A faceted sphere (a polyhedron, 12 segments) minus an exact skew hole.
        "x07" => (
            op(
                'D',
                vec![
                    Faceted(Box::new(Sphere(10.0, ID)), 12),
                    cyl(30.0, 3.0, 3.0, true, rot(30.0, 20.0, 0.0)),
                ],
            ),
            None,
        ),
        // A faceted slab unioned with an exact cylinder standing in it.
        "x08" => (
            op(
                'U',
                vec![
                    Faceted(Box::new(cube(20.0, 20.0, 5.0, false, ID)), 0),
                    cyl(10.0, 4.0, 4.0, false, t(10.0, 10.0, 2.0)),
                ],
            ),
            Some(2000.0 + 112.0 * PI),
        ),
        // difference(){cube(20,center=true); sphere(5);}: a void.
        "x09" => (
            op('D', vec![cube(20.0, 20.0, 20.0, true, ID), Sphere(5.0, ID)]),
            Some(8000.0 - 500.0 * PI / 3.0),
        ),
        // sphere(7): one face, no edges but its seam.
        "x10" => (Sphere(7.0, ID), Some(4.0 * PI * 343.0 / 3.0)),
        // cylinder(r1=5, r2=0, h=10): a cone with its apex in the face.
        "x11" => (Cyl(10.0, 5.0, 0.0, ID), Some(250.0 * PI / 3.0)),
        // A fillet as CSG: difference(){cube(20); translate([17,17,-1]) difference(){cube([4,4,22]); cylinder(r=3,h=22);}}
        "f01" => (
            op(
                'D',
                vec![
                    cube(20.0, 20.0, 20.0, false, ID),
                    op(
                        'D',
                        vec![
                            cube(4.0, 4.0, 22.0, false, t(17.0, 17.0, -1.0)),
                            cyl(22.0, 3.0, 3.0, false, t(17.0, 17.0, -1.0)),
                        ],
                    ),
                ],
            ),
            Some(7820.0 + 45.0 * PI),
        ),
        // Four vertical edges filleted r=3, plus a through hole.
        "f02" => {
            let mut k = vec![cube(20.0, 20.0, 10.0, false, ID)];
            for (x, y, a) in [
                (17.0, 17.0, 0.0),
                (3.0, 17.0, 90.0),
                (3.0, 3.0, 180.0),
                (17.0, 3.0, 270.0),
            ] {
                let m = t(x, y, -1.0).then_after(&rot(0.0, 0.0, a));
                k.push(op(
                    'D',
                    vec![
                        cube(4.0, 4.0, 12.0, false, m),
                        cyl(12.0, 3.0, 3.0, false, m),
                    ],
                ));
            }
            k.push(cyl(12.0, 4.0, 4.0, false, t(10.0, 10.0, -1.0)));
            (op('D', k), Some(3640.0 - 70.0 * PI))
        }
        // rotate_extrude() translate([10,0]) circle(3): a whole torus,
        // one face with two seams.
        "t01" => (Torus(10.0, 3.0, 360.0, ID), Some(180.0 * PI * PI)),
        // rotate_extrude(angle=90) ...: wraps the tube, not the axis.
        "t02" => (Torus(10.0, 3.0, 90.0, ID), Some(45.0 * PI * PI)),
        // The same at 270 degrees, tilted.
        "t03" => (
            Torus(10.0, 3.0, 270.0, rot(30.0, 20.0, 10.0)),
            Some(135.0 * PI * PI),
        ),
        // difference(){torus; translate([-20,-20,-10]) cube([40,40,10]);}:
        // the upper half, a band about the axis.
        "t04" => (
            op(
                'D',
                vec![
                    Torus(10.0, 3.0, 360.0, ID),
                    cube(40.0, 40.0, 10.0, false, t(-20.0, -20.0, -10.0)),
                ],
            ),
            Some(90.0 * PI * PI),
        ),
        // A quarter cut away by a cube: ends on meridians.
        "t05" => (
            op(
                'D',
                vec![
                    Torus(10.0, 3.0, 360.0, ID),
                    cube(20.0, 20.0, 10.0, false, t(0.0, 0.0, -5.0)),
                ],
            ),
            Some(135.0 * PI * PI),
        ),
        // A hole drilled down through the tube: a whole torus with two
        // holes, so both seams must miss them.
        "t06" => (
            op(
                'D',
                vec![
                    Torus(10.0, 3.0, 360.0, ID),
                    cyl(10.0, 1.0, 1.0, false, t(10.0, 0.0, -5.0)),
                ],
            ),
            None,
        ),
        // A disc with a fully rounded rim: cylinder(r=7,h=6,center=true)
        // ∪ the torus, its flat faces tangent to the tube.
        "t07" => (
            op(
                'U',
                vec![cyl(6.0, 7.0, 7.0, true, ID), Torus(7.0, 3.0, 360.0, ID)],
            ),
            Some(330.0 * PI + 63.0 * PI * PI),
        ),
        // A coaxial cylinder through the tube: circles where they cross.
        "t08" => (
            op(
                'U',
                vec![Torus(10.0, 3.0, 360.0, ID), cyl(2.0, 11.0, 11.0, true, ID)],
            ),
            None,
        ),
        _ => panic!("unknown case {name}"),
    }
}

pub const FIFTEEN: [&str; 15] = [
    "b01", "b03", "c01", "c02", "c03", "c06", "c07", "c08", "c09", "c10", "c11", "c12", "c13",
    "c14", "c15",
];
pub const IDIOMS: [&str; 6] = ["x01", "x02", "x03", "x04", "x05", "x06"];
pub const MORE: [&str; 7] = ["x07", "x08", "x09", "x10", "x11", "f01", "f02"];
pub const TORI: [&str; 8] = ["t01", "t02", "t03", "t04", "t05", "t06", "t07", "t08"];

/// The volume of sphere(10) ∩ cube(15, center = true): the sphere less six
/// caps of height 2.5 (they do not overlap).
fn sphere_in_cube() -> f64 {
    let (r, h) = (10.0, 2.5);
    4.0 / 3.0 * PI * r * r * r - 6.0 * PI * h * h * (3.0 * r - h) / 3.0
}

/// The area of the union of `n` discs of radius `r` centred evenly on a
/// circle of radius `c`, by Green's theorem over the uncovered arcs.
fn disc_union_area(n: usize, c: f64, r: f64) -> f64 {
    let centers: Vec<(f64, f64)> = (0..n)
        .map(|i| {
            let a = 2.0 * PI * i as f64 / n as f64;
            (c * a.cos(), c * a.sin())
        })
        .collect();
    let mut area = 0.0;
    for (i, &(cx, cy)) in centers.iter().enumerate() {
        // Angular intervals of circle i covered by the other discs.
        let mut cov: Vec<(f64, f64)> = Vec::new();
        for (j, &(dx, dy)) in centers.iter().enumerate() {
            if i == j {
                continue;
            }
            let (ex, ey) = (dx - cx, dy - cy);
            let d = (ex * ex + ey * ey).sqrt();
            if d >= 2.0 * r {
                continue;
            }
            let mid = ey.atan2(ex);
            let half = (d / (2.0 * r)).acos();
            cov.push((mid - half, mid + half));
        }
        // Uncovered arcs, by sampling the angle finely and integrating
        // exactly per uncovered interval.
        let steps = 1 << 16;
        let covered = |a: f64| {
            cov.iter().any(|&(lo, hi)| {
                let mut x = a - lo;
                x -= 2.0 * PI * (x / (2.0 * PI)).floor();
                x < hi - lo
            })
        };
        let mut k = 0;
        while k < steps {
            let a0 = 2.0 * PI * k as f64 / steps as f64;
            if covered(a0 + 1e-12) {
                k += 1;
                continue;
            }
            let mut k1 = k;
            while k1 < steps && !covered(2.0 * PI * k1 as f64 / steps as f64 + 1e-12) {
                k1 += 1;
            }
            // Refine both ends onto the covering interval boundaries.
            let snap = |a: f64| {
                let mut best = a;
                let mut bd = f64::INFINITY;
                for &(lo, hi) in &cov {
                    for e in [lo, hi] {
                        let mut d = e - a;
                        d -= 2.0 * PI * (d / (2.0 * PI)).round();
                        if d.abs() < bd {
                            bd = d.abs();
                            best = a + d;
                        }
                    }
                }
                if bd < 1e-3 { best } else { a }
            };
            let t0 = snap(a0);
            let t1 = snap(2.0 * PI * k1 as f64 / steps as f64);
            area += 0.5
                * (r * r * (t1 - t0)
                    + r * (cx * (t1.sin() - t0.sin()) - cy * (t1.cos() - t0.cos())));
            k = k1;
        }
    }
    area
}

/// Evaluates a tree with Manifold. Surfaces go in one table, and each
/// triangle's `face_id` is its surface's index there.
pub fn eval(n: &Node, res: Res, table: &mut Vec<Surface>) -> Manifold {
    match n {
        Op(c, kids) => {
            let ms: Vec<Manifold> = kids.iter().map(|k| eval(k, res, table)).collect();
            match c {
                'U' => Manifold::batch_boolean(&ms, OpType::Add),
                'D' => {
                    let rest = Manifold::batch_boolean(&ms[1..], OpType::Add);
                    ms[0].boolean(&rest, OpType::Subtract)
                }
                _ => Manifold::batch_boolean(&ms, OpType::Intersect),
            }
        }
        Faceted(k, segs) => {
            let mut m = prim(k, if *segs > 0 { Res::Fn(*segs) } else { res });
            m = primitives::faceted(m);
            to_manifold(&m, table)
        }
        _ => to_manifold(&prim(n, res), table),
    }
}

fn prim(n: &Node, res: Res) -> TaggedMesh {
    match n {
        Cube(s, m) => primitives::cuboid(*s, m),
        Cyl(h, r1, r2, m) => primitives::frustum(*h, *r1, *r2, res.segments(r1.max(*r2)), m),
        Sphere(r, m) => primitives::sphere(*r, res.segments(*r), m),
        Torus(big, r, a, m) => {
            primitives::torus(*big, *r, res.segments(*big), res.segments(*r), *a, m)
        }
        Prism(p, h, m) => primitives::prism(p, *h, m),
        _ => unreachable!(),
    }
}

fn to_manifold(p: &TaggedMesh, table: &mut Vec<Surface>) -> Manifold {
    let off = table.len() as u64;
    table.extend(p.surfaces.iter().cloned());
    let id = Manifold::reserve_ids(1);
    let mesh = MeshGL64 {
        num_prop: 3,
        vert_properties: p.positions.iter().flatten().copied().collect(),
        tri_verts: p.triangles.iter().flatten().map(|&i| i as u64).collect(),
        face_id: p.triangle_surface.iter().map(|&s| s as u64 + off).collect(),
        run_index: vec![0, 3 * p.triangles.len() as u64],
        run_original_id: vec![id],
        ..Default::default()
    };
    let m = Manifold::from_mesh_gl64(&mesh);
    assert_eq!(
        m.status(),
        manifold_rust::types::Error::NoError,
        "primitive is not manifold"
    );
    m
}

/// Reconstructs a case at `res`, retrying with a finer attribution mesh
/// (up to three times) when the mesh's topology differs from the exact
/// model's, as a caller would (the audit's "retry at a second
/// resolution"). Returns the mesh, the B-rep and the resolutions that were
/// rejected first.
pub fn build(
    name: &str,
    res: Res,
) -> (
    TaggedMesh,
    f64,
    Option<f64>,
    Result<meshbrep::Brep, meshbrep::Error>,
    Vec<Res>,
) {
    let mut res = res;
    let mut rejected = Vec::new();
    loop {
        let (mesh, vol, reference) = tagged(name, res);
        let r = meshbrep::reconstruct(&mesh, &meshbrep::Options::default());
        if matches!(r, Err(meshbrep::Error::TopologyMismatch(_))) && rejected.len() < 3 {
            rejected.push(res);
            res = res.finer();
            continue;
        }
        return (mesh, vol, reference, r, rejected);
    }
}

/// The tagged mesh of a case at a resolution, with Manifold's volume.
pub fn tagged(name: &str, res: Res) -> (TaggedMesh, f64, Option<f64>) {
    let (node, reference) = case(name);
    let mut table = Vec::new();
    let m = eval(&node, res, &mut table);
    let gl = m.get_mesh_gl64(-1);
    let np = gl.num_prop as usize;
    let mesh = TaggedMesh {
        positions: gl
            .vert_properties
            .chunks(np)
            .map(|c| [c[0], c[1], c[2]])
            .collect(),
        triangles: gl
            .tri_verts
            .chunks(3)
            .map(|c| [c[0] as u32, c[1] as u32, c[2] as u32])
            .collect(),
        triangle_surface: gl.face_id.iter().map(|&f| f as u32).collect(),
        surfaces: table,
    };
    (mesh, m.volume(), reference)
}
