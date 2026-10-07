//! The exact surfaces as reconstruction works with them: implicit form
//! (signed distance and gradient), equality, and the parametrisation of a
//! face's surface in a [`Param`] frame.

use crate::math::*;
use crate::model::{Frame, Surface};

/// An exact surface. The natural normal points away from the axis or
/// centre (cylinder, cone, sphere) or along `n` (plane).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Surf {
    Plane {
        o: V,
        n: V,
    },
    Cyl {
        o: V,
        a: V,
        r: f64,
    },
    /// radius = k * ((p - apex) . a), on the nappe where that is >= 0.
    Cone {
        apex: V,
        a: V,
        k: f64,
    },
    Sphere {
        c: V,
        r: f64,
    },
}

impl Surf {
    /// The internal form of a supported surface; `None` for faceted
    /// entries and the kinds not implemented yet.
    pub fn from_public(s: &Surface) -> Option<Surf> {
        Some(match s {
            Surface::Plane { origin, normal } => Surf::Plane {
                o: V::from(*origin),
                n: V::from(*normal).norm(),
            },
            Surface::Cylinder {
                origin,
                axis,
                radius,
            } => Surf::Cyl {
                o: V::from(*origin),
                a: V::from(*axis).norm(),
                r: *radius,
            },
            Surface::Cone { apex, axis, slope } => Surf::Cone {
                apex: V::from(*apex),
                a: V::from(*axis).norm(),
                k: *slope,
            },
            Surface::Sphere { center, radius } => Surf::Sphere {
                c: V::from(*center),
                r: *radius,
            },
            _ => return None,
        })
    }

    pub fn to_public(self) -> Surface {
        match self {
            Surf::Plane { o, n } => Surface::Plane {
                origin: o.arr(),
                normal: n.arr(),
            },
            Surf::Cyl { o, a, r } => Surface::Cylinder {
                origin: o.arr(),
                axis: a.arr(),
                radius: r,
            },
            Surf::Cone { apex, a, k } => Surface::Cone {
                apex: apex.arr(),
                axis: a.arr(),
                slope: k,
            },
            Surf::Sphere { c, r } => Surface::Sphere {
                center: c.arr(),
                radius: r,
            },
        }
    }

    /// Whether the numbers make a surface (finite, positive radii, unit
    /// directions that were not zero).
    pub fn well_formed(&self) -> bool {
        match *self {
            Surf::Plane { o, n } => o.is_finite() && n.is_finite() && n.len() > 0.5,
            Surf::Cyl { o, a, r } => {
                o.is_finite() && a.is_finite() && a.len() > 0.5 && r.is_finite() && r > 0.0
            }
            Surf::Cone { apex, a, k } => {
                apex.is_finite() && a.is_finite() && a.len() > 0.5 && k.is_finite() && k > 0.0
            }
            Surf::Sphere { c, r } => c.is_finite() && r.is_finite() && r > 0.0,
        }
    }

    /// Order of the kinds when a pair is normalised: plane first.
    pub fn rank(&self) -> u8 {
        match self {
            Surf::Plane { .. } => 0,
            Surf::Cyl { .. } => 1,
            Surf::Cone { .. } => 2,
            Surf::Sphere { .. } => 3,
        }
    }

    pub fn is_plane(&self) -> bool {
        matches!(self, Surf::Plane { .. })
    }

    /// Signed distance along the natural normal (exact, except for a cone
    /// far from its nappe, where it is still a smooth defining function).
    pub fn f(&self, p: V) -> f64 {
        match *self {
            Surf::Plane { o, n } => (p - o).dot(n),
            Surf::Cyl { o, a, r } => (p - o).reject(a).len() - r,
            Surf::Cone { apex, a, k } => {
                let d = p - apex;
                let t = d.dot(a);
                let rho = d.reject(a).len();
                (rho - k * t) / (1.0 + k * k).sqrt()
            }
            Surf::Sphere { c, r } => (p - c).len() - r,
        }
    }

    pub fn grad(&self, p: V) -> V {
        match *self {
            Surf::Plane { n, .. } => n,
            Surf::Cyl { o, a, .. } => (p - o).reject(a).norm(),
            Surf::Cone { apex, a, k } => {
                let q = (p - apex).reject(a).norm();
                (q - a * k) * (1.0 / (1.0 + k * k).sqrt())
            }
            Surf::Sphere { c, .. } => (p - c).norm(),
        }
    }

    /// The axis (point, unit direction) of a surface of revolution with a
    /// fixed axis.
    pub fn axis(&self) -> Option<(V, V)> {
        match *self {
            Surf::Cyl { o, a, .. } => Some((o, a)),
            Surf::Cone { apex, a, .. } => Some((apex, a)),
            _ => None,
        }
    }

    /// Whether `o` is geometrically the same surface, within `tol` for
    /// lengths (angles within 1e-12, which transform rounding stays far
    /// below).
    pub fn same(&self, o: &Surf, tol: f64) -> bool {
        let line_dist = |p: V, q: V, a: V| (q - p).reject(a).len();
        match (*self, *o) {
            (Surf::Plane { o: o1, n: n1 }, Surf::Plane { o: o2, n: n2 }) => {
                n1.dot(n2).abs() > 1.0 - 1e-12 && (o2 - o1).dot(n1).abs() < tol
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
            ) => {
                a1.dot(a2).abs() > 1.0 - 1e-12
                    && (r1 - r2).abs() < tol
                    && line_dist(o1, o2, a1) < tol
            }
            (Surf::Sphere { c: c1, r: r1 }, Surf::Sphere { c: c2, r: r2 }) => {
                (c1 - c2).len() < tol && (r1 - r2).abs() < tol
            }
            (
                Surf::Cone {
                    apex: p1,
                    a: a1,
                    k: k1,
                },
                Surf::Cone {
                    apex: p2,
                    a: a2,
                    k: k2,
                },
            ) => (p1 - p2).len() < tol && a1.dot(a2) > 1.0 - 1e-12 && (k1 - k2).abs() < 1e-9,
            _ => false,
        }
    }
}

/// A face's surface with its parametrisation frame.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Param {
    pub s: Surf,
    pub o: V,
    pub x: V,
    pub y: V,
    pub z: V,
    /// Cone: radius at `o`.
    pub r0: f64,
}

impl Param {
    pub fn new(s: Surf, o: V, z: V, x: V, r0: f64) -> Param {
        let z = z.norm();
        let x = x.reject(z).norm();
        Param {
            s,
            o,
            x,
            y: z.cross(x),
            z,
            r0,
        }
    }

    pub fn frame(&self) -> Frame {
        Frame {
            origin: self.o.arr(),
            z: self.z.arr(),
            x: self.x.arr(),
        }
    }

    /// Whether `u` is an angle (periodic).
    pub fn periodic(&self) -> bool {
        !self.s.is_plane()
    }

    fn radius(&self) -> f64 {
        match self.s {
            Surf::Cyl { r, .. } | Surf::Sphere { r, .. } => r,
            _ => 0.0,
        }
    }

    /// The angle of `p` about the frame's z axis.
    pub fn angle(&self, p: V) -> f64 {
        let d = p - self.o;
        atan2(d.dot(self.y), d.dot(self.x))
    }

    /// Parameters of a point on (or near) the surface; `u` in (-π, π] for
    /// periodic surfaces.
    pub fn uv(&self, p: V) -> (f64, f64) {
        let d = p - self.o;
        match self.s {
            Surf::Plane { .. } => (d.dot(self.x), d.dot(self.y)),
            Surf::Cyl { .. } | Surf::Cone { .. } => (self.angle(p), d.dot(self.z)),
            Surf::Sphere { .. } => {
                let h = d.dot(self.z);
                let rho = d.reject(self.z).len();
                (self.angle(p), atan2(h, rho))
            }
        }
    }

    pub fn eval(&self, u: f64, w: f64) -> V {
        let dir = |u: f64| self.x * cos(u) + self.y * sin(u);
        match self.s {
            Surf::Plane { .. } => self.o + self.x * u + self.y * w,
            Surf::Cyl { r, .. } => self.o + dir(u) * r + self.z * w,
            Surf::Cone { k, .. } => self.o + dir(u) * (self.r0 + w * k) + self.z * w,
            Surf::Sphere { r, .. } => self.o + dir(u) * (r * cos(w)) + self.z * (r * sin(w)),
        }
    }

    /// Partial derivatives (∂u, ∂v).
    pub fn derivs(&self, u: f64, w: f64) -> (V, V) {
        let (su, cu) = (sin(u), cos(u));
        let dir = self.x * cu + self.y * su;
        let ddir = self.y * cu - self.x * su;
        match self.s {
            Surf::Plane { .. } => (self.x, self.y),
            Surf::Cyl { r, .. } => (ddir * r, self.z),
            Surf::Cone { k, .. } => (ddir * (self.r0 + w * k), dir * k + self.z),
            Surf::Sphere { r, .. } => {
                let (sw, cw) = (sin(w), cos(w));
                (ddir * (r * cw), dir * (-r * sw) + self.z * (r * cw))
            }
        }
    }

    /// `v` at the apex of a cone or the poles of a sphere, where `u` is
    /// undefined: (low, high).
    pub fn poles(&self) -> (Option<f64>, Option<f64>) {
        match self.s {
            Surf::Sphere { .. } => (Some(-PI / 2.0), Some(PI / 2.0)),
            Surf::Cone { k, .. } => (Some(-self.r0 / k), None),
            _ => (None, None),
        }
    }

    /// Whether `p` is so near the axis that its angle is meaningless.
    pub fn near_axis(&self, p: V, tol: f64) -> bool {
        self.periodic() && (p - self.o).reject(self.z).len() < tol.max(1e-12 * self.radius())
    }
}
