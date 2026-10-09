//! Blends between faces that share no axis ([`super::Path::Curve`]): a
//! cylinder meeting a cylinder at a tee, a boss on a cylinder's side, a
//! cylinder through a plane at a slant, a rod meeting a ball off its
//! centre. No cross-section is the same along such an edge, so nothing
//! is swept or revolved: the ball is rolled.
//!
//! - **The spine.** A ball of radius `r` touching both faces has its
//!   centre at distance `r` from each, on the side away from the material
//!   for a concave edge and inside it for a convex one: on both faces
//!   offset by `r`. Planes, cylinders, cones, spheres and tori offset to
//!   surfaces of the same kinds, and each face here is its exact signed
//!   distance ([`Field`]), so the spine is the curve where two level sets
//!   meet. It is traced by marching from the exact surfaces: a predictor
//!   step along the cross product of the two normals, then Newton onto
//!   both offsets in the plane across the step (the corrector), started
//!   from the offset of the edge's own start point and carried past the
//!   edge's ends until the planes the tool ends on are behind it (or
//!   round a closed edge back to the start).
//! - **The fit.** The marched points are interpolated by a cubic (the
//!   guide), and the spine is defined at every parameter of the guide as
//!   the corrector's point in the guide's normal plane there: exact, and
//!   smooth in the parameter. The spine and the two contact curves (each
//!   spine point's foot on each face) are fitted on one knot vector
//!   ([`crate::spline::fit_curves`]), and the blend is the rational canal
//!   surface between them ([`crate::spline::canal_surface`]), or for a
//!   chamfer the ruled surface between the two curves where it meets the
//!   faces ([`crate::spline::ruled_surface`]). The contacts are the
//!   patch's `u = 0` and `u = 1` sides, which reconstruction recognises
//!   as tangent contacts. A closed edge's blend is two patches (a patch
//!   does not wrap), split at two stations about half the spine apart
//!   and clear of where its contacts cross the facets' creases
//!   ([`splits`]), whose shared rows are the same bits.
//! - **The tool.** Its cross-section at each station is the blend's
//!   arc, then the region's other sides as the straight and revolved
//!   tools have them (out into the air past a convex edge's faces, or
//!   into the material behind a concave edge's curved faces, along a
//!   plane face for a concave edge). Its blend vertices are points of the
//!   patch ([`crate::spline::Evaluator::eval`]). Where the caller gives a
//!   face's facets as the mesh the tool is applied to has them
//!   ([`super::Path::Curve`]'s `facets`), the tool is conformed to them
//!   ([`Facets`]): its contact has vertices on the facets' creases where
//!   it crosses them and lies on the facets between, and its vertices near
//!   the face are moved by the facets' depth under the exact face, so the
//!   kernel's boolean leaves the blend meeting the face along the tool's
//!   own row. Rows with different stations are joined by zipping them in
//!   the order of their parameters.
//!
//! Checks: the corrector must converge all along (the ball fits), each
//! arc must be shorter than a half circle, and the swept arc must not
//! fold: the ball's radius times the spine's curvature towards each arc
//! point stays below one.

use super::{BlendEdge, BlendError, BlendFace, BlendSpec, End, Mesh, Path, Profile, Section};
use crate::bspline;
use crate::math::*;
use crate::model::{BSpline, BSplineSurface, Surface};
use crate::spline;

/// The most stations a march takes before it gives up (a spine that does
/// not close or reach its ends).
const MAX_STEPS: usize = 20_000;

/// The most points the spine and contact fit may use per piece.
const MAX_FIT: usize = 8193;

/// The relative fitting tolerance: of the edge's size (its length plus
/// the blend's).
const FIT: f64 = 1e-9;

/// A face as its exact signed distance: positive in the air, negative in
/// the material, with its gradient the outward unit normal. Every face a
/// blend meets has one that is exact near the face, so the level set at
/// `h` is the face offset by `h`.
#[derive(Clone, Copy, Debug)]
pub(super) enum Field {
    Plane {
        o: V,
        n: V,
    },
    Cyl {
        o: V,
        a: V,
        r: f64,
        s: f64,
    },
    Cone {
        apex: V,
        a: V,
        k: f64,
        s: f64,
    },
    Sphere {
        c: V,
        r: f64,
        s: f64,
    },
    Torus {
        c: V,
        a: V,
        big: f64,
        r: f64,
        s: f64,
    },
}

fn sign(convex: bool) -> f64 {
    if convex { 1.0 } else { -1.0 }
}

impl Field {
    pub(super) fn of(f: &BlendFace) -> Field {
        match *f {
            BlendFace::Plane { origin, normal } => Field::Plane {
                o: V::from(origin),
                n: V::from(normal).norm(),
            },
            BlendFace::Cylinder {
                origin,
                axis,
                radius,
                convex,
            } => Field::Cyl {
                o: V::from(origin),
                a: V::from(axis).norm(),
                r: radius,
                s: sign(convex),
            },
            BlendFace::Cone {
                apex,
                axis,
                slope,
                convex,
            } => Field::Cone {
                apex: V::from(apex),
                a: V::from(axis).norm(),
                k: slope,
                s: sign(convex),
            },
            BlendFace::Sphere {
                center,
                radius,
                convex,
            } => Field::Sphere {
                c: V::from(center),
                r: radius,
                s: sign(convex),
            },
            BlendFace::Torus {
                center,
                axis,
                major_radius,
                minor_radius,
                convex,
            } => Field::Torus {
                c: V::from(center),
                a: V::from(axis).norm(),
                big: major_radius,
                r: minor_radius,
                s: sign(convex),
            },
        }
    }

    /// The signed distance of `p` from the face.
    pub(super) fn f(&self, p: V) -> f64 {
        match *self {
            Field::Plane { o, n } => (p - o).dot(n),
            Field::Cyl { o, a, r, s } => s * ((p - o).reject(a).len() - r),
            Field::Cone { apex, a, k, s } => {
                let q = p - apex;
                let h = q.dot(a);
                let rho = (q - a * h).len();
                s * (rho - k * h) / (1.0 + k * k).sqrt()
            }
            Field::Sphere { c, r, s } => s * ((p - c).len() - r),
            Field::Torus { c, a, big, r, s } => {
                let q = p - c;
                let tube = c + q.reject(a).norm() * big;
                s * ((p - tube).len() - r)
            }
        }
    }

    /// The gradient of [`Field::f`]: the outward unit normal of the face
    /// (offset through `p`).
    pub(super) fn grad(&self, p: V) -> V {
        match *self {
            Field::Plane { n, .. } => n,
            Field::Cyl { o, a, s, .. } => (p - o).reject(a).norm() * s,
            Field::Cone { apex, a, k, s } => {
                let q = p - apex;
                let radial = q.reject(a).norm();
                (radial - a * k) * (s / (1.0 + k * k).sqrt())
            }
            Field::Sphere { c, s, .. } => (p - c).norm() * s,
            Field::Torus { c, a, big, s, .. } => {
                let q = p - c;
                let tube = c + q.reject(a).norm() * big;
                (p - tube).norm() * s
            }
        }
    }

    /// The least radius of curvature of the face (`None` for a plane):
    /// what the tool's margin beside it must stay well under.
    fn radius_near(&self, p: V) -> Option<f64> {
        match *self {
            Field::Plane { .. } => None,
            Field::Cyl { r, .. } | Field::Sphere { r, .. } => Some(r),
            Field::Cone { apex, a, k, .. } => {
                let q = p - apex;
                Some(q.reject(a).len() * (1.0 + k * k).sqrt())
            }
            Field::Torus { r, .. } => Some(r),
        }
    }

    /// Whether a point of the face is on its real part: a cone's nappe
    /// (not past the apex), a torus's outer part (off the axis).
    fn on_part(&self, p: V) -> bool {
        match *self {
            Field::Cone { apex, a, .. } => (p - apex).dot(a) > 0.0,
            Field::Torus { c, a, .. } => (p - c).reject(a).len() > 0.0,
            _ => true,
        }
    }
}

/// Newton onto the level sets `f_k = h_k` of both faces within the plane
/// through `o` with unit normal `n`, from `x0`; `None` when it does not
/// converge, or lands further than `reach` from `x0` (another branch).
fn solve(fields: &[Field; 2], h: [f64; 2], x0: V, o: V, n: V, scale: f64, reach: f64) -> Option<V> {
    let mut x = x0;
    let tiny = 1e-15 * scale;
    for _ in 0..60 {
        let g = [fields[0].grad(x), fields[1].grad(x)];
        let mut a = [
            [g[0].x, g[0].y, g[0].z],
            [g[1].x, g[1].y, g[1].z],
            [n.x, n.y, n.z],
        ];
        let mut b = [
            h[0] - fields[0].f(x),
            h[1] - fields[1].f(x),
            -(x - o).dot(n),
        ];
        let d = solve_dense(&mut a, &mut b, 3)?;
        let dx = v(d[0], d[1], d[2]);
        if !dx.is_finite() {
            return None;
        }
        x = x + dx;
        if (x - x0).len() > reach {
            return None;
        }
        if dx.len() <= tiny {
            break;
        }
    }
    let ok = (0..2).all(|k| (fields[k].f(x) - h[k]).abs() <= 1e-10 * scale)
        && (x - o).dot(n).abs() <= 1e-10 * scale;
    ok.then_some(x)
}

/// The point at offsets `h` from both faces in the plane through `c` with
/// normal `t`, followed from `c` (at its own offsets) in eight steps, each
/// started from both tangent planes at the last point and corrected by
/// Newton. One linearised step from `c` lands the corner of a region far
/// from a strongly curved face (a large ball beside a thin branch of a
/// tee) on another branch of the offsets' intersection, or nowhere; small
/// steps keep to the one through `c`.
fn offset_point(fields: &[Field; 2], h: [f64; 2], c: V, t: V, scale: f64, reach: f64) -> Option<V> {
    const STEPS: usize = 8;
    let start = [fields[0].f(c), fields[1].f(c)];
    let mut x = c;
    for j in 1..=STEPS {
        let w = j as f64 / STEPS as f64;
        let target = [
            start[0] + (h[0] - start[0]) * w,
            start[1] + (h[1] - start[1]) * w,
        ];
        let g = [fields[0].grad(x), fields[1].grad(x)];
        let mut a = [
            [g[0].x, g[0].y, g[0].z],
            [g[1].x, g[1].y, g[1].z],
            [t.x, t.y, t.z],
        ];
        let mut b = [
            target[0] - fields[0].f(x),
            target[1] - fields[1].f(x),
            -(x - c).dot(t),
        ];
        let y = solve_dense(&mut a, &mut b, 3)?;
        let guess = x + v(y[0], y[1], y[2]);
        x = solve(fields, target, guess, c, t, scale, reach)?;
    }
    Some(x)
}

/// A blend's edge prepared for marching: its faces, the spine's offsets
/// and the region's.
pub(super) struct Rolled {
    pub index: usize,
    fields: [Field; 2],
    /// The spine's offsets: `-σ r` for a fillet (inside the material for
    /// a convex edge), 0 for a chamfer (its "spine" is the edge).
    h: [f64; 2],
    /// The region's sides' offsets from each face.
    mu: [f64; 2],
    size: f64,
    chamfer: bool,
    convex: bool,
    /// A length of the edge's order, for tolerances.
    scale: f64,
    /// The edge's points, from `from` to `to`.
    points: Vec<V>,
    closed: bool,
    /// Each face as the mesh the tool is applied to has it, if given.
    facets: [Option<Facets>; 2],
    /// The planes the tool is cut by at each end (`None` when closed),
    /// pointing away from the tool, and the caps' surfaces.
    cuts: [Option<(V, V, Surface)>; 2],
    /// How far the region reaches past the faces (`mu`'s size).
    margin: f64,
}

/// An edge's direction at polyline point `i` (the chord to the next).
fn direction(p: &[V], i: usize) -> V {
    let n = p.len();
    let (a, b) = if i + 1 < n {
        (p[i], p[i + 1])
    } else {
        (p[n - 2], p[n - 1])
    };
    (b - a).norm()
}

impl Rolled {
    pub(super) fn new(spec: &BlendSpec, i: usize) -> Result<Rolled, BlendError> {
        let e = &spec.edges[i];
        let Path::Curve { points, facets } = &e.path else {
            return Err(BlendError::Invalid(format!("edge {i} is not a curve")));
        };
        let size = spec.size;
        if !(size > 0.0 && size.is_finite()) {
            return Err(BlendError::Invalid("the size must be positive".into()));
        }
        let mut pts: Vec<V> = points.iter().map(|p| V::from(*p)).collect();
        pts.dedup();
        if pts.len() < 2 || pts.iter().any(|p| !p.is_finite()) {
            return Err(BlendError::Invalid(format!(
                "edge {i}: a curve needs at least two points"
            )));
        }
        let closed = e.from == e.to;
        if closed && pts.len() > 2 && pts[0] == pts[pts.len() - 1] {
            pts.pop();
        }
        if closed && pts.len() < 3 {
            return Err(BlendError::Invalid(format!(
                "edge {i}: a closed curve needs at least three points"
            )));
        }
        let fields = [Field::of(&e.faces[0]), Field::of(&e.faces[1])];
        if fields.iter().all(|f| matches!(f, Field::Plane { .. })) {
            return Err(BlendError::Invalid(format!(
                "edge {i}: two planes meet in a line"
            )));
        }
        let sigma = sign(e.convex);
        let chamfer = spec.profile == Profile::Chamfer;
        let h = if chamfer {
            [0.0; 2]
        } else {
            [-sigma * size; 2]
        };
        let length: f64 = pts.windows(2).map(|w| (w[1] - w[0]).len()).sum::<f64>()
            + if closed {
                (pts[0] - pts[pts.len() - 1]).len()
            } else {
                0.0
            };
        let scale = length + size;
        // The margin: the size, at most half each curved face's radius
        // (so the region's sides keep clear of the axis, and of the far
        // side of a hole), at most the caller's.
        let mut margin = size;
        for f in &fields {
            if let Some(r) = f.radius_near(pts[0]) {
                margin = margin.min(0.5 * r);
            }
        }
        if let Some(c) = e.margin
            && c > 0.0
        {
            margin = margin.min(c);
        }
        let mut mu = [0.0; 2];
        for k in 0..2 {
            mu[k] = if e.convex {
                margin
            } else if e.faces[k].curved() {
                -margin
            } else {
                0.0
            };
        }
        let facets = [
            Facets::new(fields[0], &facets[0]),
            Facets::new(fields[1], &facets[1]),
        ];
        let mut r = Rolled {
            index: i,
            fields,
            h,
            mu,
            size,
            chamfer,
            convex: e.convex,
            scale,
            points: pts,
            closed,
            facets,
            cuts: [None, None],
            margin,
        };
        if !closed {
            for end in 0..2 {
                r.cuts[end] = Some(r.cut(e, end)?);
            }
        }
        Ok(r)
    }

    /// The plane the tool is cut by at end `end` (pointing away from it)
    /// and the cap's surface.
    fn cut(&self, e: &BlendEdge, end: usize) -> Result<(V, V, Surface), BlendError> {
        let p = &self.points;
        let (at, d_out) = if end == 0 {
            (p[0], direction(p, 0) * -1.0)
        } else {
            (p[p.len() - 1], direction(p, p.len() - 2))
        };
        // How far past the end face an open end runs into the air: far
        // enough that the cap is clear of the face, no further than needed.
        let ext = 0.25 * (self.size + self.margin);
        match &e.ends[end] {
            End::Plane { origin, normal } => {
                let n = V::from(*normal).norm();
                Ok((
                    V::from(*origin),
                    n,
                    Surface::Plane {
                        origin: *origin,
                        normal: n.arr(),
                    },
                ))
            }
            End::Open { face } => {
                let (o, n) = match face {
                    Some((o, n)) => {
                        let n = V::from(*n).norm();
                        (V::from(*o) + n * ext, n)
                    }
                    None => (at + d_out * ext, d_out),
                };
                Ok((
                    o,
                    n,
                    Surface::Plane {
                        origin: o.arr(),
                        normal: n.arr(),
                    },
                ))
            }
            _ => Err(BlendError::Invalid(format!(
                "edge {}: a blend along a curve ends only on a plane or in the air",
                self.index
            ))),
        }
    }

    /// The spine's direction at `x` (unnormalised): across both normals.
    fn tangent(&self, x: V) -> V {
        self.fields[0].grad(x).cross(self.fields[1].grad(x))
    }

    /// The spine's point in the plane through `o` with unit normal `n`,
    /// from `guess`.
    fn spine_in(&self, guess: V, o: V, n: V, reach: f64) -> Option<V> {
        solve(&self.fields, self.h, guess, o, n, self.scale, reach)
    }

    /// The spine's point across the edge at its point `e` (direction `d`).
    fn spine_at_edge(&self, e: V, d: V) -> Option<V> {
        offset_point(
            &self.fields,
            self.h,
            e,
            d,
            self.scale,
            4.0 * (self.size + self.margin) + self.scale * 0.25,
        )
    }

    /// The feet of the station at spine point `c`: where the ball touches
    /// each face (a fillet), or where the chamfer meets it.
    fn feet(&self, c: V) -> Option<[V; 2]> {
        if !self.chamfer {
            let f = [
                c - self.fields[0].grad(c) * self.h[0],
                c - self.fields[1].grad(c) * self.h[1],
            ];
            return (self.fields[0].on_part(f[0]) && self.fields[1].on_part(f[1])).then_some(f);
        }
        // A chamfer: on each face, in the plane across the edge, the point
        // at distance `d` from the edge on the material's side.
        let t = self.tangent(c).norm();
        let g = [self.fields[0].grad(c), self.fields[1].grad(c)];
        let mut out = [V::default(); 2];
        for k in 0..2 {
            let w = g[k].cross(t).norm();
            let s = w.dot(g[1 - k]);
            let w = if (self.convex && s > 0.0) || (!self.convex && s < 0.0) {
                w * -1.0
            } else {
                w
            };
            let d = self.size;
            let mut x = c + w * d;
            let mut ok = false;
            for _ in 0..60 {
                let gk = self.fields[k].grad(x);
                let r = x - c;
                let mut a = [
                    [gk.x, gk.y, gk.z],
                    [t.x, t.y, t.z],
                    [2.0 * r.x, 2.0 * r.y, 2.0 * r.z],
                ];
                let mut b = [-self.fields[k].f(x), -r.dot(t), d * d - r.dot(r)];
                let dx = solve_dense(&mut a, &mut b, 3)?;
                let dx = v(dx[0], dx[1], dx[2]);
                x = x + dx;
                if !x.is_finite() || (x - c).len() > 4.0 * d {
                    return None;
                }
                if dx.len() <= 1e-15 * self.scale {
                    ok = true;
                    break;
                }
            }
            if !ok && self.fields[k].f(x).abs() > 1e-10 * self.scale {
                return None;
            }
            if !self.fields[k].on_part(x) || (x - c).dot(w) <= 0.0 {
                return None;
            }
            out[k] = x;
        }
        Some(out)
    }

    /// The region's corner at station `c` (spine point, direction `t`):
    /// the edge's point in the station's plane, moved off both faces by
    /// the sides' offsets in their tangent planes there. Exact on a plane
    /// face (a concave tool's side lies in it); behind a curved face only
    /// near its offset, which is all that side needs (it is inside the
    /// material, or in the air). Solving for both offsets exactly instead
    /// fails where the offsets of a strongly curved face do not meet the
    /// plane near the edge (a large ball beside a thin branch of a tee).
    fn corner(&self, c: V, t: V) -> Option<V> {
        let e = if self.chamfer {
            c
        } else {
            offset_point(
                &self.fields,
                [0.0; 2],
                c,
                t,
                self.scale,
                4.0 * (self.size + self.margin) + 0.25 * self.scale,
            )?
        };
        let g = [self.fields[0].grad(e), self.fields[1].grad(e)];
        let mut a = [
            [g[0].x, g[0].y, g[0].z],
            [g[1].x, g[1].y, g[1].z],
            [t.x, t.y, t.z],
        ];
        let mut b = [
            self.mu[0] - self.fields[0].f(e),
            self.mu[1] - self.fields[1].f(e),
            0.0,
        ];
        let y = solve_dense(&mut a, &mut b, 3)?;
        let q = e + v(y[0], y[1], y[2]);
        q.is_finite().then_some(q)
    }

    /// The whole ring at a station, exactly: the arc (`n` segments) from
    /// the feet about `c`, then the region's other points
    /// ([`Rolled::rest`]). `None` where the station fails.
    fn ring_exact(&self, c: V, n: usize, sides: [usize; 2]) -> Option<Vec<V>> {
        let feet = self.feet(c)?;
        let t = self.tangent(c).norm();
        let mut ring = Vec::with_capacity(n + 8);
        if self.chamfer {
            ring.push(feet[0]);
            ring.push(feet[1]);
        } else {
            let (ma, mb) = ((feet[0] - c).norm(), (feet[1] - c).norm());
            let th = atan2(ma.cross(mb).len(), ma.dot(mb));
            let w = (mb - ma * ma.dot(mb)).norm();
            ring.push(feet[0]);
            for j in 1..n {
                let a = th * j as f64 / n as f64;
                ring.push(c + (ma * cos(a) + w * sin(a)) * self.size);
            }
            ring.push(feet[1]);
        }
        ring.extend(self.rest(c, t, feet, sides, [None, None])?);
        Some(ring)
    }

    /// The region's points after the blend's: near foot `b` (`inner[1]`,
    /// see [`Rolled::inner`]), beside it (when its offset is not zero),
    /// along face `b`'s offset to the corner, the corner, along face
    /// `a`'s offset, beside foot `a`, near it (`inner[0]`).
    fn rest(
        &self,
        c: V,
        t: V,
        feet: [V; 2],
        sides: [usize; 2],
        inner: [Option<f64>; 2],
    ) -> Option<Vec<V>> {
        let q = self.corner(c, t)?;
        let beside = |k: usize| feet[k] + self.fields[k].grad(feet[k]) * self.mu[k];
        let near =
            |k: usize, d: f64| feet[k] + self.fields[k].grad(feet[k]) * (self.mu[k].signum() * d);
        let along = |k: usize, from: V, to: V, out: &mut Vec<V>| {
            let n = sides[k];
            for j in 1..=n {
                let x = from + (to - from) * (j as f64 / (n + 1) as f64);
                let f = &self.fields[k];
                out.push(x - f.grad(x) * (f.f(x) - self.mu[k]));
            }
        };
        let mut out = Vec::new();
        let pb = if self.mu[1] != 0.0 {
            if let Some(d) = inner[1] {
                out.push(near(1, d));
            }
            let p = beside(1);
            out.push(p);
            p
        } else {
            feet[1]
        };
        along(1, pb, q, &mut out);
        out.push(q);
        let pa = if self.mu[0] != 0.0 {
            beside(0)
        } else {
            feet[0]
        };
        let mut tail = Vec::new();
        along(0, q, pa, &mut tail);
        out.extend(tail);
        if self.mu[0] != 0.0 {
            out.push(pa);
            if let Some(d) = inner[0] {
                out.push(near(0, d));
            }
        }
        Some(out)
    }

    /// Per face, how far off it the region's side gets a row of its own
    /// (`None`: none): half the band its facets warp within, when it is
    /// conformed to them and the side leaves it (an offset). The side
    /// running straight from the contact out to the offset would cross
    /// the facets' ridges between its rows' vertices, a sliver of it left
    /// standing in the result; a row near the face is conformed as the
    /// blend's rows near it are.
    fn inner(&self) -> [Option<f64>; 2] {
        [0, 1].map(|k| {
            let f = self.facets[k].as_ref()?;
            (self.mu[k] != 0.0 && 0.5 * f.band < self.mu[k].abs()).then_some(0.5 * f.band)
        })
    }

    /// How many points each face's side of the region gets between the
    /// point beside its foot and the corner: a curved face's offset is
    /// followed in steps of at most π/16 of its normal's turn.
    fn sides_at(&self, c: V) -> [usize; 2] {
        let mut out = [0; 2];
        let (Some(feet), Some(q)) = (self.feet(c), self.corner(c, self.tangent(c).norm())) else {
            return out;
        };
        for k in 0..2 {
            if self.mu[k] == 0.0 || matches!(self.fields[k], Field::Plane { .. }) {
                continue;
            }
            let (n0, n1) = (self.fields[k].grad(feet[k]), self.fields[k].grad(q));
            let ang = atan2(n0.cross(n1).len(), n0.dot(n1));
            out[k] = ((ang / (PI / 16.0)).ceil() as usize)
                .saturating_sub(1)
                .min(16);
        }
        out
    }

    /// Whether every point of the ring at spine point `c` lies past cut
    /// plane `end`.
    fn past(&self, c: V, end: usize) -> bool {
        let Some((o, n, _)) = &self.cuts[end] else {
            return false;
        };
        let sides = [8, 8];
        match self.ring_exact(c, 4, sides) {
            Some(r) => r.iter().all(|p| (*p - *o).dot(*n) > 1e-9 * self.scale),
            None => false,
        }
    }

    /// Marches the spine from `x0` along `d0` until `stop` says so (or, with
    /// `close`, until it comes back round to `x0`). Returns the points
    /// after `x0`, and whether it closed.
    fn march(
        &self,
        x0: V,
        d0: V,
        close: bool,
        stop: &dyn Fn(V, f64) -> bool,
    ) -> Result<Vec<V>, BlendError> {
        let step_max = (self.scale / 48.0).min(0.5 * self.size + 0.25 * self.margin);
        let step_min = 1e-7 * self.scale;
        let max_len = 4.0 * self.scale + 40.0 * (self.size + self.margin);
        let mut out = Vec::new();
        let mut x = x0;
        let mut dir = d0;
        let mut step = step_max;
        let mut travelled = 0.0;
        loop {
            let t0 = self.tangent(x).norm();
            let t0 = if t0.dot(dir) < 0.0 { t0 * -1.0 } else { t0 };
            if t0.len() < 0.5 {
                return Err(self.too_large());
            }
            if close && travelled > 2.0 * step_max {
                let back = x0 - x;
                if back.dot(t0) > 0.0 && back.len() <= 1.25 * step {
                    return Ok(out);
                }
            }
            let pred = x + t0 * step;
            let next = self.spine_in(pred, pred, t0, 0.5 * step).and_then(|y| {
                let t1 = self.tangent(y).norm();
                let t1 = if t1.dot(t0) < 0.0 { t1 * -1.0 } else { t1 };
                let turn = atan2(t0.cross(t1).len(), t0.dot(t1));
                let moved = (y - x).len();
                (turn <= 0.2 && (y - pred).len() <= 0.25 * step && moved >= 0.5 * step)
                    .then_some((y, t1, turn, moved))
            });
            match next {
                Some((y, t1, turn, moved)) => {
                    out.push(y);
                    travelled += moved;
                    x = y;
                    dir = t1;
                    if turn < 0.05 {
                        step = (step * 1.5).min(step_max);
                    }
                    if !close && stop(x, travelled) {
                        return Ok(out);
                    }
                }
                None => {
                    step *= 0.5;
                    if step < step_min {
                        return Err(self.too_large());
                    }
                }
            }
            if out.len() > MAX_STEPS || travelled > max_len {
                return Err(BlendError::Invalid(format!(
                    "edge {}: its spine does not {}",
                    self.index,
                    if close { "close" } else { "reach its ends" }
                )));
            }
        }
    }

    fn too_large(&self) -> BlendError {
        BlendError::TooLarge(self.index)
    }

    /// The marched spine: its points in order (a closed one without the
    /// repeated start), from before the start's cut to past the end's.
    pub(super) fn marched(&self) -> Result<Vec<V>, BlendError> {
        let p = &self.points;
        let d0 = direction(p, 0);
        let x0 = self
            .spine_at_edge(p[0], d0)
            .ok_or_else(|| self.too_large())?;
        if self.closed {
            let mut out = vec![x0];
            out.extend(self.march(x0, d0, true, &|_, _| false)?);
            if out.len() < 4 {
                return Err(BlendError::Invalid(format!(
                    "edge {}: its spine is too short to close",
                    self.index
                )));
            }
            return Ok(out);
        }
        // Past the end point along the edge, and the whole ring past the
        // cut there.
        let n = p.len();
        let (e0, e1) = (p[0], p[n - 1]);
        let (d_start, d_end) = (d0, direction(p, n - 2));
        let length: f64 = p.windows(2).map(|w| (w[1] - w[0]).len()).sum();
        let ahead = self.march(x0, d0, false, &|x, gone| {
            gone > 0.5 * length && (x - e1).dot(d_end) > 0.0 && self.past(x, 1)
        })?;
        let behind = self.march(x0, d0 * -1.0, false, &|x, _| {
            (x - e0).dot(d_start) < 0.0 && self.past(x, 0)
        })?;
        // One more station beyond each, so the patch runs past the cut.
        let mut out: Vec<V> = behind.into_iter().rev().collect();
        out.push(x0);
        out.extend(ahead);
        Ok(out)
    }
}

/// The guide: a cubic through the marched points at their chord lengths
/// (a closed spine's padded round its start so that it is smooth there),
/// and the parameter range of the spine on it.
struct Guide {
    curve: BSpline<3>,
    deriv: BSpline<3>,
    range: [f64; 2],
}

fn guide(pts: &[V], closed: bool) -> Result<Guide, BlendError> {
    let n = pts.len();
    let mut ts = vec![0.0; n];
    for k in 1..n {
        ts[k] = ts[k - 1] + (pts[k] - pts[k - 1]).len();
    }
    let (all, params, range) = if closed {
        // Round the loop three times, from -L to 2L, so that the pieces
        // can start anywhere in [0, L) and the guide is smooth there.
        let l = ts[n - 1] + (pts[0] - pts[n - 1]).len();
        let mut all = Vec::with_capacity(3 * n + 1);
        let mut params = Vec::with_capacity(3 * n + 1);
        for (shift, last) in [(-l, false), (0.0, false), (l, true)] {
            for k in 0..n {
                all.push(pts[k].arr());
                params.push(ts[k] + shift);
            }
            if last {
                all.push(pts[0].arr());
                params.push(2.0 * l);
            }
        }
        (all, params, [0.0, l])
    } else {
        (
            pts.iter().map(|p| p.arr()).collect(),
            ts.clone(),
            [0.0, ts[n - 1]],
        )
    };
    let curve = spline::interpolate_curve(&all, &params).map_err(BlendError::Invalid)?;
    let deriv = bspline::derivative(&curve);
    Ok(Guide {
        curve,
        deriv,
        range,
    })
}

/// One fitted piece of a blend: its parameter range, the spine and the
/// two contacts on one knot vector, the patch and its evaluator.
struct Piece {
    range: [f64; 2],
    curves: Vec<BSpline<3>>,
    patch: BSplineSurface,
    eval: spline::Evaluator,
}

/// An exact station (the checks): its parameter and its spine point.
#[derive(Clone, Copy)]
struct Station {
    t: f64,
    c: V,
}

/// The fitted blend of edge `r`: the guide's parameter range split into
/// pieces, each fitted. Returns the pieces and the largest fit error.
fn fit(r: &Rolled, g: &Guide, bounds: &[[f64; 2]]) -> Result<(Vec<Piece>, f64), BlendError> {
    // A closed spine's last piece ends where the first starts.
    let (first, last) = (bounds[0][0], bounds[bounds.len() - 1][1]);
    let spine = |t: f64| -> V {
        let t = if r.closed && t >= last { first } else { t };
        let o = V::from(bspline::eval(&g.curve, t));
        let d = V::from(bspline::eval(&g.deriv, t)).norm();
        r.spine_in(o, o, d, 0.25 * (r.size + r.margin) + 1e-6 * r.scale)
            .unwrap_or(v(f64::NAN, f64::NAN, f64::NAN))
    };
    let foot = |k: usize, t: f64| -> V {
        let c = spine(t);
        match r.feet(c) {
            Some(f) => f[k],
            None => v(f64::NAN, f64::NAN, f64::NAN),
        }
    };
    let s_fn = |t: f64| spine(t).arr();
    let a_fn = |t: f64| foot(0, t).arr();
    let b_fn = |t: f64| foot(1, t).arr();
    let mut pieces = Vec::new();
    let mut worst: f64 = 0.0;
    for &range in bounds {
        let mut tol = FIT * r.scale;
        let mut tries = 0;
        loop {
            let fs: [&dyn Fn(f64) -> [f64; 3]; 3] = [&s_fn, &a_fn, &b_fn];
            let fitted = spline::fit_curves(&fs, range, tol, MAX_FIT).map_err(|e| {
                BlendError::Invalid(format!(
                    "edge {}: its spine could not be fitted: {e}",
                    r.index
                ))
            })?;
            if fitted
                .curves
                .iter()
                .any(|c| c.control.iter().flatten().any(|x| !x.is_finite()))
            {
                return Err(r.too_large());
            }
            let (patch, err) = if r.chamfer {
                let p = spline::ruled_surface(&fitted.curves[1], &fitted.curves[2])
                    .map_err(BlendError::Invalid)?;
                (p, fitted.error)
            } else {
                let c = spline::canal_surface(
                    &fitted.curves[0],
                    &fitted.curves[1],
                    &fitted.curves[2],
                    r.size,
                )
                .map_err(|e| BlendError::Invalid(format!("edge {}: {e}", r.index)))?;
                (c.surface, c.error.max(fitted.error))
            };
            if err > 10.0 * FIT * r.scale && tries < 2 {
                tol *= 0.125;
                tries += 1;
                continue;
            }
            worst = worst.max(err);
            let eval = spline::Evaluator::new(&patch).map_err(BlendError::Invalid)?;
            pieces.push(Piece {
                range,
                curves: fitted.curves,
                patch,
                eval,
            });
            break;
        }
    }
    Ok((pieces, worst))
}

/// Where a closed spine is split into its two pieces: the parameters
/// `[s1, s2]` and `[s2, s1 + L]` of its guide (`L` its length), each split
/// as far as can be from where a conformed contact crosses a crease of
/// its face's facets. A split is a vertex of every row of the tool: near
/// a crossing, the two would be a sliver of an edge apart, which the
/// reconstruction's corners then fold over (a tee's contact on a 32-gon
/// crossed itself so).
fn splits(r: &Rolled, marched: &[V], l: f64) -> Vec<[f64; 2]> {
    let n = marched.len();
    let mut ts = vec![0.0; n];
    for k in 1..n {
        ts[k] = ts[k - 1] + (marched[k] - marched[k - 1]).len();
    }
    // The crossings, from the exact feet at the marched points (linear
    // between them: close enough to stay clear of).
    let mut cross: Vec<f64> = Vec::new();
    for k in 0..2 {
        let Some(f) = &r.facets[k] else {
            continue;
        };
        let mut pts: Vec<(f64, V)> = Vec::with_capacity(n + 1);
        for j in 0..=n {
            let (t, c) = if j == n {
                (l, marched[0])
            } else {
                (ts[j], marched[j])
            };
            if let Some(feet) = r.feet(c) {
                pts.push((t, feet[k]));
            }
        }
        cross.extend(f.rough_crossings(&pts));
    }
    if cross.is_empty() {
        return vec![[0.0, 0.5 * l], [0.5 * l, l]];
    }
    cross.sort_by(f64::total_cmp);
    // The middles of the gaps between crossings (round the loop), each
    // with its gap: the widest near each half of the loop wins.
    let m = cross.len();
    let gaps: Vec<(f64, f64)> = (0..m)
        .map(|i| {
            let (a, b) = (
                cross[i],
                if i + 1 < m {
                    cross[i + 1]
                } else {
                    cross[0] + l
                },
            );
            let mid = 0.5 * (a + b);
            (if mid >= l { mid - l } else { mid }, b - a)
        })
        .collect();
    // The widest gap whose middle (or the middle a turn later) lies in
    // `[from, to]`, as that parameter.
    let best = |from: f64, to: f64| -> Option<f64> {
        gaps.iter()
            .flat_map(|g| [(g.0 - l, g.1), (g.0, g.1), (g.0 + l, g.1)])
            .filter(|g| g.0 >= from && g.0 <= to)
            .fold(None, |acc: Option<(f64, f64)>, g| match acc {
                Some(a) if a.1 >= g.1 => Some(a),
                _ => Some(g),
            })
            .map(|g| g.0)
    };
    let s1 = best(0.0, l).filter(|&t| t < l).unwrap_or(0.0);
    // The guide runs from -L to 2L, and its ends are less accurate (an
    // interpolant's end conditions): keep the pieces at least L/2 inside.
    let s1 = if s1 > 0.5 * l { s1 - l } else { s1 };
    let s2 = best(s1 + 0.3 * l, s1 + 0.7 * l).unwrap_or(s1 + 0.5 * l);
    vec![[s1, s2], [s2, s1 + l]]
}

/// The median gap between consecutive parameters (sorted).
fn median_gap(ts: &[f64]) -> f64 {
    let mut g: Vec<f64> = ts.windows(2).map(|w| w[1] - w[0]).collect();
    if g.is_empty() {
        return 0.0;
    }
    g.sort_by(f64::total_cmp);
    g[g.len() / 2]
}

/// The piece whose range holds `t`.
fn piece_of(pieces: &[Piece], t: f64) -> &Piece {
    pieces
        .iter()
        .find(|p| t <= p.range[1])
        .unwrap_or(&pieces[pieces.len() - 1])
}

/// The tool's stations along the fitted pieces: each piece split evenly
/// in 8, then halved where a chord of the spine or a contact strays from
/// its curve by more than `sag`.
fn base_stations(pieces: &[Piece], sag: f64) -> Vec<f64> {
    let mut out = Vec::new();
    for p in pieces {
        let at = |t: f64| -> [V; 3] { [0, 1, 2].map(|k| V::from(bspline::eval(&p.curves[k], t))) };
        let [a, b] = p.range;
        let mut ts = Vec::new();
        fn split(
            at: &dyn Fn(f64) -> [V; 3],
            t0: f64,
            t1: f64,
            sag: f64,
            depth: u32,
            out: &mut Vec<f64>,
        ) {
            let (p0, p1, pm) = (at(t0), at(t1), at(0.5 * (t0 + t1)));
            let far = (0..3).any(|k| {
                let mid = (p0[k] + p1[k]) * 0.5;
                (pm[k] - mid).len() > sag
            });
            if far && depth < 10 {
                split(at, t0, 0.5 * (t0 + t1), sag, depth + 1, out);
                split(at, 0.5 * (t0 + t1), t1, sag, depth + 1, out);
            } else {
                out.push(t1);
            }
        }
        ts.push(a);
        for k in 0..8 {
            let t0 = a + (b - a) * k as f64 / 8.0;
            let t1 = if k == 7 {
                b
            } else {
                a + (b - a) * (k + 1) as f64 / 8.0
            };
            split(&at, t0, t1, sag, 0, &mut ts);
        }
        out.extend(ts);
    }
    out.sort_by(f64::total_cmp);
    out.dedup();
    out
}

/// A face as the mesh the tool is applied to has it: its triangles near
/// the edge, the creases between them, and how deep under (or over) the
/// exact face they lie. A faceted face is inscribed in its exact one, so a
/// blend tangent to the exact face rises over the facets near its
/// contact (or dips under them), and a kernel's boolean then leaves a
/// sliver of facet standing over the blend instead of the blend meeting
/// the face along the tool's row. The tool is conformed instead: its
/// contact has vertices where the contact crosses the creases (on the
/// crease itself), and every tool vertex near the face is moved along the
/// face's normal by the facets' depth there, fading out within a band a
/// few times that depth, which keeps every vertex on the side of the
/// faceted face it had of the exact one.
struct Facets {
    field: Field,
    tris: Vec<[V; 3]>,
    /// Creases: their ends, and the normal of the plane through each
    /// along the face's normal (where a contact crosses it).
    creases: Vec<(V, V, V)>,
    /// How far from the exact face the warp reaches: a few times the
    /// facets' greatest depth under it.
    band: f64,
    grid: Grid,
}

/// Triangles bucketed in a uniform grid, for casting short rays.
struct Grid {
    lo: V,
    cell: f64,
    dims: [usize; 3],
    cells: Vec<Vec<u32>>,
}

fn bbox(pts: &[V]) -> (V, V) {
    let mut lo = v(f64::INFINITY, f64::INFINITY, f64::INFINITY);
    let mut hi = v(f64::NEG_INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY);
    for p in pts {
        lo = v(lo.x.min(p.x), lo.y.min(p.y), lo.z.min(p.z));
        hi = v(hi.x.max(p.x), hi.y.max(p.y), hi.z.max(p.z));
    }
    (lo, hi)
}

impl Grid {
    fn new(tris: &[[V; 3]]) -> Grid {
        let all: Vec<V> = tris.iter().flatten().copied().collect();
        let (lo, hi) = bbox(&all);
        let d = hi - lo;
        let cell = (d.x.max(d.y).max(d.z) / 48.0).max(1e-9);
        let dims = [d.x, d.y, d.z].map(|x| ((x / cell) as usize + 1).min(64));
        let mut g = Grid {
            lo,
            cell,
            dims,
            cells: vec![Vec::new(); dims[0] * dims[1] * dims[2]],
        };
        for (i, t) in tris.iter().enumerate() {
            let (a, b) = bbox(t);
            let (i0, i1) = (g.index(a), g.index(b));
            for x in i0[0]..=i1[0] {
                for y in i0[1]..=i1[1] {
                    for z in i0[2]..=i1[2] {
                        let c = (x * g.dims[1] + y) * g.dims[2] + z;
                        g.cells[c].push(i as u32);
                    }
                }
            }
        }
        g
    }

    fn index(&self, p: V) -> [usize; 3] {
        let q = p - self.lo;
        let at = |x: f64, d: usize| {
            let i = (x / self.cell).floor();
            if i.is_nan() || i < 0.0 {
                0
            } else {
                (i as usize).min(d - 1)
            }
        };
        [
            at(q.x, self.dims[0]),
            at(q.y, self.dims[1]),
            at(q.z, self.dims[2]),
        ]
    }

    /// The triangles whose cells the box of `pts` touches, each once, in
    /// ascending order.
    fn near(&self, pts: &[V]) -> Vec<u32> {
        let (lo, hi) = bbox(pts);
        let (i0, i1) = (self.index(lo), self.index(hi));
        let mut out = Vec::new();
        for x in i0[0]..=i1[0] {
            for y in i0[1]..=i1[1] {
                for z in i0[2]..=i1[2] {
                    out.extend(&self.cells[(x * self.dims[1] + y) * self.dims[2] + z]);
                }
            }
        }
        out.sort_unstable();
        out.dedup();
        out
    }
}

/// Where the line through `o` along unit `d` meets triangle `t`: the
/// parameter along the line, if it meets it (edges included, a little).
fn ray_triangle(o: V, d: V, t: &[V; 3]) -> Option<f64> {
    let e1 = t[1] - t[0];
    let e2 = t[2] - t[0];
    let p = d.cross(e2);
    let det = e1.dot(p);
    if det.abs() <= 1e-300 {
        return None;
    }
    let inv = 1.0 / det;
    let s = o - t[0];
    let u = s.dot(p) * inv;
    let q = s.cross(e1);
    let w = d.dot(q) * inv;
    let eps = 1e-9;
    if u < -eps || w < -eps || u + w > 1.0 + eps {
        return None;
    }
    Some(e2.dot(q) * inv)
}

impl Facets {
    /// The face's triangles `given` (those of the mesh lying on it near
    /// the edge), or `None` when there are none (a plane needs none: its
    /// facets are the plane).
    fn new(field: Field, given: &[[[f64; 3]; 3]]) -> Option<Facets> {
        if matches!(field, Field::Plane { .. }) || given.is_empty() {
            return None;
        }
        let tris: Vec<[V; 3]> = given
            .iter()
            .map(|t| t.map(V::from))
            .filter(|t| (t[1] - t[0]).cross(t[2] - t[0]).len() > 0.0)
            .collect();
        if tris.is_empty() {
            return None;
        }
        // The greatest depth: at the triangles' centroids and edges'
        // midpoints.
        let mut depth: f64 = 0.0;
        for t in &tris {
            for p in [
                (t[0] + t[1] + t[2]) * (1.0 / 3.0),
                (t[0] + t[1]) * 0.5,
                (t[1] + t[2]) * 0.5,
                (t[2] + t[0]) * 0.5,
            ] {
                depth = depth.max(field.f(p).abs());
            }
        }
        // Creases: edges of two triangles of the face that are not
        // coplanar. An edge of one triangle only is the edge of what was
        // given, not a crease.
        let key = |p: V| [p.x.to_bits(), p.y.to_bits(), p.z.to_bits()];
        let mut edges: std::collections::BTreeMap<([u64; 3], [u64; 3]), Vec<usize>> =
            std::collections::BTreeMap::new();
        for (i, t) in tris.iter().enumerate() {
            for k in 0..3 {
                let (a, b) = (key(t[k]), key(t[(k + 1) % 3]));
                let e = if a < b { (a, b) } else { (b, a) };
                edges.entry(e).or_default().push(i);
            }
        }
        let normal = |t: &[V; 3]| (t[1] - t[0]).cross(t[2] - t[0]).norm();
        let mut creases = Vec::new();
        for ((a, b), ts) in &edges {
            if ts.len() != 2 {
                continue;
            }
            let (n0, n1) = (normal(&tris[ts[0]]), normal(&tris[ts[1]]));
            if n0.cross(n1).len() <= 1e-9 {
                continue;
            }
            let p = v(
                f64::from_bits(a[0]),
                f64::from_bits(a[1]),
                f64::from_bits(a[2]),
            );
            let q = v(
                f64::from_bits(b[0]),
                f64::from_bits(b[1]),
                f64::from_bits(b[2]),
            );
            let m = (p + q) * 0.5;
            let plane = (q - p).cross(field.grad(m)).norm();
            if plane.len() > 0.5 {
                creases.push((p, q, plane));
            }
        }
        let grid = Grid::new(&tris);
        Some(Facets {
            field,
            tris,
            creases,
            band: (4.0 * depth).max(1e-12),
            grid,
        })
    }

    /// How far the faceted face lies along the exact face's inward normal
    /// from the point `foot` of the exact face (negative when outside it):
    /// 0 where no facet is within the band.
    fn depth(&self, foot: V) -> f64 {
        let n = self.field.grad(foot);
        let l = self.band;
        let ends = [foot - n * l, foot + n * l];
        let mut best: Option<f64> = None;
        for i in self.grid.near(&ends) {
            if let Some(s) = ray_triangle(foot, n * -1.0, &self.tris[i as usize])
                && s.abs() <= l
                && best.is_none_or(|b| s.abs() < b.abs())
            {
                best = Some(s);
            }
        }
        best.unwrap_or(0.0)
    }

    /// `x` moved along the face's normal by the facets' depth under its
    /// foot, in full on the face and fading to nothing a band away.
    fn warp(&self, x: V) -> V {
        let h = self.field.f(x);
        let w = 1.0 - h.abs() / self.band;
        if w <= 0.0 {
            return x;
        }
        let n = self.field.grad(x);
        let foot = x - n * h;
        x - n * (self.depth(foot) * w)
    }

    /// Where the curve `c` (on the face) crosses a crease between the
    /// parameters `ts` (sampled finely enough to see each crossing):
    /// (parameter, the point on the crease).
    fn crossings(&self, c: &dyn Fn(f64) -> V, ts: &[f64], out: &mut Vec<(f64, V, usize)>) {
        if self.creases.is_empty() {
            return;
        }
        // Samples no further apart than a quarter of the shortest crease.
        let short = self
            .creases
            .iter()
            .map(|cr| (cr.1 - cr.0).len())
            .fold(f64::INFINITY, f64::min);
        let step = 0.25 * short;
        let pad = self.band;
        let mut samples: Vec<(f64, V)> = Vec::new();
        for w in ts.windows(2) {
            let (t0, t1) = (w[0], w[1]);
            let (p0, p1) = (c(t0), c(t1));
            let k = (((p1 - p0).len() / step).ceil() as usize).clamp(1, 64);
            if samples.is_empty() {
                samples.push((t0, p0));
            }
            for j in 1..k {
                let t = t0 + (t1 - t0) * j as f64 / k as f64;
                samples.push((t, c(t)));
            }
            samples.push((t1, p1));
        }
        for w in samples.windows(2) {
            let ((ta, pa), (tb, pb)) = (w[0], w[1]);
            let (lo, hi) = bbox(&[pa, pb]);
            let (lo, hi) = (lo - v(pad, pad, pad), hi + v(pad, pad, pad));
            for (ci, &(p, q, nrm)) in self.creases.iter().enumerate() {
                // A crease beside the segment's box only.
                let (clo, chi) = bbox(&[p, q]);
                if clo.x > hi.x
                    || clo.y > hi.y
                    || clo.z > hi.z
                    || chi.x < lo.x
                    || chi.y < lo.y
                    || chi.z < lo.z
                {
                    continue;
                }
                let (da, db) = ((pa - p).dot(nrm), (pb - p).dot(nrm));
                if !((da < 0.0 && db >= 0.0) || (da > 0.0 && db <= 0.0)) {
                    continue;
                }
                // Bisection to the plane.
                let (mut lo_t, mut hi_t) = (ta, tb);
                let side = da > 0.0;
                for _ in 0..60 {
                    let m = 0.5 * (lo_t + hi_t);
                    if ((c(m) - p).dot(nrm) > 0.0) == side {
                        lo_t = m;
                    } else {
                        hi_t = m;
                    }
                }
                let t = 0.5 * (lo_t + hi_t);
                let x = c(t);
                // On the crease, where the face's normal through `x`
                // meets it; only within the crease.
                let e = q - p;
                let n = self.field.grad(x);
                // Closest points of the lines p + e s and x + n r.
                let (a, b, cc) = (e.dot(e), e.dot(n), n.dot(n));
                let w0 = p - x;
                let (d, ee) = (e.dot(w0), n.dot(w0));
                let den = a * cc - b * b;
                if den.abs() <= 1e-300 {
                    continue;
                }
                let s = (b * ee - cc * d) / den;
                if !(-1e-9..=1.0 + 1e-9).contains(&s) {
                    continue;
                }
                out.push((t, p + e * s.clamp(0.0, 1.0), ci));
            }
        }
        out.sort_by(|a, b| a.0.total_cmp(&b.0));
        // One vertex where the contact passes through a corner of the
        // facets (several creases at once).
        let near = 1e-9 * (ts[ts.len() - 1] - ts[0]).abs().max(1e-300);
        out.dedup_by(|a, b| (a.0 - b.0).abs() <= near);
    }

    /// The parameters where the polyline `pts` (parameter, point on the
    /// face) crosses a crease, roughly (linear between the points): what
    /// a split keeps clear of.
    fn rough_crossings(&self, pts: &[(f64, V)]) -> Vec<f64> {
        let mut out = Vec::new();
        for w in pts.windows(2) {
            let ((ta, pa), (tb, pb)) = (w[0], w[1]);
            for &(p, q, nrm) in &self.creases {
                let (da, db) = ((pa - p).dot(nrm), (pb - p).dot(nrm));
                if !((da < 0.0 && db >= 0.0) || (da > 0.0 && db <= 0.0)) {
                    continue;
                }
                let f = da / (da - db);
                let x = pa + (pb - pa) * f;
                let e = q - p;
                let s = (x - p).dot(e) / e.dot(e);
                if (-0.01..=1.01).contains(&s) && (x - (p + e * s)).len() <= 4.0 * self.band + 1e-9
                {
                    out.push(ta + (tb - ta) * f);
                }
            }
        }
        out
    }
}

/// A row of the tool: points with their parameters along the spine.
type Row = Vec<(f64, V)>;

/// The rows of the exact stations (the checks): per station, the ring
/// (the arc, `n` segments, then the region's other points, `sides` per
/// face).
fn rows_exact(
    r: &Rolled,
    stations: &[Station],
    n: usize,
    sides: [usize; 2],
) -> Result<Vec<Row>, BlendError> {
    let mut out: Vec<Row> = Vec::new();
    for (si, s) in stations.iter().enumerate() {
        let ring = r
            .ring_exact(s.c, if r.chamfer { 1 } else { n }, sides)
            .ok_or_else(|| r.too_large())?;
        if si == 0 {
            out = vec![Vec::with_capacity(stations.len()); ring.len()];
        }
        if ring.len() != out.len() {
            return Err(BlendError::Invalid(format!(
                "edge {}: its sections differ",
                r.index
            )));
        }
        for (j, p) in ring.into_iter().enumerate() {
            out[j].push((s.t, p));
        }
    }
    Ok(out)
}

/// `row` cut by the plane `(o, n)` at its start (`end` 0: what lies
/// before the first crossing into the plane's negative side is dropped)
/// or its end. `None` when it never crosses.
fn cut_row(row: &Row, o: V, n: V, end: usize) -> Option<Row> {
    let d = |p: V| (p - o).dot(n);
    let m = row.len();
    if end == 0 {
        // The last point outside before the row goes in.
        let i = (0..m.saturating_sub(1)).find(|&i| d(row[i].1) > 0.0 && d(row[i + 1].1) <= 0.0)?;
        let (a, b) = (row[i], row[i + 1]);
        let (da, db) = (d(a.1), d(b.1));
        let s = da / (da - db);
        let p = (a.0 + (b.0 - a.0) * s, a.1 + (b.1 - a.1) * s);
        let mut out = vec![p];
        out.extend(row[i + 1..].iter().copied().filter(|x| x.0 > p.0));
        Some(out)
    } else {
        let i = (0..m.saturating_sub(1))
            .rev()
            .find(|&i| d(row[i].1) <= 0.0 && d(row[i + 1].1) > 0.0)?;
        let (a, b) = (row[i], row[i + 1]);
        let (da, db) = (d(a.1), d(b.1));
        let s = da / (da - db);
        let p = (a.0 + (b.0 - a.0) * s, a.1 + (b.1 - a.1) * s);
        let mut out: Row = row[..=i].iter().copied().filter(|x| x.0 < p.0).collect();
        out.push(p);
        Some(out)
    }
}

/// The triangles joining two rows, in the order of their parameters:
/// (indices into the two rows) as triangles of row points, `true` for a
/// point of the first row.
fn zip(a: &Row, b: &Row) -> Vec<[(bool, usize); 3]> {
    let (mut i, mut j) = (0usize, 0usize);
    let mut out = Vec::with_capacity(a.len() + b.len());
    while i + 1 < a.len() || j + 1 < b.len() {
        let adv_a = if i + 1 >= a.len() {
            false
        } else if j + 1 >= b.len() {
            true
        } else {
            // Advance the row whose next point comes first; on a tie,
            // the shorter diagonal.
            let (na, nb) = (a[i + 1].0, b[j + 1].0);
            if na != nb {
                na < nb
            } else {
                (a[i + 1].1 - b[j].1).len() <= (b[j + 1].1 - a[i].1).len()
            }
        };
        if adv_a {
            out.push([(true, i), (true, i + 1), (false, j)]);
            i += 1;
        } else {
            out.push([(true, i), (false, j + 1), (false, j)]);
            j += 1;
        }
    }
    out
}

/// [`zip`], with each crease both rows cross joined by an edge between
/// their points on it: the rows are zipped piece by piece between those
/// pairs (taken in order, keeping each later than the last in both), so
/// no triangle spans a crease of the faceted face beside them, whose
/// ridge would stand through it.
fn zip_marked(
    a: &Row,
    b: &Row,
    ma: &[(f64, (usize, usize))],
    mb: &[(f64, (usize, usize))],
) -> Vec<[(bool, usize); 3]> {
    let index = |row: &Row, t: f64| row.iter().position(|x| x.0 == t);
    let mut pairs: Vec<(usize, usize)> = Vec::new();
    for &(ta, id) in ma {
        let Some(&(tb, _)) = mb.iter().find(|x| x.1 == id) else {
            continue;
        };
        let (Some(ia), Some(ib)) = (index(a, ta), index(b, tb)) else {
            continue;
        };
        if pairs.last().is_none_or(|&(pa, pb)| ia > pa && ib > pb) {
            pairs.push((ia, ib));
        }
    }
    if pairs.is_empty() {
        return zip(a, b);
    }
    let mut out = Vec::new();
    let mut from = (0usize, 0usize);
    pairs.push((a.len() - 1, b.len() - 1));
    for &(ia, ib) in &pairs {
        if ia < from.0 || ib < from.1 {
            continue;
        }
        let sa: Row = a[from.0..=ia].to_vec();
        let sb: Row = b[from.1..=ib].to_vec();
        for t in zip(&sa, &sb) {
            out.push(t.map(|(first, i)| {
                if first {
                    (true, i + from.0)
                } else {
                    (false, i + from.1)
                }
            }));
        }
        from = (ia, ib);
    }
    out
}

/// What the tool of a curve edge is: its mesh in `m`, and its blends'
/// surface entries (one per piece).
pub(super) struct Built {
    pub blends: Vec<u32>,
    pub fit: f64,
}

/// The arc's segments and the region's side points for edge `r`, from
/// the marched spine.
fn shape(r: &Rolled, marched: &[V], segments: &dyn Fn(f64) -> u32) -> (usize, [usize; 2], f64) {
    let mut sweep: f64 = 0.0;
    let mut sides = [0usize; 2];
    let step = (marched.len() / 16).max(1);
    for c in marched.iter().step_by(step) {
        if let Some(f) = r.feet(*c) {
            let (ma, mb) = ((f[0] - *c).norm(), (f[1] - *c).norm());
            sweep = sweep.max(atan2(ma.cross(mb).len(), ma.dot(mb)));
        }
        let s = r.sides_at(*c);
        sides = [sides[0].max(s[0]), sides[1].max(s[1])];
    }
    let n = if r.chamfer {
        1
    } else {
        segments(sweep.max(1e-3)).max(1) as usize
    };
    // The sagitta of the arc's chords: the along-spine chords may stray as
    // far.
    let ang = if r.chamfer {
        PI / 2.0 / segments(PI / 2.0).max(1) as f64
    } else {
        sweep.max(1e-3) / n as f64
    };
    let sag = r.size * (1.0 - cos(0.5 * ang));
    (n, sides, sag.max(1e-9 * r.scale))
}

/// The checks of a curve edge without building its tool: the spine
/// exists from end to end (or round), its arcs are under half a circle,
/// and the swept arc does not fold.
pub(super) fn check(spec: &BlendSpec, i: usize) -> Result<(), BlendError> {
    let r = Rolled::new(spec, i)?;
    let marched = r.marched()?;
    folds(&r, &marched)?;
    // The region's sections at the marched stations (the corner beside
    // the edge must be found), and at an open edge's ends the rows, cut
    // at both ends, must not cross.
    let stations: Vec<Station> = stations_exact(&r, &marched)?;
    let (n, sides, _) = shape(&r, &marched, &|_| 4);
    let rws = rows_exact(&r, &stations, n, sides)?;
    if !r.closed {
        cut_all(&r, &rws)?;
    }
    Ok(())
}

fn stations_exact(r: &Rolled, marched: &[V]) -> Result<Vec<Station>, BlendError> {
    let mut t = 0.0;
    let mut out = Vec::with_capacity(marched.len());
    for (k, &c) in marched.iter().enumerate() {
        if k > 0 {
            t += (c - marched[k - 1]).len();
        }
        r.feet(c).ok_or_else(|| r.too_large())?;
        out.push(Station { t, c });
    }
    Ok(out)
}

/// The swept arc must not fold: at each marched point, the ball's radius
/// times the spine's curvature towards each point of the arc stays under
/// one (with a margin), each arc is under a half circle, and the feet are
/// on the faces' real parts.
fn folds(r: &Rolled, marched: &[V]) -> Result<(), BlendError> {
    let n = marched.len();
    for k in 0..n {
        let c = marched[k];
        let feet = r.feet(c).ok_or_else(|| r.too_large())?;
        if r.chamfer {
            continue;
        }
        let (ma, mb) = ((feet[0] - c).norm(), (feet[1] - c).norm());
        if ma.dot(mb) <= -1.0 + 1e-6 {
            return Err(r.too_large());
        }
        // The curvature from the neighbours (a circle through three
        // marched points).
        let (prev, next) = if r.closed {
            (marched[(k + n - 1) % n], marched[(k + 1) % n])
        } else if k == 0 || k + 1 == n {
            continue;
        } else {
            (marched[k - 1], marched[k + 1])
        };
        let (u, w) = (c - prev, next - c);
        let (lu, lw) = (u.len(), w.len());
        if lu <= 0.0 || lw <= 0.0 {
            continue;
        }
        let chord = next - prev;
        let kappa = 2.0 * u.cross(w).len() / (lu * lw * chord.len());
        // The curvature vector: towards the centre of that circle, which
        // is on the side of the chord's midpoint away from `c`.
        let toward = ((prev + next) * 0.5 - c).reject(chord.norm()).norm();
        let kv = toward * kappa;
        let th = atan2(ma.cross(mb).len(), ma.dot(mb));
        let wv = (mb - ma * ma.dot(mb)).norm();
        for j in 0..=8 {
            let a = th * j as f64 / 8.0;
            let m = ma * cos(a) + wv * sin(a);
            if r.size * kv.dot(m) >= 0.95 {
                return Err(r.too_large());
            }
        }
    }
    Ok(())
}

/// Every row cut at both ends; `TooShort` when an end's cut comes after
/// the other's.
fn cut_all(r: &Rolled, rws: &[Row]) -> Result<Vec<Row>, BlendError> {
    let mut out = Vec::with_capacity(rws.len());
    for row in rws {
        let mut row = row.clone();
        for end in 0..2 {
            let Some((o, n, _)) = &r.cuts[end] else {
                continue;
            };
            row = cut_row(&row, *o, *n, end).ok_or(BlendError::TooShort(r.index))?;
        }
        if row.len() < 2 || row[0].0 >= row[row.len() - 1].0 {
            return Err(BlendError::TooShort(r.index));
        }
        out.push(row);
    }
    Ok(out)
}

/// The tool of curve edge `i` into `m`.
pub(super) fn tool_into(
    m: &mut Mesh,
    spec: &BlendSpec,
    i: usize,
    segments: &dyn Fn(f64) -> u32,
) -> Result<Built, BlendError> {
    let r = Rolled::new(spec, i)?;
    let marched = r.marched()?;
    folds(&r, &marched)?;
    let g = guide(&marched, r.closed)?;
    let bounds = if r.closed {
        splits(&r, &marched, g.range[1])
    } else {
        vec![g.range]
    };
    let (pieces, fit_error) = fit(&r, &g, &bounds)?;
    let (start, end) = (bounds[0][0], bounds[bounds.len() - 1][1]);
    let (n, sides, sag) = shape(&r, &marched, segments);
    let arc_rows = if r.chamfer { 2 } else { n + 1 };
    let mut ts = base_stations(&pieces, sag);
    // Crossings are looked for over every interval, a closed spine's last
    // one (back round to its start) included.
    let spans = ts.clone();
    if r.closed {
        ts.retain(|&t| t < end);
    }
    // The ring at any parameter, from the fitted curves: the blend's
    // points on the patch, then the region's other points.
    let inner = r.inner();
    let ring_at = |t: f64| -> Option<Vec<V>> {
        let p = piece_of(&pieces, t);
        let c = V::from(bspline::eval(&p.curves[0], t));
        let feet = [
            V::from(bspline::eval(&p.curves[1], t)),
            V::from(bspline::eval(&p.curves[2], t)),
        ];
        let mut ring: Vec<V> = (0..arc_rows)
            .map(|j| V::from(p.eval.eval(j as f64 / (arc_rows - 1) as f64, t)))
            .collect();
        ring.extend(r.rest(c, r.tangent(c).norm(), feet, sides, inner)?);
        Some(ring)
    };
    // Rows: (parameter, point, whether placed on each face's facets, and
    // the crease it is placed on).
    type Pt = (f64, V, [bool; 2], Option<(usize, usize)>);
    let mut table: Vec<Vec<Pt>> = Vec::new();
    for (si, &t) in ts.iter().enumerate() {
        let ring = ring_at(t).ok_or_else(|| r.too_large())?;
        if si == 0 {
            table = vec![Vec::with_capacity(ts.len()); ring.len()];
        }
        if ring.len() != table.len() {
            return Err(BlendError::Invalid(format!(
                "edge {}: its sections differ",
                r.index
            )));
        }
        for (j, x) in ring.into_iter().enumerate() {
            table[j].push((t, x, [false; 2], None));
        }
    }
    // Conformed to each faceted face: every row that comes near it gets
    // vertices where it crosses the facets' creases (the contact on the
    // crease itself, other rows over it), so that no chord of a row cuts
    // across a crease (a ridge of a faceted rod standing through a concave
    // blend between two of its vertices, or a convex blend's chord sinking
    // under a valley); then every vertex is moved by the facets' depth
    // under it. A station a sliver away from a row's crossing would be a
    // sliver of an edge in the result, which the reconstruction's exact
    // corners fold over: the row drops it (piece bounds stay).
    let near = 0.2 * median_gap(&ts);
    let bounds_at: Vec<f64> = bounds.iter().flat_map(|b| *b).collect();
    for k in 0..2 {
        let Some(fac) = &r.facets[k] else { continue };
        let contact_row = if k == 0 { 0 } else { arc_rows - 1 };
        for (j, row) in table.iter_mut().enumerate() {
            let within = row.iter().any(|x| r.fields[k].f(x.1).abs() < fac.band);
            if j != contact_row && !within {
                continue;
            }
            let mut cross: Vec<(f64, V, usize)> = Vec::new();
            if j == contact_row {
                let contact =
                    |t: f64| V::from(bspline::eval(&piece_of(&pieces, t).curves[1 + k], t));
                fac.crossings(&contact, &spans, &mut cross);
            } else {
                let curve =
                    |t: f64| ring_at(t).map_or(v(f64::NAN, f64::NAN, f64::NAN), |ring| ring[j]);
                fac.crossings(&curve, &spans, &mut cross);
                for c in &mut cross {
                    c.1 = fac.warp(curve(c.0));
                }
            }
            if r.closed {
                cross.retain(|c| c.0 < end);
            }
            if cross.is_empty() {
                continue;
            }
            let ct: Vec<f64> = cross.iter().map(|c| c.0).collect();
            let close = |t: f64| {
                let i = ct.partition_point(|&c| c < t);
                [i.checked_sub(1), Some(i)]
                    .into_iter()
                    .flatten()
                    .filter_map(|i| ct.get(i))
                    .any(|&c| (c - t).abs() < near)
            };
            row.retain(|x| bounds_at.contains(&x.0) || x.2.iter().any(|&p| p) || !close(x.0));
            for c in cross {
                if !row.iter().any(|x| x.0 == c.0) {
                    let mut placed = [false; 2];
                    placed[k] = true;
                    row.push((c.0, c.1, placed, Some((k, c.2))));
                }
            }
            row.sort_by(|a, b| a.0.total_cmp(&b.0));
        }
        for row in table.iter_mut() {
            for x in row.iter_mut() {
                if !x.2[k] {
                    x.1 = fac.warp(x.1);
                }
            }
        }
    }
    // Which crease each row's point is on, for zipping rows crease to
    // crease.
    let marks: Vec<Vec<(f64, (usize, usize))>> = table
        .iter()
        .map(|row| row.iter().filter_map(|x| x.3.map(|c| (x.0, c))).collect())
        .collect();
    let mut rws: Vec<Row> = table
        .into_iter()
        .map(|row| row.into_iter().map(|x| (x.0, x.1)).collect())
        .collect();
    if r.closed {
        // Round: every row ends where it started.
        for row in &mut rws {
            let first = row[0];
            if first.0 != start {
                return Err(BlendError::Invalid(format!(
                    "edge {}: a row misses the spine's start",
                    r.index
                )));
            }
            row.push((end, first.1));
        }
    } else {
        rws = cut_all(&r, &rws)?;
    }
    // The surfaces: the patches, the faces a concave tool's sides lie in,
    // the rest faceted (in the air or in the material, never kept).
    let blends: Vec<u32> = pieces
        .iter()
        .map(|p| m.surf(Surface::BSpline(p.patch.clone())))
        .collect();
    let faceted = m.surf(Surface::Faceted);
    let e = &spec.edges[i];
    let k = rws.len();
    let plane_side = |face: usize| -> Option<u32> {
        (r.mu[face] == 0.0 && !e.faces[face].curved()).then_some(face as u32)
    };
    let mut face_surf: [Option<u32>; 2] = [None, None];
    for face in 0..2 {
        if plane_side(face).is_some() {
            face_surf[face] = Some(m.surf(super::face_surface(&e.faces[face])));
        }
    }
    for j in 0..k {
        let j1 = (j + 1) % k;
        let surf = if j + 1 < arc_rows {
            None
        } else if j == arc_rows - 1 && r.mu[1] == 0.0 {
            // From foot b straight to the corner, along plane b.
            face_surf[1].or(Some(faceted))
        } else if j1 == 0 && r.mu[0] == 0.0 {
            face_surf[0].or(Some(faceted))
        } else {
            Some(faceted)
        };
        for tri in zip_marked(&rws[j], &rws[j1], &marks[j], &marks[j1]) {
            let pts = tri.map(|(first, idx)| if first { rws[j][idx] } else { rws[j1][idx] });
            let s = match surf {
                Some(s) => s,
                None => {
                    let lo = pts.iter().map(|x| x.0).fold(f64::INFINITY, f64::min);
                    let hi = pts.iter().map(|x| x.0).fold(f64::NEG_INFINITY, f64::max);
                    let mid = 0.5 * (lo + hi);
                    let pi = pieces
                        .iter()
                        .position(|p| mid <= p.range[1])
                        .unwrap_or(pieces.len() - 1);
                    blends[pi]
                }
            };
            let ids = pts.map(|x| m.vert(x.1));
            m.tri(ids, s);
        }
    }
    if !r.closed {
        for end in 0..2 {
            let Some((_, _, cap)) = &r.cuts[end] else {
                continue;
            };
            let pts: Vec<V> = rws
                .iter()
                .map(|row| {
                    if end == 0 {
                        row[0].1
                    } else {
                        row[row.len() - 1].1
                    }
                })
                .collect();
            let s = m.surf(cap.clone());
            cap_into(m, &pts, s)?;
        }
    }
    Ok(Built {
        blends,
        fit: fit_error,
    })
}

/// A cap polygon, oriented against the tool's triangles beside it.
fn cap_into(m: &mut Mesh, pts: &[V], s: u32) -> Result<(), BlendError> {
    let ids: Vec<u32> = pts.iter().map(|p| m.vert(*p)).collect();
    let n = ids.len();
    // The tube's triangles hold the cap's edges in one direction; the
    // cap must hold them in the other.
    let mut forward = 0i32;
    for k in 0..n {
        let (a, b) = (ids[k], ids[(k + 1) % n]);
        if a == b {
            continue;
        }
        for t in &m.m.triangles {
            for e in 0..3 {
                if t[e] == a && t[(e + 1) % 3] == b {
                    forward += 1;
                } else if t[e] == b && t[(e + 1) % 3] == a {
                    forward -= 1;
                }
            }
        }
    }
    let order: Vec<V> = if forward > 0 {
        pts.iter().rev().copied().collect()
    } else {
        pts.to_vec()
    };
    let mut nrm = V::default();
    for k in 0..order.len() {
        nrm = nrm + order[k].cross(order[(k + 1) % order.len()]);
    }
    m.polygon(&order, nrm.norm(), s)
}

/// Stations across the edge at fractions of its length: the edge's point
/// and direction there, and its blend's section (the checks).
pub(super) fn sections(
    spec: &BlendSpec,
    i: usize,
    fracs: &[f64],
) -> Result<Vec<(V, V, Section)>, BlendError> {
    let r = Rolled::new(spec, i)?;
    let p = &r.points;
    let mut cum = vec![0.0; p.len()];
    for k in 1..p.len() {
        cum[k] = cum[k - 1] + (p[k] - p[k - 1]).len();
    }
    let total = cum[p.len() - 1];
    let mut out = Vec::with_capacity(fracs.len());
    for &f in fracs {
        let s = f.clamp(0.0, 1.0) * total;
        let k = cum.partition_point(|&c| c < s).clamp(1, p.len() - 1);
        let (a, b) = (p[k - 1], p[k]);
        let seg = cum[k] - cum[k - 1];
        let x = if seg > 0.0 {
            (s - cum[k - 1]) / seg
        } else {
            0.0
        };
        let e = a + (b - a) * x;
        let d = (b - a).norm();
        let c = r.spine_at_edge(e, d).ok_or_else(|| r.too_large())?;
        let feet = r.feet(c).ok_or_else(|| r.too_large())?;
        let widths = [(feet[0] - e).len(), (feet[1] - e).len()];
        out.push((
            e,
            d,
            Section {
                center: (!r.chamfer).then_some(c.arr()),
                tangents: [feet[0].arr(), feet[1].arr()],
                widths,
            },
        ));
    }
    Ok(out)
}
