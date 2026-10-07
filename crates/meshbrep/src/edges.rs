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

#[derive(Debug)]
pub(crate) struct Built {
    pub curve: Curve,
    pub range: [f64; 2],
    /// Largest distance of curve samples from either surface.
    pub dev: f64,
}

/// The point on `a ∩ b` nearest `p`.
fn on_curve(a: &Surf, b: &Surf, p: V) -> V {
    solve(&[*a, *b], p).0
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
    match (*s, *t) {
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
        _ => match (s.axis(), t.axis(), *t) {
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
    let q = on_curve(s, t, chain[chain.len() / 2]);
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
    if !closed && !s.is_plane() {
        if let Some(cyl) = [*s, *t].into_iter().find(|x| matches!(x, Surf::Cyl { .. })) {
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
                    {
                        if !matches!(curve, Curve::Line { .. }) {
                            let dev = deviation(&curve, range, s, t);
                            return Built { curve, range, dev };
                        }
                    }
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
    chain
        .iter()
        .map(|&p| (on_curve(a, b, p) - p).len())
        .fold(0.0, f64::max)
}
