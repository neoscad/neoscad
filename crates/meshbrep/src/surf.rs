//! The exact surfaces as reconstruction works with them: implicit form
//! (signed distance and gradient), equality, and the parametrisation of a
//! face's surface in a [`Param`] frame.

use std::sync::Arc;

use crate::math::*;
use crate::model::{Frame, Surface};
use crate::nurbs::Spline;

/// An exact surface. The natural normal points away from the axis or
/// centre (cylinder, cone, sphere) or along `n` (plane), and is `∂u × ∂v`
/// on a B-spline.
#[derive(Clone, Debug, PartialEq)]
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
    /// A ring torus: the points at distance `r` from the circle of radius
    /// `big` about the axis (c, a). `big > r`, so it does not cross its
    /// axis.
    Torus {
        c: V,
        a: V,
        big: f64,
        r: f64,
    },
    /// A B-spline patch. Its implicit form is the signed distance along
    /// the normal at a point's projection, the projection allowed a little
    /// past the patch's boundary ([`Spline::project`]). Shared, because a
    /// surface is cloned wherever it is used.
    Spline(Arc<Spline>),
}

/// The point of a torus's tube circle (the circle of radius `big` about
/// the axis (c, a)) nearest `p`. On the axis every point of the circle is
/// as near; one of them is taken.
pub(crate) fn tube_centre(c: V, a: V, big: f64, p: V) -> V {
    let w = (p - c).reject(a);
    let l = w.len();
    let d = if l > 0.0 { w * (1.0 / l) } else { a.perp() };
    c + d * big
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
            Surface::Torus {
                center,
                axis,
                major_radius,
                minor_radius,
            } => Surf::Torus {
                c: V::from(*center),
                a: V::from(*axis).norm(),
                big: *major_radius,
                r: *minor_radius,
            },
            Surface::BSpline(b) => Surf::Spline(Arc::new(Spline::new(b))),
            _ => return None,
        })
    }

    pub fn to_public(&self) -> Surface {
        match *self {
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
            Surf::Torus { c, a, big, r } => Surface::Torus {
                center: c.arr(),
                axis: a.arr(),
                major_radius: big,
                minor_radius: r,
            },
            Surf::Spline(ref s) => Surface::BSpline(s.public.clone()),
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
            // A spindle or horn torus (`big <= r`) crosses its own axis.
            // Its outer part (the apple: the points whose nearest point of
            // the tube's centre circle is on their own side of the axis)
            // is what a convex rim's large fillet sweeps, and every
            // formula here (the tube centre a point projects to, the tube
            // angle) holds there; STEP writes it as a
            // `degenerate_toroidal_surface` selecting the outer part. A
            // tube centred on the axis (`big = 0`) is a sphere.
            Surf::Torus { c, a, big, r } => {
                c.is_finite()
                    && a.is_finite()
                    && a.len() > 0.5
                    && r.is_finite()
                    && r > 0.0
                    && big.is_finite()
                    && big > 0.0
            }
            Surf::Spline(ref s) => s.valid,
        }
    }

    /// Order of the kinds when a pair is normalised: plane first.
    pub fn rank(&self) -> u8 {
        match self {
            Surf::Plane { .. } => 0,
            Surf::Cyl { .. } => 1,
            Surf::Cone { .. } => 2,
            Surf::Sphere { .. } => 3,
            Surf::Torus { .. } => 4,
            Surf::Spline(_) => 5,
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
            Surf::Torus { c, a, big, r } => (p - tube_centre(c, a, big, p)).len() - r,
            Surf::Spline(ref s) => s.f(p),
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
            Surf::Torus { c, a, big, .. } => (p - tube_centre(c, a, big, p)).norm(),
            Surf::Spline(ref s) => s.grad(p),
        }
    }

    /// [`Surf::f`] and [`Surf::grad`] at once, the same values: a
    /// B-spline face projects `p` once for both.
    pub fn f_grad(&self, p: V) -> (f64, V) {
        match *self {
            Surf::Spline(ref s) => s.f_grad(p),
            _ => (self.f(p), self.grad(p)),
        }
    }

    /// The axis (point, unit direction) of a surface of revolution with a
    /// fixed axis.
    pub fn axis(&self) -> Option<(V, V)> {
        match *self {
            Surf::Cyl { o, a, .. } => Some((o, a)),
            Surf::Cone { apex, a, .. } => Some((apex, a)),
            Surf::Torus { c, a, .. } => Some((c, a)),
            _ => None,
        }
    }

    /// The point a surface is placed by (a plane's origin, an axis point,
    /// an apex, a centre): its distance from the origin bounds how much a
    /// direction's rounding moves the surface ([`crate::reconstruct`]'s
    /// index of equal surfaces).
    pub fn key_point(&self) -> V {
        match *self {
            Surf::Plane { o, .. } | Surf::Cyl { o, .. } => o,
            Surf::Cone { apex, .. } => apex,
            Surf::Sphere { c, .. } | Surf::Torus { c, .. } => c,
            Surf::Spline(ref s) => V::from(s.public.control[0][0]),
        }
    }

    /// Whether `o` is geometrically the same surface, within `tol` for
    /// lengths (angles within 1e-12, which transform rounding stays far
    /// below).
    pub fn same(&self, o: &Surf, tol: f64) -> bool {
        let line_dist = |p: V, q: V, a: V| (q - p).reject(a).len();
        match (self.clone(), o.clone()) {
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
            (
                Surf::Torus {
                    c: c1,
                    a: a1,
                    big: b1,
                    r: r1,
                },
                Surf::Torus {
                    c: c2,
                    a: a2,
                    big: b2,
                    r: r2,
                },
            ) => {
                (c1 - c2).len() < tol
                    && a1.dot(a2).abs() > 1.0 - 1e-12
                    && (b1 - b2).abs() < tol
                    && (r1 - r2).abs() < tol
            }
            (Surf::Spline(a), Surf::Spline(b)) => Arc::ptr_eq(&a, &b) || a.same(&b, tol),
            _ => false,
        }
    }
}

/// A face's surface with its parametrisation frame. A B-spline face uses
/// its surface's own parameters; its frame (a point of the patch, its
/// normal and `∂u` there) is only a placement for reports.
#[derive(Clone, Debug)]
pub(crate) struct Param {
    pub s: Surf,
    pub o: V,
    pub x: V,
    pub y: V,
    pub z: V,
    /// Cone: radius at `o`.
    pub r0: f64,
    /// Torus: work in (tube angle, axis angle) instead of STEP's (axis
    /// angle, tube angle). A face that wraps around the tube but not
    /// around the axis (a partial `rotate_extrude` of a whole circle) is
    /// then seamed by the same code that seams a cylinder; its curves are
    /// swapped back before they are written.
    pub swap: bool,
    /// Torus: the tube angle that internal tube coordinates are measured
    /// from. Without `swap` the tube angle stays STEP's own, taken on the
    /// branch `(t0 - π, t0 + π]`, so `t0` keeps a face clear of the cut.
    pub t0: f64,
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
            swap: false,
            t0: 0.0,
        }
    }

    /// The same frame with x turned by `u0` about z: for a swapped torus,
    /// the internal `u` (a tube angle) is moved instead.
    pub fn rotated(&self, u0: f64) -> Param {
        if self.swap {
            return Param {
                t0: self.t0 + u0,
                ..self.clone()
            };
        }
        if !self.periodic() {
            return self.clone();
        }
        // Through `new`, as the frame was always made, so the bits (and
        // the files) are those of before tori.
        let x = self.x * cos(u0) + self.y * sin(u0);
        Param {
            swap: self.swap,
            t0: self.t0,
            ..Param::new(self.s.clone(), self.o, self.z, x, self.r0)
        }
    }

    /// The face sense in internal coordinates: swapping a torus's
    /// coordinates reverses the orientation of its parameter plane.
    pub fn sense(&self, same_sense: bool) -> bool {
        same_sense != self.swap
    }

    /// A point's (axis angle, tube angle) on a torus, both raw (atan2).
    fn torus_angles(&self, p: V) -> (f64, f64) {
        let Surf::Torus { big, .. } = self.s else {
            return (0.0, 0.0);
        };
        let d = p - self.o;
        let h = d.dot(self.z);
        let rho = d.reject(self.z).len();
        (self.angle_about_z(p), atan2(h, rho - big))
    }

    fn angle_about_z(&self, p: V) -> f64 {
        let d = p - self.o;
        atan2(d.dot(self.y), d.dot(self.x))
    }

    /// STEP's torus point at (axis angle, tube angle).
    fn torus_eval(&self, u: f64, w: f64) -> V {
        let Surf::Torus { big, r, .. } = self.s else {
            return self.o;
        };
        let dir = self.x * cos(u) + self.y * sin(u);
        self.o + dir * (big + r * cos(w)) + self.z * (r * sin(w))
    }

    /// STEP's torus partial derivatives at (axis angle, tube angle).
    fn torus_derivs(&self, u: f64, w: f64) -> (V, V) {
        let Surf::Torus { big, r, .. } = self.s else {
            return (self.x, self.y);
        };
        let (su, cu) = (sin(u), cos(u));
        let dir = self.x * cu + self.y * su;
        let ddir = self.y * cu - self.x * su;
        let (sw, cw) = (sin(w), cos(w));
        (ddir * (big + r * cw), dir * (-r * sw) + self.z * (r * cw))
    }

    /// Internal parameter-space coordinates `q` as STEP's (u, v).
    pub fn step_coords(&self, q: [f64; 2]) -> [f64; 2] {
        if self.swap { [q[1], q[0] + self.t0] } else { q }
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
        !matches!(self.s, Surf::Plane { .. } | Surf::Spline(_))
    }

    /// Whether the face's edges get parameter-space curves: every surface
    /// but a plane, whose coordinates are the frame's lengths.
    pub fn has_pcurves(&self) -> bool {
        !self.s.is_plane()
    }

    /// The B-spline patch of a B-spline face.
    pub fn spline(&self) -> Option<&Spline> {
        match &self.s {
            Surf::Spline(s) => Some(s),
            _ => None,
        }
    }

    fn radius(&self) -> f64 {
        match self.s {
            Surf::Cyl { r, .. } | Surf::Sphere { r, .. } => r,
            _ => 0.0,
        }
    }

    /// The angle of `p` about the frame's z axis (for a swapped torus,
    /// its internal `u`, the tube angle from `t0`).
    pub fn angle(&self, p: V) -> f64 {
        match self.s {
            Surf::Torus { .. } | Surf::Spline(_) => self.uv(p).0,
            _ => self.angle_about_z(p),
        }
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
            Surf::Torus { .. } => {
                let (phi, th) = self.torus_angles(p);
                let rel = wrap_pi(th - self.t0);
                if self.swap {
                    (rel, phi)
                } else {
                    (phi, self.t0 + rel)
                }
            }
            Surf::Spline(ref s) => s.project(p, false),
        }
    }

    pub fn eval(&self, u: f64, w: f64) -> V {
        let dir = |u: f64| self.x * cos(u) + self.y * sin(u);
        match self.s {
            Surf::Plane { .. } => self.o + self.x * u + self.y * w,
            Surf::Cyl { r, .. } => self.o + dir(u) * r + self.z * w,
            Surf::Cone { k, .. } => self.o + dir(u) * (self.r0 + w * k) + self.z * w,
            Surf::Sphere { r, .. } => self.o + dir(u) * (r * cos(w)) + self.z * (r * sin(w)),
            Surf::Torus { .. } => {
                if self.swap {
                    self.torus_eval(w, u + self.t0)
                } else {
                    self.torus_eval(u, w)
                }
            }
            Surf::Spline(ref s) => s.eval(u, w),
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
            Surf::Torus { .. } => {
                if self.swap {
                    let (a, b) = self.torus_derivs(w, u + self.t0);
                    (b, a)
                } else {
                    self.torus_derivs(u, w)
                }
            }
            Surf::Spline(ref s) => {
                let d = s.ders(u, w, 1);
                (d[1][0], d[0][1])
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
        // A ring torus keeps clear of its axis; a spindle torus's apple
        // reaches it at its two poles.
        self.periodic()
            && !matches!(self.s, Surf::Torus { big, r, .. } if big > r)
            && (p - self.o).reject(self.z).len() < tol.max(1e-12 * self.radius())
    }
}

/// `x` moved by whole turns into (-π, π].
pub(crate) fn wrap_pi(x: f64) -> f64 {
    let mut d = x - TAU * (x / TAU).round();
    if d <= -PI {
        d += TAU;
    }
    d
}
