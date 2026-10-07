//! Analytic tangency: pairs of exact surfaces that touch without crossing,
//! found from the surface records alone.
//!
//! The mesh cannot be trusted near a tangency. Two tangent surfaces meet at
//! a grazing angle, so solving for a vertex on both is ill-conditioned (a
//! rounding error in the input moves the solution by its square root), and
//! the tessellated pair may touch, cross or miss depending on where the
//! polygon vertices fall. Knowing the contact set exactly lets
//! reconstruction put vertices on it and use it as the edge's curve.

use crate::math::*;
use crate::model::{Contact, Surface, Tangency};
use crate::surf::Surf;

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Cont {
    Line { p: V, d: V },
    Circle { c: V, n: V, r: f64 },
    Point { p: V },
}

impl Cont {
    /// Surfaces whose common points are exactly the contact set, for
    /// solving vertices on it.
    pub fn constraints(&self) -> Vec<Surf> {
        match *self {
            Cont::Line { p, d } => {
                let n1 = d.perp();
                vec![
                    Surf::Plane { o: p, n: n1 },
                    Surf::Plane {
                        o: p,
                        n: d.cross(n1).norm(),
                    },
                ]
            }
            Cont::Circle { c, n, r } => vec![Surf::Plane { o: c, n }, Surf::Sphere { c, r }],
            Cont::Point { p } => vec![
                Surf::Plane {
                    o: p,
                    n: v(1.0, 0.0, 0.0),
                },
                Surf::Plane {
                    o: p,
                    n: v(0.0, 1.0, 0.0),
                },
                Surf::Plane {
                    o: p,
                    n: v(0.0, 0.0, 1.0),
                },
            ],
        }
    }

    /// Whether the contact is a curve (an edge between the two surfaces
    /// runs along it) rather than a point.
    pub fn is_curve(&self) -> bool {
        !matches!(self, Cont::Point { .. })
    }

    pub fn to_public(self) -> Contact {
        match self {
            Cont::Line { p, d } => Contact::Line {
                point: p.arr(),
                direction: d.arr(),
            },
            Cont::Circle { c, n, r } => Contact::Circle {
                center: c.arr(),
                normal: n.arr(),
                radius: r,
            },
            Cont::Point { p } => Contact::Point { point: p.arr() },
        }
    }
}

/// The contact set of two surfaces that are tangent, within `tol` for
/// lengths. `None` for surfaces that cross, miss or coincide, and for the
/// pairs not handled (cone–plane along a generator, cone–cylinder).
pub(crate) fn contact(a: &Surf, b: &Surf, tol: f64) -> Option<Cont> {
    let (a, b) = if a.rank() <= b.rank() { (a, b) } else { (b, a) };
    let par = |x: V, y: V| x.dot(y).abs() > 1.0 - 1e-12;
    match (*a, *b) {
        (Surf::Plane { o: po, n }, Surf::Cyl { o, a: ax, r }) => {
            let h = (o - po).dot(n);
            (ax.dot(n).abs() < 1e-12 && (h.abs() - r).abs() < tol).then(|| Cont::Line {
                p: o - n * h,
                d: ax,
            })
        }
        (Surf::Plane { o: po, n }, Surf::Sphere { c, r }) => {
            let h = (c - po).dot(n);
            ((h.abs() - r).abs() < tol).then(|| Cont::Point { p: c - n * h })
        }
        (
            Surf::Cyl {
                o: o1,
                a: a1,
                r: r1,
            },
            Surf::Cyl {
                o: o2,
                a: a2,
                r: r2,
            },
        ) if par(a1, a2) => {
            let w = (o2 - o1).reject(a1);
            let d = w.len();
            if d < tol {
                return None;
            }
            let u = w * (1.0 / d);
            if (d - (r1 + r2)).abs() < tol {
                Some(Cont::Line {
                    p: o1 + u * r1,
                    d: a1,
                })
            } else if (d - (r1 - r2).abs()).abs() < tol {
                let p = if r1 > r2 { o1 + u * r1 } else { o1 - u * r1 };
                Some(Cont::Line { p, d: a1 })
            } else {
                None
            }
        }
        (Surf::Cyl { o, a: ax, r: rc }, Surf::Sphere { c, r }) => {
            let off = (c - o).reject(ax).len();
            (off < tol && (r - rc).abs() < tol).then_some(Cont::Circle { c, n: ax, r: rc })
        }
        (Surf::Cone { apex, a: ax, k }, Surf::Sphere { c, r }) => {
            let d = c - apex;
            let t = d.dot(ax);
            if d.reject(ax).len() >= tol || t <= 0.0 {
                return None;
            }
            let cos2 = 1.0 / (1.0 + k * k);
            let dist = t * k * cos2.sqrt();
            ((dist - r).abs() < tol).then(|| {
                let tf = t * cos2;
                Cont::Circle {
                    c: apex + ax * tf,
                    n: ax,
                    r: k * tf,
                }
            })
        }
        (Surf::Sphere { c: c1, r: r1 }, Surf::Sphere { c: c2, r: r2 }) => {
            let w = c2 - c1;
            let d = w.len();
            if d < tol {
                return None;
            }
            let u = w * (1.0 / d);
            if (d - (r1 + r2)).abs() < tol {
                Some(Cont::Point { p: c1 + u * r1 })
            } else if (d - (r1 - r2).abs()).abs() < tol {
                let p = if r1 > r2 { c1 + u * r1 } else { c1 - u * r1 };
                Some(Cont::Point { p })
            } else {
                None
            }
        }
        _ => None,
    }
}

/// Every pair of surfaces in `surfaces` that is tangent, with where they
/// touch. Lengths are compared within `tolerance`.
///
/// A caller that tessellates the surfaces can use this to put polygon
/// vertices on the contact lines and circles, which is what keeps the mesh
/// topology the same as the exact topology there. The search is quadratic
/// in the number of supported surfaces.
pub fn find_tangencies(surfaces: &[Surface], tolerance: f64) -> Vec<Tangency> {
    let surfs: Vec<(u32, Surf)> = surfaces
        .iter()
        .enumerate()
        .filter_map(|(i, s)| Surf::from_public(s).map(|s| (i as u32, s)))
        .collect();
    let mut out = Vec::new();
    for (i, (ia, a)) in surfs.iter().enumerate() {
        for (ib, b) in &surfs[i + 1..] {
            if let Some(c) = contact(a, b, tolerance) {
                out.push(Tangency {
                    surfaces: [*ia, *ib],
                    contact: c.to_public(),
                });
            }
        }
    }
    out
}
