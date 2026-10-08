//! Analytic tangency: pairs of exact surfaces that touch without crossing,
//! found from the surface records alone.
//!
//! The mesh cannot be trusted near a tangency. Two tangent surfaces meet at
//! a grazing angle, so solving for a vertex on both is ill-conditioned (a
//! rounding error in the input moves the solution by its square root), and
//! the tessellated pair may touch, cross or miss depending on where the
//! polygon vertices fall. Knowing the contact set exactly lets
//! reconstruction put vertices on it and use it as the edge's curve.

use std::sync::Arc;

use crate::math::*;
use crate::model::{Contact, Surface, Tangency};
use crate::nurbs::Spline;
use crate::surf::Surf;

/// How far apart (the sine of the angle) the normals of a B-spline
/// patch and another surface may be along a boundary of the patch that
/// lies on the other surface, for the two to count as tangent there. A
/// blend whose spine and contacts are fitted has normals off by about the
/// fit's error over its point spacing (1e-7 over a few tenths of a
/// millimetre for the default tolerance); a boundary that crosses the
/// other surface at a smaller angle still has the boundary as their
/// intersection, so treating it as a contact is harmless.
const BOUNDARY_ANGLE: f64 = 1e-4;

/// A side of a B-spline patch's domain: fixed `u` (else fixed `v`) at
/// `at`, an end of the domain.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Side {
    pub fixed_u: bool,
    pub at: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Cont {
    Line {
        p: V,
        d: V,
    },
    Circle {
        c: V,
        n: V,
        r: f64,
    },
    Point {
        p: V,
    },
    /// Boundaries of a B-spline patch along which it touches the other
    /// surface (one or more sides of its domain). `first`: the patch is
    /// the first of the pair [`contact`] was given.
    Boundary {
        s: Arc<Spline>,
        sides: Vec<Side>,
        first: bool,
    },
}

impl Cont {
    /// Surfaces whose common points are exactly the contact set, for
    /// solving vertices on it. A boundary contact has no such pair: its
    /// vertices are solved along the boundary curve instead
    /// (`reconstruct::place_vertex`), and this gives only the patch.
    pub fn constraints(&self) -> Vec<Surf> {
        match *self {
            Cont::Boundary { ref s, .. } => vec![Surf::Spline(s.clone())],
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

    /// The published contacts, `a` and `b` the input indices of the pair
    /// in the order [`contact`] was given them (one per side for a
    /// boundary contact).
    pub fn to_public(&self, a: u32, b: u32) -> Vec<Contact> {
        if let Cont::Boundary { sides, first, .. } = self {
            return sides
                .iter()
                .map(|sd| Contact::Boundary {
                    surface: if *first { a } else { b },
                    fixed_u: sd.fixed_u,
                    parameter: sd.at,
                })
                .collect();
        }
        vec![match *self {
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
            Cont::Boundary { .. } => unreachable!("handled above"),
        }]
    }

    /// The side of a boundary contact nearest `p`, and its patch.
    pub fn nearest_side(&self, p: V) -> Option<(&Arc<Spline>, Side)> {
        let Cont::Boundary { s, sides, .. } = self else {
            return None;
        };
        let (u, v) = s.project(p, false);
        let mut best: Option<(f64, Side)> = None;
        for &sd in sides {
            let q = if sd.fixed_u {
                s.eval(sd.at, v)
            } else {
                s.eval(u, sd.at)
            };
            let d = (q - p).len();
            if best.is_none_or(|b| d < b.0) {
                best = Some((d, sd));
            }
        }
        best.map(|b| (s, b.1))
    }
}

/// The sides of patch `s` that lie on `other` within `tol` with normals
/// parallel within [`BOUNDARY_ANGLE`]: sampled at four points a knot span
/// (at most 64 and the ends) along each side.
fn boundary_contact(s: &Arc<Spline>, other: &Surf, tol: f64, first: bool) -> Option<Cont> {
    let (du, dv) = s.domain();
    let mut sides = Vec::new();
    for sd in [
        Side {
            fixed_u: true,
            at: du[0],
        },
        Side {
            fixed_u: true,
            at: du[1],
        },
        Side {
            fixed_u: false,
            at: dv[0],
        },
        Side {
            fixed_u: false,
            at: dv[1],
        },
    ] {
        let on = s.iso_samples(sd.fixed_u).into_iter().all(|t| {
            let (u, v) = if sd.fixed_u { (sd.at, t) } else { (t, sd.at) };
            let p = s.eval(u, v);
            other.f(p).abs() <= tol && s.normal(u, v).cross(other.grad(p)).len() <= BOUNDARY_ANGLE
        });
        if on {
            sides.push(sd);
        }
    }
    (!sides.is_empty()).then(|| Cont::Boundary {
        s: s.clone(),
        sides,
        first,
    })
}

/// A surface of revolution's profile in the half-plane of an axis
/// (o, a), in coordinates (distance from the axis, height along it): a
/// line (point, unit direction) or a circle (centre, radius).
#[derive(Clone, Copy, Debug)]
enum Profile {
    Line([f64; 2], [f64; 2]),
    Circle([f64; 2], f64),
}

/// `s`'s profile about the axis (o, a), when `s` is a surface of
/// revolution about that axis (a plane across it counts).
fn profile(s: &Surf, o: V, a: V, tol: f64) -> Option<Profile> {
    let on_axis = |p: V| (p - o).reject(a).len() < tol;
    let par = |x: V| x.dot(a).abs() > 1.0 - 1e-12;
    let h = |p: V| (p - o).dot(a);
    match *s {
        Surf::Plane { o: po, n } if par(n) => Some(Profile::Line([0.0, h(po)], [1.0, 0.0])),
        Surf::Cyl { o: oc, a: ac, r } if par(ac) && on_axis(oc) => {
            Some(Profile::Line([r, 0.0], [0.0, 1.0]))
        }
        Surf::Cone { apex, a: ac, k } if par(ac) && on_axis(apex) => {
            let sg = ac.dot(a).signum();
            let l = (1.0 + k * k).sqrt();
            Some(Profile::Line([0.0, h(apex)], [k / l, sg / l]))
        }
        Surf::Sphere { c, r } if on_axis(c) => Some(Profile::Circle([0.0, h(c)], r)),
        Surf::Torus { c, a: at, big, r } if par(at) && on_axis(c) => {
            Some(Profile::Circle([big, h(c)], r))
        }
        _ => None,
    }
}

/// Where two profiles touch without crossing, as a point of the
/// half-plane, within `tol`.
fn profile_contact(p: Profile, q: Profile, tol: f64) -> Option<[f64; 2]> {
    let line_circle = |o: [f64; 2], d: [f64; 2], c: [f64; 2], r: f64| {
        let t = (c[0] - o[0]) * d[0] + (c[1] - o[1]) * d[1];
        let f = [o[0] + d[0] * t, o[1] + d[1] * t];
        let dist = ((c[0] - f[0]).powi(2) + (c[1] - f[1]).powi(2)).sqrt();
        ((dist - r).abs() < tol).then_some(f)
    };
    match (p, q) {
        (Profile::Line(o, d), Profile::Circle(c, r))
        | (Profile::Circle(c, r), Profile::Line(o, d)) => line_circle(o, d, c, r),
        (Profile::Circle(c1, r1), Profile::Circle(c2, r2)) => {
            let w = [c2[0] - c1[0], c2[1] - c1[1]];
            let d = (w[0] * w[0] + w[1] * w[1]).sqrt();
            if d < tol {
                return None;
            }
            let u = [w[0] / d, w[1] / d];
            let k = if (d - (r1 + r2)).abs() < tol {
                r1
            } else if (d - (r1 - r2).abs()).abs() < tol {
                if r1 > r2 { r1 } else { -r1 }
            } else {
                return None;
            };
            Some([c1[0] + u[0] * k, c1[1] + u[1] * k])
        }
        _ => None,
    }
}

/// The contact of two coaxial surfaces of revolution, one of them a torus,
/// from their profiles: a circle about the axis, or a point on it. Pairs
/// that are not coaxial are not handled.
fn coaxial_contact(a: &Surf, b: &Surf, tol: f64) -> Option<Cont> {
    let (o, ax) = [a, b].into_iter().find_map(|s| match *s {
        Surf::Torus { c, a, .. } => Some((c, a)),
        _ => None,
    })?;
    let p = profile(a, o, ax, tol)?;
    let q = profile(b, o, ax, tol)?;
    let [rho, h] = profile_contact(p, q, tol)?;
    if rho < -tol {
        return None;
    }
    let c = o + ax * h;
    Some(if rho < tol {
        Cont::Point { p: c }
    } else {
        Cont::Circle { c, n: ax, r: rho }
    })
}

/// The contact set of two surfaces that are tangent, within `tol` for
/// lengths. `None` for surfaces that cross, miss or coincide, and for the
/// pairs not handled (cone–plane along a generator, cone–cylinder).
///
/// A B-spline patch touches another surface along sides of its domain
/// (a blend along the faces it rolls on): those are found from the
/// records too, by sampling, within `tol + fit`. `fit` is how far a
/// patch may be from the surfaces it was made to meet
/// ([`crate::Tolerances::surface_fit`]): a blend whose contact on a
/// cylinder was fitted touches the cylinder only to within the fit, and
/// without it would be taken for a crossing, whose ill-conditioned
/// intersection the edge would then be fitted to.
pub(crate) fn contact(a: &Surf, b: &Surf, tol: f64, fit: f64) -> Option<Cont> {
    match (a, b) {
        (Surf::Spline(s), o) | (o, Surf::Spline(s)) if !matches!(o, Surf::Spline(_)) => {
            let first = matches!(a, Surf::Spline(_));
            return boundary_contact(s, o, tol + fit, first);
        }
        (Surf::Spline(s), Surf::Spline(t)) => {
            return boundary_contact(s, b, tol + fit, true)
                .or_else(|| boundary_contact(t, a, tol + fit, false));
        }
        _ => {}
    }
    let (a, b) = if a.rank() <= b.rank() { (a, b) } else { (b, a) };
    if let Surf::Torus { c, a: at, big, r } = *b {
        if let Surf::Cyl { o, a: ax, r: rc } = *a
            && ax.dot(at).abs() < 1e-12
        {
            // A cylinder whose axis is tangent to the tube's centre
            // circle, of the tube's radius, touches the torus along the
            // tube's circle there (a rounded edge meeting its rounded
            // corner, the same profile extruded and revolved).
            let h = (o - c).dot(at);
            let w = (c - o).reject(ax);
            let foot = c - w;
            if h.abs() < tol && (w.len() - big).abs() < tol && (rc - r).abs() < tol {
                return Some(Cont::Circle { c: foot, n: ax, r });
            }
            return None;
        }
        return coaxial_contact(a, b, tol);
    }
    let par = |x: V, y: V| x.dot(y).abs() > 1.0 - 1e-12;
    match (a.clone(), b.clone()) {
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
        // A plane through the apex at the cone's half-angle to its axis
        // touches it along a generator. A profile both extruded and
        // revolved makes this pair from each of its lines (BOSL2's edge
        // and corner masks): the extruded line's plane is the revolved
        // line's cone's tangent plane where the two meet.
        (Surf::Plane { o: po, n }, Surf::Cone { apex, a: ax, k }) => {
            let sin_half = k / (1.0 + k * k).sqrt();
            let c = n.dot(ax);
            if (apex - po).dot(n).abs() >= tol || (c.abs() - sin_half).abs() >= 1e-9 {
                return None;
            }
            let g = (ax - n * c).norm();
            g.is_finite().then_some(Cont::Line { p: apex, d: g })
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
/// touch. Lengths are compared within `tolerance` (for a B-spline
/// surface, which touches others along sides of its domain, this must
/// cover how far it was fitted from them too).
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
        // A malformed record (a B-spline whose knots do not fit its net)
        // touches nothing; evaluating it would index out of its arrays.
        .filter(|(_, s)| !matches!(s, Surf::Spline(_)) || s.well_formed())
        .collect();
    let mut out = Vec::new();
    for (i, (ia, a)) in surfs.iter().enumerate() {
        for (ib, b) in &surfs[i + 1..] {
            if let Some(c) = contact(a, b, tolerance, 0.0) {
                for contact in c.to_public(*ia, *ib) {
                    out.push(Tangency {
                        surfaces: [*ia, *ib],
                        contact,
                    });
                }
            }
        }
    }
    out
}
