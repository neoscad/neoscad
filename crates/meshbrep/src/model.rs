//! The plain data that goes in and comes out: surfaces, curves, the
//! tagged mesh and the B-rep. Every field is public; nothing here holds a
//! reference or a handle.

/// An exact surface, in model coordinates.
///
/// A surface has no orientation of its own: which side is outside is read
/// from the mesh triangles that carry it.
#[derive(Clone, Debug, PartialEq)]
pub enum Surface {
    /// The plane through `origin` with unit `normal`.
    Plane {
        /// A point on the plane.
        origin: [f64; 3],
        /// The unit normal.
        normal: [f64; 3],
    },
    /// The circular cylinder of `radius` about the line through `origin`
    /// along the unit `axis`.
    Cylinder {
        /// A point on the axis.
        origin: [f64; 3],
        /// The unit axis direction.
        axis: [f64; 3],
        /// The radius.
        radius: f64,
    },
    /// One nappe of a circular cone: the points `p` with
    /// `(p - apex) · axis = t >= 0` whose distance from the axis is
    /// `slope * t`. `slope` is the tangent of the half-angle.
    Cone {
        /// The apex.
        apex: [f64; 3],
        /// The unit axis, pointing from the apex into the nappe.
        axis: [f64; 3],
        /// The radius gained per unit length along the axis.
        slope: f64,
    },
    /// The sphere of `radius` about `center`.
    Sphere {
        /// The centre.
        center: [f64; 3],
        /// The radius.
        radius: f64,
    },
    /// A ring torus about the unit `axis` through `center`: what an arc
    /// off the axis sweeps in `rotate_extrude`. `major_radius` must be
    /// larger than `minor_radius` (a torus that crosses its own axis is
    /// refused as malformed).
    ///
    /// Its frame (STEP's `TOROIDAL_SURFACE`) puts the point at
    /// `(u, v)` at `origin + (R + r cos v)(cos u x + sin u y) + r sin v z`.
    Torus {
        /// The centre.
        center: [f64; 3],
        /// The unit axis.
        axis: [f64; 3],
        /// The distance from the axis to the centre of the tube.
        major_radius: f64,
        /// The radius of the tube.
        minor_radius: f64,
    },
    /// The surface swept by `profile` moving along `direction`. Declared
    /// for extruded free-form 2D curves (text outlines); not accepted yet.
    LinearExtrusion {
        /// The swept curve.
        profile: Curve,
        /// The sweep direction (unit).
        direction: [f64; 3],
    },
    /// The surface swept by `profile` turning about the line through
    /// `origin` along `axis`. Declared for revolved 2D curves; not
    /// accepted yet.
    Revolution {
        /// The revolved curve.
        profile: Curve,
        /// A point on the axis.
        origin: [f64; 3],
        /// The unit axis.
        axis: [f64; 3],
    },
    /// No exact surface: the triangles carrying this entry are kept as
    /// they are, and become planar faces (coplanar neighbours merged).
    /// This is the fallback for mesh-only geometry (`polyhedron`, `hull`,
    /// imported meshes).
    Faceted,
}

impl Surface {
    /// A short name for reports: `"plane"`, `"cylinder"`, ...
    pub fn kind(&self) -> &'static str {
        match self {
            Surface::Plane { .. } => "plane",
            Surface::Cylinder { .. } => "cylinder",
            Surface::Cone { .. } => "cone",
            Surface::Sphere { .. } => "sphere",
            Surface::Torus { .. } => "torus",
            Surface::LinearExtrusion { .. } => "linear extrusion",
            Surface::Revolution { .. } => "revolution",
            Surface::Faceted => "faceted",
        }
    }
}

/// A 3D curve. Each kind has a natural parameter `t`; an [`Edge`] uses
/// the interval [`Edge::range`] of it.
#[derive(Clone, Debug, PartialEq)]
pub enum Curve {
    /// `origin + t * direction`, `direction` a unit vector.
    Line {
        /// The point at `t = 0`.
        origin: [f64; 3],
        /// The unit direction.
        direction: [f64; 3],
    },
    /// `center + radius * (cos t * x_axis + sin t * (normal × x_axis))`.
    Circle {
        /// The centre.
        center: [f64; 3],
        /// The unit normal; the curve turns counter-clockwise about it.
        normal: [f64; 3],
        /// The unit direction of `t = 0`, perpendicular to `normal`.
        x_axis: [f64; 3],
        /// The radius.
        radius: f64,
    },
    /// `center + major * cos t * x_axis + minor * sin t * (normal × x_axis)`.
    Ellipse {
        /// The centre.
        center: [f64; 3],
        /// The unit normal.
        normal: [f64; 3],
        /// The unit direction of the major axis.
        x_axis: [f64; 3],
        /// The semi-major axis.
        major: f64,
        /// The semi-minor axis.
        minor: f64,
    },
    /// A non-rational B-spline: the fallback for intersections of two
    /// curved surfaces that have no closed form.
    BSpline(BSpline<3>),
}

impl Curve {
    /// A short name for reports: `"line"`, `"circle"`, ...
    pub fn kind(&self) -> &'static str {
        match self {
            Curve::Line { .. } => "line",
            Curve::Circle { .. } => "circle",
            Curve::Ellipse { .. } => "ellipse",
            Curve::BSpline(_) => "bspline",
        }
    }
}

/// A clamped, non-rational B-spline curve in `D` dimensions: 3 for edges,
/// 2 for curves in a surface's parameter space.
#[derive(Clone, Debug, PartialEq)]
pub struct BSpline<const D: usize> {
    /// The degree (1 or 3 here).
    pub degree: u32,
    /// The control points.
    pub control: Vec<[f64; D]>,
    /// The full knot vector, `control.len() + degree + 1` long, with the
    /// end knots repeated `degree + 1` times.
    pub knots: Vec<f64>,
}

/// The input: a closed, oriented triangle mesh whose triangles each name
/// the exact surface they approximate.
///
/// The mesh must be a 2-manifold (every edge shared by exactly two
/// triangles, in opposite directions), with triangles counter-clockwise
/// seen from outside, as Manifold produces. Positions shared by triangles
/// must be shared indices.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TaggedMesh {
    /// Vertex positions.
    pub positions: Vec<[f64; 3]>,
    /// Triangles as indices into `positions`.
    pub triangles: Vec<[u32; 3]>,
    /// For each triangle, an index into `surfaces`.
    pub triangle_surface: Vec<u32>,
    /// The surface table.
    pub surfaces: Vec<Surface>,
}

/// The parametrisation of a face's surface: the placement written to STEP
/// as its `AXIS2_PLACEMENT_3D`.
///
/// With `y = z × x`, the surface point at parameters `(u, v)` is
/// - plane: `origin + u x + v y`;
/// - cylinder of radius `r`: `origin + r (cos u x + sin u y) + v z`;
/// - cone: `origin + (r + v · slope) (cos u x + sin u y) + v z`, where `r`
///   is the radius at `origin` ([`Face::ref_radius`]);
/// - sphere of radius `r`: `origin + r cos v (cos u x + sin u y) + r sin v z`;
/// - torus of radii `R` and `r`:
///   `origin + (R + r cos v) (cos u x + sin u y) + r sin v z`.
///
/// `u` is in radians, and a face's curves on a periodic surface use `u` in
/// `[0, 2π]`, with the seam (if any) at `u = 0` and `u = 2π`. A torus's `v`
/// is an angle too: a face that wraps around the tube has a seam along a
/// parallel, at `v0` and `v0 + 2π`; one that does not uses any interval
/// of `v` clear of its cut.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Frame {
    /// The origin.
    pub origin: [f64; 3],
    /// The unit z axis (the plane normal, or the axis of revolution).
    pub z: [f64; 3],
    /// The unit x axis, perpendicular to `z`.
    pub x: [f64; 3],
}

/// The reconstructed boundary representation.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Brep {
    /// Vertex positions.
    pub vertices: Vec<[f64; 3]>,
    /// Edges.
    pub edges: Vec<Edge>,
    /// Faces.
    pub faces: Vec<Face>,
    /// Closed shells, as lists of face indices.
    pub shells: Vec<Shell>,
    /// Measurements and diagnostics from reconstruction.
    pub report: Report,
}

/// An edge: a bounded piece of a curve between two vertices.
#[derive(Clone, Debug, PartialEq)]
pub struct Edge {
    /// The start vertex.
    pub start: u32,
    /// The end vertex (equal to `start` for a closed edge).
    pub end: u32,
    /// The curve, oriented from `start` to `end`.
    pub curve: Curve,
    /// The parameter interval of `curve` used, increasing.
    pub range: [f64; 2],
    /// A seam: the edge where a periodic face meets itself. Used twice by
    /// the same face, once in each direction.
    pub seam: bool,
}

/// A face: a region of one surface bounded by loops.
#[derive(Clone, Debug, PartialEq)]
pub struct Face {
    /// The surface (never [`Surface::Faceted`]: faceted regions become
    /// planes).
    pub surface: Surface,
    /// The parametrisation the loops' parameter-space curves use.
    pub frame: Frame,
    /// For a cone, its radius at `frame.origin`; otherwise 0.
    pub ref_radius: f64,
    /// Whether the face's outward normal agrees with the surface's natural
    /// normal (`∂u × ∂v` of the frame's parametrisation).
    pub same_sense: bool,
    /// The boundary loops. Seen from outside, each runs counter-clockwise
    /// around the face, so the face is on its left.
    pub loops: Vec<Loop>,
    /// Built from [`Surface::Faceted`] triangles.
    pub faceted: bool,
}

/// A closed loop of oriented edges.
#[derive(Clone, Debug, PartialEq)]
pub struct Loop {
    /// The edge uses in order: each ends where the next starts.
    pub coedges: Vec<Coedge>,
    /// The outer boundary of its face (at most one per face; none for a
    /// face such as a sphere with holes, whose loops are all holes).
    pub outer: bool,
}

/// One use of an edge by a face.
#[derive(Clone, Debug, PartialEq)]
pub struct Coedge {
    /// The edge.
    pub edge: u32,
    /// Whether the loop runs along the edge's direction.
    pub forward: bool,
    /// The edge in the face's parameter space, oriented like the edge (not
    /// like the loop). Present on curved faces; planar faces have none.
    pub pcurve: Option<BSpline<2>>,
}

/// A connected closed shell.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Shell {
    /// Its faces.
    pub faces: Vec<u32>,
    /// A void: a cavity inside another shell, whose faces point inwards.
    pub void: bool,
}

/// What reconstruction measured and noticed.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Report {
    /// The largest edge length of the input's bounding box, which scales
    /// the relative tolerances.
    pub scale: f64,
    /// The input mesh's genus, from its Euler characteristic, summed over
    /// its connected components.
    pub mesh_genus: i64,
    /// The input mesh's connected components.
    pub mesh_components: usize,
    /// The largest distance of a vertex from one of its surfaces.
    pub max_vertex_residual: f64,
    /// The largest distance of a sampled edge point from one of its two
    /// surfaces.
    pub max_edge_deviation: f64,
    /// The largest distance of a parameter-space curve's image from its
    /// edge's other surface.
    pub max_pcurve_deviation: f64,
    /// The largest distance of a mesh vertex on an edge from the edge's
    /// exact curve: how far the tessellation is from the exact model.
    /// Only edges with a curved face count, and not those between tangent
    /// surfaces (there the mesh's curve wanders wherever its polygons
    /// cross); each is scaled by the sine of the angle its faces meet at,
    /// so a grazing intersection counts its distance across the surfaces,
    /// not along them. Much more than the
    /// tessellation's own sagitta means the mesh's topology differs from
    /// the exact model's even where reconstruction succeeded.
    pub max_chain_deviation: f64,
    /// Pairs of surfaces found tangent analytically.
    pub tangencies: Vec<Tangency>,
    /// Things worth a look, in words.
    pub notes: Vec<String>,
}

/// Two surfaces of the input that touch without crossing.
#[derive(Clone, Debug, PartialEq)]
pub struct Tangency {
    /// The surfaces, as indices into the input's table (the lower first).
    pub surfaces: [u32; 2],
    /// Where they touch.
    pub contact: Contact,
}

/// The set where two tangent surfaces touch.
#[derive(Clone, Debug, PartialEq)]
pub enum Contact {
    /// A line through `point` along the unit `direction`.
    Line {
        /// A point on the line.
        point: [f64; 3],
        /// The unit direction.
        direction: [f64; 3],
    },
    /// A circle.
    Circle {
        /// The centre.
        center: [f64; 3],
        /// The unit normal.
        normal: [f64; 3],
        /// The radius.
        radius: f64,
    },
    /// A single point.
    Point {
        /// The point.
        point: [f64; 3],
    },
}
