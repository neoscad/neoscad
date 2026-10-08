//! Blend tools: the solids that round or chamfer the edges of a solid
//! when a mesh kernel subtracts them (convex edges) or adds them (concave
//! edges): straight edges, and circles and arcs about an axis
//! ([`Path::Arc`], whose cross-section is revolved: `revolve`).
//!
//! A constant-radius fillet is the envelope of a ball rolling in contact
//! with both faces of an edge. When both faces are swept along the edge (a
//! plane containing it, or a cylinder whose axis is parallel to it), the
//! ball's centre moves on a line and the problem is a 2D one in the plane
//! across the edge: a circle of radius `r` tangent to two lines or
//! circles. Swept along the edge, its arc is a **cylinder**; an
//! equal-distance chamfer's line is a **plane**. Where three filleted
//! edges of equal radius meet at a corner of three planes, the ball
//! touching all three planes leaves a **sphere** patch.
//!
//! Every triangle of a tool names its exact surface, as the
//! [`crate::primitives`] do, so that after the boolean the result
//! reconstructs with true blend faces. The tools are built so that what
//! survives the boolean is only the blend surface (and the caps where a
//! tool is cut by a face of the solid):
//!
//! - a **convex** tool's region is the material between the corner and
//!   the blend, extended outwards past the faces by a margin, so none of
//!   its other faces is coplanar with the solid's (coplanar faces in a
//!   subtraction leave slivers);
//! - a **concave** tool's region is the empty space between the faces and
//!   the blend. Its contact side on a plane lies in that plane (a union
//!   with a coplanar face merges cleanly); on a cylinder it overlaps into
//!   the material by a margin instead, because two coincident curved
//!   faces with different tessellations do not reconstruct.
//!
//! Points the tools of one corner share (the tangent points and the
//! offsets beside them on each face) are computed by the same expressions
//! from the same inputs, so they are the same bits in every tool and the
//! corner patch, and the kernel's union of them has no seam.
//!
//! The arcs' vertices are exactly at both tangent lines: a blend meets its
//! faces along mesh edges, which is what makes the tangency reconstruct.

use crate::math::*;
use crate::model::{Surface, TaggedMesh};

mod revolve;

/// A face beside a blended edge.
#[derive(Clone, Debug, PartialEq)]
pub enum BlendFace {
    /// A plane containing the edge.
    Plane {
        /// A point on it.
        origin: [f64; 3],
        /// Its unit normal, pointing out of the material.
        normal: [f64; 3],
    },
    /// A circular cylinder whose axis is parallel to a straight edge, or
    /// is the axis of a circular one.
    Cylinder {
        /// A point on the axis.
        origin: [f64; 3],
        /// The unit axis.
        axis: [f64; 3],
        /// The radius.
        radius: f64,
        /// The material is inside it (a boss or a rod), so its outward
        /// normal points away from the axis; `false` for a hole.
        convex: bool,
    },
    /// A cone about a circular edge's axis (a countersink, a chamfer made
    /// earlier): circular edges only.
    Cone {
        /// The apex, on the axis.
        apex: [f64; 3],
        /// The unit axis, from the apex into the nappe.
        axis: [f64; 3],
        /// The radius gained per unit length along the axis.
        slope: f64,
        /// The material is inside it, so its outward normal points away
        /// from the axis (tilted by the slope); `false` for a conical hole.
        convex: bool,
    },
    /// A sphere centred on a circular edge's axis: circular edges only.
    Sphere {
        /// The centre.
        center: [f64; 3],
        /// The radius.
        radius: f64,
        /// The material is inside it (a ball); `false` for a spherical
        /// pocket.
        convex: bool,
    },
    /// A ring torus about a circular edge's axis (an earlier blend's):
    /// circular edges only.
    Torus {
        /// The centre.
        center: [f64; 3],
        /// The unit axis.
        axis: [f64; 3],
        /// The distance from the axis to the tube's centre.
        major_radius: f64,
        /// The tube's radius.
        minor_radius: f64,
        /// The material is inside the tube.
        convex: bool,
    },
}

/// Whether `x` is a number above zero (`false` for NaN), as a function so
/// the checks read as what they ask.
fn positive(x: f64) -> bool {
    x > 0.0
}

impl BlendFace {
    /// Whether the face is curved in space (anything but a plane).
    fn curved(&self) -> bool {
        !matches!(self, BlendFace::Plane { .. })
    }
}

/// The curve an edge runs along.
#[derive(Clone, Debug, PartialEq)]
pub enum Path {
    /// The straight segment from `from` to `to`: the tool is the
    /// cross-section swept along it (the translational class).
    Line,
    /// A circular arc about an axis, from `from` turning counter-clockwise
    /// about `axis` by `sweep` (a whole circle when `sweep` is `2π`, and
    /// `to` is then `from`): the tool is the cross-section in the
    /// meridian half-plane through `from`, revolved (the rotational
    /// class). Both faces must be surfaces of revolution about the axis: a
    /// plane perpendicular to it, a coaxial cylinder, cone or torus, or a
    /// sphere centred on it.
    Arc {
        /// The centre of the edge's circle, on the axis.
        center: [f64; 3],
        /// The unit axis.
        axis: [f64; 3],
        /// The circle's radius. The cross-section is taken on the circle
        /// at `from`'s angle, not at `from` itself: a vertex solved where
        /// two faces nearly touch can lie a little off the circle, and the
        /// tool's tangent ring must run through the polygon of the face
        /// beside it, which is the circle's.
        radius: f64,
        /// The angle the edge turns through, in radians, in `(0, 2π]`.
        sweep: f64,
        /// The tool's sections, so that it conforms to the faceted face
        /// it blends into: per vertex of that face's polygon on the edge,
        /// its angle (radians from `from`) and how far it lies outside the
        /// circle (negative inside), by which the whole section is moved
        /// out along its radial. The tool's tangent ring then runs through
        /// the polygon's vertices themselves, which a mesh made from a
        /// slightly different circle (a 2D offset's, rounded on its grid)
        /// has off the exact one by more than a kernel's tolerance. A
        /// partial arc's ends are always sections; angles outside the
        /// sweep are ignored. Empty: regular sections, 32 to a turn.
        sections: Vec<[f64; 2]>,
    },
}

/// What a tool's cross-section is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Profile {
    /// A circular arc tangent to both faces.
    Fillet,
    /// A straight line between the points at the same distance from the
    /// edge on both faces.
    Chamfer,
}

/// How a tool ends at one end of its edge.
#[derive(Clone, Debug, PartialEq)]
pub enum End {
    /// Cut by this plane: a face of the solid the edge runs into, the
    /// plane across a tangent chain, or a mitre. The cap is tagged with
    /// exactly this plane, so that it merges with a face on it.
    Plane {
        /// A point on the plane.
        origin: [f64; 3],
        /// Its unit normal, pointing away from the tool.
        normal: [f64; 3],
    },
    /// The material ends here: the tool runs on into the air past the
    /// end, beyond this face of the solid (or the end point) by the
    /// tool's own size.
    Open {
        /// The end face, when it is a plane: the tool runs past it
        /// wherever it crosses the cross-section.
        face: Option<([f64; 3], [f64; 3])>,
    },
    /// A corner of three filleted edges ([`BlendSpec::corners`], by
    /// index): the tool ends on the plane across the edge through the
    /// ball's centre, where the corner's sphere patch takes over.
    Corner(usize),
    /// The edge runs on smoothly into another selected edge (`with`: its
    /// index and end, which must be a `Chain` back to this one), a line
    /// into an arc of a rounded outline: the tool is cut across the edge
    /// there (by the meridian plane, for an arc). Where the two
    /// cross-sections meet point for point, the two tools are one solid
    /// with no cap between them, as for a [`End::Mitre`].
    Chain {
        /// The other edge and its end.
        with: (usize, usize),
    },
    /// Cut by the plane bisecting this edge and another one that ends
    /// at the same vertex (`with`: its index and end, 0 or 1), which is
    /// cut by the same plane. Where the two cross-sections meet in that
    /// plane point for point (equal angles on both sides), the two tools
    /// are one solid with no cap between them.
    Mitre {
        /// A point on the plane.
        origin: [f64; 3],
        /// Its unit normal, pointing away from this edge's tool.
        normal: [f64; 3],
        /// The other edge and its end.
        with: (usize, usize),
    },
}

/// One straight edge to blend.
#[derive(Clone, Debug, PartialEq)]
pub struct BlendEdge {
    /// The start point.
    pub from: [f64; 3],
    /// The end point.
    pub to: [f64; 3],
    /// The two faces beside it.
    pub faces: [BlendFace; 2],
    /// The caller's names for the two faces: a corner's three edges name
    /// its three faces with these.
    pub face_ids: [u32; 2],
    /// The material angle is under 180°: the tool is subtracted.
    /// Otherwise it is added.
    pub convex: bool,
    /// How the tool ends at `from` and at `to`. A partial arc's tool
    /// ends at the arc's ends whatever the end: an [`End::Plane`] there
    /// must contain the axis (it only names the cap's surface), and
    /// [`End::Open`] runs the tool on past the end by an angle. Arcs take
    /// no [`End::Corner`] or [`End::Mitre`].
    pub ends: [End; 2],
    /// The curve: a line, or an arc about an axis.
    pub path: Path,
    /// The most the tool may reach past a face: into the air beside a
    /// convex edge's faces, or into the material behind a concave edge's
    /// curved faces (whose tessellations a coincident side would not
    /// match). `None`: the blend's size, and half a cylinder's radius or
    /// half the distance of an arc from its axis. A caller that knows how
    /// thin the material behind a face is passes less.
    pub margin: Option<f64>,
}

/// A corner where three filleted edges of the same sense meet, between
/// three planes.
#[derive(Clone, Debug, PartialEq)]
pub struct Corner {
    /// The vertex.
    pub vertex: [f64; 3],
    /// The three edges, as (index in [`BlendSpec::edges`], 0 for its
    /// `from` end or 1 for its `to` end).
    pub edges: [(usize, usize); 3],
}

/// Everything one blending operation asks for.
#[derive(Clone, Debug, PartialEq)]
pub struct BlendSpec {
    /// Fillet or chamfer.
    pub profile: Profile,
    /// The radius, or the chamfer's distance along each face.
    pub size: f64,
    /// The edges.
    pub edges: Vec<BlendEdge>,
    /// The sphere corners (fillets only).
    pub corners: Vec<Corner>,
}

/// What made a tool.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    /// The edge of this index.
    Edge(usize),
    /// The corner of this index.
    Corner(usize),
}

/// One tool solid.
#[derive(Clone, Debug, PartialEq)]
pub struct Tool {
    /// A closed, oriented mesh, each triangle tagged with its surface.
    pub mesh: TaggedMesh,
    /// Add it to the solid (a concave edge), or subtract it.
    pub add: bool,
    /// Which entries of `mesh.surfaces` are the blend: what the result
    /// keeps of the tool. One per edge and corner it was made for: the
    /// edges of a sphere corner and the corner's patch are one tool.
    pub blend: Vec<u32>,
    /// What each entry of `blend` was made for.
    pub sources: Vec<Source>,
}

/// Why a tool could not be made.
#[derive(Clone, Debug, PartialEq)]
pub enum BlendError {
    /// No blend of this size fits in the edge's cross-section: the ball
    /// does not touch both faces on the material's side (a fillet larger
    /// than a boss, a chamfer longer than a face's arc).
    TooLarge(usize),
    /// The tool's two end caps cross: the edge is too short for the
    /// blend at the angles it ends at.
    TooShort(usize),
    /// The input cannot be blended: faces that are not beside the edge,
    /// tangent faces, a corner whose faces are not three planes.
    Invalid(String),
}

impl std::fmt::Display for BlendError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BlendError::TooLarge(e) => write!(f, "edge {e}: the blend does not fit"),
            BlendError::TooShort(e) => write!(f, "edge {e}: the edge is too short"),
            BlendError::Invalid(s) => f.write_str(s),
        }
    }
}

impl std::error::Error for BlendError {}

/// The cross-section of a blend where its edge starts: what a caller
/// checks against the faces before building anything.
#[derive(Clone, Debug, PartialEq)]
pub struct Section {
    /// The fillet's centre (`None` for a chamfer).
    pub center: Option<[f64; 3]>,
    /// The tangent points (a chamfer's ends) on the two faces.
    pub tangents: [[f64; 3]; 2],
    /// Per face, how far the blend reaches into it from the edge: a
    /// distance on a plane, an arc length on a cylinder.
    pub widths: [f64; 2],
}

/// A face in the cross-section, ready for the arithmetic.
#[derive(Clone, Copy, Debug)]
enum F {
    /// A plane: `(x - e0) · n = c`. A face of the solid contains the
    /// edge (`c` is 0 but for rounding); a facet a caller conforms the
    /// blend to need not.
    Line { n: V, c: f64 },
    /// The axis point in the cross-section plane, the radius, and the
    /// sign `s` of the outward normal along the radial direction.
    Circle { a: V, r: f64, s: f64 },
}

impl F {
    /// The outward unit normal at a point of the face.
    fn normal_at(&self, p: V) -> V {
        match *self {
            F::Line { n, .. } => n,
            F::Circle { a, s, .. } => (p - a).norm() * s,
        }
    }
}

/// The edge's frame: `e0` the start, `d` the unit direction (an arc's
/// tangent at its start), `len` the length, and the faces in the
/// cross-section through `e0` (an arc's meridian half-plane there, where a
/// cylinder or a cone is a line and a sphere or a torus a circle).
struct Frame {
    e0: V,
    d: V,
    len: f64,
    f: [F; 2],
    /// Whether each face is curved in space (a cylinder or a cone is a
    /// line in an arc's meridian, but a concave tool's side along it must
    /// still not lie on it).
    curved: [bool; 2],
    /// An arc's axis, when the edge is one.
    arc: Option<ArcFrame>,
}

/// An arc's axis in its frame: `c` the centre, `a` the unit axis, `u0` the
/// unit radial through the edge's start, `rho` the edge's distance from
/// the axis, `sweep` the angle it turns through.
#[derive(Clone, Copy, Debug)]
struct ArcFrame {
    c: V,
    a: V,
    u0: V,
    rho: f64,
    sweep: f64,
}

impl ArcFrame {
    /// The unit radial at angle `t` from the start.
    fn radial(&self, t: f64) -> V {
        self.u0 * cos(t) + self.a.cross(self.u0) * sin(t)
    }
    /// A point's distance from the axis along the start's radial (negative
    /// across the axis) and its height along it.
    fn meridian(&self, p: V) -> (f64, f64) {
        let q = p - self.c;
        (q.dot(self.u0), q.dot(self.a))
    }
    /// The point at meridian coordinates `(rho, z)`, turned by `t`.
    fn at(&self, rho: f64, z: f64, t: f64) -> V {
        self.c + self.radial(t) * rho + self.a * z
    }
}

fn frame(e: &BlendEdge, index: usize) -> Result<Frame, BlendError> {
    if let Path::Arc {
        center,
        axis,
        radius,
        sweep,
        ..
    } = &e.path
    {
        return arc_frame(e, index, V::from(*center), V::from(*axis), *radius, *sweep);
    }
    let e0 = V::from(e.from);
    let e1 = V::from(e.to);
    let len = (e1 - e0).len();
    if !(len > 0.0 && len.is_finite()) {
        return Err(BlendError::Invalid(format!("edge {index} has no length")));
    }
    let d = (e1 - e0) * (1.0 / len);
    let mut f = [F::Line {
        n: V::default(),
        c: 0.0,
    }; 2];
    for (k, face) in e.faces.iter().enumerate() {
        f[k] = match face {
            BlendFace::Plane { origin, normal } => {
                // Used exactly as given, not re-projected across this
                // edge: a corner's tools share points computed from it,
                // and each tool's edge direction differs in the last bits.
                let n = V::from(*normal);
                if n.dot(d).abs() > 1e-6 || (n.len() - 1.0).abs() > 1e-6 {
                    return Err(BlendError::Invalid(format!(
                        "edge {index}: a face's plane does not contain the edge"
                    )));
                }
                F::Line {
                    n,
                    c: (V::from(*origin) - e0).dot(n),
                }
            }
            BlendFace::Cylinder {
                origin,
                axis,
                radius,
                convex,
            } => {
                if V::from(*axis).cross(d).len() > 1e-6 || radius.is_nan() || *radius <= 0.0 {
                    return Err(BlendError::Invalid(format!(
                        "edge {index}: a cylinder beside it is not parallel to it"
                    )));
                }
                let o = V::from(*origin);
                let a = e0 + (o - e0).reject(d);
                F::Circle {
                    a,
                    r: *radius,
                    s: if *convex { 1.0 } else { -1.0 },
                }
            }
            _ => {
                return Err(BlendError::Invalid(format!(
                    "edge {index}: a straight edge's faces are planes and parallel cylinders"
                )));
            }
        };
    }
    Ok(Frame {
        e0,
        d,
        len,
        f,
        curved: [e.faces[0].curved(), e.faces[1].curved()],
        arc: None,
    })
}

/// An arc's frame: the faces in the meridian half-plane through its start.
fn arc_frame(
    e: &BlendEdge,
    index: usize,
    c: V,
    axis: V,
    radius: f64,
    sweep: f64,
) -> Result<Frame, BlendError> {
    let bad = |what: &str| BlendError::Invalid(format!("edge {index}: {what}"));
    if !(axis.is_finite() && (axis.len() - 1.0).abs() < 1e-6) {
        return Err(bad("an arc's axis is not a unit vector"));
    }
    let a = axis.norm();
    if !(sweep > 0.0 && sweep <= TAU * (1.0 + 1e-12)) {
        return Err(bad("an arc turns through no angle"));
    }
    let radial = (V::from(e.from) - c).reject(a);
    let rho = radius;
    if !(rho > 0.0 && rho.is_finite() && radial.len() > 0.0) {
        return Err(bad("an arc lies on its own axis"));
    }
    let u0 = radial.norm();
    let e0 = c + u0 * rho;
    let d = a.cross(u0);
    // How far off the axis a centre may be: rounding in the B-rep, not
    // geometry.
    let tol = 1e-7 * (rho + (e0 - c).len());
    let on_axis = |p: V| (p - c).reject(a).len() <= tol;
    let coaxial = |ax: V| ax.cross(a).len() <= 1e-6;
    let side = |convex: bool| if convex { 1.0 } else { -1.0 };
    let mut f = [F::Line {
        n: V::default(),
        c: 0.0,
    }; 2];
    for (k, face) in e.faces.iter().enumerate() {
        f[k] = match face {
            BlendFace::Plane { origin, normal } => {
                let n = V::from(*normal);
                if n.cross(a).len() > 1e-6 {
                    return Err(bad(
                        "a plane beside an arc is not perpendicular to its axis",
                    ));
                }
                F::Line {
                    n,
                    c: (V::from(*origin) - e0).dot(n),
                }
            }
            BlendFace::Cylinder {
                origin,
                axis,
                radius,
                convex,
            } => {
                if !coaxial(V::from(*axis)) || !on_axis(V::from(*origin)) || !positive(*radius) {
                    return Err(bad("a cylinder beside an arc is not about its axis"));
                }
                // In the meridian, the line at `radius` from the axis.
                let s = side(*convex);
                F::Line {
                    n: u0 * s,
                    c: s * (radius - rho),
                }
            }
            BlendFace::Cone {
                apex,
                axis,
                slope,
                convex,
            } => {
                let ax = V::from(*axis);
                if !coaxial(ax) || !on_axis(V::from(*apex)) || !positive(*slope) {
                    return Err(bad("a cone beside an arc is not about its axis"));
                }
                // The generator through the apex runs along `ax + slope
                // u0`; the outward normal is across it.
                let n = (u0 - ax * *slope).norm() * side(*convex);
                F::Line {
                    n,
                    c: (V::from(*apex) - e0).dot(n),
                }
            }
            BlendFace::Sphere {
                center,
                radius,
                convex,
            } => {
                let sc = V::from(*center);
                if !on_axis(sc) || !positive(*radius) {
                    return Err(bad("a sphere beside an arc is not centred on its axis"));
                }
                F::Circle {
                    a: sc,
                    r: *radius,
                    s: side(*convex),
                }
            }
            BlendFace::Torus {
                center,
                axis,
                major_radius,
                minor_radius,
                convex,
            } => {
                let tc = V::from(*center);
                if !coaxial(V::from(*axis)) || !on_axis(tc) || !positive(*minor_radius) {
                    return Err(bad("a torus beside an arc is not about its axis"));
                }
                // The tube's circle in this meridian.
                F::Circle {
                    a: tc + u0 * *major_radius,
                    r: *minor_radius,
                    s: side(*convex),
                }
            }
        };
    }
    Ok(Frame {
        e0,
        d,
        len: rho * sweep,
        f,
        curved: [e.faces[0].curved(), e.faces[1].curved()],
        arc: Some(ArcFrame {
            c,
            a,
            u0,
            rho,
            sweep: sweep.min(TAU),
        }),
    })
}

/// The points at signed offset `h_k` along the outward normal from face
/// `k`, for both faces at once (in the plane across the edge through
/// `e0`), nearest `near`. Lines: `(x - e0)·n = c + h`; circles: radius
/// `r + s h`.
fn meet(fr: &Frame, h: [f64; 2], near: V) -> Option<V> {
    let d = fr.d;
    match (fr.f[0], fr.f[1]) {
        (F::Line { n: na, c: ca }, F::Line { n: nb, c: cb }) => {
            let h = [h[0] + ca, h[1] + cb];
            let c = na.dot(nb);
            let det = 1.0 - c * c;
            if det < 1e-12 {
                return None;
            }
            let al = (h[0] - c * h[1]) / det;
            let be = (h[1] - c * h[0]) / det;
            Some(fr.e0 + na * al + nb * be)
        }
        (F::Line { n, c: cl }, F::Circle { a, r, s })
        | (F::Circle { a, r, s }, F::Line { n, c: cl }) => {
            let (hl, hc) = if matches!(fr.f[0], F::Line { .. }) {
                (h[0] + cl, h[1])
            } else {
                (h[1] + cl, h[0])
            };
            let rho = r + s * hc;
            if rho <= 0.0 {
                return None;
            }
            let p0 = fr.e0 + n * hl;
            let u = n.cross(d).norm();
            let w = p0 - a;
            let b = w.dot(u);
            let c = w.dot(w) - rho * rho;
            let disc = b * b - c;
            if disc < 0.0 {
                return None;
            }
            let sq = disc.sqrt();
            let x1 = p0 + u * (-b - sq);
            let x2 = p0 + u * (-b + sq);
            Some(nearest(x1, x2, near))
        }
        (
            F::Circle {
                a: a1,
                r: r1,
                s: s1,
            },
            F::Circle {
                a: a2,
                r: r2,
                s: s2,
            },
        ) => {
            let (p, q) = (r1 + s1 * h[0], r2 + s2 * h[1]);
            if p <= 0.0 || q <= 0.0 {
                return None;
            }
            let dv = a2 - a1;
            let dist = dv.len();
            if dist <= 0.0 || dist > p + q || dist < (p - q).abs() {
                return None;
            }
            let x = (dist * dist + p * p - q * q) / (2.0 * dist);
            let y2 = p * p - x * x;
            if y2 < 0.0 {
                return None;
            }
            let ux = dv * (1.0 / dist);
            let uy = d.cross(ux).norm();
            let base = a1 + ux * x;
            let y = y2.sqrt();
            Some(nearest(base + uy * y, base - uy * y, near))
        }
    }
}

fn nearest(a: V, b: V, p: V) -> V {
    let (da, db) = ((a - p).len(), (b - p).len());
    if da < db || (da == db && a.lex_cmp(b).is_le()) {
        a
    } else {
        b
    }
}

/// The unit direction, at the edge, into face `k` and away from the edge,
/// across the edge.
fn into_face(fr: &Frame, k: usize, convex: bool) -> V {
    let n = fr.f[k].normal_at(fr.e0);
    let other = fr.f[1 - k].normal_at(fr.e0);
    let w = n.cross(fr.d).norm();
    // Moving into a face of a convex edge goes against the other face's
    // outward normal; into a face of a concave edge, along it.
    let s = w.dot(other);
    if (convex && s > 0.0) || (!convex && s < 0.0) {
        w * -1.0
    } else {
        w
    }
}

/// The signed angle about `axis` from `from` to `to` (unit vectors
/// across the axis), in `(-π, π]`.
fn angle_about(axis: V, from: V, to: V) -> f64 {
    atan2(from.cross(to).dot(axis), from.dot(to))
}

/// The geometry of one edge's cross-section, relative to its base point:
/// the fillet's centre, or the edge's start for a chamfer.
struct Prof {
    /// The base point at the start cross-section.
    base: V,
    /// The blend's points, from face 0's tangent to face 1's, as offsets
    /// from the base.
    arc: Vec<V>,
    /// The rest of the region: from beside face 1's tangent around to
    /// beside face 0's, as offsets from the base.
    rest: Vec<V>,
    /// Where in `rest` the points beside the tangents and the corner
    /// offset are (`p1` first, `p0` last, each `None` when the offset is
    /// zero and the point is the tangent itself).
    p: [Option<usize>; 2],
    q: usize,
    /// The outward unit normals of the faces at the tangents.
    nu: [V; 2],
    /// The offsets of the two faces' sides of the region.
    mu: [f64; 2],
    sigma: f64,
    section: Section,
}

impl Prof {
    fn ring(&self) -> Vec<V> {
        self.arc.iter().chain(self.rest.iter()).copied().collect()
    }
}

/// How far an edge's tool reaches past a face into the air, and into the
/// material behind a curved face of a concave edge: the size, at most
/// half a circle's radius in the cross-section, at most half an arc's
/// distance from its axis (so the revolved region stays clear of the
/// axis), and at most the caller's [`BlendEdge::margin`].
fn natural_margin(size: f64, fr: &Frame, cap: Option<f64>) -> f64 {
    let mut m = size;
    for x in &fr.f {
        if let F::Circle { r, .. } = x {
            m = m.min(*r * 0.5);
        }
    }
    if let Some(a) = fr.arc {
        m = m.min(a.rho * 0.5);
    }
    if let Some(c) = cap
        && c > 0.0
    {
        m = m.min(c);
    }
    m
}

/// What [`tools`] settles across the edges before making each one's
/// cross-section: the margin, equal along a tangent chain so that the
/// cross-sections of its tools meet point for point, and which plane
/// faces of a concave edge its tool overlaps into rather than lies in
/// (those whose chained neighbour's matching face is curved).
#[derive(Clone, Copy, Debug)]
struct Adjust {
    margin: f64,
    overlap: [bool; 2],
}

/// The offset of a point beside a tangent: `σ r ν + μ ν`, the one
/// expression every tool and corner patch uses, so equal inputs give the
/// same bits.
fn beside(nu: V, sigma: f64, r: f64, mu: f64) -> V {
    nu * (sigma * r) + nu * mu
}

fn tangent_offset(nu: V, sigma: f64, r: f64) -> V {
    nu * (sigma * r)
}

fn profile(
    spec: &BlendSpec,
    e: &BlendEdge,
    index: usize,
    segments: &dyn Fn(f64) -> u32,
    adjust: Option<Adjust>,
) -> Result<(Frame, Prof), BlendError> {
    let fr = frame(e, index)?;
    let size = spec.size;
    if !(size > 0.0 && size.is_finite()) {
        return Err(BlendError::Invalid("the size must be positive".into()));
    }
    let sigma = if e.convex { 1.0 } else { -1.0 };
    let w = [into_face(&fr, 0, e.convex), into_face(&fr, 1, e.convex)];
    // Base point, tangents and their normals, absolute for now.
    let (base, t, nu, arc_abs): (V, [V; 2], [V; 2], Option<V>) = match spec.profile {
        Profile::Fillet => {
            let c = meet(&fr, [-sigma * size, -sigma * size], fr.e0)
                .ok_or(BlendError::TooLarge(index))?;
            let nu = [fr.f[0].normal_at(c), fr.f[1].normal_at(c)];
            // A ball on a circle's material side: its centre is off the
            // axis, so the normal at the tangent is the centre's radial.
            let nu = [
                match fr.f[0] {
                    F::Circle { a, s, .. } => (c - a).norm() * s,
                    _ => nu[0],
                },
                match fr.f[1] {
                    F::Circle { a, s, .. } => (c - a).norm() * s,
                    _ => nu[1],
                },
            ];
            let t = [
                c + tangent_offset(nu[0], sigma, size),
                c + tangent_offset(nu[1], sigma, size),
            ];
            (c, t, nu, Some(c))
        }
        Profile::Chamfer => {
            let mut t = [V::default(); 2];
            let mut nu = [V::default(); 2];
            for k in 0..2 {
                match fr.f[k] {
                    F::Line { n, c } => {
                        t[k] = fr.e0 + n * c + w[k] * size;
                        nu[k] = n;
                    }
                    F::Circle { a, r, s } => {
                        let rad = (fr.e0 - a).norm();
                        let phi = size / r;
                        if phi >= PI {
                            return Err(BlendError::TooLarge(index));
                        }
                        // Turn the radial towards the face's direction.
                        let tan = fr.d.cross(rad);
                        let dir = if tan.dot(w[k]) >= 0.0 { 1.0 } else { -1.0 };
                        let ry = tan * dir;
                        let p = a + (rad * cos(phi) + ry * sin(phi)) * r;
                        t[k] = p;
                        nu[k] = (p - a).norm() * s;
                    }
                }
            }
            (fr.e0, t, nu, None)
        }
    };
    // Widths: how far into each face, the right way.
    let mut widths = [0.0; 2];
    for k in 0..2 {
        widths[k] = match fr.f[k] {
            F::Line { .. } => (t[k] - fr.e0).dot(w[k]),
            F::Circle { a, r, .. } => {
                let from = (fr.e0 - a).norm();
                let to = (t[k] - a).norm();
                let ang = angle_about(fr.d, from, to);
                let s = if fr.d.cross(from).dot(w[k]) >= 0.0 {
                    1.0
                } else {
                    -1.0
                };
                ang * s * r
            }
        };
        if widths[k].is_nan() || widths[k] <= 0.0 {
            return Err(BlendError::TooLarge(index));
        }
    }
    let section = Section {
        center: arc_abs.map(|c| c.arr()),
        tangents: [t[0].arr(), t[1].arr()],
        widths,
    };
    // The blend's points as offsets from the base.
    let arc: Vec<V> = match spec.profile {
        Profile::Fillet => {
            let a0 = tangent_offset(nu[0], sigma, size);
            let a1 = tangent_offset(nu[1], sigma, size);
            let u = a0 * (1.0 / size);
            let b = a1 * (1.0 / size);
            let sweep = atan2(u.cross(b).len(), u.dot(b));
            if sweep.is_nan() || sweep <= 1e-9 {
                return Err(BlendError::Invalid(format!(
                    "edge {index}: its faces are tangent"
                )));
            }
            // The arc between the tangents that faces the edge.
            if (fr.e0 - base).dot(u + b) <= 0.0 {
                return Err(BlendError::TooLarge(index));
            }
            let v2 = (b - u * u.dot(b)).norm();
            let n = segments(sweep).max(1) as usize;
            let mut pts = Vec::with_capacity(n + 1);
            pts.push(a0);
            for i in 1..n {
                let th = sweep * i as f64 / n as f64;
                pts.push((u * cos(th) + v2 * sin(th)) * size);
            }
            pts.push(a1);
            pts
        }
        Profile::Chamfer => vec![t[0] - base, t[1] - base],
    };
    // The rest of the region.
    let adjust = adjust.unwrap_or(Adjust {
        margin: natural_margin(size, &fr, e.margin),
        overlap: [false; 2],
    });
    let m = adjust.margin;
    let mut mu = [0.0; 2];
    for k in 0..2 {
        mu[k] = if e.convex {
            m
        } else if fr.curved[k] || adjust.overlap[k] {
            -m
        } else {
            0.0
        };
    }
    // The corner point: both faces at their offsets. Relative to the base
    // a plane face is at `σ r` (fillet) or 0 (chamfer, base on the edge).
    let q_abs = meet(&fr, mu, fr.e0).ok_or(BlendError::TooLarge(index))?;
    let q_off = match (spec.profile, fr.f) {
        // Planes: by the offsets' own expression from the base, the one
        // a corner patch uses too.
        (Profile::Fillet, [F::Line { n: na, .. }, F::Line { n: nb, .. }]) => {
            let hc = [sigma * size + mu[0], sigma * size + mu[1]];
            let c = na.dot(nb);
            let det = 1.0 - c * c;
            na * ((hc[0] - c * hc[1]) / det) + nb * ((hc[1] - c * hc[0]) / det)
        }
        _ => q_abs - base,
    };
    let mut rest = Vec::new();
    let mut p = [None, None];
    let side = |k: usize, from: V, to: V, out: &mut Vec<V>| {
        // Points along face k's offset curve strictly between two
        // offsets (only curved faces need them).
        if let F::Circle { a, r, s } = fr.f[k] {
            let rho = r + s * mu[k];
            let ac = a - base;
            let f0 = (from - ac).norm();
            let f1 = (to - ac).norm();
            let ang = angle_about(fr.d, f0, f1);
            let n = (ang.abs() / (PI / 16.0)).ceil() as usize;
            let y = fr.d.cross(f0);
            for i in 1..n {
                let th = ang * i as f64 / n as f64;
                out.push(ac + (f0 * cos(th) + y * sin(th)) * rho);
            }
        }
    };
    let last = *arc.last().expect("an arc has two ends");
    let first = arc[0];
    let p1 = if mu[1] != 0.0 {
        let v = match spec.profile {
            Profile::Fillet => beside(nu[1], sigma, size, mu[1]),
            Profile::Chamfer => last + nu[1] * mu[1],
        };
        p[1] = Some(rest.len());
        rest.push(v);
        v
    } else {
        last
    };
    side(1, p1, q_off, &mut rest);
    let q = rest.len();
    rest.push(q_off);
    let p0 = if mu[0] != 0.0 {
        match spec.profile {
            Profile::Fillet => beside(nu[0], sigma, size, mu[0]),
            Profile::Chamfer => first + nu[0] * mu[0],
        }
    } else {
        first
    };
    side(0, q_off, p0, &mut rest);
    if mu[0] != 0.0 {
        p[0] = Some(rest.len());
        rest.push(p0);
    }
    if let Some(a) = fr.arc {
        // Revolved, a point across the axis would turn the region inside
        // out. And the blend must be a ring torus: its centre further
        // from the axis than its radius (a boss's convex rim blend up to
        // half the boss's radius), the only torus a B-rep writes.
        let eps = 1e-9 * (a.rho + size);
        if arc
            .iter()
            .chain(rest.iter())
            .any(|q| !positive(a.meridian(base + *q).0 - eps))
        {
            return Err(BlendError::TooLarge(index));
        }
        if spec.profile == Profile::Fillet && !positive(a.meridian(base).0 - size * (1.0 + 1e-9)) {
            return Err(BlendError::TooLarge(index));
        }
    }
    Ok((
        fr,
        Prof {
            base,
            arc,
            rest,
            p,
            q,
            nu,
            mu,
            sigma,
            section,
        },
    ))
}

/// The cross-section of `spec`'s edge `index` where it starts: its
/// tangent points and how far they reach into each face.
pub fn section(spec: &BlendSpec, index: usize) -> Result<Section, BlendError> {
    let e = spec
        .edges
        .get(index)
        .ok_or_else(|| BlendError::Invalid(format!("no edge {index}")))?;
    profile(spec, e, index, &|_| 1, None).map(|(_, p)| p.section)
}

/// The ball's centre at a corner: `σ r` inside each of its three planes.
fn corner_center(planes: &[(V, V); 3], sigma: f64, r: f64) -> Option<V> {
    let mut a = [[0.0; 3]; 3];
    let mut b = [0.0; 3];
    for (i, (o, n)) in planes.iter().enumerate() {
        a[i] = [n.x, n.y, n.z];
        b[i] = n.dot(*o) - sigma * r;
    }
    let x = solve_dense(&mut a, &mut b, 3)?;
    Some(v(x[0], x[1], x[2]))
}

/// The point at offset `h_i` from each of three planes through `base`
/// (relative to it): `w · n_i = h_i`.
fn corner_offset(n: [V; 3], h: [f64; 3]) -> Option<V> {
    let mut a = [[0.0; 3]; 3];
    let mut b = h;
    for i in 0..3 {
        a[i] = [n[i].x, n[i].y, n[i].z];
    }
    let x = solve_dense(&mut a, &mut b, 3)?;
    Some(v(x[0], x[1], x[2]))
}

/// The tools for `spec`: one per edge, except that the edges of a sphere
/// corner, the corner's patch, and mitred pairs that meet point for point
/// are one tool each, in the order of their lowest edge ([`Tool::sources`]
/// says what each blend surface is for). `segments(sweep)` is how many
/// segments a fillet arc sweeping `sweep` radians gets (at least 1).
pub fn tools(spec: &BlendSpec, segments: &dyn Fn(f64) -> u32) -> Result<Vec<Tool>, BlendError> {
    let adjust = adjustments(spec)?;
    let mut profs = Vec::with_capacity(spec.edges.len());
    for (i, e) in spec.edges.iter().enumerate() {
        profs.push(profile(spec, e, i, segments, Some(adjust[i]))?);
    }
    // Corners: the ball's centre, and which face is which.
    let sigma_of = |i: usize| if spec.edges[i].convex { 1.0 } else { -1.0 };
    let mut centres = Vec::with_capacity(spec.corners.len());
    for (ci, c) in spec.corners.iter().enumerate() {
        if spec.profile != Profile::Fillet {
            return Err(BlendError::Invalid(format!(
                "corner {ci}: only fillets have sphere corners"
            )));
        }
        let planes = corner_planes(spec, c, ci)?;
        let sigma = sigma_of(c.edges[0].0);
        if c.edges.iter().any(|&(e, _)| sigma_of(e) != sigma) {
            return Err(BlendError::Invalid(format!(
                "corner {ci}: its edges are not all convex or all concave"
            )));
        }
        let s = corner_center(&planes.map(|(_, o, n)| (o, n)), sigma, spec.size)
            .ok_or_else(|| BlendError::Invalid(format!("corner {ci}: its planes do not meet")))?;
        centres.push((s, planes, sigma));
    }
    let mut ends: Vec<([Vec<V>; 2], [Surface; 2])> = Vec::with_capacity(spec.edges.len());
    let mut corner_rings: Vec<Vec<(usize, Vec<V>)>> = vec![Vec::new(); spec.corners.len()];
    // An arc's section angles, its ends included.
    let mut angles: Vec<Vec<revolve::Sect>> = vec![Vec::new(); spec.edges.len()];
    for (i, (e, (fr, prof))) in spec.edges.iter().zip(&profs).enumerate() {
        if let Some(a) = fr.arc {
            let (t, r, c) = revolve::arc_ends(e, i, fr, &a, prof)?;
            angles[i] = t;
            ends.push((r, c));
            continue;
        }
        let ring = prof.ring();
        let k = ring.len();
        let ext = 2.0 * ring.iter().map(|q| q.len()).fold(0.0, f64::max) + spec.size;
        let mut rings: [Vec<V>; 2] = [Vec::new(), Vec::new()];
        let mut caps: [Surface; 2] = [Surface::Faceted, Surface::Faceted];
        for end in 0..2 {
            let out_dir = if end == 0 { fr.d * -1.0 } else { fr.d };
            let along = |p: V, n: V| -> Option<Vec<V>> {
                let dn = fr.d.dot(n);
                if dn.abs() < 1e-6 {
                    return None;
                }
                Some(
                    ring.iter()
                        .map(|q| {
                            let x = prof.base + *q;
                            x + fr.d * ((p - x).dot(n) / dn)
                        })
                        .collect(),
                )
            };
            match &e.ends[end] {
                End::Plane { origin, normal } | End::Mitre { origin, normal, .. } => {
                    let (o, n) = (V::from(*origin), V::from(*normal));
                    rings[end] = along(o, n).ok_or_else(|| {
                        BlendError::Invalid(format!("edge {i}: an end plane runs along the edge"))
                    })?;
                    caps[end] = Surface::Plane {
                        origin: *origin,
                        normal: *normal,
                    };
                }
                End::Chain { .. } => {
                    // Across the edge at its end.
                    let o = if end == 0 { fr.e0 } else { V::from(e.to) };
                    rings[end] = along(o, out_dir).ok_or_else(|| {
                        BlendError::Invalid(format!("edge {i}: an end plane runs along the edge"))
                    })?;
                    caps[end] = Surface::Plane {
                        origin: o.arr(),
                        normal: out_dir.arr(),
                    };
                }
                End::Open { face } => {
                    // The furthest the end face reaches along the edge
                    // within the cross-section, then the tool's size on.
                    let ts: Vec<f64> = match face {
                        Some((o, n)) => match along(V::from(*o), V::from(*n)) {
                            Some(r) => r.iter().map(|x| (*x - fr.e0).dot(fr.d)).collect(),
                            None => vec![if end == 0 { 0.0 } else { fr.len }],
                        },
                        None => vec![if end == 0 { 0.0 } else { fr.len }],
                    };
                    let t = if end == 0 {
                        ts.iter().copied().fold(f64::INFINITY, f64::min) - ext
                    } else {
                        ts.iter().copied().fold(f64::NEG_INFINITY, f64::max) + ext
                    };
                    let o = fr.e0 + fr.d * t;
                    rings[end] = ring
                        .iter()
                        .map(|q| {
                            let x = prof.base + *q;
                            x + fr.d * (o - x).dot(fr.d)
                        })
                        .collect();
                    caps[end] = Surface::Plane {
                        origin: o.arr(),
                        normal: out_dir.arr(),
                    };
                }
                End::Corner(ci) => {
                    let Some((s, _, _)) = centres.get(*ci) else {
                        return Err(BlendError::Invalid(format!("edge {i}: no corner {ci}")));
                    };
                    rings[end] = ring.iter().map(|q| *s + *q).collect();
                    caps[end] = Surface::Plane {
                        origin: s.arr(),
                        normal: out_dir.arr(),
                    };
                    corner_rings[*ci].push((i, rings[end].clone()));
                }
            }
        }
        // The caps must not cross inside the tool.
        for j in 0..k {
            let t0 = (rings[0][j] - fr.e0).dot(fr.d);
            let t1 = (rings[1][j] - fr.e0).dot(fr.d);
            if t1.is_nan() || t1 <= t0 + 1e-9 * (fr.len + ext) {
                return Err(BlendError::TooShort(i));
            }
        }
        ends.push((rings, caps));
    }
    // The edges of a corner and its patch are one solid, so that the caps
    // they share there are not faces at all: as faces of two operands they
    // coincide exactly only on exact coordinates, and once a rotation has
    // rounded them apart the boolean leaves slivers between them.
    let mut uf = crate::math::UnionFind::new(spec.edges.len());
    for c in &spec.corners {
        uf.join(c.edges[0].0, c.edges[1].0);
        uf.join(c.edges[0].0, c.edges[2].0);
    }
    // Mitred pairs and tangent chains, the same way, where their
    // cross-sections meet point for point: the later edge takes the
    // earlier one's ring there (the same bits), and neither has a cap.
    let mut joined = vec![[false; 2]; spec.edges.len()];
    for i in 0..spec.edges.len() {
        for end in 0..2 {
            let (j, je) = match spec.edges[i].ends[end] {
                End::Mitre { with, .. } | End::Chain { with } => with,
                _ => continue,
            };
            let back = spec.edges.get(j).is_some_and(|o| {
                je < 2
                    && matches!(o.ends[je], End::Mitre { with, .. } | End::Chain { with } if with == (i, end))
            });
            if j <= i || !back {
                continue;
            }
            let n_arc = profs[i].1.arc.len();
            let ri = ends[i].0[end].clone();
            let rj = &ends[j].0[je];
            if ri.len() != rj.len() || profs[j].1.arc.len() != n_arc || ri.is_empty() {
                continue;
            }
            let len = ri.len();
            // The arcs run the same way, or one runs from the other's far
            // face (its faces listed the other way round).
            let map = |rev: bool, k: usize| {
                if !rev {
                    k
                } else if k < n_arc {
                    n_arc - 1 - k
                } else {
                    n_arc + (len - 1 - k)
                }
            };
            let tol = 1e-7 * (spec.size + profs[i].0.len + profs[j].0.len);
            let fits = |rev: bool| (0..len).all(|k| (rj[k] - ri[map(rev, k)]).len() <= tol);
            let Some(rev) = [false, true].into_iter().find(|&r| fits(r)) else {
                continue;
            };
            ends[j].0[je] = (0..len).map(|k| ri[map(rev, k)]).collect();
            joined[i][end] = true;
            joined[j][je] = true;
            uf.join(i, j);
        }
    }
    let mut out = Vec::new();
    for root in 0..spec.edges.len() {
        if uf.find(root) != root {
            continue;
        }
        let mut m = Mesh::new();
        let mut blend = Vec::new();
        for i in 0..spec.edges.len() {
            if uf.find(i) != root {
                continue;
            }
            let (fr, prof) = &profs[i];
            let (rings, caps) = &ends[i];
            let e = &spec.edges[i];
            let skip = [0, 1].map(|k| matches!(e.ends[k], End::Corner(_)) || joined[i][k]);
            let b = match fr.arc {
                Some(a) => {
                    revolve::arc_into(&mut m, spec, e, fr, &a, prof, &angles[i], rings, caps, skip)?
                }
                None => edge_into(&mut m, spec, e, fr, prof, rings, caps, skip)?,
            };
            blend.push((b, Source::Edge(i)));
        }
        for (ci, c) in spec.corners.iter().enumerate() {
            if uf.find(c.edges[0].0) != root {
                continue;
            }
            let (s, planes, sigma) = centres[ci];
            let b = corner_into(
                &mut m,
                spec,
                c,
                ci,
                s,
                &planes,
                sigma,
                &profs,
                &corner_rings[ci],
            )?;
            blend.push((b, Source::Corner(ci)));
        }
        let what = if blend.len() == 1 {
            format!("the tool of edge {root}")
        } else {
            format!("the tools joined at the corners of edge {root}")
        };
        let mesh = m.finish(&what)?;
        out.push(Tool {
            mesh,
            add: !spec.edges[root].convex,
            blend: blend.iter().map(|b| b.0).collect(),
            sources: blend.iter().map(|b| b.1).collect(),
        });
    }
    Ok(out)
}

/// The margins and overlaps of every edge ([`Adjust`]): each edge's own
/// margin, then the smallest along each tangent chain of selected edges,
/// and, for a concave chain, the plane face of one tool that matches a
/// curved face of its neighbour's overlaps too.
fn adjustments(spec: &BlendSpec) -> Result<Vec<Adjust>, BlendError> {
    let mut out = Vec::with_capacity(spec.edges.len());
    let mut frames = Vec::with_capacity(spec.edges.len());
    for (i, e) in spec.edges.iter().enumerate() {
        let fr = frame(e, i)?;
        out.push(Adjust {
            margin: natural_margin(spec.size, &fr, e.margin),
            overlap: [false; 2],
        });
        frames.push(fr);
    }
    let mut uf = crate::math::UnionFind::new(spec.edges.len());
    for (i, e) in spec.edges.iter().enumerate() {
        for end in &e.ends {
            let End::Chain { with: (j, _) } = *end else {
                continue;
            };
            if j >= spec.edges.len() {
                return Err(BlendError::Invalid(format!(
                    "edge {i}: chained to no edge {j}"
                )));
            }
            uf.join(i, j);
            let o = &spec.edges[j];
            if e.convex || o.convex {
                continue;
            }
            // The face each has that the other does not: the side the
            // chain turns along (a plane beside a line, a cylinder beside
            // the arc it runs into).
            let own = |x: &BlendEdge, y: &BlendEdge| {
                (0..2).find(|&k| !y.face_ids.contains(&x.face_ids[k]))
            };
            if let (Some(ki), Some(kj)) = (own(e, o), own(o, e)) {
                if frames[j].curved[kj] {
                    out[i].overlap[ki] = true;
                }
                if frames[i].curved[ki] {
                    out[j].overlap[kj] = true;
                }
            }
        }
    }
    let mut least: Vec<f64> = vec![f64::INFINITY; spec.edges.len()];
    for i in 0..spec.edges.len() {
        let r = uf.find(i);
        least[r] = least[r].min(out[i].margin);
    }
    for i in 0..spec.edges.len() {
        out[i].margin = least[uf.find(i)];
    }
    Ok(out)
}

/// A corner's three planes: (face id, origin, outward normal).
fn corner_planes(spec: &BlendSpec, c: &Corner, ci: usize) -> Result<[(u32, V, V); 3], BlendError> {
    let mut planes: Vec<(u32, V, V)> = Vec::new();
    for &(ei, _) in &c.edges {
        let e = spec
            .edges
            .get(ei)
            .ok_or_else(|| BlendError::Invalid(format!("corner {ci}: no edge {ei}")))?;
        for k in 0..2 {
            let BlendFace::Plane { origin, normal } = &e.faces[k] else {
                return Err(BlendError::Invalid(format!(
                    "corner {ci}: a face there is not a plane"
                )));
            };
            if !planes.iter().any(|p| p.0 == e.face_ids[k]) {
                planes.push((e.face_ids[k], V::from(*origin), V::from(*normal)));
            }
        }
    }
    if planes.len() != 3 {
        return Err(BlendError::Invalid(format!(
            "corner {ci}: its edges are not between three faces"
        )));
    }
    Ok([planes[0], planes[1], planes[2]])
}

/// A mesh being built: positions deduplicated by their exact bits, so
/// points computed alike in several faces are one vertex.
struct Mesh {
    m: TaggedMesh,
    index: std::collections::HashMap<[u64; 3], u32>,
}

impl Mesh {
    fn new() -> Mesh {
        Mesh {
            m: TaggedMesh::default(),
            index: std::collections::HashMap::new(),
        }
    }

    fn vert(&mut self, p: V) -> u32 {
        let key = [p.x.to_bits(), p.y.to_bits(), p.z.to_bits()];
        if let Some(&i) = self.index.get(&key) {
            return i;
        }
        let i = self.m.positions.len() as u32;
        self.m.positions.push(p.arr());
        self.index.insert(key, i);
        i
    }

    fn surf(&mut self, s: Surface) -> u32 {
        self.m.surfaces.push(s);
        (self.m.surfaces.len() - 1) as u32
    }

    fn tri(&mut self, t: [u32; 3], s: u32) {
        if t[0] != t[1] && t[1] != t[2] && t[0] != t[2] {
            self.m.triangles.push(t);
            self.m.triangle_surface.push(s);
        }
    }

    /// A planar polygon, triangulated, facing `out`.
    fn polygon(&mut self, pts: &[V], out: V, s: u32) -> Result<(), BlendError> {
        let ids: Vec<u32> = pts.iter().map(|p| self.vert(*p)).collect();
        // Drop repeated neighbours (a zero offset).
        let mut poly: Vec<(u32, V)> = Vec::with_capacity(pts.len());
        for (i, p) in ids.iter().zip(pts) {
            if poly.last().is_some_and(|l| l.0 == *i) {
                continue;
            }
            poly.push((*i, *p));
        }
        while poly.len() > 1 && poly[0].0 == poly[poly.len() - 1].0 {
            poly.pop();
        }
        if poly.len() < 3 {
            return Ok(());
        }
        let x = out.perp();
        let y = out.cross(x);
        let p2: Vec<[f64; 2]> = poly.iter().map(|(_, p)| [p.dot(x), p.dot(y)]).collect();
        let tris = ear_clip(&p2)
            .ok_or_else(|| BlendError::Invalid("a tool face is not a simple polygon".into()))?;
        for t in tris {
            self.tri([poly[t[0]].0, poly[t[1]].0, poly[t[2]].0], s);
        }
        Ok(())
    }

    fn volume(&self) -> f64 {
        let p = &self.m.positions;
        self.m
            .triangles
            .iter()
            .map(|t| {
                let [a, b, c] = t.map(|i| V::from(p[i as usize]));
                a.dot(b.cross(c)) / 6.0
            })
            .sum()
    }

    /// Whether every edge is used once in each direction.
    fn closed(&self) -> bool {
        let mut count: std::collections::HashMap<(u32, u32), i32> =
            std::collections::HashMap::new();
        for t in &self.m.triangles {
            for k in 0..3 {
                let (a, b) = (t[k], t[(k + 1) % 3]);
                *count.entry((a.min(b), a.max(b))).or_insert(0) += if a < b { 1 } else { -1 };
            }
        }
        let mut uses: std::collections::HashMap<(u32, u32), u32> = std::collections::HashMap::new();
        for t in &self.m.triangles {
            for k in 0..3 {
                let (a, b) = (t[k], t[(k + 1) % 3]);
                *uses.entry((a.min(b), a.max(b))).or_insert(0) += 1;
            }
        }
        count.values().all(|&c| c == 0) && uses.values().all(|&u| u == 2)
    }

    fn finish(mut self, what: &str) -> Result<TaggedMesh, BlendError> {
        if !self.closed() {
            return Err(BlendError::Invalid(format!("{what} is not closed")));
        }
        if self.volume() < 0.0 {
            for t in &mut self.m.triangles {
                t.swap(1, 2);
            }
        }
        Ok(self.m)
    }
}

/// Ear clipping of a simple polygon (either winding); triangles are in
/// counter-clockwise order. `None` when no ear is found (a polygon that
/// crosses itself).
fn ear_clip(p: &[[f64; 2]]) -> Option<Vec<[usize; 3]>> {
    let n = p.len();
    let area: f64 = (0..n)
        .map(|i| {
            let (a, b) = (p[i], p[(i + 1) % n]);
            a[0] * b[1] - a[1] * b[0]
        })
        .sum();
    let mut idx: Vec<usize> = (0..n).collect();
    if area < 0.0 {
        idx.reverse();
    }
    let cross = |a: [f64; 2], b: [f64; 2], c: [f64; 2]| {
        (b[0] - a[0]) * (c[1] - a[1]) - (b[1] - a[1]) * (c[0] - a[0])
    };
    let scale = p
        .iter()
        .flat_map(|q| [q[0].abs(), q[1].abs()])
        .fold(0.0, f64::max)
        .max(1e-300);
    let eps = 1e-14 * scale * scale;
    let mut out = Vec::with_capacity(n.saturating_sub(2));
    while idx.len() > 3 {
        let m = idx.len();
        let mut found = None;
        // A proper ear first; a degenerate (collinear) one only if
        // nothing else is left.
        for pass in 0..2 {
            for i in 0..m {
                let (a, b, c) = (idx[(i + m - 1) % m], idx[i], idx[(i + 1) % m]);
                let cr = cross(p[a], p[b], p[c]);
                if pass == 0 && cr <= eps {
                    continue;
                }
                if pass == 1 && cr < -eps {
                    continue;
                }
                let inside = idx.iter().any(|&j| {
                    j != a
                        && j != b
                        && j != c
                        && p[j] != p[a]
                        && p[j] != p[b]
                        && p[j] != p[c]
                        && cross(p[a], p[b], p[j]) >= -eps
                        && cross(p[b], p[c], p[j]) >= -eps
                        && cross(p[c], p[a], p[j]) >= -eps
                });
                if !inside {
                    found = Some(i);
                    break;
                }
            }
            if found.is_some() {
                break;
            }
        }
        let i = found?;
        let (a, b, c) = (idx[(i + m - 1) % m], idx[i], idx[(i + 1) % m]);
        out.push([a, b, c]);
        idx.remove(i);
    }
    if idx.len() == 3 {
        out.push([idx[0], idx[1], idx[2]]);
    }
    Some(out)
}

/// One edge's tool into `m`, without the caps `skip` names (those at a
/// corner, where the patch continues the solid). Returns its blend's
/// surface entry.
#[allow(clippy::too_many_arguments)]
fn edge_into(
    m: &mut Mesh,
    spec: &BlendSpec,
    e: &BlendEdge,
    fr: &Frame,
    prof: &Prof,
    rings: &[Vec<V>; 2],
    caps: &[Surface; 2],
    skip: [bool; 2],
) -> Result<u32, BlendError> {
    let blend = m.surf(match spec.profile {
        Profile::Fillet => Surface::Cylinder {
            origin: prof.base.arr(),
            axis: fr.d.arr(),
            radius: spec.size,
        },
        Profile::Chamfer => {
            let (a, b) = (prof.arc[0], prof.arc[1]);
            Surface::Plane {
                origin: (prof.base + a).arr(),
                normal: fr.d.cross(b - a).norm().arr(),
            }
        }
    });
    let k = rings[0].len();
    let n_arc = prof.arc.len();
    // The ring's winding about the edge's direction decides which way the
    // sides face.
    let ring = prof.ring();
    let wind: f64 = (0..k)
        .map(|j| ring[j].cross(ring[(j + 1) % k]).dot(fr.d))
        .sum();
    let ccw = wind > 0.0;
    // Each side's surface: the blend, a face of the solid the side lies
    // in, or the plane of the side itself.
    for j in 0..k {
        let j1 = (j + 1) % k;
        let s = if j + 1 < n_arc {
            blend
        } else {
            let a = ring[j];
            let b = ring[j1];
            // A concave tool's side along a plane face lies in it.
            if let Some(f) = side_face(spec, e, fr, prof, a, b) {
                m.surf(face_surface(&e.faces[f]))
            } else {
                let n = fr.d.cross(b - a).norm();
                m.surf(Surface::Plane {
                    origin: (prof.base + a).arr(),
                    normal: n.arr(),
                })
            }
        };
        let (a0, b0) = (m.vert(rings[0][j]), m.vert(rings[0][j1]));
        let (a1, b1) = (m.vert(rings[1][j]), m.vert(rings[1][j1]));
        if ccw {
            m.tri([a0, b0, b1], s);
            m.tri([a0, b1, a1], s);
        } else {
            m.tri([a0, b1, b0], s);
            m.tri([a0, a1, b1], s);
        }
    }
    for end in 0..2 {
        if skip[end] {
            continue;
        }
        let s = m.surf(caps[end].clone());
        let out = if end == 0 { fr.d * -1.0 } else { fr.d };
        // The cap's own normal, facing out of the tool.
        let pts = &rings[end];
        let mut nrm = V::default();
        for j in 0..k {
            nrm = nrm + pts[j].cross(pts[(j + 1) % k]);
        }
        let nrm = if nrm.dot(out) < 0.0 { nrm * -1.0 } else { nrm };
        m.polygon(pts, nrm.norm(), s)?;
    }
    Ok(blend)
}

fn face_surface(f: &BlendFace) -> Surface {
    match f {
        BlendFace::Plane { origin, normal } => Surface::Plane {
            origin: *origin,
            normal: *normal,
        },
        BlendFace::Cylinder {
            origin,
            axis,
            radius,
            ..
        } => Surface::Cylinder {
            origin: *origin,
            axis: *axis,
            radius: *radius,
        },
        BlendFace::Cone {
            apex, axis, slope, ..
        } => Surface::Cone {
            apex: *apex,
            axis: *axis,
            slope: *slope,
        },
        BlendFace::Sphere { center, radius, .. } => Surface::Sphere {
            center: *center,
            radius: *radius,
        },
        BlendFace::Torus {
            center,
            axis,
            major_radius,
            minor_radius,
            ..
        } => Surface::Torus {
            center: *center,
            axis: *axis,
            major_radius: *major_radius,
            minor_radius: *minor_radius,
        },
    }
}

/// The face of the solid that the region's side from ring point `a` to
/// `b` (offsets from the base) lies in, if any: a concave tool's side
/// along a plane face it does not overlap into.
fn side_face(
    spec: &BlendSpec,
    e: &BlendEdge,
    fr: &Frame,
    prof: &Prof,
    a: V,
    b: V,
) -> Option<usize> {
    (0..2).find(|&f| {
        prof.mu[f] == 0.0 && matches!(e.faces[f], BlendFace::Plane { .. }) && {
            let n = prof.nu[f];
            let h = prof.sigma * spec.size;
            let base_h = match spec.profile {
                Profile::Fillet => h,
                Profile::Chamfer => match fr.f[f] {
                    F::Line { c, .. } => c,
                    F::Circle { .. } => 0.0,
                },
            };
            (a.dot(n) - base_h).abs() <= 1e-9 * (1.0 + spec.size)
                && (b.dot(n) - base_h).abs() <= 1e-9 * (1.0 + spec.size)
        }
    })
}

/// A corner's sphere patch into `m`: bounded by the three edge tools'
/// rings at the corner (shared point for point; their caps are left out,
/// so the patch and the tools are one solid), the three faces offset as
/// the tools offset them, and the sphere. Returns the sphere's entry.
#[allow(clippy::too_many_arguments)]
fn corner_into(
    m: &mut Mesh,
    spec: &BlendSpec,
    c: &Corner,
    ci: usize,
    s: V,
    planes: &[(u32, V, V); 3],
    sigma: f64,
    profs: &[(Frame, Prof)],
    rings: &[(usize, Vec<V>)],
) -> Result<u32, BlendError> {
    let r = spec.size;
    if rings.len() != 3 {
        return Err(BlendError::Invalid(format!(
            "corner {ci}: its three edges do not all end there"
        )));
    }
    let sphere = m.surf(Surface::Sphere {
        center: s.arr(),
        radius: r,
    });
    let _ = c;
    // The far corner: every face at its offset.
    let mu = profs[rings[0].0].1.mu[0];
    let n3 = [planes[0].2, planes[1].2, planes[2].2];
    let w_off = corner_offset(n3, [sigma * r + mu; 3])
        .ok_or_else(|| BlendError::Invalid(format!("corner {ci}: its planes do not meet")))?;
    let w = s + w_off;
    // Per face: the points of its offset quad, [P, Q (with one
    // neighbour), W, Q (with the other)].
    let mut boundary: Vec<(u32, u32, Vec<V>)> = Vec::new(); // (face a, face b, arc a->b)
    let mut face_pts: Vec<Vec<(u32, V)>> = vec![Vec::new(); 3]; // per plane: (other face, Q)
    let mut p_of: [Option<V>; 3] = [None; 3];
    let plane_index = |id: u32| planes.iter().position(|p| p.0 == id);
    for (ei, ring) in rings {
        let (_, prof) = &profs[*ei];
        let e = &spec.edges[*ei];
        let n_arc = prof.arc.len();
        let arc: Vec<V> = ring[..n_arc].to_vec();
        let ids = e.face_ids;
        let rest = &ring[n_arc..];
        let q = rest[prof.q];
        for k in 0..2 {
            let pi = plane_index(ids[k])
                .ok_or_else(|| BlendError::Invalid(format!("corner {ci}: a face is missing")))?;
            face_pts[pi].push((ids[1 - k], q));
            let pk = match prof.p[k] {
                Some(j) => rest[j],
                None => {
                    if k == 0 {
                        arc[0]
                    } else {
                        arc[n_arc - 1]
                    }
                }
            };
            p_of[pi] = Some(pk);
        }
        boundary.push((ids[0], ids[1], arc));
    }
    // The offset quads on the three faces.
    for pi in 0..3 {
        let (id, o, n) = planes[pi];
        let _ = o;
        let pts = &face_pts[pi];
        if pts.len() != 2 {
            return Err(BlendError::Invalid(format!(
                "corner {ci}: face {id} has {} edges there",
                pts.len()
            )));
        }
        let p = p_of[pi].ok_or_else(|| BlendError::Invalid(format!("corner {ci}: no point")))?;
        let quad = [p, pts[0].1, w, pts[1].1];
        // Outward: along the face's normal for a convex corner, against
        // it for a concave one (the patch is then in the air).
        let surf = if mu == 0.0 {
            m.surf(Surface::Plane {
                origin: planes[pi].1.arr(),
                normal: n.arr(),
            })
        } else {
            m.surf(Surface::Plane {
                origin: (s + beside(n, sigma, r, mu)).arr(),
                normal: n.arr(),
            })
        };
        // Order the quad so it does not cross itself: P, Q, W, Q'.
        m.polygon(&quad, n * sigma, surf)?;
    }
    // The sphere triangle: the three arcs joined end to end.
    let mut loop_pts: Vec<V> = Vec::new();
    {
        let (a, b, arc) = &boundary[0];
        loop_pts.extend(arc.iter().copied());
        let mut at = *b;
        let start = *a;
        let mut used = [true, false, false];
        while at != start {
            let next = (0..3).find(|&j| !used[j] && (boundary[j].0 == at || boundary[j].1 == at));
            let Some(j) = next else {
                return Err(BlendError::Invalid(format!(
                    "corner {ci}: its arcs do not close"
                )));
            };
            used[j] = true;
            let (x, y, arc) = &boundary[j];
            let pts: Vec<V> = if *x == at {
                arc.clone()
            } else {
                arc.iter().rev().copied().collect()
            };
            loop_pts.extend(pts.into_iter().skip(1));
            at = if *x == at { *y } else { *x };
        }
        // The loop's last point is its first.
        loop_pts.pop();
    }
    sphere_patch(m, s, r, &loop_pts, sphere);
    Ok(sphere)
}

/// Triangles on the sphere about `s` of radius `r` filling the loop
/// `pts` (points on it): rings shrinking towards the loop's middle, each
/// point pulled back onto the sphere. The patch's outward side faces the
/// centre, so triangles turn that way.
fn sphere_patch(m: &mut Mesh, s: V, r: f64, pts: &[V], surf: u32) {
    let n = pts.len();
    let mid = pts.iter().fold(V::default(), |a, p| a + (*p - s)).norm();
    let levels = (n / 6).max(1);
    let mut rings: Vec<Vec<u32>> = vec![pts.iter().map(|p| m.vert(*p)).collect()];
    for l in 1..levels {
        let f = 1.0 - l as f64 / levels as f64;
        let ring: Vec<u32> = pts
            .iter()
            .map(|p| {
                let d = mid * (1.0 - f) + (*p - s).norm() * f;
                m.vert(s + d.norm() * r)
            })
            .collect();
        rings.push(ring);
    }
    let apex = m.vert(s + mid * r);
    // Orientation: the boundary's winding seen from outside the sphere.
    let mut wind = V::default();
    for i in 0..n {
        wind = wind + (pts[i] - s).cross(pts[(i + 1) % n] - s);
    }
    // Facing the centre: the loop must run clockwise seen from outside.
    let flip = wind.dot(mid) > 0.0;
    let put = |m: &mut Mesh, t: [u32; 3]| {
        if flip {
            m.tri([t[0], t[2], t[1]], surf);
        } else {
            m.tri(t, surf);
        }
    };
    for l in 0..rings.len() {
        for i in 0..n {
            let j = (i + 1) % n;
            if l + 1 < rings.len() {
                let (a, b) = (rings[l][i], rings[l][j]);
                let (c, d) = (rings[l + 1][j], rings[l + 1][i]);
                put(m, [a, b, c]);
                put(m, [a, c, d]);
            } else {
                put(m, [rings[l][i], rings[l][j], apex]);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn box_edge(convex: bool, ends: [End; 2]) -> BlendEdge {
        // The edge along x at y = 0, z = 0 of a box in y, z > 0 (convex)
        // or of the inside corner of the region y, z < 0 (concave).
        let (ny, nz) = if convex {
            ([0.0, -1.0, 0.0], [0.0, 0.0, -1.0])
        } else {
            ([0.0, 1.0, 0.0], [0.0, 0.0, 1.0])
        };
        BlendEdge {
            from: [0.0; 3],
            to: [10.0, 0.0, 0.0],
            faces: [
                BlendFace::Plane {
                    origin: [0.0; 3],
                    normal: ny,
                },
                BlendFace::Plane {
                    origin: [0.0; 3],
                    normal: nz,
                },
            ],
            face_ids: [0, 1],
            convex,
            ends,
            path: Path::Line,
            margin: None,
        }
    }

    fn volume(m: &TaggedMesh) -> f64 {
        m.triangles
            .iter()
            .map(|t| {
                let [a, b, c] = t.map(|i| V::from(m.positions[i as usize]));
                a.dot(b.cross(c)) / 6.0
            })
            .sum()
    }

    fn plane(o: [f64; 3], n: [f64; 3]) -> End {
        End::Plane {
            origin: o,
            normal: n,
        }
    }

    #[test]
    fn a_right_angle_fillet_section() {
        let spec = BlendSpec {
            profile: Profile::Fillet,
            size: 2.0,
            edges: vec![box_edge(
                true,
                [
                    plane([0.0; 3], [-1.0, 0.0, 0.0]),
                    plane([10.0, 0.0, 0.0], [1.0, 0.0, 0.0]),
                ],
            )],
            corners: vec![],
        };
        let s = section(&spec, 0).unwrap();
        assert_eq!(s.center, Some([0.0, 2.0, 2.0]));
        assert_eq!(s.widths, [2.0, 2.0]);
        let t = tools(&spec, &|_| 8).unwrap();
        assert_eq!(t.len(), 1);
        assert!(!t[0].add);
        // The region: [0, 10] x (corner square with margin, minus the
        // quarter disc) is the outward box [-2, 2]^2 less 3/4 of nothing:
        // (r + m)^2 - πr²/4 with m = r.
        let want = 10.0 * (16.0 - std::f64::consts::PI);
        let got = volume(&t[0].mesh);
        // The polygonal arc (8 segments) leaves a little more.
        assert!((got - want).abs() < 0.25, "{got} vs {want}");
        assert!(got > want);
    }

    #[test]
    fn a_concave_tool_is_the_spandrel() {
        let spec = BlendSpec {
            profile: Profile::Fillet,
            size: 3.0,
            edges: vec![box_edge(
                false,
                [
                    plane([0.0; 3], [-1.0, 0.0, 0.0]),
                    plane([10.0, 0.0, 0.0], [1.0, 0.0, 0.0]),
                ],
            )],
            corners: vec![],
        };
        let t = tools(&spec, &|_| 64).unwrap();
        assert!(t[0].add);
        let want = 10.0 * 9.0 * (1.0 - std::f64::consts::PI / 4.0);
        let got = volume(&t[0].mesh);
        assert!((got - want).abs() < 0.02, "{got} vs {want}");
    }

    #[test]
    fn a_chamfer_at_sixty_degrees() {
        // Faces at a 60° material angle: z = 0 (normal -z) and the plane
        // through the x axis at 60° from +y (normal pointing out).
        let a = 60f64.to_radians();
        let n2 = [0.0, -sin(a), cos(a)];
        let spec = BlendSpec {
            profile: Profile::Chamfer,
            size: 1.0,
            edges: vec![BlendEdge {
                from: [0.0; 3],
                to: [5.0, 0.0, 0.0],
                faces: [
                    BlendFace::Plane {
                        origin: [0.0; 3],
                        normal: [0.0, 0.0, -1.0],
                    },
                    BlendFace::Plane {
                        origin: [0.0; 3],
                        normal: n2,
                    },
                ],
                face_ids: [0, 1],
                convex: true,
                ends: [
                    plane([0.0; 3], [-1.0, 0.0, 0.0]),
                    plane([5.0, 0.0, 0.0], [1.0, 0.0, 0.0]),
                ],
                path: Path::Line,
                margin: None,
            }],
            corners: vec![],
        };
        let s = section(&spec, 0).unwrap();
        assert!((s.widths[0] - 1.0).abs() < 1e-12 && (s.widths[1] - 1.0).abs() < 1e-12);
        let t = s.tangents;
        assert!((t[0][1] - 1.0).abs() < 1e-12 && t[0][2].abs() < 1e-12);
        assert!((t[1][1] - cos(a)).abs() < 1e-12 && (t[1][2] - sin(a)).abs() < 1e-12);
        assert!(tools(&spec, &|_| 1).is_ok());
    }

    #[test]
    fn too_large_on_a_boss() {
        // A concave edge where a plane meets a rod of radius 1 is fine;
        // a convex fillet of radius 2 on a rod of radius 1 is not.
        let spec = BlendSpec {
            profile: Profile::Fillet,
            size: 2.0,
            edges: vec![BlendEdge {
                from: [1.0, 0.0, 0.0],
                to: [1.0, 0.0, 5.0],
                faces: [
                    BlendFace::Plane {
                        origin: [0.0; 3],
                        normal: [0.0, -1.0, 0.0],
                    },
                    BlendFace::Cylinder {
                        origin: [0.0; 3],
                        axis: [0.0, 0.0, 1.0],
                        radius: 1.0,
                        convex: true,
                    },
                ],
                face_ids: [0, 1],
                convex: true,
                ends: [
                    plane([0.0; 3], [0.0, 0.0, -1.0]),
                    plane([0.0, 0.0, 5.0], [0.0, 0.0, 1.0]),
                ],
                path: Path::Line,
                margin: None,
            }],
            corners: vec![],
        };
        assert_eq!(section(&spec, 0), Err(BlendError::TooLarge(0)));
    }

    #[test]
    fn a_box_corner_closes() {
        // Three convex edges of the box [0, 10]^3 at the origin.
        let r = 2.0;
        let planes = [
            ([0.0; 3], [-1.0, 0.0, 0.0]),
            ([0.0; 3], [0.0, -1.0, 0.0]),
            ([0.0; 3], [0.0, 0.0, -1.0]),
        ];
        let face = |i: usize| BlendFace::Plane {
            origin: planes[i].0,
            normal: planes[i].1,
        };
        let edge = |to: [f64; 3], f: [usize; 2], far: [f64; 3]| BlendEdge {
            from: [0.0; 3],
            to,
            faces: [face(f[0]), face(f[1])],
            face_ids: [f[0] as u32, f[1] as u32],
            convex: true,
            ends: [End::Corner(0), plane(to, far)],
            path: Path::Line,
            margin: None,
        };
        let spec = BlendSpec {
            profile: Profile::Fillet,
            size: r,
            edges: vec![
                edge([10.0, 0.0, 0.0], [1, 2], [1.0, 0.0, 0.0]),
                edge([0.0, 10.0, 0.0], [2, 0], [0.0, 1.0, 0.0]),
                edge([0.0, 0.0, 10.0], [0, 1], [0.0, 0.0, 1.0]),
            ],
            corners: vec![Corner {
                vertex: [0.0; 3],
                edges: [(0, 0), (1, 0), (2, 0)],
            }],
        };
        let t = tools(&spec, &|_| 16).unwrap();
        // The three edges and the patch are one solid.
        assert_eq!(t.len(), 1);
        let tool = &t[0];
        assert!(!tool.add);
        assert_eq!(
            tool.sources,
            [
                Source::Edge(0),
                Source::Edge(1),
                Source::Edge(2),
                Source::Corner(0)
            ]
        );
        // Each edge's part runs from the plane through the ball's centre
        // (2 from the vertex) to its far end (10): (r + m)^2 less a
        // quarter disc, 8 long. The patch: the box [-m, r]^3 less the
        // eighth of the ball.
        let m = r;
        let pi = std::f64::consts::PI;
        let want = 3.0 * 8.0 * ((r + m) * (r + m) - pi * r * r / 4.0) + (r + m).powi(3)
            - pi * r * r * r / 6.0;
        let got = volume(&tool.mesh);
        assert!((got - want).abs() < 0.01 * want, "{got} vs {want}");
    }

    #[test]
    fn ear_clipping_a_notched_square() {
        let p = [[0.0, 0.0], [2.0, 0.0], [2.0, 2.0], [1.0, 1.0], [0.0, 2.0]];
        let t = ear_clip(&p).unwrap();
        assert_eq!(t.len(), 3);
        let area: f64 = t
            .iter()
            .map(|t| {
                let (a, b, c) = (p[t[0]], p[t[1]], p[t[2]]);
                ((b[0] - a[0]) * (c[1] - a[1]) - (b[1] - a[1]) * (c[0] - a[0])) / 2.0
            })
            .sum();
        assert!((area - 3.0).abs() < 1e-12);
    }

    /// The top rim of a hole of radius 5 about the z axis through a plate
    /// whose top is z = 0, turning by `sweep` from the +x axis, with
    /// sections every `step` radians.
    fn hole_rim(sweep: f64, step: f64, ends: [End; 2]) -> BlendEdge {
        let n = (sweep / step).round() as usize;
        BlendEdge {
            from: [5.0, 0.0, 0.0],
            to: [5.0 * cos(sweep), 5.0 * sin(sweep), 0.0],
            faces: [
                BlendFace::Plane {
                    origin: [0.0; 3],
                    normal: [0.0, 0.0, 1.0],
                },
                BlendFace::Cylinder {
                    origin: [0.0, 0.0, -10.0],
                    axis: [0.0, 0.0, 1.0],
                    radius: 5.0,
                    convex: false,
                },
            ],
            face_ids: [0, 1],
            convex: true,
            ends,
            path: Path::Arc {
                center: [0.0; 3],
                axis: [0.0, 0.0, 1.0],
                radius: 5.0,
                sweep,
                sections: (0..n).map(|j| [step * j as f64, 0.0]).collect(),
            },
            margin: None,
        }
    }

    /// The volume a closed polygon `(ρ, z)` sweeps about the axis in a
    /// whole turn (Pappus: 2π times its first moment about the axis).
    fn swept(ring: &[(f64, f64)]) -> f64 {
        let n = ring.len();
        let mut m = 0.0;
        for i in 0..n {
            let ((r1, z1), (r2, z2)) = (ring[i], ring[(i + 1) % n]);
            m += (r1 * r1 + r1 * r2 + r2 * r2) / 6.0 * (z2 - z1);
        }
        (2.0 * PI * m).abs()
    }

    #[test]
    fn a_rim_tool_is_its_section_revolved() {
        let spec = |sweep: f64, ends: [End; 2]| BlendSpec {
            profile: Profile::Fillet,
            size: 1.0,
            edges: vec![hole_rim(sweep, TAU / 2000.0, ends)],
            corners: vec![],
        };
        let full = spec(TAU, [End::Open { face: None }, End::Open { face: None }]);
        let s = section(&full, 0).unwrap();
        // The ball's centre is 1 below the top and 1 out from the wall;
        // the tangents are on the plane and the wall.
        let c = s.center.unwrap();
        assert!(
            (c[0] - 6.0).abs() < 1e-12 && (c[2] + 1.0).abs() < 1e-12,
            "{c:?}"
        );
        assert!((s.widths[0] - 1.0).abs() < 1e-12 && (s.widths[1] - 1.0).abs() < 1e-12);
        let t = tools(&full, &|_| 8).unwrap();
        assert_eq!(t.len(), 1);
        assert!(!t[0].add);
        let Surface::Torus {
            major_radius,
            minor_radius,
            ..
        } = t[0].mesh.surfaces[t[0].blend[0] as usize]
        else {
            panic!("not a torus");
        };
        assert_eq!((major_radius, minor_radius), (6.0, 1.0));
        // The section's polygon, revolved: the arc's 8 chords from the
        // plane's tangent to the wall's, then into the hole by the margin
        // (1), up past the plane by it, and back.
        let mut ring: Vec<(f64, f64)> = (0..=8)
            .map(|k| {
                let th = PI / 2.0 + PI / 2.0 * k as f64 / 8.0;
                (6.0 + cos(th), -1.0 + sin(th))
            })
            .collect();
        ring.extend([(4.0, -1.0), (4.0, 1.0), (6.0, 1.0)]);
        let want = swept(&ring);
        let got = volume(&t[0].mesh);
        // 2000 sections: the polygon about the axis loses (π/2000)²/6.
        assert!((got - want).abs() / want < 1e-5, "{got} vs {want}");
        // A quarter, cut on the planes through the axis at both ends: a
        // quarter of the volume, closed by its caps.
        let ends = [
            End::Plane {
                origin: [0.0; 3],
                normal: [0.0, -1.0, 0.0],
            },
            End::Plane {
                origin: [0.0; 3],
                normal: [-1.0, 0.0, 0.0],
            },
        ];
        let q = tools(&spec(PI / 2.0, ends), &|_| 8).unwrap();
        let got = volume(&q[0].mesh);
        assert!(
            (got - want / 4.0).abs() / want < 1e-5,
            "{got} vs {}",
            want / 4.0
        );
        // Open ends run on past the arc's ends into the air.
        let open = [
            End::Open {
                face: Some(([0.0; 3], [0.0, -1.0, 0.0])),
            },
            End::Open {
                face: Some(([0.0; 3], [-1.0, 0.0, 0.0])),
            },
        ];
        let o = tools(&spec(PI / 2.0, open), &|_| 8).unwrap();
        assert!(volume(&o[0].mesh) > got * 1.2);
    }

    #[test]
    fn a_boss_rim_needs_a_ring_torus() {
        // A boss of radius 6 about z, top at z = 0: a convex fillet's
        // centre is 6 - r from the axis, which must be more than r.
        let edge = |r: f64| BlendSpec {
            profile: Profile::Fillet,
            size: r,
            edges: vec![BlendEdge {
                from: [6.0, 0.0, 0.0],
                to: [6.0, 0.0, 0.0],
                faces: [
                    BlendFace::Plane {
                        origin: [0.0; 3],
                        normal: [0.0, 0.0, 1.0],
                    },
                    BlendFace::Cylinder {
                        origin: [0.0, 0.0, -10.0],
                        axis: [0.0, 0.0, 1.0],
                        radius: 6.0,
                        convex: true,
                    },
                ],
                face_ids: [0, 1],
                convex: true,
                ends: [End::Open { face: None }, End::Open { face: None }],
                path: Path::Arc {
                    center: [0.0; 3],
                    axis: [0.0, 0.0, 1.0],
                    radius: 6.0,
                    sweep: TAU,
                    sections: vec![],
                },
                margin: None,
            }],
            corners: vec![],
        };
        assert!(section(&edge(2.9), 0).is_ok());
        assert_eq!(section(&edge(3.1), 0), Err(BlendError::TooLarge(0)));
        let t = tools(&edge(2.0), &|_| 4).unwrap();
        assert!(t[0].mesh.triangles.len() > 32 * 4);
    }
}
