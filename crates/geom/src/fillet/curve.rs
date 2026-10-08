//! Points, tangents, lengths and centres of a B-rep edge's curve, and the
//! outward normals of its faces: the measurements edge selection makes.
//!
//! Sines and cosines come from `libm` rather than the platform, so a
//! report's numbers (and the selections decided from them) are the same
//! bits natively and on wasm32, as `meshbrep`'s are.

use meshbrep::{BSpline, Curve, Edge, Face, Surface};

pub(crate) type V = [f64; 3];

pub(crate) fn add(a: V, b: V) -> V {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}

pub(crate) fn sub(a: V, b: V) -> V {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

pub(crate) fn mul(a: V, s: f64) -> V {
    [a[0] * s, a[1] * s, a[2] * s]
}

pub(crate) fn dot(a: V, b: V) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

pub(crate) fn cross(a: V, b: V) -> V {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

pub(crate) fn norm(a: V) -> f64 {
    dot(a, a).sqrt()
}

/// `a` scaled to unit length; the zero vector stays zero.
pub(crate) fn unit(a: V) -> V {
    let l = norm(a);
    if l > 0.0 { mul(a, 1.0 / l) } else { a }
}

fn sin(x: f64) -> f64 {
    libm::sin(x)
}

fn cos(x: f64) -> f64 {
    libm::cos(x)
}

/// The point of a B-spline at `t` (de Boor's algorithm).
fn bspline_point(b: &BSpline<3>, t: f64) -> V {
    let p = b.degree as usize;
    let n = b.control.len();
    if n == 0 {
        return [0.0; 3];
    }
    let k = &b.knots;
    let lo = k[p];
    let hi = k[n];
    let t = t.clamp(lo, hi);
    // The span: k[s] <= t < k[s + 1], the last non-empty one at the end.
    let mut s = p;
    while s + 1 < n && k[s + 1] <= t {
        s += 1;
    }
    let mut d: Vec<V> = (0..=p).map(|j| b.control[j + s - p]).collect();
    for r in 1..=p {
        for j in (r..=p).rev() {
            let i = j + s - p;
            let den = k[i + p + 1 - r] - k[i];
            let a = if den > 0.0 { (t - k[i]) / den } else { 0.0 };
            d[j] = add(mul(d[j - 1], 1.0 - a), mul(d[j], a));
        }
    }
    d[p]
}

/// The point of `c` at parameter `t`.
pub(crate) fn point(c: &Curve, t: f64) -> V {
    match c {
        Curve::Line { origin, direction } => add(*origin, mul(*direction, t)),
        Curve::Circle {
            center,
            normal,
            x_axis,
            radius,
        } => {
            let y = cross(*normal, *x_axis);
            add(
                *center,
                add(mul(*x_axis, radius * cos(t)), mul(y, radius * sin(t))),
            )
        }
        Curve::Ellipse {
            center,
            normal,
            x_axis,
            major,
            minor,
        } => {
            let y = cross(*normal, *x_axis);
            add(
                *center,
                add(mul(*x_axis, major * cos(t)), mul(y, minor * sin(t))),
            )
        }
        Curve::BSpline(b) => bspline_point(b, t),
    }
}

/// The unit tangent of `c` at `t`, in the direction of increasing `t`.
pub(crate) fn tangent(c: &Curve, t: f64, range: [f64; 2]) -> V {
    match c {
        Curve::Line { direction, .. } => unit(*direction),
        Curve::Circle { normal, x_axis, .. } => {
            let y = cross(*normal, *x_axis);
            unit(add(mul(*x_axis, -sin(t)), mul(y, cos(t))))
        }
        Curve::Ellipse {
            normal,
            x_axis,
            major,
            minor,
            ..
        } => {
            let y = cross(*normal, *x_axis);
            unit(add(mul(*x_axis, -major * sin(t)), mul(y, minor * cos(t))))
        }
        Curve::BSpline(_) => {
            // A central difference well inside the parameter range.
            let h = (range[1] - range[0]) * 1e-4;
            let a = (t - h).max(range[0]);
            let b = (t + h).min(range[1]);
            unit(sub(point(c, b), point(c, a)))
        }
    }
}

/// `n + 1` points of the edge, evenly spaced in its parameter.
pub(crate) fn samples(e: &Edge, n: usize) -> Vec<V> {
    let [a, b] = e.range;
    (0..=n)
        .map(|i| point(&e.curve, a + (b - a) * i as f64 / n as f64))
        .collect()
}

/// How many samples draw or measure a curve of this kind: two for a line,
/// enough for a smooth polyline otherwise.
pub(crate) fn sample_count(e: &Edge) -> usize {
    match e.curve {
        Curve::Line { .. } => 1,
        Curve::Circle { .. } => {
            // One sample per 5 degrees, at least 8.
            let turn = (e.range[1] - e.range[0]).abs();
            ((turn / (5f64.to_radians())).ceil() as usize).clamp(8, 72)
        }
        _ => 64,
    }
}

/// The edge's length.
pub(crate) fn length(e: &Edge) -> f64 {
    let d = e.range[1] - e.range[0];
    match &e.curve {
        Curve::Line { direction, .. } => d * norm(*direction),
        Curve::Circle { radius, .. } => d * radius,
        _ => {
            let p = samples(e, 256);
            p.windows(2).map(|w| norm(sub(w[1], w[0]))).sum()
        }
    }
}

/// The edge's centre of mass (as a wire of uniform density): what
/// CadQuery's `Center()` gives an edge, and what `>z` and `>>z[i]`
/// compare (`cadquery/occ_impl/shapes.py`, `Mixin1D.Center`).
pub(crate) fn centre(e: &Edge) -> V {
    let [a, b] = e.range;
    match &e.curve {
        Curve::Line { .. } => mul(add(point(&e.curve, a), point(&e.curve, b)), 0.5),
        Curve::Circle {
            center,
            normal,
            x_axis,
            radius,
        } => {
            let turn = b - a;
            if turn >= std::f64::consts::TAU * (1.0 - 1e-12) {
                return *center;
            }
            // An arc's centroid lies on its bisector, r sin(h) / h from
            // the centre, h half the arc's angle.
            let h = 0.5 * turn;
            let m = a + h;
            let y = cross(*normal, *x_axis);
            let dir = add(mul(*x_axis, cos(m)), mul(y, sin(m)));
            add(*center, mul(dir, radius * sin(h) / h))
        }
        _ => {
            let p = samples(e, 256);
            let mut total = 0.0;
            let mut acc = [0.0; 3];
            for w in p.windows(2) {
                let l = norm(sub(w[1], w[0]));
                total += l;
                acc = add(acc, mul(add(w[0], w[1]), 0.5 * l));
            }
            if total > 0.0 {
                mul(acc, 1.0 / total)
            } else {
                p[0]
            }
        }
    }
}

/// The outward unit normal of face `f` at `p`, a point on its surface.
///
/// `Face::same_sense` is decided against the surface's own gradient
/// (`meshbrep`'s `Surf::grad`, before any frame is chosen): a plane's
/// normal, and for the curved surfaces the direction away from the axis
/// or centre (for a cone, tilted by its slope; for a torus, away from the
/// circle its tube is swept about). The same directions are computed here
/// from the surface record, and flipped where the solid's outside is the
/// other way.
pub(crate) fn outward(f: &Face, p: V) -> V {
    let n = match &f.surface {
        Surface::Plane { normal, .. } => unit(*normal),
        Surface::Cylinder { origin, axis, .. } => {
            let d = sub(p, *origin);
            unit(sub(d, mul(*axis, dot(d, *axis))))
        }
        Surface::Cone { apex, axis, slope } => {
            let d = sub(p, *apex);
            let radial = unit(sub(d, mul(*axis, dot(d, *axis))));
            unit(sub(radial, mul(*axis, *slope)))
        }
        Surface::Sphere { center, .. } => unit(sub(p, *center)),
        Surface::Torus {
            center,
            axis,
            major_radius,
            ..
        } => {
            let d = sub(p, *center);
            let radial = unit(sub(d, mul(*axis, dot(d, *axis))));
            let tube = add(*center, mul(radial, *major_radius));
            unit(sub(p, tube))
        }
        // Not produced by reconstruction (faceted regions become planes);
        // a direction of no use is better than a panic.
        Surface::LinearExtrusion { .. } | Surface::Revolution { .. } | Surface::Faceted => {
            f.frame.z
        }
    };
    if f.same_sense { n } else { mul(n, -1.0) }
}
