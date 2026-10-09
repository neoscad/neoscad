//! Blends between faces that share no axis (`blend::Path::Curve`): the
//! tools of a tee of two cylinders, a cylinder through a plane at a
//! slant, a rod meeting a ball off its centre, applied with Manifold to
//! tagged primitives as a caller would, then reconstructed, validated,
//! measured and written twice to the same bytes. Each contact on a
//! cylinder is conformed to the cylinder's polygon (`generators`), from
//! the mesh's own vertices on it.

use manifold_rust::manifold::Manifold;
use manifold_rust::types::{MeshGL64, OpType};
use meshbrep::blend::{self, BlendEdge, BlendFace, BlendSpec, End, Path, Profile};
use meshbrep::primitives::{self, Transform};
use meshbrep::{
    Contact, Options, StepOptions, Surface, TaggedMesh, Tolerances, measure, reconstruct, validate,
    write_step,
};

/// Surfaces in one table, each triangle's `face_id` its index there.
struct Scene {
    table: Vec<Surface>,
}

impl Scene {
    fn solid(&mut self, p: &TaggedMesh) -> Manifold {
        let off = self.table.len() as u64;
        self.table.extend(p.surfaces.iter().cloned());
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
            "a solid is not manifold"
        );
        m
    }

    fn tagged(&self, m: &Manifold) -> TaggedMesh {
        let gl = m.get_mesh_gl64(-1);
        let np = gl.num_prop as usize;
        TaggedMesh {
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
            surfaces: self.table.clone(),
        }
    }
}

fn sub(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}
fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
fn norm(a: [f64; 3]) -> f64 {
    dot(a, a).sqrt()
}

/// The mesh's triangles on a face's surface (its record in the table is
/// the face's), as a caller conforms a tool to them.
fn facets(m: &TaggedMesh, face: &BlendFace) -> Vec<[[f64; 3]; 3]> {
    let close = |a: [f64; 3], b: [f64; 3]| norm(sub(a, b)) <= 1e-9;
    let parallel = |a: [f64; 3], b: [f64; 3]| dot(a, b).abs() >= 1.0 - 1e-12;
    let same = |s: &Surface| match (s, face) {
        (
            Surface::Cylinder {
                origin,
                axis,
                radius,
            },
            BlendFace::Cylinder {
                origin: o,
                axis: a,
                radius: r,
                ..
            },
        ) => {
            parallel(*axis, *a) && (radius - r).abs() <= 1e-9 && {
                let d = sub(*origin, *o);
                let h = dot(d, *a);
                norm(sub(d, [a[0] * h, a[1] * h, a[2] * h])) <= 1e-9
            }
        }
        (
            Surface::Cone { apex, axis, slope },
            BlendFace::Cone {
                apex: p,
                axis: a,
                slope: k,
                ..
            },
        ) => close(*apex, *p) && parallel(*axis, *a) && (slope - k).abs() <= 1e-12,
        (
            Surface::Sphere { center, radius },
            BlendFace::Sphere {
                center: c,
                radius: r,
                ..
            },
        ) => close(*center, *c) && (radius - r).abs() <= 1e-9,
        _ => false,
    };
    m.triangles
        .iter()
        .zip(&m.triangle_surface)
        .filter(|(_, s)| same(&m.surfaces[**s as usize]))
        .map(|(t, _)| t.map(|i| m.positions[i as usize]))
        .collect()
}

/// Points along a closed curve given as a function of an angle.
fn closed_points(f: &dyn Fn(f64) -> [f64; 3], n: usize) -> Vec<[f64; 3]> {
    let mut out: Vec<[f64; 3]> = (0..n)
        .map(|k| f(std::f64::consts::TAU * k as f64 / n as f64))
        .collect();
    out.push(out[0]);
    out
}

struct Case {
    name: &'static str,
    /// The solid before blending, and the scene's table.
    solid: Manifold,
    scene: Scene,
    edge: BlendEdge,
    /// Further edges blended with it (one call over several).
    more: Vec<BlendEdge>,
    profile: Profile,
    size: f64,
}

/// A tee: a rod of radius `big` along x (length 30), and one of radius
/// `small` standing on it along z from its axis to z = 12; their concave
/// junction filleted (`r`).
fn tee(name: &'static str, big: f64, small: f64, n: u32, profile: Profile, r: f64) -> Case {
    let mut scene = Scene { table: Vec::new() };
    let main = primitives::frustum(
        30.0,
        big,
        big,
        n,
        &Transform::rotate([0.0, 90.0, 0.0]).then_after(&Transform::translate([0.0, 0.0, -15.0])),
    );
    let branch = primitives::frustum(
        12.0,
        small,
        small,
        n,
        &Transform::translate([0.0, 0.0, 0.0]),
    );
    let a = scene.solid(&main);
    let b = scene.solid(&branch);
    let solid = a.boolean(&b, OpType::Add);
    let points = closed_points(
        &|t| {
            let (x, y) = (small * t.cos(), small * t.sin());
            [x, y, (big * big - y * y).sqrt()]
        },
        64,
    );
    let faces = [
        BlendFace::Cylinder {
            origin: [0.0; 3],
            axis: [1.0, 0.0, 0.0],
            radius: big,
            convex: true,
        },
        BlendFace::Cylinder {
            origin: [0.0; 3],
            axis: [0.0, 0.0, 1.0],
            radius: small,
            convex: true,
        },
    ];
    let edge = BlendEdge {
        from: points[0],
        to: points[0],
        faces,
        face_ids: [0, 1],
        convex: false,
        ends: [End::Open { face: None }, End::Open { face: None }],
        path: Path::Curve {
            points,
            facets: [Vec::new(), Vec::new()],
        },
        margin: None,
    };
    Case {
        name,
        solid,
        scene,
        edge,
        more: Vec::new(),
        profile,
        size: r,
    }
}

/// A tee as OpenSCAD makes one: a rod of radius `big` along x (`len`
/// long, `nb` sides) and a branch of radius `small` (`ns` sides) from its
/// axis up to `h`, its axis at `y = off`; the junction blended.
#[allow(clippy::too_many_arguments)]
fn tee_off(
    name: &'static str,
    big: f64,
    small: f64,
    off: f64,
    h: f64,
    nb: u32,
    ns: u32,
    profile: Profile,
    r: f64,
) -> Case {
    let mut scene = Scene { table: Vec::new() };
    let len = 4.0 * big;
    let main = primitives::frustum(
        len,
        big,
        big,
        nb,
        &Transform::rotate([0.0, 90.0, 0.0]).then_after(&Transform::translate([
            0.0,
            0.0,
            -len / 2.0,
        ])),
    );
    let branch = primitives::frustum(h, small, small, ns, &Transform::translate([0.0, off, 0.0]));
    let a = scene.solid(&main);
    let b = scene.solid(&branch);
    let solid = a.boolean(&b, OpType::Add);
    let points = closed_points(
        &|t| {
            let (x, y) = (small * t.cos(), off + small * t.sin());
            [x, y, (big * big - y * y).sqrt()]
        },
        64,
    );
    let faces = [
        x_cylinder(big, true),
        BlendFace::Cylinder {
            origin: [0.0, off, 0.0],
            axis: [0.0, 0.0, 1.0],
            radius: small,
            convex: true,
        },
    ];
    Case {
        name,
        solid,
        scene,
        edge: curve_edge(points, faces, false, OPEN),
        more: Vec::new(),
        profile,
        size: r,
    }
}

/// A plate 6 thick with a hole of radius 4 through it, its axis tilted
/// by 30° about x; the top rim (convex) blended.
fn oblique_hole(name: &'static str, n: u32, profile: Profile, r: f64) -> Case {
    let mut scene = Scene { table: Vec::new() };
    let plate = primitives::cuboid(
        [30.0, 30.0, 6.0],
        &Transform::translate([-15.0, -15.0, 0.0]),
    );
    let tilt = Transform::translate([0.0, 0.0, 3.0])
        .then_after(&Transform::rotate([30.0, 0.0, 0.0]))
        .then_after(&Transform::translate([0.0, 0.0, -15.0]));
    let hole = primitives::frustum(30.0, 4.0, 4.0, n, &tilt);
    let a = scene.solid(&plate);
    let b = scene.solid(&hole);
    let solid = a.boolean(&b, OpType::Subtract);
    let axis = tilt.direction([0.0, 0.0, 1.0]);
    let origin = tilt.point([0.0, 0.0, 15.0]);
    // The top rim: where the hole's cylinder meets z = 6.
    let x0 = [1.0, 0.0, 0.0];
    let y0 = [
        axis[1] * x0[2] - axis[2] * x0[1],
        axis[2] * x0[0] - axis[0] * x0[2],
        axis[0] * x0[1] - axis[1] * x0[0],
    ];
    let points = closed_points(
        &|t| {
            let d = [
                4.0 * (t.cos() * x0[0] + t.sin() * y0[0]),
                4.0 * (t.cos() * x0[1] + t.sin() * y0[1]),
                4.0 * (t.cos() * x0[2] + t.sin() * y0[2]),
            ];
            let p = [origin[0] + d[0], origin[1] + d[1], origin[2] + d[2]];
            let s = (6.0 - p[2]) / axis[2];
            [p[0] + axis[0] * s, p[1] + axis[1] * s, 6.0]
        },
        64,
    );
    let faces = [
        BlendFace::Plane {
            origin: [0.0, 0.0, 6.0],
            normal: [0.0, 0.0, 1.0],
        },
        BlendFace::Cylinder {
            origin,
            axis,
            radius: 4.0,
            convex: false,
        },
    ];
    let edge = BlendEdge {
        from: points[0],
        to: points[0],
        faces,
        face_ids: [0, 1],
        convex: true,
        ends: [End::Open { face: None }, End::Open { face: None }],
        path: Path::Curve {
            points,
            facets: [Vec::new(), Vec::new()],
        },
        margin: None,
    };
    Case {
        name,
        solid,
        scene,
        edge,
        more: Vec::new(),
        profile,
        size: r,
    }
}

/// The main rod of [`tee`] and friends: radius `big` along x, 30 long.
fn rod_x(big: f64, n: u32) -> TaggedMesh {
    primitives::frustum(
        30.0,
        big,
        big,
        n,
        &Transform::rotate([0.0, 90.0, 0.0]).then_after(&Transform::translate([0.0, 0.0, -15.0])),
    )
}

fn x_cylinder(big: f64, convex: bool) -> BlendFace {
    BlendFace::Cylinder {
        origin: [0.0; 3],
        axis: [1.0, 0.0, 0.0],
        radius: big,
        convex,
    }
}

fn curve_edge(
    points: Vec<[f64; 3]>,
    faces: [BlendFace; 2],
    convex: bool,
    ends: [End; 2],
) -> BlendEdge {
    let n = points.len();
    BlendEdge {
        from: points[0],
        to: points[n - 1],
        faces,
        face_ids: [0, 1],
        convex,
        ends,
        path: Path::Curve {
            points,
            facets: [Vec::new(), Vec::new()],
        },
        margin: None,
    }
}

const OPEN: [End; 2] = [End::Open { face: None }, End::Open { face: None }];

/// A rod of radius 5 along x with a hole of radius `small` drilled
/// through it along z: the hole's top rim (convex) blended.
fn cross_hole(name: &'static str, small: f64, n: u32, profile: Profile, r: f64) -> Case {
    let mut scene = Scene { table: Vec::new() };
    let a = scene.solid(&rod_x(5.0, n));
    let b = scene.solid(&primitives::frustum(
        20.0,
        small,
        small,
        n,
        &Transform::translate([0.0, 0.0, -10.0]),
    ));
    let solid = a.boolean(&b, OpType::Subtract);
    let points = closed_points(
        &|t| {
            let (x, y) = (small * t.cos(), small * t.sin());
            [x, y, (25.0 - y * y).sqrt()]
        },
        64,
    );
    let faces = [
        x_cylinder(5.0, true),
        BlendFace::Cylinder {
            origin: [0.0; 3],
            axis: [0.0, 0.0, 1.0],
            radius: small,
            convex: false,
        },
    ];
    Case {
        name,
        solid,
        scene,
        edge: curve_edge(points, faces, true, OPEN),
        more: Vec::new(),
        profile,
        size: r,
    }
}

/// A rod of radius `big` along x (length 3 `big`) with a hole of radius
/// `small` through it along z at `(x0, y0)`: both rims (convex) blended.
#[allow(clippy::too_many_arguments)]
fn cross_hole_both(
    name: &'static str,
    big: f64,
    small: f64,
    x0: f64,
    y0: f64,
    n: u32,
    profile: Profile,
    r: f64,
) -> Case {
    let mut scene = Scene { table: Vec::new() };
    let rod = primitives::frustum(
        3.0 * big,
        big,
        big,
        n,
        &Transform::rotate([0.0, 90.0, 0.0]).then_after(&Transform::translate([
            0.0,
            0.0,
            -1.5 * big,
        ])),
    );
    let a = scene.solid(&rod);
    let b = scene.solid(&primitives::frustum(
        4.0 * big,
        small,
        small,
        n,
        &Transform::translate([x0, y0, -2.0 * big]),
    ));
    let solid = a.boolean(&b, OpType::Subtract);
    let faces = [
        x_cylinder(big, true),
        BlendFace::Cylinder {
            origin: [x0, y0, 0.0],
            axis: [0.0, 0.0, 1.0],
            radius: small,
            convex: false,
        },
    ];
    let rim = |s: f64| {
        closed_points(
            &|t| {
                let (x, y) = (x0 + small * t.cos(), y0 + small * t.sin());
                [x, y, s * (big * big - y * y).sqrt()]
            },
            64,
        )
    };
    Case {
        name,
        solid,
        scene,
        edge: curve_edge(rim(1.0), faces.clone(), true, OPEN),
        more: vec![curve_edge(rim(-1.0), faces, true, OPEN)],
        profile,
        size: r,
    }
}

/// A ball of radius 6 with a rod of radius 2 standing in it, its axis
/// 2.5 off the ball's centre: the concave junction blended.
fn ball_rod(name: &'static str, n: u32, profile: Profile, r: f64) -> Case {
    let mut scene = Scene { table: Vec::new() };
    let a = scene.solid(&primitives::sphere(6.0, n, &Transform::translate([0.0; 3])));
    let b = scene.solid(&primitives::frustum(
        12.0,
        2.0,
        2.0,
        n,
        &Transform::translate([2.5, 0.0, 0.0]),
    ));
    let solid = a.boolean(&b, OpType::Add);
    let points = closed_points(
        &|t| {
            let (x, y) = (2.5 + 2.0 * t.cos(), 2.0 * t.sin());
            [x, y, (36.0 - x * x - y * y).sqrt()]
        },
        64,
    );
    let faces = [
        BlendFace::Sphere {
            center: [0.0; 3],
            radius: 6.0,
            convex: true,
        },
        BlendFace::Cylinder {
            origin: [2.5, 0.0, 0.0],
            axis: [0.0, 0.0, 1.0],
            radius: 2.0,
            convex: true,
        },
    ];
    Case {
        name,
        solid,
        scene,
        edge: curve_edge(points, faces, false, OPEN),
        more: Vec::new(),
        profile,
        size: r,
    }
}

/// A rod of radius 5 along x with a cone standing on it (radius 3 at
/// z = 0 to 1.5 at z = 10): the concave junction blended.
fn cone_boss(name: &'static str, n: u32, profile: Profile, r: f64) -> Case {
    let mut scene = Scene { table: Vec::new() };
    let a = scene.solid(&rod_x(5.0, n));
    let b = scene.solid(&primitives::frustum(
        10.0,
        3.0,
        1.5,
        n,
        &Transform::translate([0.0; 3]),
    ));
    let solid = a.boolean(&b, OpType::Add);
    let k = 0.15;
    let points = closed_points(
        &|t| {
            // (ρ(z) sin t)² + z² = 25 with ρ = 3 - k z: Newton from z = 4.5.
            let mut z: f64 = 4.5;
            for _ in 0..50 {
                let rho = 3.0 - k * z;
                let f = (rho * t.sin()).powi(2) + z * z - 25.0;
                let df = 2.0 * rho * t.sin() * t.sin() * -k + 2.0 * z;
                z -= f / df;
            }
            let rho = 3.0 - k * z;
            [rho * t.cos(), rho * t.sin(), z]
        },
        64,
    );
    let faces = [
        x_cylinder(5.0, true),
        BlendFace::Cone {
            apex: [0.0, 0.0, 20.0],
            axis: [0.0, 0.0, -1.0],
            slope: k,
            convex: true,
        },
    ];
    Case {
        name,
        solid,
        scene,
        edge: curve_edge(points, faces, false, OPEN),
        more: Vec::new(),
        profile,
        size: r,
    }
}

/// [`tee`] cut in half by the plane y = 0 (y >= 0 kept): the junction is
/// half the loop, ending on that plane both ways.
fn half_tee(name: &'static str, n: u32, profile: Profile, r: f64) -> Case {
    let mut scene = Scene { table: Vec::new() };
    let a = scene.solid(&rod_x(5.0, n));
    let b = scene.solid(&primitives::frustum(
        12.0,
        3.0,
        3.0,
        n,
        &Transform::translate([0.0; 3]),
    ));
    let cut = scene.solid(&primitives::cuboid(
        [40.0, 20.0, 40.0],
        &Transform::translate([-20.0, -20.0, -20.0]),
    ));
    let solid = a.boolean(&b, OpType::Add).boolean(&cut, OpType::Subtract);
    let points: Vec<[f64; 3]> = (0..=32)
        .map(|k| {
            let t = std::f64::consts::PI * k as f64 / 32.0;
            let (x, y) = (3.0 * t.cos(), 3.0 * t.sin());
            [x, y, (25.0 - y * y).sqrt()]
        })
        .collect();
    let faces = [
        x_cylinder(5.0, true),
        BlendFace::Cylinder {
            origin: [0.0; 3],
            axis: [0.0, 0.0, 1.0],
            radius: 3.0,
            convex: true,
        },
    ];
    let end = End::Plane {
        origin: [0.0; 3],
        normal: [0.0, -1.0, 0.0],
    };
    Case {
        name,
        solid,
        scene,
        edge: curve_edge(points, faces, false, [end.clone(), end]),
        more: Vec::new(),
        profile,
        size: r,
    }
}

/// A boss of radius 6 on a plate (the golden `boss_plate`), its rim
/// blended as a curve rather than revolved: the volume has a closed form
/// (Pappus), 7565.744034295069 for r = 2.
fn boss_plate(name: &'static str, n: u32, r: f64) -> Case {
    let mut scene = Scene { table: Vec::new() };
    let a = scene.solid(&primitives::cuboid(
        [40.0, 40.0, 4.0],
        &Transform::translate([-20.0, -20.0, 0.0]),
    ));
    let b = scene.solid(&primitives::frustum(
        14.0,
        6.0,
        6.0,
        n,
        &Transform::translate([0.0; 3]),
    ));
    let solid = a.boolean(&b, OpType::Add);
    let points = closed_points(&|t| [6.0 * t.cos(), 6.0 * t.sin(), 4.0], 64);
    let faces = [
        BlendFace::Plane {
            origin: [0.0, 0.0, 4.0],
            normal: [0.0, 0.0, 1.0],
        },
        BlendFace::Cylinder {
            origin: [0.0; 3],
            axis: [0.0, 0.0, 1.0],
            radius: 6.0,
            convex: true,
        },
    ];
    Case {
        name,
        solid,
        scene,
        edge: curve_edge(points, faces, false, OPEN),
        more: Vec::new(),
        profile: Profile::Fillet,
        size: r,
    }
}

struct Outcome {
    faces: usize,
    bspline: usize,
    boundary: usize,
    volume: f64,
    mesh_volume: f64,
    fit: f64,
    step: String,
}

/// Arc segments for a sweep: 16 to a half turn.
fn segments(sweep: f64) -> u32 {
    ((sweep / (std::f64::consts::PI / 16.0)).ceil() as u32).max(2)
}

/// Builds the case's tool (conformed to the solid's facets), applies it
/// with Manifold, reconstructs, validates at 1e-6, measures, and writes
/// STEP twice (to the same bytes).
fn run(mut c: Case) -> Result<Outcome, String> {
    let before = c.scene.tagged(&c.solid);
    let mut edges = vec![c.edge.clone()];
    edges.extend(c.more.iter().cloned());
    for e in &mut edges {
        let given = [0, 1].map(|k| facets(&before, &e.faces[k]));
        if let Path::Curve { facets: f, .. } = &mut e.path {
            *f = given;
        }
    }
    let spec = BlendSpec {
        profile: c.profile,
        size: c.size,
        edges,
        corners: vec![],
    };
    blend::check(&spec).map_err(|e| format!("check: {e}"))?;
    // A name ending in "coarse" gets OpenSCAD's default arcs for a small
    // radius: two segments to a quarter turn.
    let coarse = |_: f64| 2;
    let seg: &dyn Fn(f64) -> u32 = if c.name.ends_with("coarse") {
        &coarse
    } else {
        &segments
    };
    let tools = blend::tools(&spec, seg).map_err(|e| format!("tools: {e}"))?;
    let mut solid = c.solid.clone();
    let mut fit: f64 = 0.0;
    for t in &tools {
        fit = fit.max(t.fit);
        let m = c.scene.solid(&t.mesh);
        solid = solid.boolean(&m, if t.add { OpType::Add } else { OpType::Subtract });
    }
    let mesh = c.scene.tagged(&solid);
    let options = Options {
        tolerances: Tolerances {
            surface_fit: Tolerances::default().surface_fit.max(10.0 * fit),
            ..Default::default()
        },
        ..Default::default()
    };
    let brep = reconstruct(&mesh, &options).map_err(|e| format!("reconstruct: {e}"))?;
    let v = validate(&brep, 1e-6);
    if !v.is_valid() {
        return Err(format!("invalid: {:?}", v.errors));
    }
    let m = measure(&brep).map_err(|e| format!("measure: {e}"))?;
    let step = write_step(&brep, &StepOptions::default());
    let again = write_step(
        &reconstruct(&mesh, &options).map_err(|e| format!("again: {e}"))?,
        &StepOptions::default(),
    );
    if step != again {
        return Err("STEP differs between runs".into());
    }
    let faceted = brep.faces.iter().filter(|f| f.faceted).count();
    if faceted > 0 {
        return Err(format!("{faceted} faceted faces"));
    }
    Ok(Outcome {
        faces: brep.faces.len(),
        bspline: brep
            .faces
            .iter()
            .filter(|f| matches!(f.surface, Surface::BSpline(_)))
            .count(),
        boundary: brep
            .report
            .tangencies
            .iter()
            .filter(|t| matches!(t.contact, Contact::Boundary { .. }))
            .count(),
        volume: m.volume,
        mesh_volume: solid.volume(),
        fit,
        step,
    })
}

/// A case's constructor, and the B-spline faces and boundary contacts
/// the result must have.
type Expect = (Box<dyn Fn() -> Case>, usize, usize);

/// The cases (a closed blend is two patches touching each other at both
/// ends; a chamfer touches its faces at an angle).
fn cases() -> Vec<Expect> {
    vec![
        (Box::new(|| boss_plate("boss plate", 32, 2.0)), 2, 6),
        (
            Box::new(|| tee("tee 5/3 fillet", 5.0, 3.0, 32, Profile::Fillet, 1.0)),
            2,
            6,
        ),
        (
            Box::new(|| tee("tee 5/3 fillet 64", 5.0, 3.0, 64, Profile::Fillet, 1.0)),
            2,
            6,
        ),
        (
            Box::new(|| tee("tee 5/3 fillet 0.5", 5.0, 3.0, 32, Profile::Fillet, 0.5)),
            2,
            6,
        ),
        (
            Box::new(|| tee("tee 5/3 chamfer", 5.0, 3.0, 32, Profile::Chamfer, 0.8)),
            2,
            2,
        ),
        (
            Box::new(|| half_tee("half tee fillet", 32, Profile::Fillet, 1.0)),
            1,
            2,
        ),
        (
            Box::new(|| half_tee("half tee fillet 16", 16, Profile::Fillet, 1.0)),
            1,
            2,
        ),
        (
            Box::new(|| half_tee("half tee chamfer", 32, Profile::Chamfer, 0.8)),
            1,
            0,
        ),
        (
            Box::new(|| oblique_hole("oblique hole fillet", 32, Profile::Fillet, 1.0)),
            2,
            6,
        ),
        (
            Box::new(|| oblique_hole("oblique hole chamfer", 32, Profile::Chamfer, 1.0)),
            2,
            2,
        ),
        (
            Box::new(|| cross_hole("cross hole fillet", 2.0, 32, Profile::Fillet, 0.8)),
            2,
            6,
        ),
        (
            Box::new(|| cross_hole("cross hole chamfer", 2.0, 32, Profile::Chamfer, 0.8)),
            2,
            2,
        ),
        (
            Box::new(|| ball_rod("ball rod fillet", 32, Profile::Fillet, 1.0)),
            2,
            6,
        ),
        (
            Box::new(|| ball_rod("ball rod chamfer", 32, Profile::Chamfer, 0.8)),
            2,
            2,
        ),
        (
            Box::new(|| cone_boss("cone boss fillet", 32, Profile::Fillet, 1.0)),
            2,
            6,
        ),
        (
            Box::new(|| tee("tee 5/3 fillet 4", 5.0, 3.0, 32, Profile::Fillet, 4.0)),
            2,
            6,
        ),
        // A blend of 0.15 beside a branch whose polygon (48 sides) lies at
        // most 0.007 under it, with the coarse arcs OpenSCAD's defaults
        // give: at 12 sides (0.11 deep) such a blend cannot follow the
        // facets, which the export cures by retrying at four times the
        // segments (`geom::exact`).
        (
            Box::new(|| {
                tee_off(
                    "tee small coarse",
                    8.1,
                    3.23,
                    -0.51,
                    9.51,
                    128,
                    48,
                    Profile::Fillet,
                    0.15,
                )
            }),
            2,
            6,
        ),
        (
            Box::new(|| {
                cross_hole_both(
                    "cross hole both",
                    9.37,
                    3.32,
                    7.52,
                    -1.64,
                    24,
                    Profile::Fillet,
                    0.6,
                )
            }),
            4,
            12,
        ),
        (
            Box::new(|| cross_hole("cross hole fillet 1", 2.0, 32, Profile::Fillet, 1.0)),
            2,
            6,
        ),
    ]
}

/// Every case is valid, all exact, written the same twice, with the
/// expected B-spline faces and contacts; a fillet's volume is the
/// reference's ([`reference`]) within 1e-9, a chamfer's agrees between a
/// tee and its half, and the mesh's volume is the exact one's within the
/// facets' share.
#[test]
fn curve_blends_reconstruct_exact() {
    let mut failures = Vec::new();
    let mut volumes = std::collections::BTreeMap::new();
    for (make, bspline, boundary) in cases() {
        let c = make();
        let name = c.name;
        let t0 = std::time::Instant::now();
        let o = match run(c) {
            Ok(o) => o,
            Err(e) => {
                failures.push(format!("{name}: {e}"));
                continue;
            }
        };
        let reference = reference_volume(name);
        eprintln!(
            "{name}: {} faces ({} B-spline), {} boundary contacts, volume {:.9}{}, mesh {:.6}, fit {:.1e}, {} bytes, {:.0} ms",
            o.faces,
            o.bspline,
            o.boundary,
            o.volume,
            reference.map_or(String::new(), |r| format!(
                " (reference {r:.9}, rel {:.1e})",
                (o.volume - r) / r
            )),
            o.mesh_volume,
            o.fit,
            o.step.len(),
            t0.elapsed().as_secs_f64() * 1e3
        );
        if o.bspline != bspline || o.boundary != boundary {
            failures.push(format!(
                "{name}: {} B-spline faces and {} boundary contacts, {bspline} and {boundary} expected",
                o.bspline, o.boundary
            ));
        }
        if let Some(r) = reference
            && ((o.volume - r) / r).abs() > 1e-9
        {
            failures.push(format!("{name}: volume {} vs reference {r}", o.volume));
        }
        // The polygons inscribed in the exact faces lose at most a few
        // percent of a thousandth.
        if ((o.mesh_volume - o.volume) / o.volume).abs() > 0.03 {
            failures.push(format!(
                "{name}: mesh volume {} vs {}",
                o.mesh_volume, o.volume
            ));
        }
        volumes.insert(name, o.volume);
    }
    // A tee's chamfer is twice its half's.
    if let (Some(full), Some(half)) = (
        volumes.get("tee 5/3 chamfer"),
        volumes.get("half tee chamfer"),
    ) && ((full - 2.0 * half) / full).abs() > 1e-10
    {
        failures.push(format!("tee chamfer {full} is not twice its half's {half}"));
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// OCCT reads every case back (`MESHBREP_OCCT_CHECK`, see `occt.rs`): one
/// valid closed solid, no free edges, no tolerance raised past 1e-6, and
/// a volume within 1e-6 of ours by the better of its two integrators
/// (each misjudges some rational patches).
#[test]
fn occt_reads_curve_blends_back() {
    let Some(check) = std::env::var_os("MESHBREP_OCCT_CHECK") else {
        eprintln!("skipped: set MESHBREP_OCCT_CHECK to oracle/build.sh's check");
        return;
    };
    let dir = std::env::temp_dir().join(format!("meshbrep-sweep-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let mut files = Vec::new();
    let mut ours = Vec::new();
    for (make, _, _) in cases() {
        let c = make();
        let name = c.name;
        let o = run(c).unwrap_or_else(|e| panic!("{name}: {e}"));
        let path = dir.join(format!("{}.step", name.replace([' ', '/'], "_")));
        std::fs::write(&path, &o.step).unwrap();
        files.push(path);
        ours.push((name, o.volume));
    }
    let out = std::process::Command::new(&check)
        .args(&files)
        .output()
        .expect("run the oracle");
    let text = String::from_utf8_lossy(&out.stdout);
    let lines: Vec<&str> = text.lines().filter(|l| l.starts_with('{')).collect();
    assert_eq!(lines.len(), files.len(), "oracle output:\n{text}");
    let field = |json: &str, key: &str| -> f64 {
        let k = format!("\"{key}\":");
        let rest = &json[json.find(&k).map_or(json.len(), |i| i + k.len())..];
        let end = rest.find([',', '}']).unwrap_or(rest.len());
        rest[..end].parse().unwrap_or(f64::NAN)
    };
    let mut failures = Vec::new();
    for (line, (name, volume)) in lines.iter().zip(&ours) {
        let rel = |x: f64| ((x - volume) / volume).abs();
        // The best of OCCT's three integrators (`oracle/check.cpp`).
        let best = rel(field(line, "volume"))
            .min(rel(field(line, "volume_fixed")))
            .min(rel(field(line, "volume_gk")));
        eprintln!("{name}: OCCT {line}");
        let ok = line.contains("\"valid\":true")
            && field(line, "solids") == 1.0
            && field(line, "closed_shells") == 1.0
            && field(line, "free_edges") == 0.0
            && field(line, "max_tol") <= 1e-6
            && best <= 1e-6;
        if !ok {
            failures.push(format!("{name}: {line} (volume rel {best:.1e})"));
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// Reference volumes, computed independently of `meshbrep`'s fitting:
/// a blend's region is swept by its cross-section in the normal plane of
/// the exact spine, so its volume is `∫ ds ∬ (1 − κ ξ) dA` (the Jacobian
/// of sweeping a plane region along a curve: `κ` the spine's curvature,
/// `ξ` the distance from the spine towards its centre of curvature). The
/// section is star-shaped from the ball's centre: between the arc (radius
/// `r`) and the faces, along each ray from the centre, so the inner
/// integral is `∫ [(ρ² − r²)/2 − κ cos(θ − θ_N) (ρ³ − r³)/3] dθ`, with
/// `ρ(θ)` where the ray meets a face. The spine is solved exactly (Newton
/// on both faces offset by `r`) in the normal plane of the edge at each
/// parameter of a smooth closed parametrisation of the edge, so the
/// outer integral is periodic and the trapezoidal rule converges
/// geometrically; the inner one is Gauss–Legendre in two panels split at
/// the edge's direction.
mod reference {
    pub type P = [f64; 3];
    fn add(a: P, b: P) -> P {
        [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
    }
    fn sub(a: P, b: P) -> P {
        [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
    }
    fn mul(a: P, s: f64) -> P {
        [a[0] * s, a[1] * s, a[2] * s]
    }
    fn dot(a: P, b: P) -> f64 {
        a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
    }
    fn cross(a: P, b: P) -> P {
        [
            a[1] * b[2] - a[2] * b[1],
            a[2] * b[0] - a[0] * b[2],
            a[0] * b[1] - a[1] * b[0],
        ]
    }
    fn norm(a: P) -> f64 {
        dot(a, a).sqrt()
    }
    fn unit(a: P) -> P {
        mul(a, 1.0 / norm(a))
    }

    /// A face as its signed distance (positive in the air).
    #[derive(Clone, Copy)]
    pub enum Face {
        Plane(P, P),
        /// Axis point, unit axis, radius, material inside.
        Cyl(P, P, f64, bool),
        Sphere(P, f64, bool),
    }

    impl Face {
        pub fn f(&self, p: P) -> f64 {
            match *self {
                Face::Plane(o, n) => dot(sub(p, o), n),
                Face::Cyl(o, a, r, convex) => {
                    let q = sub(p, o);
                    let d = norm(sub(q, mul(a, dot(q, a)))) - r;
                    if convex { d } else { -d }
                }
                Face::Sphere(c, r, convex) => {
                    let d = norm(sub(p, c)) - r;
                    if convex { d } else { -d }
                }
            }
        }
        pub fn grad(&self, p: P) -> P {
            match *self {
                Face::Plane(_, n) => n,
                Face::Cyl(o, a, _, convex) => {
                    let q = sub(p, o);
                    let g = unit(sub(q, mul(a, dot(q, a))));
                    if convex { g } else { mul(g, -1.0) }
                }
                Face::Sphere(c, _, convex) => {
                    let g = unit(sub(p, c));
                    if convex { g } else { mul(g, -1.0) }
                }
            }
        }
    }

    fn solve3(a: [[f64; 3]; 3], b: [f64; 3]) -> P {
        let det = |m: [[f64; 3]; 3]| {
            m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1])
                - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
                + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0])
        };
        let d = det(a);
        let mut x = [0.0; 3];
        for c in 0..3 {
            let mut m = a;
            for r in 0..3 {
                m[r][c] = b[r];
            }
            x[c] = det(m) / d;
        }
        x
    }

    /// The point at offsets `h` from both faces in the plane through `o`
    /// with normal `n`, by Newton from `x`.
    fn corrector(faces: &[Face; 2], h: f64, mut x: P, o: P, n: P) -> P {
        for _ in 0..100 {
            let g = [faces[0].grad(x), faces[1].grad(x)];
            let b = [h - faces[0].f(x), h - faces[1].f(x), -dot(sub(x, o), n)];
            let d = solve3([g[0], g[1], n], b);
            x = add(x, d);
            if norm(d) < 1e-15 {
                break;
            }
        }
        x
    }

    const GL: [(f64, f64); 16] = [
        (-0.989_400_934_991_649_9, 0.027_152_459_411_754_095),
        (-0.944_575_023_073_232_6, 0.062_253_523_938_647_89),
        (-0.865_631_202_387_831_8, 0.095_158_511_682_492_78),
        (-0.755_404_408_355_003, 0.124_628_971_255_533_87),
        (-0.617_876_244_402_643_8, 0.149_595_988_816_576_73),
        (-0.458_016_777_657_227_4, 0.169_156_519_395_002_54),
        (-0.281_603_550_779_258_9, 0.182_603_415_044_923_58),
        (-0.095_012_509_837_637_44, 0.189_450_610_455_068_5),
        (0.095_012_509_837_637_44, 0.189_450_610_455_068_5),
        (0.281_603_550_779_258_9, 0.182_603_415_044_923_58),
        (0.458_016_777_657_227_4, 0.169_156_519_395_002_54),
        (0.617_876_244_402_643_8, 0.149_595_988_816_576_73),
        (0.755_404_408_355_003, 0.124_628_971_255_533_87),
        (0.865_631_202_387_831_8, 0.095_158_511_682_492_78),
        (0.944_575_023_073_232_6, 0.062_253_523_938_647_89),
        (0.989_400_934_991_649_9, 0.027_152_459_411_754_095),
    ];

    /// `∫_a^b f` by 16-point Gauss–Legendre on `panels` panels.
    pub fn gauss(f: &dyn Fn(f64) -> f64, a: f64, b: f64, panels: usize) -> f64 {
        let mut s = 0.0;
        for k in 0..panels {
            let (x0, x1) = (
                a + (b - a) * k as f64 / panels as f64,
                a + (b - a) * (k + 1) as f64 / panels as f64,
            );
            let (m, hw) = (0.5 * (x0 + x1), 0.5 * (x1 - x0));
            for (x, w) in GL {
                s += w * hw * f(m + hw * x);
            }
        }
        s
    }

    /// The volume a fillet of radius `r` adds (concave) or removes
    /// (convex) along the closed edge `edge(φ)`, `φ` in `[0, 2π)`.
    pub fn fillet_region(
        faces: [Face; 2],
        convex: bool,
        r: f64,
        edge: &dyn Fn(f64) -> P,
        n: usize,
    ) -> f64 {
        let h = if convex { -r } else { r };
        let spine = |phi: f64| -> P {
            let e = edge(phi);
            let d = 1e-6;
            let t = unit(sub(edge(phi + d), edge(phi - d)));
            // Start from the tangent planes' offsets at the edge.
            let g = [faces[0].grad(e), faces[1].grad(e)];
            let y = solve3([g[0], g[1], t], [h, h, 0.0]);
            corrector(&faces, h, add(e, y), e, t)
        };
        let mut total = 0.0;
        for k in 0..n {
            let phi = std::f64::consts::TAU * k as f64 / n as f64;
            let d = 1e-4;
            let (cm2, cm, c0, cp, cp2) = (
                spine(phi - 2.0 * d),
                spine(phi - d),
                spine(phi),
                spine(phi + d),
                spine(phi + 2.0 * d),
            );
            // Fourth-order central differences.
            let d1 = mul(add(sub(cm2, cp2), mul(sub(cp, cm), 8.0)), 1.0 / (12.0 * d));
            let d2 = mul(
                add(
                    add(mul(cm2, -1.0), mul(cp2, -1.0)),
                    add(mul(add(cm, cp), 16.0), mul(c0, -30.0)),
                ),
                1.0 / (12.0 * d * d),
            );
            let speed = norm(d1);
            let t = mul(d1, 1.0 / speed);
            // Curvature vector: the second derivative's part across the
            // tangent, over the speed squared.
            let kv = mul(sub(d2, mul(t, dot(d2, t))), 1.0 / (speed * speed));
            let feet = [
                sub(c0, mul(faces[0].grad(c0), h)),
                sub(c0, mul(faces[1].grad(c0), h)),
            ];
            // The edge in this plane: both faces at 0.
            let e = corrector(&faces, 0.0, edge(phi), c0, t);
            // An orthonormal frame of the plane, angles from foot a.
            let x = unit(sub(feet[0], c0));
            let y = unit(cross(t, x));
            let ang = |p: P| {
                let q = sub(p, c0);
                dot(q, y).atan2(dot(q, x))
            };
            let (tb, te) = (ang(feet[1]), ang(e));
            // The short way from a to b passes the edge's direction.
            assert!(
                te.signum() == tb.signum() && te.abs() < tb.abs(),
                "the edge is not between the feet"
            );
            let rho = |th: f64, face: &Face| -> f64 {
                let u = add(mul(x, th.cos()), mul(y, th.sin()));
                let mut s = r;
                for _ in 0..100 {
                    let p = add(c0, mul(u, s));
                    let f = face.f(p);
                    let df = dot(face.grad(p), u);
                    let ds = -f / df;
                    s += ds;
                    if ds.abs() < 1e-15 {
                        break;
                    }
                }
                s
            };
            let integrand = |th: f64, face: &Face| -> f64 {
                let p = rho(th, face);
                let u = add(mul(x, th.cos()), mul(y, th.sin()));
                (p * p - r * r) / 2.0 - dot(kv, u) * (p * p * p - r * r * r) / 3.0
            };
            let area = gauss(&|th| integrand(th, &faces[0]), 0.0, te, 8)
                + gauss(&|th| integrand(th, &faces[1]), te, tb, 8);
            total += area.abs() * speed;
        }
        total * std::f64::consts::TAU / n as f64
    }

    /// The volume an equal-distance chamfer of `d` removes (convex) or
    /// adds (concave) along the closed edge `edge(φ)`: in the plane across
    /// the edge at each of its points `E` (the edge's normal plane, the
    /// edge its own spine), the region between the faces and the segment
    /// joining the points of each face at distance `d` from `E` on the
    /// material's side, integrated by Green's theorem as
    /// `∮ (X − k₁X²/2 − k₂XY) dY` (`(k₁, k₂)` the edge's curvature vector in
    /// the plane's frame), each face's trace by Gauss–Legendre in its chord's
    /// parameter, the point on the face found across the chord by Newton.
    pub fn chamfer_region(
        faces: [Face; 2],
        convex: bool,
        d: f64,
        edge: &dyn Fn(f64) -> P,
        n: usize,
    ) -> f64 {
        let mut total = 0.0;
        for k in 0..n {
            let phi = std::f64::consts::TAU * k as f64 / n as f64;
            let h = 1e-4;
            let (em2, em, e0, ep, ep2) = (
                edge(phi - 2.0 * h),
                edge(phi - h),
                edge(phi),
                edge(phi + h),
                edge(phi + 2.0 * h),
            );
            let d1 = mul(add(sub(em2, ep2), mul(sub(ep, em), 8.0)), 1.0 / (12.0 * h));
            let d2 = mul(
                add(
                    add(mul(em2, -1.0), mul(ep2, -1.0)),
                    add(mul(add(em, ep), 16.0), mul(e0, -30.0)),
                ),
                1.0 / (12.0 * h * h),
            );
            let speed = norm(d1);
            let t = mul(d1, 1.0 / speed);
            let kv = mul(sub(d2, mul(t, dot(d2, t))), 1.0 / (speed * speed));
            // The feet, as the tool makes them: on each face, in this
            // plane, `d` from the edge, into the face away from the edge.
            let g = [faces[0].grad(e0), faces[1].grad(e0)];
            let mut feet = [e0; 2];
            for j in 0..2 {
                let w = unit(cross(g[j], t));
                let s = dot(w, g[1 - j]);
                let w = if (convex && s > 0.0) || (!convex && s < 0.0) {
                    mul(w, -1.0)
                } else {
                    w
                };
                let mut x = add(e0, mul(w, d));
                for _ in 0..100 {
                    let gk = faces[j].grad(x);
                    let r = sub(x, e0);
                    let dx = solve3(
                        [gk, t, mul(r, 2.0)],
                        [-faces[j].f(x), -dot(r, t), d * d - dot(r, r)],
                    );
                    x = add(x, dx);
                    if norm(dx) < 1e-15 {
                        break;
                    }
                }
                feet[j] = x;
            }
            // A frame of the plane; the region's boundary E → a → b → E.
            let ex = unit(sub(feet[0], e0));
            let ey = cross(t, ex);
            let (k1, k2) = (dot(kv, ex), dot(kv, ey));
            let xy = |p: P| {
                let q = sub(p, e0);
                (dot(q, ex), dot(q, ey))
            };
            let pdy = |p: (f64, f64), dy: f64| (p.0 - k1 * p.0 * p.0 / 2.0 - k2 * p.0 * p.1) * dy;
            // A face's trace from `from` to `to` (both on it): across the
            // chord at parameter λ, Newton onto the face within the plane.
            let trace = |face: &Face, from: P, to: P, lam: f64| -> P {
                let c = add(from, mul(sub(to, from), lam));
                let across = unit(cross(t, sub(to, from)));
                let mut mu = 0.0;
                for _ in 0..100 {
                    let x = add(c, mul(across, mu));
                    let step = -face.f(x) / dot(face.grad(x), across);
                    mu += step;
                    if step.abs() < 1e-15 {
                        break;
                    }
                }
                add(c, mul(across, mu))
            };
            let along = |face: &Face, from: P, to: P| -> f64 {
                gauss(
                    &|lam: f64| {
                        let dl = 1e-5;
                        let p = xy(trace(face, from, to, lam));
                        let (a, b) = (
                            xy(trace(face, from, to, lam - dl)),
                            xy(trace(face, from, to, lam + dl)),
                        );
                        pdy(p, (b.1 - a.1) / (2.0 * dl))
                    },
                    0.0,
                    1.0,
                    4,
                )
            };
            let (fa, fb) = (xy(feet[0]), xy(feet[1]));
            let chord = gauss(
                &|lam: f64| {
                    let p = (fa.0 + (fb.0 - fa.0) * lam, fa.1 + (fb.1 - fa.1) * lam);
                    pdy(p, fb.1 - fa.1)
                },
                0.0,
                1.0,
                1,
            );
            let area = along(&faces[0], e0, feet[0]) + chord + along(&faces[1], feet[1], e0);
            total += area.abs() * speed;
        }
        total * std::f64::consts::TAU / n as f64
    }
}

/// Reference volumes of whole solids: each base solid's volume (closed
/// forms, or a smooth one-dimensional integral by Gauss–Legendre), plus
/// or minus its blends' regions ([`reference`]).
mod solids {
    use super::reference::{Face, P, chamfer_region, fillet_region, gauss};
    use std::f64::consts::{FRAC_PI_2, PI, TAU};

    /// A fillet of radius `r` or a chamfer of distance `d`.
    #[derive(Clone, Copy)]
    pub enum Blend {
        Fillet(f64),
        Chamfer(f64),
    }

    const X: P = [1.0, 0.0, 0.0];
    const Z: P = [0.0, 0.0, 1.0];
    const N: usize = 256;

    fn region(faces: [Face; 2], convex: bool, blend: Blend, edge: &dyn Fn(f64) -> P) -> f64 {
        match blend {
            Blend::Fillet(r) => fillet_region(faces, convex, r, edge, N),
            Blend::Chamfer(d) => chamfer_region(faces, convex, d, edge, N),
        }
    }

    /// `∫ 2√(r² − (y − c)²) g(y) dy` over the chord `|y − c| ≤ r`, by
    /// `y = c + r sin θ` (smooth).
    fn over_disc(r: f64, c: f64, g: &dyn Fn(f64) -> f64) -> f64 {
        gauss(
            &|th: f64| {
                let k = th.cos();
                2.0 * r * k * g(c + r * th.sin()) * r * k
            },
            -FRAC_PI_2,
            FRAC_PI_2,
            16,
        )
    }

    /// A rod of radius `big` along x, `len` long and centred, and a branch
    /// of radius `small` along z at `y = off` from `z = 0` to `h`; their
    /// junction blended.
    pub fn tee(big: f64, small: f64, off: f64, len: f64, h: f64, blend: Blend) -> f64 {
        let overlap = over_disc(small, off, &|y| (big * big - y * y).sqrt());
        let base = PI * big * big * len + PI * small * small * h - overlap;
        base + region(
            [
                Face::Cyl([0.0; 3], X, big, true),
                Face::Cyl([0.0, off, 0.0], Z, small, true),
            ],
            false,
            blend,
            &|t: f64| {
                let (x, y) = (small * t.cos(), off + small * t.sin());
                [x, y, (big * big - y * y).sqrt()]
            },
        )
    }

    /// [`cross_hole`] unblended.
    pub fn cross_hole_base(big: f64, len: f64, small: f64, y0: f64) -> f64 {
        PI * big * big * len - over_disc(small, y0, &|y| 2.0 * (big * big - y * y).sqrt())
    }

    /// A rod of radius `big` along x, `len` long and centred, with a hole
    /// of radius `small` along z through it at `(x0, y0)`; both rims
    /// blended.
    pub fn cross_hole(big: f64, len: f64, small: f64, x0: f64, y0: f64, blend: Blend) -> f64 {
        let mut v = cross_hole_base(big, len, small, y0);
        for side in [1.0, -1.0] {
            v -= region(
                [
                    Face::Cyl([0.0; 3], X, big, true),
                    Face::Cyl([x0, y0, 0.0], Z, small, false),
                ],
                true,
                blend,
                &|t: f64| {
                    let (x, y) = (x0 + small * t.cos(), y0 + small * t.sin());
                    [x, y, side * (big * big - y * y).sqrt()]
                },
            );
        }
        v
    }

    /// A plate `[l, w, t]` at the origin with a cylinder of radius `r`
    /// through its centre, its axis turned by `deg` about x (OpenSCAD's
    /// `rotate([deg, 0, 0])`), `len` long and centred: added (`rod`) or
    /// cut; both rims blended.
    #[allow(clippy::too_many_arguments)]
    pub fn oblique(
        l: f64,
        w: f64,
        t: f64,
        r: f64,
        deg: f64,
        len: f64,
        rod: bool,
        blend: Blend,
    ) -> f64 {
        let a = deg.to_radians();
        let axis = [0.0, -a.sin(), a.cos()];
        let c = [l / 2.0, w / 2.0, t / 2.0];
        let inside = PI * r * r * t / a.cos();
        let base = if rod {
            l * w * t + PI * r * r * len - inside
        } else {
            l * w * t - inside
        };
        let e2 = [0.0, axis[2], -axis[1]];
        let mut v = base;
        for (z, n) in [(t, 1.0), (0.0, -1.0)] {
            let reg = region(
                [
                    Face::Plane([0.0, 0.0, z], [0.0, 0.0, n]),
                    Face::Cyl(c, axis, r, rod),
                ],
                !rod,
                blend,
                &|u: f64| {
                    let p = [
                        c[0] + r * u.cos(),
                        c[1] + r * u.sin() * e2[1],
                        c[2] + r * u.sin() * e2[2],
                    ];
                    let s = (z - p[2]) / axis[2];
                    [p[0] + axis[0] * s, p[1] + axis[1] * s, z]
                },
            );
            v += if rod { reg } else { -reg };
        }
        v
    }

    /// A ball of radius `big` at the origin and a rod of radius `r` along
    /// z at `(x0, 0)` from `z = 0` to `h`; the junction blended.
    pub fn ball_rod(big: f64, r: f64, x0: f64, h: f64, blend: Blend) -> f64 {
        let overlap = gauss(
            &|th: f64| {
                gauss(
                    &|rho: f64| {
                        let (x, y) = (x0 + rho * th.cos(), rho * th.sin());
                        (big * big - x * x - y * y).sqrt() * rho
                    },
                    0.0,
                    r,
                    4,
                )
            },
            0.0,
            TAU,
            8,
        );
        let base = 4.0 / 3.0 * PI * big * big * big + PI * r * r * h - overlap;
        base + region(
            [
                Face::Sphere([0.0; 3], big, true),
                Face::Cyl([x0, 0.0, 0.0], Z, r, true),
            ],
            false,
            blend,
            &|t: f64| {
                let (x, y) = (x0 + r * t.cos(), r * t.sin());
                [x, y, (big * big - x * x - y * y).sqrt()]
            },
        )
    }
}

/// The reference volume of a case by name (see [`solids`]), where one is
/// worked out.
fn reference_volume(name: &str) -> Option<f64> {
    use solids::Blend::{Chamfer, Fillet};
    use solids::*;
    let tee53 = |b| tee(5.0, 3.0, 0.0, 30.0, 12.0, b);
    let pi = std::f64::consts::PI;
    match name {
        "tee 5/3 fillet" | "tee 5/3 fillet 64" => Some(tee53(Fillet(1.0))),
        "tee 5/3 fillet 0.5" => Some(tee53(Fillet(0.5))),
        "tee 5/3 fillet 4" => Some(tee53(Fillet(4.0))),
        "tee 5/3 chamfer" => Some(tee53(Chamfer(0.8))),
        "half tee fillet" | "half tee fillet 64" | "half tee fillet 16" => {
            Some(0.5 * tee53(Fillet(1.0)))
        }
        "half tee chamfer" => Some(0.5 * tee53(Chamfer(0.8))),
        // The harness's cross hole blends its top rim only: by symmetry,
        // half the two rims' regions.
        "cross hole fillet" | "cross hole fillet 1" | "cross hole chamfer" => {
            let b = match name {
                "cross hole fillet" => Fillet(0.8),
                "cross hole fillet 1" => Fillet(1.0),
                _ => Chamfer(0.8),
            };
            let both = cross_hole(5.0, 30.0, 2.0, 0.0, 0.0, b);
            let base = cross_hole_base(5.0, 30.0, 2.0, 0.0);
            Some(0.5 * (base + both))
        }
        "cross hole both" => Some(cross_hole(9.37, 3.0 * 9.37, 3.32, 7.52, -1.64, Fillet(0.6))),
        "oblique hole fillet" => {
            // The harness blends the top rim only (30° hole of radius 4
            // through a 30 × 30 × 6 plate centred on the origin).
            let both = oblique(30.0, 30.0, 6.0, 4.0, 30.0, 30.0, false, Fillet(1.0));
            let base = 900.0 * 6.0 - pi * 16.0 * 6.0 / 30f64.to_radians().cos();
            Some(0.5 * (base + both))
        }
        "oblique hole chamfer" => {
            let both = oblique(30.0, 30.0, 6.0, 4.0, 30.0, 30.0, false, Chamfer(1.0));
            let base = 900.0 * 6.0 - pi * 16.0 * 6.0 / 30f64.to_radians().cos();
            Some(0.5 * (base + both))
        }
        "ball rod fillet" => Some(ball_rod(6.0, 2.0, 2.5, 12.0, Fillet(1.0))),
        "ball rod chamfer" => Some(ball_rod(6.0, 2.0, 2.5, 12.0, Chamfer(0.8))),
        _ => None,
    }
}

/// The reference volumes of the golden models in
/// `conformance/extensions/fillet` (written there as `// volume:`), as
/// [`solids`] integrates them.
#[test]
fn golden_reference_volumes() {
    use solids::Blend::{Chamfer, Fillet};
    use solids::*;
    let golden = [
        ("curved_tee", tee(5.0, 3.0, 0.0, 30.0, 12.0, Fillet(1.0))),
        ("curved_boss", tee(8.0, 3.0, 2.0, 40.0, 12.0, Fillet(1.5))),
        (
            "curved_tee_chamfer",
            tee(5.0, 3.0, 0.0, 30.0, 12.0, Chamfer(0.8)),
        ),
        (
            "curved_cross_hole",
            cross_hole(8.0, 40.0, 3.0, 4.0, 0.0, Fillet(1.0)),
        ),
        (
            "curved_oblique_hole",
            oblique(40.0, 40.0, 8.0, 4.0, 30.0, 40.0, false, Fillet(1.0)),
        ),
        (
            "curved_oblique_chamfer",
            oblique(40.0, 40.0, 8.0, 4.0, 30.0, 40.0, false, Chamfer(1.0)),
        ),
        (
            "curved_oblique_rod",
            oblique(40.0, 40.0, 8.0, 4.0, 30.0, 30.0, true, Fillet(1.5)),
        ),
        (
            "curved_ball_rod",
            ball_rod(8.0, 3.0, 2.0, 14.0, Fillet(1.0)),
        ),
    ];
    for (name, v) in golden {
        eprintln!("{name}: {v:.9}");
    }
}

#[test]
fn the_reference_integration_matches_pappus() {
    use reference::{Face, fillet_region};
    // A boss of radius 6 on a plane, its rim filleted with r = 2: the
    // spandrel r²(1 − π/4) revolved at 6 + r(5/6 − π/4)/(1 − π/4).
    let r = 2.0;
    let pi = std::f64::consts::PI;
    let a = r * r * (1.0 - pi / 4.0);
    let rho = 6.0 + r * (5.0 / 6.0 - pi / 4.0) / (1.0 - pi / 4.0);
    let want = 2.0 * pi * rho * a;
    for n in [64, 256] {
        let got = fillet_region(
            [
                Face::Plane([0.0; 3], [0.0, 0.0, 1.0]),
                Face::Cyl([0.0; 3], [0.0, 0.0, 1.0], 6.0, true),
            ],
            false,
            r,
            &|t: f64| [6.0 * t.cos(), 6.0 * t.sin(), 0.0],
            n,
        );
        eprintln!(
            "pappus {want:.12} reference ({n}) {got:.12} rel {:.1e}",
            (got - want) / want
        );
    }
}

/// The reference volumes of fillet corpus models (`conformance
/// fillet-corpus`, seed 2) whose exports OCCT read back with volumes 1e-6
/// to 1e-4 off ours, or whose export's mesh cross-check refused them
/// (`docs/fillets.md`, 15.10). Each is the solid [`solids`] integrates,
/// against the volume NeoSCAD's export of it measures: ours agree to
/// 5e-9 or better, where OCCT's adaptive and fixed-order integrators were
/// 2e-6 and 8e-6 off on 0382 (its Gauss-Kronrod one split at the knots,
/// `volume_gk`, agrees). A rotation about z after the slant
/// (`rotate([a, 0, b])`) leaves a rod through a plate's volume as
/// `rotate([a, 0, 0])` has it, as long as the rod's ends clear the
/// plate's sides.
#[test]
fn corpus_reference_volumes() {
    use solids::Blend::{Chamfer, Fillet};
    use solids::*;
    let cross = |big, len, small, x0, y0, r| {
        // Only the top rim is selected (`>z`): by symmetry, half of both
        // rims' blends on the base.
        let both = cross_hole(big, len, small, x0, y0, Fillet(r));
        0.5 * (cross_hole_base(big, len, small, y0) + both)
    };
    let cases = [
        (
            "0382",
            oblique(42.5, 49.72, 6.82, 3.46, 37.99, 35.97, true, Chamfer(1.58)),
            EXPORT_0382,
        ),
        (
            "1405",
            oblique(56.81, 36.84, 4.34, 3.24, 35.54, 33.62, true, Fillet(1.47)),
            EXPORT_1405,
        ),
        (
            "1056",
            cross(6.39, 20.89, 3.72, 0.15, -0.57, 0.19),
            EXPORT_1056,
        ),
        (
            "1623",
            ball_rod(5.68, 1.46, 2.1, 8.23, Chamfer(0.14)),
            EXPORT_1623,
        ),
        (
            "1643",
            ball_rod(5.03, 1.57, 0.63, 10.55, Chamfer(0.58)),
            EXPORT_1643,
        ),
    ];
    for (id, reference, export) in cases {
        let rel = (export - reference).abs() / reference;
        eprintln!("{id}: reference {reference:.9}, export {export:.9}, rel {rel:.1e}");
        assert!(rel < 1e-8, "{id}: {rel:.1e}");
    }
}

// The volumes NeoSCAD's STEP export of each model measured.
const EXPORT_0382: f64 = 15500.382669542634;
const EXPORT_1405: f64 = 10049.675369127259;
const EXPORT_1056: f64 = 2150.9058992366818;
const EXPORT_1623: f64 = 788.2012303265576;
const EXPORT_1643: f64 = 578.7930124048897;
