//! B-spline surfaces end to end: solids with a face tagged
//! `Surface::BSpline` reconstruct, validate, measure their closed-form
//! volume and write the same STEP bytes twice, at several tagging
//! resolutions; with `MESHBREP_OCCT_CHECK` set (see `oracle/build.sh`)
//! OCCT reads every file back as one valid closed solid of that volume.
//!
//! The cases:
//!
//! - `cyl_exact`: a quarter of a cylinder whose curved face is the exact
//!   rational B-spline of the cylinder (and `cyl_analytic`, the same with
//!   `Surface::Cylinder`, for comparison).
//! - `bump`: a box whose top is a bicubic patch over the whole top
//!   (the sides meet it in curves), its volume integrated in closed form.
//! - `torus_exact`: a quarter of a boss on a disc with the r = 2 fillet
//!   between them as the exact rational torus patch (degree 2 by 2),
//!   tangent to the disc's top and the boss along two of its sides
//!   (`torus_analytic`: the same with `Surface::Torus`).
//! - `canal`: the same fillet from `spline::canal_surface`, its spine and
//!   contact curves fitted to the circles within 1e-9, so the contact on
//!   the boss is only that close to it.
//! - `chamfer`: the boss's rim chamfered instead, the chamfer the ruled
//!   surface between two fitted circles (`spline::ruled_surface`).
//!
//! `cargo test -p meshbrep --release --test bspline -- --nocapture`
//! prints a line per case and resolution.

use std::collections::BTreeMap;
use std::f64::consts::{FRAC_1_SQRT_2, FRAC_PI_2, PI};

use meshbrep::spline::{self, Evaluator};
use meshbrep::{
    BSplineSurface, Brep, Contact, Options, StepOptions, Surface, TaggedMesh, measure, reconstruct,
    validate, write_step,
};

/// A closed triangle mesh assembled from faces given separately: shared
/// vertices are found by their exact coordinates, so every face must
/// compute a shared point the same way.
#[derive(Default)]
struct Builder {
    mesh: TaggedMesh,
    index: BTreeMap<[u64; 3], u32>,
}

impl Builder {
    fn surface(&mut self, s: Surface) -> u32 {
        self.mesh.surfaces.push(s);
        self.mesh.surfaces.len() as u32 - 1
    }

    fn vert(&mut self, p: [f64; 3]) -> u32 {
        // -0.0 and 0.0 are one point.
        let key = p.map(|x| (x + 0.0).to_bits());
        if let Some(&i) = self.index.get(&key) {
            return i;
        }
        self.mesh.positions.push(p);
        let i = self.mesh.positions.len() as u32 - 1;
        self.index.insert(key, i);
        i
    }

    /// A triangle facing `out` (it is flipped if it does not).
    fn tri(&mut self, a: [f64; 3], b: [f64; 3], c: [f64; 3], s: u32, out: [f64; 3]) {
        let (ia, ib, ic) = (self.vert(a), self.vert(b), self.vert(c));
        if ia == ib || ib == ic || ia == ic {
            return;
        }
        let n = cross(sub(b, a), sub(c, a));
        let t = if dot(n, out) >= 0.0 {
            [ia, ib, ic]
        } else {
            [ia, ic, ib]
        };
        self.mesh.triangles.push(t);
        self.mesh.triangle_surface.push(s);
    }

    fn quad(&mut self, q: [[f64; 3]; 4], s: u32, out: [f64; 3]) {
        self.tri(q[0], q[1], q[2], s, out);
        self.tri(q[0], q[2], q[3], s, out);
    }
}

fn sub(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}
fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

/// Cosines and sines of `n + 1` angles across a quarter turn, the ends
/// exact so that points there lie exactly on the planes x = 0 and y = 0.
fn quarter(n: usize) -> Vec<(f64, f64)> {
    (0..=n)
        .map(|j| match j {
            0 => (1.0, 0.0),
            j if j == n => (0.0, 1.0),
            j => {
                let a = FRAC_PI_2 * j as f64 / n as f64;
                (a.cos(), a.sin())
            }
        })
        .collect()
}

/// Ear clipping of a simple polygon given counter-clockwise; returns
/// index triples.
fn ear_clip(pts: &[[f64; 2]]) -> Vec<[usize; 3]> {
    let mut idx: Vec<usize> = (0..pts.len()).collect();
    let mut out = Vec::new();
    let cr = |a: [f64; 2], b: [f64; 2], c: [f64; 2]| {
        (b[0] - a[0]) * (c[1] - a[1]) - (b[1] - a[1]) * (c[0] - a[0])
    };
    while idx.len() > 3 {
        let n = idx.len();
        let mut clipped = false;
        for k in 0..n {
            let (i, j, l) = (idx[(k + n - 1) % n], idx[k], idx[(k + 1) % n]);
            let (a, b, c) = (pts[i], pts[j], pts[l]);
            if cr(a, b, c) <= 1e-12 {
                continue;
            }
            let inside = idx.iter().any(|&m| {
                m != i
                    && m != j
                    && m != l
                    && cr(a, b, pts[m]) >= 0.0
                    && cr(b, c, pts[m]) >= 0.0
                    && cr(c, a, pts[m]) >= 0.0
            });
            if inside {
                continue;
            }
            out.push([i, j, l]);
            idx.remove(k);
            clipped = true;
            break;
        }
        assert!(clipped, "ear clipping stuck");
    }
    out.push([idx[0], idx[1], idx[2]]);
    out
}

/// A quarter (0 ≤ φ ≤ 90°) of the solid of revolution of a profile in
/// the (ρ, z) half-plane, closed by the planes y = 0 and x = 0. The
/// profile runs counter-clockwise (inside on its left) from the axis at
/// its bottom back to the axis at its top, as runs of points each on one
/// surface (consecutive runs share their end points).
fn revolve(b: &mut Builder, runs: &[(Vec<[f64; 2]>, u32)], n: usize) {
    let angles = quarter(n);
    let at = |p: [f64; 2], (c, s): (f64, f64)| [p[0] * c, p[0] * s, p[1]];
    for (pts, s) in runs {
        for w in pts.windows(2) {
            let (p, q) = (w[0], w[1]);
            // Outward in the half-plane: the profile's right-hand normal.
            let o = [q[1] - p[1], -(q[0] - p[0])];
            for j in 0..n {
                let (a0, a1) = (angles[j], angles[j + 1]);
                let mid = (0.5 * (a0.0 + a1.0), 0.5 * (a0.1 + a1.1));
                let out = [o[0] * mid.0, o[0] * mid.1, o[1]];
                b.quad([at(p, a0), at(q, a0), at(q, a1), at(p, a1)], *s, out);
            }
        }
    }
    // The two cut planes: the profile polygon (with the axis).
    let mut poly: Vec<[f64; 2]> = Vec::new();
    for (pts, _) in runs {
        for &p in pts {
            if poly.last() != Some(&p) {
                poly.push(p);
            }
        }
    }
    if poly.first() == poly.last() {
        poly.pop();
    }
    let y0 = b.surface(Surface::Plane {
        origin: [0.0; 3],
        normal: [0.0, -1.0, 0.0],
    });
    let x0 = b.surface(Surface::Plane {
        origin: [0.0; 3],
        normal: [-1.0, 0.0, 0.0],
    });
    for [i, j, k] in ear_clip(&poly) {
        let (a, bb, c) = (poly[i], poly[j], poly[k]);
        b.tri(
            at(a, angles[0]),
            at(bb, angles[0]),
            at(c, angles[0]),
            y0,
            [0.0, -1.0, 0.0],
        );
        b.tri(
            at(a, angles[n]),
            at(bb, angles[n]),
            at(c, angles[n]),
            x0,
            [-1.0, 0.0, 0.0],
        );
    }
}

fn plane(z: f64, up: bool) -> Surface {
    Surface::Plane {
        origin: [0.0, 0.0, z],
        normal: [0.0, 0.0, if up { 1.0 } else { -1.0 }],
    }
}

fn cylinder(r: f64) -> Surface {
    Surface::Cylinder {
        origin: [0.0; 3],
        axis: [0.0, 0.0, 1.0],
        radius: r,
    }
}

/// The exact rational B-spline of the quarter (0 ≤ φ ≤ 90°) of the
/// surface of revolution of a rational quadratic profile arc (control
/// points and weights in the (ρ, z) half-plane): degree 2 around.
fn revolved_patch(profile: &[[f64; 2]], weights: &[f64]) -> BSplineSurface {
    let w = FRAC_1_SQRT_2;
    let dirs = [([1.0, 0.0], 1.0), ([1.0, 1.0], w), ([0.0, 1.0], 1.0)];
    BSplineSurface {
        degree_u: (profile.len() - 1) as u32,
        degree_v: 2,
        control: profile
            .iter()
            .map(|p| {
                dirs.iter()
                    .map(|(d, _)| [p[0] * d[0], p[0] * d[1], p[1]])
                    .collect()
            })
            .collect(),
        weights: Some(
            weights
                .iter()
                .map(|wi| dirs.iter().map(|(_, wj)| wi * wj).collect())
                .collect(),
        ),
        knots_u: if profile.len() == 3 {
            vec![0.0, 0.0, 0.0, 1.0, 1.0, 1.0]
        } else {
            vec![0.0, 0.0, 1.0, 1.0]
        },
        knots_v: vec![0.0, 0.0, 0.0, 1.0, 1.0, 1.0],
    }
}

/// Radius, height and the quarter cylinder's tagging (`n` segments).
const CR: f64 = 5.0;
const CH: f64 = 10.0;

fn quarter_cylinder(n: usize, exact: bool) -> (TaggedMesh, f64) {
    let mut b = Builder::default();
    let side = if exact {
        // The quarter circle as a rational quadratic, extruded along z.
        let w = FRAC_1_SQRT_2;
        b.surface(Surface::BSpline(BSplineSurface {
            degree_u: 2,
            degree_v: 1,
            control: vec![
                vec![[CR, 0.0, 0.0], [CR, 0.0, CH]],
                vec![[CR, CR, 0.0], [CR, CR, CH]],
                vec![[0.0, CR, 0.0], [0.0, CR, CH]],
            ],
            weights: Some(vec![vec![1.0, 1.0], vec![w, w], vec![1.0, 1.0]]),
            knots_u: vec![0.0, 0.0, 0.0, 1.0, 1.0, 1.0],
            knots_v: vec![0.0, 0.0, 1.0, 1.0],
        }))
    } else {
        b.surface(cylinder(CR))
    };
    let (bot, top) = (b.surface(plane(0.0, false)), b.surface(plane(CH, true)));
    let runs = vec![
        (vec![[0.0, 0.0], [CR, 0.0]], bot),
        (vec![[CR, 0.0], [CR, CH]], side),
        (vec![[CR, CH], [0.0, CH]], top),
    ];
    revolve(&mut b, &runs, n);
    (b.mesh, PI * CR * CR * CH / 4.0)
}

/// The box [0, L] × [0, W] × [0, H + bump] whose top is a bicubic patch
/// over its whole top (6 by 5 control points), tagged on an `n` by `n`
/// grid. The control points' x and y are the knots' Greville abscissae
/// scaled, so x and y are linear in u and v and the volume is
/// L W Σ zᵢⱼ ∫Nᵢ ∫Nⱼ.
const L: f64 = 12.0;
const W: f64 = 8.0;
const H: f64 = 5.0;

/// The bump's top and the box's closed-form volume.
fn bump_top() -> (BSplineSurface, f64) {
    let ku = vec![0.0, 0.0, 0.0, 0.0, 1.0 / 3.0, 2.0 / 3.0, 1.0, 1.0, 1.0, 1.0];
    let kv = vec![0.0, 0.0, 0.0, 0.0, 0.5, 1.0, 1.0, 1.0, 1.0];
    let greville = |k: &[f64], i: usize| (k[i + 1] + k[i + 2] + k[i + 3]) / 3.0;
    let z = |i: usize, j: usize| H + 0.6 * ((i as f64 * 1.3).sin() * (j as f64 * 0.9 + 0.4).cos());
    let surf = BSplineSurface {
        degree_u: 3,
        degree_v: 3,
        control: (0..6)
            .map(|i| {
                (0..5)
                    .map(|j| [L * greville(&ku, i), W * greville(&kv, j), z(i, j)])
                    .collect()
            })
            .collect(),
        weights: None,
        knots_u: ku.clone(),
        knots_v: kv.clone(),
    };
    let integral = |k: &[f64], i: usize| (k[i + 4] - k[i]) / 4.0;
    let mut volume = 0.0;
    for i in 0..6 {
        for j in 0..5 {
            volume += L * W * z(i, j) * integral(&ku, i) * integral(&kv, j);
        }
    }
    (surf, volume)
}

fn bump(n: usize) -> (TaggedMesh, f64) {
    let (surf, volume) = bump_top();
    let ev = Evaluator::new(&surf).unwrap();
    let mut b = Builder::default();
    let top = b.surface(Surface::BSpline(surf));
    let bottom = b.surface(plane(0.0, false));
    let sx0 = b.surface(Surface::Plane {
        origin: [0.0; 3],
        normal: [-1.0, 0.0, 0.0],
    });
    let sx1 = b.surface(Surface::Plane {
        origin: [L, 0.0, 0.0],
        normal: [1.0, 0.0, 0.0],
    });
    let sy0 = b.surface(Surface::Plane {
        origin: [0.0; 3],
        normal: [0.0, -1.0, 0.0],
    });
    let sy1 = b.surface(Surface::Plane {
        origin: [0.0, W, 0.0],
        normal: [0.0, 1.0, 0.0],
    });
    let t = |k: usize| k as f64 / n as f64;
    // Points of the top on the grid; x and y exactly the grid's.
    let p = |i: usize, j: usize| {
        let q = ev.eval(t(i), t(j));
        [L * t(i), W * t(j), q[2]]
    };
    let f = |i: usize, j: usize| [L * t(i), W * t(j), 0.0];
    for i in 0..n {
        for j in 0..n {
            b.quad(
                [p(i, j), p(i + 1, j), p(i + 1, j + 1), p(i, j + 1)],
                top,
                [0.0, 0.0, 1.0],
            );
            b.quad(
                [f(i, j), f(i + 1, j), f(i + 1, j + 1), f(i, j + 1)],
                bottom,
                [0.0, 0.0, -1.0],
            );
        }
    }
    for k in 0..n {
        b.quad(
            [f(0, k), f(0, k + 1), p(0, k + 1), p(0, k)],
            sx0,
            [-1.0, 0.0, 0.0],
        );
        b.quad(
            [f(n, k), f(n, k + 1), p(n, k + 1), p(n, k)],
            sx1,
            [1.0, 0.0, 0.0],
        );
        b.quad(
            [f(k, 0), f(k + 1, 0), p(k + 1, 0), p(k, 0)],
            sy0,
            [0.0, -1.0, 0.0],
        );
        b.quad(
            [f(k, n), f(k + 1, n), p(k + 1, n), p(k, n)],
            sy1,
            [0.0, 1.0, 0.0],
        );
    }
    // The top's exact grid points are not the tagging's: x and y were
    // taken from the grid, z from the patch, so the tagging is within
    // rounding of the patch.
    (b.mesh, volume)
}

/// The hole's centre and radius in `drilled`.
const HX: f64 = 6.3;
const HY: f64 = 3.7;
const HR: f64 = 2.0;

/// The bump with a vertical hole through its top, made with Manifold's
/// difference as a caller's boolean would be: the top's face gets a hole
/// bounded by one closed B-spline edge (the patch meets the cylinder in a
/// closed curve). Its volume is the bump's less the top's integral over
/// the hole's disc ([`top_over_disc`]).
fn drilled(n: usize) -> (TaggedMesh, f64) {
    use manifold_rust::manifold::Manifold;
    use manifold_rust::types::{MeshGL64, OpType};
    use meshbrep::primitives::{self, Transform};
    let (box_mesh, volume) = bump(n);
    let hole = primitives::frustum(
        H + 4.0,
        HR,
        HR,
        primitives::aligned_segments(4 * n as u32),
        &Transform::translate([HX, HY, -1.0]),
    );
    let mut table: Vec<Surface> = Vec::new();
    let mut to_manifold = |m: &TaggedMesh| {
        let off = table.len() as u64;
        table.extend(m.surfaces.iter().cloned());
        Manifold::from_mesh_gl64(&MeshGL64 {
            num_prop: 3,
            vert_properties: m.positions.iter().flatten().copied().collect(),
            tri_verts: m
                .triangles
                .iter()
                .flatten()
                .map(|&i| u64::from(i))
                .collect(),
            face_id: m
                .triangle_surface
                .iter()
                .map(|&s| u64::from(s) + off)
                .collect(),
            run_index: vec![0, 3 * m.triangles.len() as u64],
            run_original_id: vec![Manifold::reserve_ids(1)],
            ..Default::default()
        })
    };
    let (a, b) = (to_manifold(&box_mesh), to_manifold(&hole));
    assert_eq!(a.status(), manifold_rust::types::Error::NoError);
    let gl = a.boolean(&b, OpType::Subtract).get_mesh_gl64(-1);
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
    let (top, _) = bump_top();
    (mesh, volume - top_over_disc(&top, HX, HY, HR))
}

/// ∬ z over the disc of radius `r` about `(x0, y0)` of the bump's top
/// (whose x and y are L u and W v): with x = x0 + r sin φ, the inner
/// integral over y is split at the knot lines, where z is a cubic in y,
/// so 8-point Gauss–Legendre is exact there; the outer one in φ is split
/// wherever x or the y limits cross a knot line (where the integrand's
/// derivatives jump), 16 panels a piece.
fn top_over_disc(top: &BSplineSurface, x0: f64, y0: f64, r: f64) -> f64 {
    const GL8: [(f64, f64); 8] = [
        (-0.960_289_856_497_536_3, 0.101_228_536_290_376_26),
        (-0.796_666_477_413_626_7, 0.222_381_034_453_374_47),
        (-0.525_532_409_916_329, 0.313_706_645_877_887_3),
        (-0.183_434_642_495_649_8, 0.362_683_783_378_362),
        (0.183_434_642_495_649_8, 0.362_683_783_378_362),
        (0.525_532_409_916_329, 0.313_706_645_877_887_3),
        (0.796_666_477_413_626_7, 0.222_381_034_453_374_47),
        (0.960_289_856_497_536_3, 0.101_228_536_290_376_26),
    ];
    let ev = Evaluator::new(top).unwrap();
    let xk: Vec<f64> = top.knots_u[4..6].iter().map(|k| L * k).collect();
    let yk: Vec<f64> = top.knots_v[4..5].iter().map(|k| W * k).collect();
    let half = FRAC_PI_2;
    let mut cuts = vec![-half, half];
    for &x in &xk {
        if (x - x0).abs() < r {
            cuts.push(((x - x0) / r).asin());
        }
    }
    for &y in &yk {
        if (y - y0).abs() < r {
            let a = ((y - y0).abs() / r).acos();
            cuts.extend([a, -a]);
        }
    }
    cuts.sort_by(f64::total_cmp);
    let gl = |a: f64, b: f64, f: &dyn Fn(f64) -> f64| -> f64 {
        GL8.iter()
            .map(|&(x, w)| 0.5 * (b - a) * w * f(a + 0.5 * (b - a) * (x + 1.0)))
            .sum::<f64>()
    };
    let inner = |phi: f64| {
        let x = x0 + r * phi.sin();
        let c = r * phi.cos();
        let mut ys = vec![y0 - c];
        ys.extend(yk.iter().copied().filter(|&y| y > y0 - c && y < y0 + c));
        ys.push(y0 + c);
        let mut s = 0.0;
        for w in ys.windows(2) {
            s += gl(w[0], w[1], &|y| ev.eval(x / L, y / W)[2]);
        }
        s * r * phi.cos()
    };
    let mut total = 0.0;
    for w in cuts.windows(2) {
        for k in 0..16 {
            let a = w[0] + (w[1] - w[0]) * k as f64 / 16.0;
            let b = w[0] + (w[1] - w[0]) * (k + 1) as f64 / 16.0;
            total += gl(a, b, &inner);
        }
    }
    total
}

/// The boss case: a disc of radius RP and thickness T, a boss of radius
/// RB up to height HB, and between them a fillet of radius R (or a
/// chamfer of legs R). The fillet's tagging: `m` arcs across, `n`
/// segments around the quarter.
const RP: f64 = 12.0;
const T: f64 = 3.0;
const RB: f64 = 5.0;
const HB: f64 = 11.0;
const R: f64 = 2.0;

#[derive(Clone, Copy, PartialEq)]
enum Blend {
    TorusAnalytic,
    TorusExact,
    /// The canal surface, its curves fitted within this tolerance.
    Canal(f64),
    Chamfer,
}

fn boss(n: usize, m: usize, blend: Blend) -> (TaggedMesh, f64) {
    let mut b = Builder::default();
    let fillet = match blend {
        Blend::TorusAnalytic => Surface::Torus {
            center: [0.0, 0.0, T + R],
            axis: [0.0, 0.0, 1.0],
            major_radius: RB + R,
            minor_radius: R,
        },
        Blend::TorusExact => Surface::BSpline(revolved_patch(
            &[[RB + R, T], [RB, T], [RB, T + R]],
            &[1.0, FRAC_1_SQRT_2, 1.0],
        )),
        Blend::Canal(_) | Blend::Chamfer => {
            // The spine and the contact curves on the disc's top and on
            // the boss, fitted on one knot vector.
            let circle = |r: f64, z: f64| move |t: f64| [r * t.cos(), r * t.sin(), z];
            let (spine, a, c) = (circle(RB + R, T + R), circle(RB + R, T), circle(RB, T + R));
            let tol = match blend {
                Blend::Canal(tol) => tol,
                _ => 1e-9,
            };
            let fit =
                spline::fit_curves(&[&spine, &a, &c], [0.0, FRAC_PI_2], tol, 100_000).expect("fit");
            let [s, a, c] = [&fit.curves[0], &fit.curves[1], &fit.curves[2]];
            if blend == Blend::Chamfer {
                Surface::BSpline(spline::ruled_surface(a, c).expect("ruled"))
            } else {
                let canal = spline::canal_surface(s, a, c, R).expect("canal");
                assert!(canal.error < 10.0 * tol, "canal error {}", canal.error);
                Surface::BSpline(canal.surface)
            }
        }
    };
    let fs = b.surface(fillet);
    let bottom = b.surface(plane(0.0, false));
    let rim = b.surface(cylinder(RP));
    let top = b.surface(plane(T, true));
    let wall = b.surface(cylinder(RB));
    let cap = b.surface(plane(HB, true));
    let arc: Vec<[f64; 2]> = if blend == Blend::Chamfer {
        (0..=m)
            .map(|k| {
                let f = k as f64 / m as f64;
                [RB + R - R * f, T + R * f]
            })
            .collect()
    } else {
        (0..=m)
            .map(|k| match k {
                0 => [RB + R, T],
                k if k == m => [RB, T + R],
                k => {
                    let beta = FRAC_PI_2 * k as f64 / m as f64;
                    [RB + R - R * beta.sin(), T + R - R * beta.cos()]
                }
            })
            .collect()
    };
    let runs = vec![
        (
            vec![[0.0, 0.0], [RB, 0.0], [RB + R, 0.0], [RP, 0.0]],
            bottom,
        ),
        (vec![[RP, 0.0], [RP, T]], rim),
        (vec![[RP, T], [RB + R, T]], top),
        (arc, fs),
        (vec![[RB, T + R], [RB, HB]], wall),
        (vec![[RB, HB], [0.0, HB]], cap),
    ];
    revolve(&mut b, &runs, n);
    // The volume by Pappus: the disc, the boss above it, and the blend's
    // cross-section (a corner square less a quarter disc, or a right
    // triangle) turned about the axis at its centroid's radius.
    let (area, centroid) = if blend == Blend::Chamfer {
        (R * R / 2.0, RB + R / 3.0)
    } else {
        let a = R * R * (1.0 - PI / 4.0);
        let moment = R * R * (R / 2.0) - (PI * R * R / 4.0) * (R - 4.0 * R / (3.0 * PI));
        (a, RB + moment / a)
    };
    let full = PI * RP * RP * T + PI * RB * RB * (HB - T) + 2.0 * PI * centroid * area;
    (b.mesh, full / 4.0)
}

const CASES: [&str; 8] = [
    "cyl_analytic",
    "cyl_exact",
    "bump",
    "drilled",
    "torus_analytic",
    "torus_exact",
    "canal",
    "chamfer",
];

/// Tagging resolutions: segments around (or the grid) and, for the
/// boss, arcs across.
const RES: [(usize, usize); 5] = [(4, 2), (6, 3), (8, 4), (12, 6), (24, 8)];

fn mesh(name: &str, (n, m): (usize, usize)) -> (TaggedMesh, f64) {
    match name {
        "cyl_analytic" => quarter_cylinder(n, false),
        "cyl_exact" => quarter_cylinder(n, true),
        "bump" => bump(n),
        "drilled" => drilled(n),
        "torus_analytic" => boss(n, m, Blend::TorusAnalytic),
        "torus_exact" => boss(n, m, Blend::TorusExact),
        "canal" => boss(n, m, Blend::Canal(1e-9)),
        "chamfer" => boss(n, m, Blend::Chamfer),
        _ => unreachable!(),
    }
}

/// How close the volume must come to the closed form: 1e-9 relative
/// where every surface is exact, and the fit's share where the blend was
/// fitted (1e-9 in length over a cross-section of about 4 mm²).
fn volume_tolerance(name: &str) -> f64 {
    match name {
        "canal" | "chamfer" => 1e-8,
        _ => 1e-9,
    }
}

fn kinds(b: &Brep) -> String {
    let mut m = BTreeMap::new();
    for f in &b.faces {
        *m.entry(f.surface.kind()).or_insert(0) += 1;
    }
    m.iter()
        .map(|(k, v)| format!("{v} {k}"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Reconstructs, validates and measures one case; returns its STEP text
/// and closed-form volume, or the failure.
fn run(name: &str, res: (usize, usize)) -> Result<(String, f64), String> {
    let (mesh, reference) = mesh(name, res);
    let t0 = std::time::Instant::now();
    let brep = reconstruct(&mesh, &Options::default()).map_err(|e| format!("{e}"))?;
    let ms = t0.elapsed().as_secs_f64() * 1e3;
    let v = validate(&brep, 1e-6);
    let m = measure(&brep).map_err(|e| format!("measure: {e}"))?;
    let rel = (m.volume - reference).abs() / reference;
    let step = write_step(&brep, &StepOptions::default());
    let again = write_step(
        &reconstruct(&mesh, &Options::default()).map_err(|e| format!("again: {e}"))?,
        &StepOptions::default(),
    );
    let boundary = brep
        .report
        .tangencies
        .iter()
        .filter(|t| matches!(t.contact, Contact::Boundary { .. }))
        .count();
    eprintln!(
        "{name:14} {res:?}: {} faces ({}), {} edges, volume {:.10} rel {rel:.1e}, {boundary} boundary contacts, residual {:.1e}, edge dev {:.1e}, pcurve dev {:.1e}, {} bytes, reconstruction {ms:.1} ms",
        brep.faces.len(),
        kinds(&brep),
        brep.edges.len(),
        m.volume,
        brep.report.max_vertex_residual,
        brep.report.max_edge_deviation,
        brep.report.max_pcurve_deviation,
        step.len()
    );
    if !v.is_valid() {
        return Err(format!("invalid: {:?}", v.errors));
    }
    if rel >= volume_tolerance(name) || rel.is_nan() {
        return Err(format!("volume {} vs {reference}, rel {rel:.2e}", m.volume));
    }
    if step != again {
        return Err("STEP differs between runs".into());
    }
    let spline_faces = brep
        .faces
        .iter()
        .filter(|f| matches!(f.surface, Surface::BSpline(_)))
        .count();
    let expect = usize::from(!name.ends_with("analytic"));
    if spline_faces != expect {
        return Err(format!("{spline_faces} B-spline faces, {expect} expected"));
    }
    // The blends touch the disc's top and the boss along two sides.
    if matches!(name, "torus_exact" | "canal") && boundary != 2 {
        return Err(format!("{boundary} boundary contacts, 2 expected"));
    }
    Ok((step, reference))
}

#[test]
fn bspline_faces_reconstruct_validate_and_measure() {
    let mut failures = Vec::new();
    for name in CASES {
        for res in RES {
            if let Err(e) = run(name, res) {
                failures.push(format!("{name} {res:?}: {e}"));
            }
        }
    }
    assert!(
        failures.is_empty(),
        "{} failures:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

/// The exact patch and the analytic torus are one surface, so the two
/// models' volumes agree to quadrature, not just to the closed form.
#[test]
fn an_exact_torus_patch_measures_as_the_torus() {
    for res in RES {
        let vol = |blend| {
            let (mesh, _) = boss(res.0, res.1, blend);
            measure(&reconstruct(&mesh, &Options::default()).unwrap())
                .unwrap()
                .volume
        };
        let (a, e) = (vol(Blend::TorusAnalytic), vol(Blend::TorusExact));
        assert!((a - e).abs() < 1e-9 * a, "{res:?}: {a} vs {e}");
    }
}

/// A canal patch whose contact curve on the boss was fitted loosely
/// (within 5e-6) touches the boss only that closely: with no allowance
/// for the fit it is not recognised as touching (the reconstruction then
/// fails or misses the contact), with one it is, and the model is valid
/// and measures within the fit.
#[test]
fn the_fit_allowance_decides_the_fitted_contact() {
    let (mesh, reference) = boss(8, 4, Blend::Canal(5e-6));
    let count = |surface_fit: f64| {
        let mut o = Options::default();
        o.tolerances.surface_fit = surface_fit;
        reconstruct(&mesh, &o).map(|b| {
            b.report
                .tangencies
                .iter()
                .filter(|t| matches!(t.contact, Contact::Boundary { .. }))
                .count()
        })
    };
    let without = count(0.0);
    eprintln!("no allowance: {without:?}");
    assert_ne!(without, Ok(2));
    assert_eq!(count(1e-5), Ok(2));
    let mut o = Options::default();
    o.tolerances.surface_fit = 1e-5;
    let brep = reconstruct(&mesh, &o).unwrap();
    assert!(validate(&brep, 1e-5).is_valid());
    let volume = measure(&brep).unwrap().volume;
    eprintln!(
        "volume {volume} vs {reference}, rel {:.1e}",
        (volume - reference).abs() / reference
    );
    assert!(
        (volume - reference).abs() < 1e-4 * reference,
        "{volume} vs {reference}"
    );
}

/// OCCT reads every case back as one valid closed solid with the
/// closed-form volume (when `MESHBREP_OCCT_CHECK` names the oracle).
#[test]
fn occt_reads_bspline_cases_back() {
    let Some(check) = std::env::var_os("MESHBREP_OCCT_CHECK") else {
        eprintln!("skipped: set MESHBREP_OCCT_CHECK to oracle/build.sh's check");
        return;
    };
    let dir = std::env::temp_dir().join(format!("meshbrep-bspline-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let mut files = Vec::new();
    let mut expect = Vec::new();
    for name in CASES {
        for (ri, res) in RES.iter().enumerate() {
            let (step, reference) = run(name, *res).expect("case");
            let path = dir.join(format!("{name}-{ri}.step"));
            std::fs::write(&path, step).unwrap();
            files.push(path);
            expect.push((format!("{name} {res:?}"), reference, volume_tolerance(name)));
        }
    }
    let out = std::process::Command::new(&check)
        .args(&files)
        .output()
        .expect("run the oracle");
    let text = String::from_utf8_lossy(&out.stdout);
    let lines: Vec<&str> = text.lines().filter(|l| l.starts_with('{')).collect();
    assert_eq!(lines.len(), files.len(), "oracle output:\n{text}");
    let field = |json: &str, key: &str| -> Option<String> {
        let k = format!("\"{key}\":");
        let rest = &json[json.find(&k)? + k.len()..];
        let end = rest.find([',', '}']).unwrap_or(rest.len());
        Some(rest[..end].to_string())
    };
    let mut failures = Vec::new();
    for (line, (label, reference, tol)) in lines.iter().zip(&expect) {
        let num = |k: &str| {
            field(line, k)
                .and_then(|v| v.parse::<f64>().ok())
                .unwrap_or(f64::NAN)
        };
        // OCCT's two volumes: its adaptive integration to 1e-9, and its
        // fixed-order one. Each misjudges some B-spline patches (OCCT
        // 8.0.1: the adaptive one is 1.8e-8 off the closed form on
        // `cyl_exact`'s rational patch while estimating its error at
        // 3e-17, the fixed-order one 2.6e-8 off on `bump`'s bicubic),
        // and the other then agrees with the closed form within 1e-9, so
        // either may confirm the volume.
        let (volume, fixed) = (num("volume"), num("volume_fixed"));
        let rel = |x: f64| (x - reference).abs() / reference;
        let best = rel(volume).min(rel(fixed));
        let ok = field(line, "valid").as_deref() == Some("true")
            && num("solids") == 1.0
            && num("free_edges") == 0.0
            && num("shells") == num("closed_shells")
            && num("max_tol") <= 1e-6
            && best < tol.max(1e-9);
        eprintln!(
            "{label:22} {} vol {volume:.10} rel {:.1e}, fixed-order {fixed:.10} rel {:.1e}, tol {}",
            if ok { "ok  " } else { "FAIL" },
            rel(volume),
            rel(fixed),
            num("max_tol")
        );
        if !ok {
            failures.push(format!("{label}: {line}"));
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        failures.is_empty(),
        "{} failures:\n{}",
        failures.len(),
        failures.join("\n")
    );
}
