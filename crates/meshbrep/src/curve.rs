//! Evaluating the 3D curves of [`Curve`].

use crate::bspline;
use crate::math::*;
use crate::model::{BSpline, Curve};

/// A curve ready for repeated evaluation (a B-spline's derivative is
/// computed once).
#[derive(Debug)]
pub(crate) struct CurveEval<'a> {
    pub c: &'a Curve,
    d: Option<BSpline<3>>,
}

impl<'a> CurveEval<'a> {
    pub fn new(c: &'a Curve) -> CurveEval<'a> {
        let d = match c {
            Curve::BSpline(b) => Some(bspline::derivative(b)),
            _ => None,
        };
        CurveEval { c, d }
    }

    pub fn at(&self, t: f64) -> V {
        eval(self.c, t)
    }

    pub fn deriv(&self, t: f64) -> V {
        match self.c {
            Curve::Line { direction, .. } => V::from(*direction),
            Curve::Circle {
                normal,
                x_axis,
                radius,
                ..
            } => {
                let (x, n) = (V::from(*x_axis), V::from(*normal));
                let y = n.cross(x);
                (y * cos(t) - x * sin(t)) * *radius
            }
            Curve::Ellipse {
                normal,
                x_axis,
                major,
                minor,
                ..
            } => {
                let (x, n) = (V::from(*x_axis), V::from(*normal));
                let y = n.cross(x);
                y * (cos(t) * minor) - x * (sin(t) * major)
            }
            Curve::BSpline(_) => V::from(bspline::eval(self.d.as_ref().expect("derivative"), t)),
        }
    }
}

pub(crate) fn eval(c: &Curve, t: f64) -> V {
    match c {
        Curve::Line { origin, direction } => V::from(*origin) + V::from(*direction) * t,
        Curve::Circle {
            center,
            normal,
            x_axis,
            radius,
        } => {
            let (x, n) = (V::from(*x_axis), V::from(*normal));
            let y = n.cross(x);
            V::from(*center) + (x * cos(t) + y * sin(t)) * *radius
        }
        Curve::Ellipse {
            center,
            normal,
            x_axis,
            major,
            minor,
        } => {
            let (x, n) = (V::from(*x_axis), V::from(*normal));
            let y = n.cross(x);
            V::from(*center) + x * (cos(t) * major) + y * (sin(t) * minor)
        }
        Curve::BSpline(b) => V::from(bspline::eval(b, t)),
    }
}

/// The parameter of a point on a line, circle or ellipse (angles in
/// (-π, π]). Not meaningful for B-splines, whose ends are their vertices.
pub(crate) fn param_of(c: &Curve, p: V) -> f64 {
    match c {
        Curve::Line { origin, direction } => (p - V::from(*origin)).dot(V::from(*direction)),
        Curve::Circle {
            center,
            normal,
            x_axis,
            ..
        } => {
            let (x, n) = (V::from(*x_axis), V::from(*normal));
            let d = p - V::from(*center);
            atan2(d.dot(n.cross(x)), d.dot(x))
        }
        Curve::Ellipse {
            center,
            normal,
            x_axis,
            major,
            minor,
        } => {
            let (x, n) = (V::from(*x_axis), V::from(*normal));
            let d = p - V::from(*center);
            atan2(d.dot(n.cross(x)) / minor, d.dot(x) / major)
        }
        Curve::BSpline(_) => 0.0,
    }
}

/// `n + 1` points evenly spaced in parameter over `range`.
pub(crate) fn sample(c: &Curve, range: [f64; 2], n: usize) -> Vec<V> {
    (0..=n)
        .map(|i| eval(c, range[0] + (range[1] - range[0]) * i as f64 / n as f64))
        .collect()
}

/// How many samples an edge needs for a faithful polyline: conics by
/// angle, B-splines by span.
pub(crate) fn sample_count(c: &Curve, range: [f64; 2]) -> usize {
    match c {
        Curve::Line { .. } => 4,
        Curve::Circle { .. } | Curve::Ellipse { .. } => {
            (((range[1] - range[0]).abs() / 0.1).ceil() as usize).clamp(8, 128)
        }
        Curve::BSpline(b) => (4 * bspline::spans(b).len()).clamp(8, 4096),
    }
}

/// Whether two curves are the same point set (same circle, collinear
/// lines, ...), for merging edges split at a vertex that turned out not to
/// be needed.
pub(crate) fn same_carrier(a: &Curve, b: &Curve, tol: f64) -> bool {
    let close = |p: [f64; 3], q: [f64; 3]| (V::from(p) - V::from(q)).len() < tol;
    let parallel = |p: [f64; 3], q: [f64; 3]| V::from(p).dot(V::from(q)).abs() > 1.0 - 1e-12;
    match (a, b) {
        (
            Curve::Line {
                origin: o1,
                direction: d1,
            },
            Curve::Line {
                origin: o2,
                direction: d2,
            },
        ) => parallel(*d1, *d2) && (V::from(*o2) - V::from(*o1)).reject(V::from(*d1)).len() < tol,
        (
            Curve::Circle {
                center: c1,
                normal: n1,
                radius: r1,
                ..
            },
            Curve::Circle {
                center: c2,
                normal: n2,
                radius: r2,
                ..
            },
        ) => close(*c1, *c2) && parallel(*n1, *n2) && (r1 - r2).abs() < tol,
        (
            Curve::Ellipse {
                center: c1,
                normal: n1,
                x_axis: x1,
                major: a1,
                minor: b1,
            },
            Curve::Ellipse {
                center: c2,
                normal: n2,
                x_axis: x2,
                major: a2,
                minor: b2,
            },
        ) => {
            close(*c1, *c2)
                && parallel(*n1, *n2)
                && parallel(*x1, *x2)
                && (a1 - a2).abs() < tol
                && (b1 - b2).abs() < tol
        }
        _ => false,
    }
}
