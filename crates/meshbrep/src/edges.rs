//! The exact curve of an edge between two surfaces, from the mesh chain
//! that approximates it.
//!
//! Closed forms where the pair has one (lines, circles, ellipses);
//! otherwise a cubic B-spline through points projected onto both
//! surfaces, refined until it lies within the fit tolerance of both.

use crate::bspline;
use crate::curve;
use crate::math::*;
use crate::model::{BSpline, Curve};
use crate::solve::solve;
use crate::surf::Surf;
use crate::tangency::Cont;
use std::sync::Arc;

#[derive(Debug)]
pub(crate) struct Built {
    pub curve: Curve,
    pub range: [f64; 2],
    /// Largest distance of curve samples from either surface.
    pub dev: f64,
}

/// The point on `a ∩ b` nearest `p`.
fn on_curve(a: &Surf, b: &Surf, p: V) -> V {
    solve(&[a.clone(), b.clone()], p).0
}

fn par(a: V, b: V) -> bool {
    a.dot(b).abs() > 1.0 - 1e-12
}

/// Where the axis line (o, a) meets the plane (po, n).
fn axis_hit(o: V, a: V, po: V, n: V) -> V {
    o + a * ((po - o).dot(n) / a.dot(n))
}

/// An unoriented closed-form curve: a circle (centre, normal, radius), an
/// ellipse or a line direction.
enum Exact {
    Line(V),
    Circle(V, V, f64),
    Ellipse { c: V, n: V, x: V, a: f64, b: f64 },
}

fn closed_form(s: &Surf, t: &Surf, q: V) -> Option<Exact> {
    use Surf::*;
    let circ = |o: V, d: V| {
        let c = o + d * (q - o).dot(d);
        Exact::Circle(c, d, (q - c).len())
    };
    match (s.clone(), t.clone()) {
        (Plane { n: n1, .. }, Plane { n: n2, .. }) => Some(Exact::Line(n1.cross(n2).norm())),
        (Plane { o: po, n }, Cyl { o, a, r }) => {
            let c = n.dot(a);
            if c.abs() > 1.0 - 1e-12 {
                Some(Exact::Circle(axis_hit(o, a, po, n), n, r))
            } else if c.abs() < 1e-12 {
                Some(Exact::Line(a))
            } else {
                let cen = axis_hit(o, a, po, n);
                let minor = a.cross(n).norm();
                let major = n.cross(minor).norm();
                Some(Exact::Ellipse {
                    c: cen,
                    n,
                    x: major,
                    a: r / c.abs(),
                    b: r,
                })
            }
        }
        (Plane { o: po, n }, Cone { apex, a, k }) if par(n, a) => {
            let c = axis_hit(apex, a, po, n);
            Some(Exact::Circle(c, n, k * (c - apex).dot(a)))
        }
        (Plane { o: po, n }, Sphere { c, r }) => {
            let cc = c - n * (c - po).dot(n);
            let rr = (r * r - (c - cc).dot(c - cc)).max(0.0).sqrt();
            Some(Exact::Circle(cc, n, rr))
        }
        (Sphere { c: c1, r: r1 }, Sphere { c: c2, r: r2 }) => {
            let d = (c2 - c1).len();
            let u = (c2 - c1) * (1.0 / d);
            let x = (d * d + r1 * r1 - r2 * r2) / (2.0 * d);
            Some(Exact::Circle(
                c1 + u * x,
                u,
                (r1 * r1 - x * x).max(0.0).sqrt(),
            ))
        }
        (Plane { o: po, n }, Torus { c, a, big, r }) => {
            if par(n, a) {
                // A parallel (a plane across the axis cuts two at most:
                // the one through q).
                Some(circ(c, a))
            } else if n.dot(a).abs() < 1e-12 && (c - po).dot(n).abs() < 1e-9 * (1.0 + c.len()) {
                // A plane through the axis cuts two meridians: q's.
                let d = (q - c).reject(a).norm();
                Some(Exact::Circle(c + d * big, n, r))
            } else {
                None
            }
        }
        (Sphere { c: cs, .. }, Torus { c, a, .. })
            if (cs - c).reject(a).len() < 1e-9 * (1.0 + c.len()) =>
        {
            Some(circ(c, a))
        }
        _ => match (s.axis(), t.axis(), t.clone()) {
            // Coaxial surfaces of revolution meet in circles, parallel
            // cylinders in lines.
            (Some((o1, d1)), Some((o2, d2)), _) if par(d1, d2) => {
                if (o2 - o1).reject(d1).len() < 1e-9 * (1.0 + o1.len()) {
                    Some(circ(o1, d1))
                } else if matches!((s, t), (Cyl { .. }, Cyl { .. })) {
                    Some(Exact::Line(d1))
                } else {
                    None
                }
            }
            (Some((o1, d1)), None, Sphere { c, .. }) => {
                ((c - o1).reject(d1).len() < 1e-9 * (1.0 + o1.len())).then(|| circ(o1, d1))
            }
            _ => None,
        },
    }
}

/// The unwrapped angle swept by `chain` about the conic (c, n, x, a, b),
/// starting from the angle of its first point.
fn swept(c: V, n: V, x: V, a: f64, b: f64, chain: &[V]) -> f64 {
    let y = n.cross(x);
    let ang = |p: V| {
        let d = p - c;
        atan2(d.dot(y) / b, d.dot(x) / a)
    };
    let mut total = 0.0;
    let mut prev = ang(chain[0]);
    for &p in &chain[1..] {
        let cur = ang(p);
        let mut d = cur - prev;
        while d > PI {
            d -= TAU;
        }
        while d < -PI {
            d += TAU;
        }
        total += d;
        prev = cur;
    }
    total
}

/// Orients a closed-form curve along the chain from `p0` to `p1` and
/// computes its parameter range.
fn orient(e: Exact, p0: V, p1: V, chain: &[V], closed: bool) -> Option<(Curve, [f64; 2])> {
    match e {
        Exact::Line(d) => {
            if closed {
                return None;
            }
            let l = (p1 - p0).len();
            let dir = if l > 0.0 {
                (p1 - p0) * (1.0 / l)
            } else if (p1 - p0).dot(d) < 0.0 {
                -d
            } else {
                d
            };
            Some((
                Curve::Line {
                    origin: p0.arr(),
                    direction: dir.arr(),
                },
                [0.0, l],
            ))
        }
        Exact::Circle(c, n, r) => {
            let x = (p0 - c).reject(n).norm();
            let x = if x.len() == 0.0 { n.perp() } else { x };
            let sw = swept(c, n, x, 1.0, 1.0, chain);
            let n = if sw < 0.0 { -n } else { n };
            let cv = Curve::Circle {
                center: c.arr(),
                normal: n.arr(),
                x_axis: x.arr(),
                radius: r,
            };
            let t1 = if closed {
                TAU
            } else {
                let a = curve::param_of(&cv, p1);
                nearest_turn(a, sw.abs())
            };
            Some((cv, [0.0, t1]))
        }
        Exact::Ellipse { c, n, x, a, b } => {
            let sw = swept(c, n, x, a, b, chain);
            let n = if sw < 0.0 { -n } else { n };
            let cv = Curve::Ellipse {
                center: c.arr(),
                normal: n.arr(),
                x_axis: x.arr(),
                major: a,
                minor: b,
            };
            let t0 = curve::param_of(&cv, p0);
            let t1 = if closed {
                t0 + TAU
            } else {
                t0 + nearest_turn(curve::param_of(&cv, p1) - t0, sw.abs())
            };
            Some((cv, [t0, t1]))
        }
    }
}

/// `a + 2πk` closest to `target` (both angles; `target` positive).
fn nearest_turn(a: f64, target: f64) -> f64 {
    let k = ((target - a) / TAU).round();
    let t = a + k * TAU;
    if t <= 0.0 { t + TAU } else { t }
}

/// Builds the edge between surfaces `a` and `b` along the mesh chain from
/// vertex `p0` to `p1` (equal when `closed`).
#[allow(clippy::too_many_arguments)]
pub(crate) fn make_edge(
    a: &Surf,
    b: &Surf,
    contact: Option<Cont>,
    p0: V,
    p1: V,
    chain: &[V],
    closed: bool,
    fit_tol: f64,
) -> Built {
    let (s, t) = if a.rank() <= b.rank() { (a, b) } else { (b, a) };
    if let Some(c) = &contact
        && let Some(b) = boundary_edge(c, s, t, p0, p1, chain, closed, fit_tol)
    {
        return b;
    }
    // Two planes meet in a line whatever the point; the solve is for the
    // other pairs' circles and ellipses.
    let q = if s.is_plane() && t.is_plane() {
        chain[chain.len() / 2]
    } else {
        on_curve(s, t, chain[chain.len() / 2])
    };
    // The chain with its ends at the exact vertices: the mesh's own end
    // points can sit further from the vertices than a short arc is long,
    // and then they would give the arc the wrong direction.
    let mut ch = chain.to_vec();
    ch[0] = p0;
    let last = ch.len() - 1;
    ch[last] = p1;
    let chain = &ch[..];
    let exact = match contact {
        Some(Cont::Line { d, .. }) => Some(Exact::Line(d)),
        Some(Cont::Circle { c, n, r }) => Some(Exact::Circle(c, n, r)),
        _ => closed_form(s, t, q),
    };
    if let Some((curve, range)) = exact.and_then(|e| orient(e, p0, p1, chain, closed)) {
        let dev = deviation(&curve, range, s, t);
        return Built { curve, range, dev };
    }
    // Two quadrics meeting in a planar curve (equal cylinders with crossing
    // axes): the curve is that plane's section of the cylinder, an
    // ellipse.
    if !closed
        && !s.is_plane()
        && let Some(cyl) = [s.clone(), t.clone()]
            .into_iter()
            .find(|x| matches!(x, Surf::Cyl { .. }))
    {
        // Interior points only: near a crossing, projection can land
        // on the other branch.
        let k = chain.len() / 5;
        let qs: Vec<V> = chain[k..chain.len() - k]
            .iter()
            .map(|&p| on_curve(s, t, p))
            .collect();
        let chord = (p1 - p0).norm();
        let far = qs.iter().copied().max_by(|x, y| {
            let d = |z: V| (z - p0).cross(chord).len();
            d(*x).total_cmp(&d(*y))
        });
        if let Some(pm) = far {
            let n = (pm - p0).cross(p1 - p0).norm();
            let flat = qs.iter().all(|&q| (q - p0).dot(n).abs() < 1e-9);
            if flat && n.len() > 0.5 {
                let pl = Surf::Plane { o: p0, n };
                if let Some((curve, range)) =
                    closed_form(&pl, &cyl, q).and_then(|e| orient(e, p0, p1, chain, closed))
                    && !matches!(curve, Curve::Line { .. })
                {
                    let dev = deviation(&curve, range, s, t);
                    return Built { curve, range, dev };
                }
            }
        }
    }
    // Densify until the curve is within the fit tolerance of both. The
    // number of points depends on the curve, not on the mesh: a cubic
    // interpolant's error falls with the fourth power of the spacing.
    let mut n = 8;
    loop {
        let curve = fit(s, t, chain, n);
        let range = [0.0, 1.0];
        let dev = deviation(&curve, range, s, t);
        if dev < fit_tol || n >= 4096 {
            return Built { curve, range, dev };
        }
        n *= 2;
    }
}

/// The edge along a side of a B-spline patch that touches the other
/// surface (a boundary contact): that side between the vertices, which
/// were solved onto it. A polynomial side is its row of control points,
/// exactly; a rational one (a blend's arc at its end) is interpolated at
/// evenly spaced parameters of the side until within `fit_tol` of it, so
/// that the edge's parameter is the side's (shifted to start at 0) and
/// its parameter-space curve on the patch is a segment. `None` when the
/// chain does not run along the side (the surfaces also meet elsewhere),
/// for a closed chain, or when the vertices are one point of it.
#[allow(clippy::too_many_arguments)]
fn boundary_edge(
    c: &Cont,
    a: &Surf,
    b: &Surf,
    p0: V,
    p1: V,
    chain: &[V],
    closed: bool,
    fit_tol: f64,
) -> Option<Built> {
    if closed {
        return None;
    }
    let mid = chain[chain.len() / 2];
    let (s, sd) = c.nearest_side(mid)?;
    // The chain must follow the side, not another intersection of the two
    // surfaces, which lies a distance of the order of the model away. Its
    // mesh vertices were put on the contact, but a tessellation leaves
    // them only near it: a vertex conformed to the other face's polygon
    // lies on the polygon's facet, inside the exact face by up to the
    // polygon's sagitta (measured once moved back onto that face along
    // its normal, which undoes that); a kernel's boolean adds vertices on
    // the chords between them, and where the contact runs along a ridge
    // of the polygon and bulges across it, the chain follows the ridge.
    // Since the two surfaces are tangent along the whole side, any chain
    // between them near it can only be the contact: the chain is
    // accepted while it stays within a fifth of the patch's width across
    // the side (the distance to its opposite side), or 1e-4 of its own
    // length.
    let other = match a {
        Surf::Spline(x) if Arc::ptr_eq(x, s) => b,
        _ => a,
    };
    let onto = |q: V| {
        let mut x = q;
        for _ in 0..3 {
            let g = other.grad(x);
            if !g.is_finite() || g.len() == 0.0 {
                return q;
            }
            x = x - g.norm() * other.f(x);
        }
        if x.is_finite() { x } else { q }
    };
    let length: f64 = chain.windows(2).map(|w| (w[1] - w[0]).len()).sum();
    let param_on = |q: V| {
        let (u, v) = s.project(q, false);
        if sd.fixed_u { v } else { u }
    };
    let [olo, ohi] = s.iso_range(!sd.fixed_u);
    let opposite = if (sd.at - olo).abs() <= (sd.at - ohi).abs() {
        ohi
    } else {
        olo
    };
    for &q in &chain[1..chain.len() - 1] {
        let q = onto(q);
        let t = param_on(q);
        let on = s.iso(sd.fixed_u, sd.at, t).0;
        let width = (s.iso(sd.fixed_u, opposite, t).0 - on).len();
        let near = (1e-4 * length).max(0.2 * width);
        if (on - q).len() > near {
            return None;
        }
    }
    let (t0, t1) = (param_on(p0), param_on(p1));
    let span = (t1 - t0).abs();
    let [lo, hi] = s.iso_range(sd.fixed_u);
    if span <= 1e-12 * (hi - lo) || span.is_nan() {
        return None;
    }
    let (curve, range) = match s.boundary_curve(sd.fixed_u, sd.at) {
        Some(bs) if t0 < t1 => (Curve::BSpline(bs), [t0, t1]),
        Some(bs) => {
            // Reversed, so that it runs from `p0` to `p1` with an
            // increasing parameter: t ↦ lo + hi - t.
            let rev = BSpline {
                degree: bs.degree,
                control: bs.control.iter().rev().copied().collect(),
                knots: bs.knots.iter().rev().map(|&k| lo + hi - k).collect(),
            };
            (Curve::BSpline(rev), [lo + hi - t0, lo + hi - t1])
        }
        None => {
            let at = |tau: f64| s.iso(sd.fixed_u, sd.at, t0 + (t1 - t0) * (tau / span)).0;
            let mut n = 8;
            loop {
                let taus: Vec<f64> = (0..=n).map(|k| span * k as f64 / n as f64).collect();
                let mut pts: Vec<[f64; 3]> = taus.iter().map(|&tau| at(tau).arr()).collect();
                pts[0] = p0.arr();
                pts[n] = p1.arr();
                let bs = bspline::interpolate_at(&pts, &taus);
                let err = (0..n)
                    .map(|k| {
                        let tau = 0.5 * (taus[k] + taus[k + 1]);
                        (V::from(bspline::eval(&bs, tau)) - at(tau)).len()
                    })
                    .fold(0.0, f64::max);
                if err < 0.5 * fit_tol || n >= 4096 {
                    break (Curve::BSpline(bs), [0.0, span]);
                }
                n *= 2;
            }
        }
    };
    let dev = deviation(&curve, range, a, b);
    Some(Built { curve, range, dev })
}

/// A cubic through `n + 1` points spaced evenly in length along the chain
/// (whose ends are the exact vertices), the interior ones projected onto
/// `a ∩ b`.
fn fit(a: &Surf, b: &Surf, chain: &[V], n: usize) -> Curve {
    let mut cum = vec![0.0; chain.len()];
    for i in 1..chain.len() {
        cum[i] = cum[i - 1] + (chain[i] - chain[i - 1]).len();
    }
    let total = cum[chain.len() - 1];
    let mut pts = Vec::with_capacity(n + 1);
    pts.push(chain[0].arr());
    let mut seg = 0;
    for k in 1..n {
        let target = total * k as f64 / n as f64;
        while seg + 2 < chain.len() && cum[seg + 1] < target {
            seg += 1;
        }
        let len = cum[seg + 1] - cum[seg];
        let f = if len > 0.0 {
            (target - cum[seg]) / len
        } else {
            0.0
        };
        let q = chain[seg] + (chain[seg + 1] - chain[seg]) * f;
        pts.push(on_curve(a, b, q).arr());
    }
    pts.push(chain[chain.len() - 1].arr());
    Curve::BSpline(bspline::interpolate(&pts))
}

/// The largest distance of samples of the curve from either surface.
pub(crate) fn deviation(c: &Curve, range: [f64; 2], a: &Surf, b: &Surf) -> f64 {
    if let (Curve::Line { .. }, true, true) = (c, a.is_plane(), b.is_plane()) {
        // Distances from planes are linear along a line: the ends bound
        // them. Planar models have tens of thousands of these edges.
        return [range[0], range[1]]
            .iter()
            .map(|&t| {
                let p = curve::eval(c, t);
                a.f(p).abs().max(b.f(p).abs())
            })
            .fold(0.0, f64::max);
    }
    let n = match c {
        Curve::BSpline(BSpline { control, .. }) => (8 * control.len()).min(8192),
        _ => 64,
    };
    curve::sample(c, range, n)
        .iter()
        .map(|&p| a.f(p).abs().max(b.f(p).abs()))
        .fold(0.0, f64::max)
}

/// The largest distance of the chain's points from the curve, measured as
/// the distance to their projection onto both surfaces.
pub(crate) fn chain_deviation(a: &Surf, b: &Surf, chain: &[V]) -> f64 {
    if let (Surf::Plane { n: na, .. }, Surf::Plane { n: nb, .. }) = (a, b) {
        // The distance to the planes' line in closed form: the shortest
        // step δ with nₐ·δ = -rₐ and n_b·δ = -r_b has |δ|² = rᵀG⁻¹r, G the
        // normals' Gram matrix.
        let c = na.dot(*nb);
        let det = 1.0 - c * c;
        if det > 1e-12 {
            return chain
                .iter()
                .map(|&p| {
                    let (ra, rb) = (a.f(p), b.f(p));
                    ((ra * ra - 2.0 * c * ra * rb + rb * rb) / det)
                        .max(0.0)
                        .sqrt()
                })
                .fold(0.0, f64::max);
        }
    }
    chain
        .iter()
        .map(|&p| (on_curve(a, b, p) - p).len())
        .fold(0.0, f64::max)
}
