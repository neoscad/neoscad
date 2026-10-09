//! The cross-check that keeps a valid but wrong B-rep out of a STEP file
//! (the exact-geometry audit's gate 4).
//!
//! A B-rep can pass every structural check and still be the wrong solid:
//! in the audit's spike, the equal-radius tee (c14) read back valid while
//! its volume wandered by 0.5% with a fit tolerance. So the exact volume
//! is compared with an independent estimate made from the tagged mesh
//! alone, without the reconstructed edges or vertices.
//!
//! The mesh's own volume is no good for that: it is an inscribed polygon,
//! off by the fragments' sagitta (half a percent at OpenSCAD's defaults).
//! Each triangle on a curved surface is therefore corrected by the volume
//! between it and the surface beyond it, projected in the surface's own
//! coordinates (see [`cap`]) and integrated with a degree-5 quadrature
//! rule. For a triangle with its corners on the surface that is exact up
//! to the quadrature. What is left comes from the mesh's polygonal
//! intersection curves, whose corners stand off the exact curves by up to
//! the sagitta: a strip of second order. The bound kept with it scales
//! with the tessellation, so the tolerance is tight for a fine mesh and
//! loose enough for a coarse one, instead of one number that is wrong for
//! one of them.

use meshbrep::spline::Evaluator;
use meshbrep::{Surface, TaggedMesh};

/// The tagged mesh's volume corrected onto its exact surfaces, and the
/// bound on what the correction leaves.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Corrected {
    /// The mesh's own (signed) volume.
    pub mesh: f64,
    /// `mesh` plus the cap volumes.
    pub volume: f64,
    /// The second-order bound described in the module comment.
    pub residual_bound: f64,
    /// The largest cap height of a curved triangle: how far the export
    /// render's tessellation stands off its exact surfaces.
    pub max_cap: f64,
}

type V3 = [f64; 3];

fn sub(a: V3, b: V3) -> V3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}
fn dot(a: V3, b: V3) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
fn cross(a: V3, b: V3) -> V3 {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}
fn scaled(a: V3, k: f64) -> V3 {
    a.map(|x| x * k)
}
fn reject(a: V3, axis: V3) -> V3 {
    sub(a, scaled(axis, dot(a, axis)))
}

/// The integrand of the cap volume at `p` on a triangle with unit normal
/// `n`, and the distance of `p` from the surface. `None` for planes and
/// facets, which need no correction.
///
/// The cap is measured in the surface's own coordinates, not along the
/// triangle's normal: radially from a sphere's centre, perpendicular to
/// the axis of a cylinder or cone. That projection is a property of the
/// point, not of the triangle, so the caps of two triangles sharing an
/// edge meet exactly instead of overlapping, and the corrected volume is
/// exact for any triangle whose corners are on the surface.
///
/// - Sphere, `ρ = |p - c|`: a solid-angle element `dΩ = (n·ρ̂) dA / ρ²`
///   spans `(r³ - ρ³) / 3 · dΩ` between the triangle and the sphere.
/// - Cylinder and cone, `ρ` the distance from the axis and `R` the
///   surface's radius at `p`'s height: `dθ dz = (n·ρ̂) dA / ρ` spans
///   `(R² - ρ²) / 2 · dθ dz`.
/// - B-spline patch (a blend between curved faces, `ev` its evaluator):
///   along its normal from the closest point `q` (unit normal `m`, turned
///   to face as the triangle does, `p = q + h m`). In normal coordinates
///   the volume element is `(1 − 2Hs + Ks²) ds dA_q` (`H`, `K` the mean
///   and Gaussian curvatures for `m`), and the triangle's `dA` projects to
///   `dA_q = (n·m) dA / (1 − 2Hh + Kh²)`, so the cap is
///   `−(h − Hh² + Kh³/3) (n·m) / (1 − 2Hh + Kh²)` per unit of `dA`.
fn cap(s: &Surface, ev: Option<&Evaluator>, p: V3, n: V3) -> Option<(f64, f64)> {
    if let Some(ev) = ev {
        let [u, v] = ev.project(p);
        let d = ev.derivatives(u, v);
        let nn = cross(d.du, d.dv);
        let l = dot(nn, nn).sqrt();
        if l == 0.0 {
            return Some((0.0, 0.0));
        }
        let mut m = scaled(nn, 1.0 / l);
        if dot(m, n) < 0.0 {
            m = scaled(m, -1.0);
        }
        let h = dot(sub(p, d.point), m);
        let (e, f, g) = (dot(d.du, d.du), dot(d.du, d.dv), dot(d.dv, d.dv));
        let (ll, mm, nn2) = (dot(d.duu, m), dot(d.duv, m), dot(d.dvv, m));
        let det = e * g - f * f;
        if det <= 0.0 {
            return Some((0.0, h.abs()));
        }
        let k = (ll * nn2 - mm * mm) / det;
        let hm = (e * nn2 - 2.0 * f * mm + g * ll) / (2.0 * det);
        let jac = 1.0 - 2.0 * hm * h + k * h * h;
        let f = -(h - hm * h * h + k * h * h * h / 3.0) * dot(n, m) / jac;
        return Some((f, h.abs()));
    }
    match s {
        Surface::Sphere { center, radius } => {
            let d = sub(p, *center);
            let rho = dot(d, d).sqrt();
            if rho == 0.0 {
                return Some((0.0, *radius));
            }
            let r = *radius;
            let f = (r * r * r - rho * rho * rho) / (3.0 * rho * rho) * dot(n, d) / rho;
            Some((f, (r - rho).abs()))
        }
        Surface::Cylinder {
            origin,
            axis,
            radius,
        } => {
            let q = reject(sub(p, *origin), *axis);
            Some(radial(q, *radius, n))
        }
        Surface::Cone { apex, axis, slope } => {
            let d = sub(p, *apex);
            let q = reject(d, *axis);
            Some(radial(q, slope * dot(d, *axis), n))
        }
        Surface::Torus {
            center,
            axis,
            major_radius,
            minor_radius,
        } => {
            // Tube coordinates: s from the tube's centre circle, φ round
            // the tube; dV = s (R + s cos φ) ds dθ dφ, and the triangle's
            // dA projects to dθ dφ = (n·ŝ) dA / (ρ · ρ_axis).
            let (big, r) = (*major_radius, *minor_radius);
            let d = sub(p, *center);
            let w = reject(d, *axis);
            let rho_axis = dot(w, w).sqrt();
            if rho_axis == 0.0 {
                return Some((0.0, r));
            }
            let s = sub(w, scaled(w, big / rho_axis));
            let s = [0, 1, 2].map(|k| s[k] + axis[k] * dot(d, *axis));
            let rho = dot(s, s).sqrt();
            if rho == 0.0 {
                return Some((0.0, r));
            }
            let cos_phi = (rho_axis - big) / rho;
            let f = (big * (r * r - rho * rho) / 2.0
                + cos_phi * (r * r * r - rho * rho * rho) / 3.0)
                * dot(n, s)
                / (rho * rho * rho_axis);
            Some((f, (r - rho).abs()))
        }
        _ => None,
    }
}

/// The cylindrical-coordinates cap integrand for offset `q` from the axis
/// and surface radius `big_r` (see [`cap`]).
fn radial(q: V3, big_r: f64, n: V3) -> (f64, f64) {
    let rho = dot(q, q).sqrt();
    if rho == 0.0 {
        return (0.0, big_r.abs());
    }
    let f = (big_r * big_r - rho * rho) / (2.0 * rho) * dot(n, q) / rho;
    (f, (big_r - rho).abs())
}

/// Dunavant's degree-5 rule on a triangle: barycentric points and weights
/// summing to 1.
const RULE: [([f64; 3], f64); 7] = {
    const A1: f64 = 0.059_715_871_789_769_8;
    const B1: f64 = 0.470_142_064_105_115_1;
    const A2: f64 = 0.797_426_985_353_087_3;
    const B2: f64 = 0.101_286_507_323_456_3;
    const W0: f64 = 0.225;
    const W1: f64 = 0.132_394_152_788_506_2;
    const W2: f64 = 0.125_939_180_544_827_1;
    [
        ([1.0 / 3.0, 1.0 / 3.0, 1.0 / 3.0], W0),
        ([A1, B1, B1], W1),
        ([B1, A1, B1], W1),
        ([B1, B1, A1], W1),
        ([A2, B2, B2], W2),
        ([B2, A2, B2], W2),
        ([B2, B2, A2], W2),
    ]
};

/// The cap integral over a triangle (already times its area), an
/// estimate of the quadrature's error, and the largest cap height seen.
/// The error estimate is the difference from a degree-2 rule; where it is
/// more than `floor` times the triangle's area (a volume error of `floor`
/// per unit of surface, which summed is far below the check's tolerance),
/// the triangle is split in four, up to four times, so a coarse
/// tessellation (8 segments round a sphere puts 45° under one triangle)
/// is integrated as finely as a fine one.
fn integrate(
    s: &Surface,
    ev: Option<&Evaluator>,
    t: [V3; 3],
    n: V3,
    depth: u32,
    floor: f64,
) -> (f64, f64, f64) {
    let [a, b, c] = t;
    let area = dot(cross(sub(b, a), sub(c, a)), cross(sub(b, a), sub(c, a))).sqrt() / 2.0;
    let at = |w: [f64; 3]| [0, 1, 2].map(|k| w[0] * a[k] + w[1] * b[k] + w[2] * c[k]);
    let mut i5 = 0.0;
    let mut interior: f64 = 0.0;
    for (w, wt) in RULE {
        let Some((f, h)) = cap(s, ev, at(w), n) else {
            return (0.0, 0.0, 0.0);
        };
        i5 += wt * f;
        interior = interior.max(h);
    }
    let mut i2 = 0.0;
    for w in [
        [2.0 / 3.0, 1.0 / 6.0, 1.0 / 6.0],
        [1.0 / 6.0, 2.0 / 3.0, 1.0 / 6.0],
        [1.0 / 6.0, 1.0 / 6.0, 2.0 / 3.0],
    ] {
        i2 += cap(s, ev, at(w), n).map_or(0.0, |x| x.0) / 3.0;
    }
    let err = (i5 - i2).abs() * area;
    if depth < 4 && err > floor * area {
        let mid = |p: V3, q: V3| [0, 1, 2].map(|k| (p[k] + q[k]) / 2.0);
        let (ab, bc, ca) = (mid(a, b), mid(b, c), mid(c, a));
        let mut out = (0.0, 0.0, interior);
        for sub_t in [[a, ab, ca], [ab, b, bc], [ca, bc, c], [ab, bc, ca]] {
            let (v, e, h) = integrate(s, ev, sub_t, n, depth + 1, floor);
            out.0 += v;
            out.1 += e;
            out.2 = out.2.max(h);
        }
        return out;
    }
    // The degree-2 difference overstates the degree-5 rule's error by
    // orders of magnitude; it is kept as the bound all the same.
    (i5 * area, err, interior)
}

/// The closest point of a surface to `p` along the projection [`cap`]
/// uses (radial from a sphere's centre, perpendicular to an axis), and the
/// distance of `p` from the surface. A plane or facet projects nothing.
fn project(s: &Surface, ev: Option<&Evaluator>, p: V3) -> (V3, f64) {
    if let Some(ev) = ev {
        let [u, v] = ev.project(p);
        let q = ev.eval(u, v);
        return (q, dot(sub(p, q), sub(p, q)).sqrt());
    }
    let radial = |base: V3, q: V3, r: f64| {
        let rho = dot(q, q).sqrt();
        if rho == 0.0 {
            return (p, r.abs());
        }
        let k = r / rho;
        ([0, 1, 2].map(|i| base[i] + q[i] * k), (r - rho).abs())
    };
    match s {
        Surface::Sphere { center, radius } => radial(*center, sub(p, *center), *radius),
        Surface::Cylinder {
            origin,
            axis,
            radius,
        } => {
            let d = sub(p, *origin);
            let q = reject(d, *axis);
            radial(sub(p, q), q, *radius)
        }
        Surface::Cone { apex, axis, slope } => {
            let d = sub(p, *apex);
            let q = reject(d, *axis);
            radial(sub(p, q), q, slope * dot(d, *axis))
        }
        Surface::Torus {
            center,
            axis,
            major_radius,
            minor_radius,
        } => {
            let d = sub(p, *center);
            let w = reject(d, *axis);
            let l = dot(w, w).sqrt();
            if l == 0.0 {
                return (p, *minor_radius);
            }
            let tube = [0, 1, 2].map(|k| center[k] + w[k] * major_radius / l);
            radial(tube, sub(p, tube), *minor_radius)
        }
        Surface::Plane { origin, normal } => (p, dot(sub(p, *origin), *normal).abs()),
        _ => (p, 0.0),
    }
}

/// The smallest sine of the angle between two surfaces that the strip
/// bound divides by: tangent surfaces (where it is 0) meet along a curve
/// both project onto, so their gap terms vanish anyway.
const MIN_SIN: f64 = 0.02;

/// The unit normal of a surface at `p` (on or near it), up to sign. `None`
/// for a facet.
fn normal(s: &Surface, ev: Option<&Evaluator>, p: V3) -> Option<V3> {
    let unit = |v: V3| {
        let l = dot(v, v).sqrt();
        (l > 0.0).then(|| scaled(v, 1.0 / l))
    };
    if let Some(ev) = ev {
        let [u, v] = ev.project(p);
        return unit(ev.normal(u, v));
    }
    match s {
        Surface::Plane { normal, .. } => Some(*normal),
        Surface::Sphere { center, .. } => unit(sub(p, *center)),
        Surface::Cylinder { origin, axis, .. } => unit(reject(sub(p, *origin), *axis)),
        Surface::Cone { apex, axis, slope } => {
            let q = unit(reject(sub(p, *apex), *axis))?;
            unit(sub(q, scaled(*axis, *slope)))
        }
        Surface::Torus {
            center,
            axis,
            major_radius,
            ..
        } => {
            let w = reject(sub(p, *center), *axis);
            let l = dot(w, w).sqrt();
            if l == 0.0 {
                return None;
            }
            let tube = [0, 1, 2].map(|k| center[k] + w[k] * major_radius / l);
            unit(sub(p, tube))
        }
        _ => None,
    }
}

/// The part of the residual from intersection curves.
///
/// Where two surfaces meet, the mesh has a polygonal curve, and each side
/// projects it onto its own surface ([`cap`]). If one side's projection
/// lands on the other surface (a cylinder's rim on a plane square to its
/// axis), the caps meet and nothing is lost. Otherwise they leave a thin
/// wedge between them and the exact curve: about `½ d₁ e₁ + ½ d₂ e₂` in
/// section, where `eᵢ` is how far side `i` moved the point and `dᵢ` how
/// far that puts it off the other surface. It is measured at each edge's
/// ends and middle and taken along the edge's length.
fn strips(mesh: &TaggedMesh, evs: &[Option<Evaluator>]) -> f64 {
    // Ordered, so the sum (and the tolerance printed) is the same bits on
    // every run.
    let mut sides: std::collections::BTreeMap<(u32, u32), [(u32, usize); 2]> = Default::default();
    for (t, tri) in mesh.triangles.iter().enumerate() {
        let s = (mesh.triangle_surface[t], t);
        for k in 0..3 {
            let (i, j) = (tri[k], tri[(k + 1) % 3]);
            let key = (i.min(j), i.max(j));
            sides.entry(key).and_modify(|e| e[1] = s).or_insert([s, s]);
        }
    }
    let curved = |s: &Surface| !matches!(s, Surface::Plane { .. } | Surface::Faceted);
    // A faceted side is the plane of its own triangle.
    let surface = |(s, t): (u32, usize)| match &mesh.surfaces[s as usize] {
        Surface::BSpline(_) => (Surface::Faceted, evs[s as usize].as_ref()),
        Surface::Faceted => (
            {
                let [a, b, c] = mesh.triangles[t].map(|i| mesh.positions[i as usize]);
                let n = cross(sub(b, a), sub(c, a));
                let l = dot(n, n).sqrt();
                if l == 0.0 {
                    Surface::Faceted
                } else {
                    Surface::Plane {
                        origin: a,
                        normal: scaled(n, 1.0 / l),
                    }
                }
            },
            None,
        ),
        other => (other.clone(), None),
    };
    let mut total = 0.0;
    for (&(i, j), &[s1, s2]) in &sides {
        if s1.0 == s2.0 {
            continue;
        }
        let ((a, ea_), (b, eb_)) = (surface(s1), surface(s2));
        if !curved(&a) && ea_.is_none() && !curved(&b) && eb_.is_none() {
            continue;
        }
        let (p, q) = (mesh.positions[i as usize], mesh.positions[j as usize]);
        let length = dot(sub(q, p), sub(q, p)).sqrt();
        let mid = [0, 1, 2].map(|k| (p[k] + q[k]) / 2.0);
        let mut worst: f64 = 0.0;
        for m in [p, mid, q] {
            let (pa, ea) = project(&a, ea_, m);
            let (pb, eb) = project(&b, eb_, m);
            let da = project(&b, eb_, pa).1;
            let db = project(&a, ea_, pb).1;
            // Where the surfaces meet at a grazing angle θ, the exact
            // curve lies up to the gap / sin θ along them from the mesh's,
            // not the gap: a cylinder poking 0.01 mm through a plane (the
            // overlap BOSL2 gives every mask) meets it at 2.6° and left a
            // sliver ten times the bound without this.
            let sin = match (normal(&a, ea_, pa), normal(&b, eb_, pb)) {
                (Some(na), Some(nb)) => {
                    let c = cross(na, nb);
                    dot(c, c).sqrt()
                }
                _ => 1.0,
            };
            worst = worst.max(0.5 * (da * ea + db * eb) / sin.max(MIN_SIN));
        }
        total += length * worst;
    }
    total
}

/// An evaluator for each B-spline surface of the mesh's table that a
/// triangle uses (blends between curved faces), made once: projecting
/// onto one keeps a grid of seeds.
fn evaluators(mesh: &TaggedMesh) -> Vec<Option<Evaluator>> {
    let mut used = vec![false; mesh.surfaces.len()];
    for &s in &mesh.triangle_surface {
        if let Some(u) = used.get_mut(s as usize) {
            *u = true;
        }
    }
    mesh.surfaces
        .iter()
        .zip(&used)
        .map(|(s, &u)| match s {
            Surface::BSpline(b) if u => Evaluator::new(b).ok(),
            _ => None,
        })
        .collect()
}

/// The corrected volume of a tagged mesh (see the module comment).
pub fn corrected_volume(mesh: &TaggedMesh) -> Corrected {
    let mut out = Corrected {
        mesh: 0.0,
        volume: 0.0,
        residual_bound: 0.0,
        max_cap: 0.0,
    };
    let mut correction = 0.0;
    let evs = evaluators(mesh);
    // 1e-8 of the model's size, as a height: see [`integrate`]. The
    // degree-2 difference it is compared with overstates the degree-5
    // rule's error by orders of magnitude, so the integral is far more
    // accurate than this, and the cross-check's floor is 1e-7 anyway.
    let floor = {
        let (mut lo, mut hi) = ([f64::INFINITY; 3], [f64::NEG_INFINITY; 3]);
        for p in &mesh.positions {
            for k in 0..3 {
                lo[k] = lo[k].min(p[k]);
                hi[k] = hi[k].max(p[k]);
            }
        }
        1e-8 * (0..3).map(|k| hi[k] - lo[k]).fold(0.0, f64::max)
    };
    for (t, tri) in mesh.triangles.iter().enumerate() {
        let [a, b, c] = tri.map(|i| mesh.positions[i as usize]);
        out.mesh += dot(a, cross(b, c)) / 6.0;
        let s = &mesh.surfaces[mesh.triangle_surface[t] as usize];
        if matches!(s, Surface::Plane { .. } | Surface::Faceted) {
            continue;
        }
        let nn = cross(sub(b, a), sub(c, a));
        let len = dot(nn, nn).sqrt();
        if len == 0.0 {
            continue;
        }
        let n = scaled(nn, 1.0 / len);
        let ev = evs[mesh.triangle_surface[t] as usize].as_ref();
        let (integral, quad_err, interior) = integrate(s, ev, [a, b, c], n, 0, floor);
        correction += integral;
        out.residual_bound += quad_err;
        out.max_cap = out.max_cap.max(interior);
        for p in [a, b, c] {
            if let Some((_, h)) = cap(s, ev, p, n) {
                out.max_cap = out.max_cap.max(h);
            }
        }
    }
    out.residual_bound += strips(mesh, &evs);
    out.volume = out.mesh + correction;
    out
}

/// The volume between `triangles` of `mesh` and the curved surfaces they
/// are tagged with (a partial faceted fallback writes them as planar
/// facets, `super::partial`).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Caps {
    /// The caps' signed sum: what [`corrected_volume`] adds for those
    /// triangles, so a B-rep with them as facets plus this is the exact
    /// model's volume.
    pub signed: f64,
    /// Each cap counted positive: how far the facets stand off the
    /// exact model in volume.
    pub size: f64,
    /// The quadrature's error bound on `signed`.
    pub error: f64,
}

/// The caps of `triangles` of `mesh` ([`Caps`]).
pub fn cap_volume(mesh: &TaggedMesh, triangles: &[u32]) -> Caps {
    let floor = 1e-8 * {
        let (mut lo, mut hi) = ([f64::INFINITY; 3], [f64::NEG_INFINITY; 3]);
        for p in &mesh.positions {
            for k in 0..3 {
                lo[k] = lo[k].min(p[k]);
                hi[k] = hi[k].max(p[k]);
            }
        }
        (0..3).map(|k| hi[k] - lo[k]).fold(0.0, f64::max)
    };
    let mut out = Caps::default();
    let evs = evaluators(mesh);
    for &t in triangles {
        let [a, b, c] = mesh.triangles[t as usize].map(|i| mesh.positions[i as usize]);
        let s = &mesh.surfaces[mesh.triangle_surface[t as usize] as usize];
        if matches!(s, Surface::Plane { .. } | Surface::Faceted) {
            continue;
        }
        let nn = cross(sub(b, a), sub(c, a));
        let len = dot(nn, nn).sqrt();
        if len == 0.0 {
            continue;
        }
        let ev = evs[mesh.triangle_surface[t as usize] as usize].as_ref();
        let (integral, quad_err, _) = integrate(s, ev, [a, b, c], scaled(nn, 1.0 / len), 0, floor);
        out.signed += integral;
        out.size += integral.abs();
        out.error += quad_err;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use meshbrep::primitives::{self, Transform};

    fn rel(a: f64, b: f64) -> f64 {
        (a - b).abs() / b.abs()
    }

    #[test]
    fn caps_restore_the_exact_volume_of_primitives() {
        use std::f64::consts::PI;
        for n in [8, 16, 32, 64] {
            let s = corrected_volume(&primitives::sphere(5.0, n, &Transform::IDENTITY));
            let exact = 4.0 / 3.0 * PI * 125.0;
            assert!(rel(s.mesh, exact) > 1e-3, "{n}: {s:?}");
            assert!(
                (s.volume - exact).abs() <= s.residual_bound,
                "sphere {n}: {} vs {exact}, bound {}",
                s.volume,
                s.residual_bound
            );
            let c = corrected_volume(&primitives::frustum(
                10.0,
                3.0,
                3.0,
                n,
                &Transform::IDENTITY,
            ));
            let exact = PI * 90.0;
            assert!(
                (c.volume - exact).abs() <= c.residual_bound.max(1e-9 * exact),
                "cyl {n}: {c:?}"
            );
            let k = corrected_volume(&primitives::frustum(
                10.0,
                4.0,
                1.0,
                n,
                &Transform::IDENTITY,
            ));
            let exact = PI * 10.0 / 3.0 * (16.0 + 4.0 + 1.0);
            assert!(
                (k.volume - exact).abs() <= k.residual_bound.max(1e-9 * exact),
                "cone {n}: {k:?}"
            );
        }
    }
}

/// The residual bound against a closed form, through Manifold's booleans:
/// a cube with a sphere cut out (the cap strips are at their worst on a
/// small solid with long intersection curves) and the two intersected, at
/// several tessellations. The export's tolerance is 4 times the bound, so
/// the bound must hold to within that.
#[cfg(test)]
mod against_closed_forms {
    use super::*;
    use manifold_rust::manifold::Manifold;
    use manifold_rust::types::{MeshGL64, OpType};
    use meshbrep::primitives::{self, Transform};

    fn man(t: &TaggedMesh, table: &mut Vec<Surface>) -> Manifold {
        let off = table.len() as u64;
        table.extend(t.surfaces.iter().cloned());
        Manifold::from_mesh_gl64(&MeshGL64 {
            num_prop: 3,
            vert_properties: t.positions.iter().flatten().copied().collect(),
            tri_verts: t.triangles.iter().flatten().map(|&i| i as u64).collect(),
            face_id: t.triangle_surface.iter().map(|&s| s as u64 + off).collect(),
            run_index: vec![0, 3 * t.triangles.len() as u64],
            run_original_id: vec![Manifold::reserve_ids(1)],
            ..Default::default()
        })
    }

    #[test]
    fn the_residual_bound_holds_on_the_cube_and_sphere() {
        for n in [16u32, 32, 64, 128] {
            let mut table = Vec::new();
            let a = man(
                &primitives::cuboid([15.0; 3], &Transform::translate([-7.5; 3])),
                &mut table,
            );
            let b = man(
                &primitives::sphere(10.0, n, &Transform::IDENTITY),
                &mut table,
            );
            for (label, op) in [("D", OpType::Subtract), ("I", OpType::Intersect)] {
                let m = a.boolean(&b, op);
                let gl = m.get_mesh_gl64(-1);
                let mesh = TaggedMesh {
                    positions: gl
                        .vert_properties
                        .chunks(3)
                        .map(|c| [c[0], c[1], c[2]])
                        .collect(),
                    triangles: gl
                        .tri_verts
                        .chunks(3)
                        .map(|c| [c[0] as u32, c[1] as u32, c[2] as u32])
                        .collect(),
                    triangle_surface: gl.face_id.iter().map(|&f| f as u32).collect(),
                    surfaces: table.clone(),
                };
                let c = corrected_volume(&mesh);
                let (r, h) = (10.0f64, 2.5f64);
                let sic = 4.0 / 3.0 * std::f64::consts::PI * r * r * r
                    - 6.0 * std::f64::consts::PI * h * h * (3.0 * r - h) / 3.0;
                let exact = if label == "D" { 3375.0 - sic } else { sic };
                // The correction does nearly all the work...
                assert!(
                    (c.volume - exact).abs() < 0.2 * (c.mesh - exact).abs(),
                    "{label} {n}: {c:?}"
                );
                // ...and what it leaves is within the tolerance.
                assert!(
                    (c.volume - exact).abs() <= 4.0 * c.residual_bound,
                    "{label} {n}: off by {:.3e}, bound {:.3e}",
                    c.volume - exact,
                    c.residual_bound
                );
            }
        }
    }
}
