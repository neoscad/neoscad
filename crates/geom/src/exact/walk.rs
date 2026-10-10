//! The export render: the node tree built again for STEP export, with
//! every triangle tagged with the exact surface it lies on.
//!
//! This is a second render of the same tree, separate from the one behind
//! the preview, `check` and mesh exports (the exact-geometry audit's F7),
//! for three reasons:
//!
//! - **Its tessellation is not OpenSCAD's.** A curved primitive whose
//!   fragments come from `$fa`/`$fs` is tessellated with a segment count
//!   rounded up to a multiple of 4 and vertices on the axes, and spheres get
//!   poles and an equator (`meshbrep::primitives`). Only the mesh's topology
//!   reaches the STEP file, never its vertices, and this tessellation makes
//!   that topology match the exact model's at tangencies (audit F2: with
//!   OpenSCAD's own, a capsule or a CSG fillet comes out invalid). Putting
//!   it into the normal render would change every exported mesh.
//! - **Surface identity has to survive.** The normal render collapses
//!   `face_id` whenever it rebuilds a solid as one original (`render()`,
//!   `color()`, `ManifoldGeometry::make_original`), and keys its cache on
//!   subtrees whose triangles would need renumbering in each new tree. Here
//!   `render()`, `color()` and `part()` are plain unions, there is no cache
//!   of tagged solids across renders, and every leaf draws its surface
//!   numbers and its original ID from one counter in tree order, so the
//!   numbering (and so the file) does not depend on what ran before or on
//!   the thread count. Within one walk, a large subtree that recurs is
//!   built once and its copies placed, renumbered where each stands
//!   ([`memo`]).
//! - **Transforms go to the leaves.** Each primitive is built in place
//!   under the product of its ancestors' matrices, so its surface records
//!   are transformed once, exactly, instead of being carried through
//!   Manifold's transforms of whole subtrees.
//!
//! Everything without an exact surface is rendered by the normal renderer
//! (sharing its cache, so a subtree the normal render already built costs
//! nothing) and tagged [`Surface::Faceted`]: its triangles reach the file
//! as planar faces. Those are the substitutions the caller reports.

mod extrude;
mod memo;

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use eval::dump::Keys;
use eval::node::{CsgOp, Discretizer, Node, NodeKind};
use lang::diag::PathBase;
use manifold_rust::csg_tree::CsgNode;
use manifold_rust::manifold::Manifold;
use manifold_rust::types::{Error as KernelError, MeshGL64, OpType};
use meshbrep::primitives::{self, Transform};
use meshbrep::{Surface, TaggedMesh};

use crate::evaluate::{MsgLoc, RenderOptions, Renderer, Unsupported};
use crate::exact::profile::Curve2;
use crate::manifold_geom::{GlobalIds, ManifoldGeometry, kernel_token};
use crate::polyset::PolySet;
use crate::{Geometry, Matrix, fragments};

/// What one substitution was: a curve made exact, a polygon kept, or a
/// region exported as facets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum SubstitutionKind {
    /// A circle, cylinder, cone or sphere whose fragments came from
    /// `$fa`/`$fs` is exported as the exact surface, not the polygon the
    /// mesh render has. The shape differs from the mesh by the fragments'
    /// sagitta (an inscribed polygon is smaller).
    Exact,
    /// An explicit `$fn` keeps OpenSCAD's polygon, which is exact as it
    /// is (planar faces): the owner's rule, so that `$fn = 6` hexagons and
    /// polygon-sized printing holes are exported as modelled.
    Polygon,
    /// No exact surface: the region is exported as the planar facets of
    /// the mesh render.
    Faceted,
}

/// One kind of substitution at one source location, with how many nodes
/// it applied to (a `for` loop makes many from one location).
#[derive(Debug, Clone, PartialEq)]
pub struct Substitution {
    pub kind: SubstitutionKind,
    /// The module, as OpenSCAD spells it (`sphere`, `hull`, `scale`).
    pub module: &'static str,
    /// What happened, in words, without the location.
    pub detail: String,
    pub loc: Option<MsgLoc>,
    pub count: u32,
}

/// The tagged mesh of the export render, with what it needed to check the
/// result against the normal render.
#[derive(Debug)]
pub struct ExportMesh {
    pub mesh: TaggedMesh,
    /// Manifold's volume of the tagged mesh.
    pub volume: f64,
    pub substitutions: Vec<Substitution>,
    /// The largest sagitta of a curve made exact, in the normal render's
    /// tessellation: how far the exact model may stand off the mesh the
    /// user previewed.
    pub normal_sagitta: f64,
    /// A bound on the volume between the exact surfaces and the normal
    /// render's polygons: the sum, over the curved primitives made exact,
    /// of their curved area times their sagitta.
    pub normal_volume_bound: f64,
    /// How far a fitted B-spline blend may be from the faces it was made
    /// to meet (`meshbrep::blend::Tool::fit`, in the export's
    /// coordinates): what reconstruction must allow at their contacts
    /// ([`tolerances`]). 0 when there is none.
    pub surface_fit: f64,
    /// How many `linear_extrude`/`rotate_extrude` nodes were built with
    /// exact surfaces (none with [`Extrusions::Faceted`]).
    pub exact_extrusions: u32,
    /// Per entry of the mesh's surface table, the index in `origins` of
    /// the node that made it: where a region written as facets after all
    /// is reported (`super::partial`).
    pub surface_origin: Vec<u32>,
    /// The leaf nodes that made surfaces: module and source location,
    /// each location once.
    pub origins: Vec<(&'static str, Option<MsgLoc>)>,
    /// Per entry of the mesh's surface table, where it came from in the
    /// tree: filled by [`export_render_traced`] only (empty otherwise).
    pub provenance: Vec<Provenance>,
    /// Subtrees placed as copies of an earlier instance of themselves
    /// rather than built ([`memo`]).
    pub placed_copies: u32,
}

/// Where one surface record of a traced export render came from: what
/// fillet edge selection asks of a face (`docs/fillets.md`, section 5.1).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Provenance {
    /// The index of the top node's child the record lies under (in
    /// `children`, background ones counted); `None` when the top node is
    /// itself the leaf.
    pub child: Option<u32>,
    /// The full dotted name of the innermost `part()` around it.
    pub part: Option<String>,
    /// The leaf node instance that made it, numbered in tree order: two
    /// records from one leaf share it (a `for` loop's cubes do not).
    pub leaf: u32,
    /// For a facet of a polygon kept because `$fn` is set (a cylinder's
    /// sides, a sphere's quads, the sides an extrusion sweeps from a
    /// `$fn` circle), the polygon it belongs to: the edge between two
    /// records of one polygon is a seam of the tessellation the user
    /// asked for, not an edge of the shape they modelled.
    pub polygon: Option<u32>,
}

/// How the export render builds `linear_extrude` and `rotate_extrude`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Extrusions {
    /// With exact surfaces where their profiles have them (stage 2).
    Exact,
    /// As the normal render builds them, as planar facets: the fallback
    /// when a model's exact extrusions do not reconstruct, so that a model
    /// stage 1 exported still exports.
    Faceted,
}

/// A node's result in the export render. Each is moved once into its
/// parent's boolean, so the solid is not boxed.
#[allow(clippy::large_enum_variant)]
enum Res {
    /// No geometry (OpenSCAD's null geometry).
    Nothing,
    /// A solid, possibly empty.
    Solid(Manifold),
    /// 2D geometry: ignored by 3D operations, as the normal render ignores
    /// it (with the warning it already printed).
    TwoD,
}

/// Builds the export render of `top` at `mult` times the normal segment
/// counts (1, or 2 for the retry after a topology mismatch). On failure,
/// the substitutions found up to it come back with the error, so a report
/// still says what fell back.
pub fn export_render(
    renderer: &Renderer,
    top: &Node,
    keys: &Keys,
    opts: &RenderOptions,
    mult: u32,
) -> Result<ExportMesh, (Unsupported, Vec<Substitution>)> {
    export_render_with(renderer, top, keys, opts, mult, Extrusions::Exact)
}

/// [`export_render`] with extrusions built as `extrusions` says.
pub fn export_render_with(
    renderer: &Renderer,
    top: &Node,
    keys: &Keys,
    opts: &RenderOptions,
    mult: u32,
    extrusions: Extrusions,
) -> Result<ExportMesh, (Unsupported, Vec<Substitution>)> {
    run_walk(renderer, top, keys, opts, mult, extrusions, Trace::Off)
}

/// [`export_render_with`] with every subtree built in place, none placed
/// as a copy of an earlier instance ([`memo`]): what the export falls back
/// to when a model built with copies is refused, since a copy differs from
/// an in-place build by rounding, and in a model of flush or touching
/// faces rounding can decide whether the mesh reconstructs.
pub fn export_render_in_place(
    renderer: &Renderer,
    top: &Node,
    keys: &Keys,
    opts: &RenderOptions,
    mult: u32,
    extrusions: Extrusions,
) -> Result<ExportMesh, (Unsupported, Vec<Substitution>)> {
    run_walk(renderer, top, keys, opts, mult, extrusions, Trace::InPlace)
}

/// The export render of `top`'s children as one union (`top` is the
/// fillet node whose child is to be selected on), with
/// [`ExportMesh::provenance`] filled in. Fillet nodes inside it are built
/// with their blends, as in the export.
///
/// With `first_pass`, the union has `top`'s own first pass applied too,
/// the one that rounds its selected edges of that sense
/// (`docs/fillets.md`, section 15.6): what a two-pass call's second pass
/// selects on, its first pass's blends recorded with no child
/// ([`Provenance::child`] `None`).
pub fn export_render_traced(
    renderer: &Renderer,
    top: &Node,
    keys: &Keys,
    opts: &RenderOptions,
    mult: u32,
    first_pass: Option<crate::fillet::Sense>,
) -> Result<ExportMesh, (Unsupported, Vec<Substitution>)> {
    let trace = match first_pass {
        Some(s) => Trace::FirstPass(s),
        None => Trace::Children,
    };
    run_walk(renderer, top, keys, opts, mult, Extrusions::Exact, trace)
}

/// What a walk records of where surfaces came from, and what it makes of
/// its top node when that is a fillet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Trace {
    /// The export itself: no provenance.
    Off,
    /// The export with no subtree placed as a copy.
    InPlace,
    /// Provenance; a fillet top is its children's union.
    Children,
    /// Provenance; a fillet top is its children's union with its own
    /// first pass applied, the one of edges of this sense.
    FirstPass(crate::fillet::Sense),
}

fn run_walk(
    renderer: &Renderer,
    top: &Node,
    keys: &Keys,
    opts: &RenderOptions,
    mult: u32,
    extrusions: Extrusions,
    trace: Trace,
) -> Result<ExportMesh, (Unsupported, Vec<Substitution>)> {
    let mut w = Walk {
        trace: matches!(trace, Trace::Children | Trace::FirstPass(_)),
        first_pass: match trace {
            Trace::FirstPass(s) => Some(s),
            _ => None,
        },
        here: (None, None),
        leaves: 0,
        polygons: 0,
        polygon_of: HashMap::new(),
        curve_polygon: HashMap::new(),
        pending_polygons: Vec::new(),
        provenance: Vec::new(),
        renderer,
        keys,
        opts,
        mult,
        token: kernel_token(opts.interrupt.as_ref(), opts.guard.as_ref()),
        surfaces: Vec::new(),
        next_id: 1,
        subs: Vec::new(),
        sub_index: HashMap::new(),
        normal_sagitta: 0.0,
        normal_volume_bound: 0.0,
        surface_fit: 0.0,
        curves2: vec![Curve2::Faceted],
        extrusions,
        exact_extrusions: 0,
        surface_origin: Vec::new(),
        origins: Vec::new(),
        origin_index: HashMap::new(),
        // A traced walk records where each record came from, leaf by
        // leaf, which a placed copy would not.
        memo_plan: if trace == Trace::Off {
            memo::plan(top, keys)
        } else {
            HashMap::new()
        },
        memo: HashMap::new(),
        placed_copies: 0,
    };
    let res = match w.node(top, &crate::IDENTITY) {
        Ok(r) => r,
        Err(u) => return Err((u, w.subs)),
    };
    let m = match res {
        Res::Solid(m) => m,
        Res::Nothing | Res::TwoD => Manifold::empty(),
    };
    if m.status() == KernelError::Cancelled {
        return Err((Unsupported::interrupted(), w.subs));
    }
    let gl = m.get_mesh_gl64(-1);
    let np = (gl.num_prop as usize).max(3);
    let mesh = TaggedMesh {
        positions: gl
            .vert_properties
            .chunks(np)
            .map(|c| [c[0], c[1], c[2]])
            .collect(),
        triangles: gl
            .tri_verts
            .chunks(3)
            .map(|c| [c[0] as u32, c[1] as u32, c[2] as u32])
            .collect(),
        triangle_surface: gl.face_id.iter().map(|&f| f as u32).collect(),
        surfaces: std::mem::take(&mut w.surfaces),
    };
    Ok(ExportMesh {
        volume: m.volume(),
        mesh,
        substitutions: w.subs,
        normal_sagitta: w.normal_sagitta,
        normal_volume_bound: w.normal_volume_bound,
        surface_fit: w.surface_fit,
        exact_extrusions: w.exact_extrusions,
        surface_origin: w.surface_origin,
        origins: w.origins,
        provenance: w.provenance,
        placed_copies: w.placed_copies,
    })
}

/// Whether two export renders are the same input to reconstruction and
/// to every check after it, bit for bit: the same mesh (positions
/// compared by their bits, surfaces by their shortest round-trip text,
/// which tells `-0.0` from `0.0`), the same tolerances and bounds, and
/// the same records of where its faces came from. An export render at
/// twice the segments of a model with no curve the multiplier changes is
/// one ([`super::export_step`] then reuses the first attempt's outcome).
/// Which subtrees were placed as copies is not compared: it does not
/// reach reconstruction except through the mesh.
pub fn same_export(a: &ExportMesh, b: &ExportMesh) -> bool {
    let bits = |x: f64, y: f64| x.to_bits() == y.to_bits();
    let (m, n) = (&a.mesh, &b.mesh);
    m.triangles == n.triangles
        && m.triangle_surface == n.triangle_surface
        && m.positions.len() == n.positions.len()
        && m.positions
            .iter()
            .zip(&n.positions)
            .all(|(p, q)| (0..3).all(|k| bits(p[k], q[k])))
        && bits(a.volume, b.volume)
        && bits(a.normal_sagitta, b.normal_sagitta)
        && bits(a.normal_volume_bound, b.normal_volume_bound)
        && bits(a.surface_fit, b.surface_fit)
        && a.exact_extrusions == b.exact_extrusions
        && a.surface_origin == b.surface_origin
        && a.origins == b.origins
        && a.substitutions == b.substitutions
        && a.provenance == b.provenance
        && m.surfaces.len() == n.surfaces.len()
        && m.surfaces == n.surfaces
        && m.surfaces
            .iter()
            .zip(&n.surfaces)
            .all(|(s, t)| format!("{s:?}") == format!("{t:?}"))
}

/// The reconstruction tolerances for an export render: the defaults, with
/// the B-spline blends' contacts allowed ten times their fit (the side of
/// a fitted patch lies on its face only to within it, and the contact is
/// recognised from samples of it).
pub fn tolerances(em: &ExportMesh) -> meshbrep::Tolerances {
    let d = meshbrep::Tolerances::default();
    meshbrep::Tolerances {
        surface_fit: d.surface_fit.max(10.0 * em.surface_fit),
        ..d
    }
}

type SubKey = (
    SubstitutionKind,
    &'static str,
    String,
    Option<(u32, u32, u32, u32)>,
);

/// A leaf's module and source span, for [`ExportMesh::origins`].
type OriginKey = (&'static str, Option<(u32, u32, u32, u32)>);

struct Walk<'a> {
    renderer: &'a Renderer,
    keys: &'a Keys,
    opts: &'a RenderOptions,
    mult: u32,
    token: Option<manifold_rust::cancel::CancelToken>,
    /// The surface table: a triangle's `face_id` indexes it.
    surfaces: Vec<Surface>,
    /// The next original ID. Drawn here in tree order rather than from
    /// Manifold's process-wide counter, whose values depend on what ran
    /// before; Manifold orders output triangles by original ID.
    next_id: u32,
    subs: Vec<Substitution>,
    sub_index: HashMap<SubKey, usize>,
    normal_sagitta: f64,
    normal_volume_bound: f64,
    surface_fit: f64,
    /// The 2D curve table of the profiles built so far
    /// ([`crate::exact::profile`]); entry 0 is the facet.
    curves2: Vec<Curve2>,
    extrusions: Extrusions,
    exact_extrusions: u32,
    /// See [`ExportMesh::surface_origin`].
    surface_origin: Vec<u32>,
    origins: Vec<(&'static str, Option<MsgLoc>)>,
    origin_index: HashMap<OriginKey, u32>,
    /// [`export_render_traced`]: record [`Provenance`].
    trace: bool,
    /// [`export_render_traced`] with the top's first pass applied.
    first_pass: Option<crate::fillet::Sense>,
    /// The top's child and the innermost part of the leaf being built.
    here: (Option<u32>, Option<String>),
    /// Leaves built so far.
    leaves: u32,
    /// `$fn` polygons seen so far ([`Provenance::polygon`]).
    polygons: u32,
    /// Surface records that are facets of a `$fn` polygon, and which.
    polygon_of: HashMap<usize, u32>,
    /// 2D curve-table entries that are sides of a `$fn` circle, and which
    /// polygon: an extrusion's side planes inherit it.
    curve_polygon: HashMap<u32, u32>,
    /// Surfaces of the primitive about to be placed that belong to a
    /// polygon: (index in its own table, polygon), taken by `place`.
    pending_polygons: Vec<(u32, u32)>,
    /// See [`ExportMesh::provenance`].
    provenance: Vec<Provenance>,
    /// The nodes whose subtrees recur ([`memo`]), and the first instances
    /// built of them.
    memo_plan: HashMap<usize, memo::MemoKey>,
    memo: HashMap<memo::MemoKey, memo::Entry>,
    /// See [`ExportMesh::placed_copies`].
    placed_copies: u32,
}

pub(crate) fn loc_of(n: &Node) -> Option<MsgLoc> {
    n.origin.as_ref().map(|o| MsgLoc {
        unit: o.unit,
        span: o.span,
        line: o.line,
        base: PathBase::MainFileDir,
    })
}

fn is_background(n: &Node) -> bool {
    n.origin.as_ref().is_some_and(|o| o.tag_background)
}

/// The module a node came from, as OpenSCAD spells it.
pub fn module_name(kind: &NodeKind) -> &'static str {
    match kind {
        NodeKind::Root => "root",
        NodeKind::Group { .. } => "group",
        NodeKind::IntersectionFor => "intersection_for",
        NodeKind::Csg(CsgOp::Union) => "union",
        NodeKind::Csg(CsgOp::Difference) => "difference",
        NodeKind::Csg(CsgOp::Intersection) => "intersection",
        NodeKind::Transform { verb, .. } => verb,
        NodeKind::Color { .. } => "color",
        NodeKind::Render { .. } => "render",
        NodeKind::Projection { .. } => "projection",
        NodeKind::Minkowski { .. } => "minkowski",
        NodeKind::Hull => "hull",
        NodeKind::Fill => "fill",
        NodeKind::Resize { .. } => "resize",
        NodeKind::Offset { .. } => "offset",
        NodeKind::LinearExtrude(_) => "linear_extrude",
        NodeKind::RotateExtrude { .. } => "rotate_extrude",
        NodeKind::Cube { .. } => "cube",
        NodeKind::Sphere { .. } => "sphere",
        NodeKind::Cylinder { .. } => "cylinder",
        NodeKind::Polyhedron { .. } => "polyhedron",
        NodeKind::Square { .. } => "square",
        NodeKind::Circle { .. } => "circle",
        NodeKind::Polygon { .. } => "polygon",
        NodeKind::Surface { .. } => "surface",
        NodeKind::Import(_) => "import",
        NodeKind::Text(_) => "text",
        NodeKind::Part { .. } => "part",
        NodeKind::Sketch(_) => "sketch",
        NodeKind::Fillet(f) => f.kind.module(),
    }
}

/// Whether a primitive keeps OpenSCAD's polygon: `$fn` was set (or is not
/// a usable number, where OpenSCAD falls back to 3 fragments). `$fa`/`$fs`
/// curves become exact. `$fe` (`discretization-by-error`) is not
/// implemented, so it cannot reach here.
fn fn_is_explicit(disc: &Discretizer) -> bool {
    disc.fn_ > 0.0 || !disc.fn_.is_finite()
}

/// `a × b` for row-major 4x4 matrices.
fn mul(a: &Matrix, b: &Matrix) -> Matrix {
    let mut r = [[0.0; 4]; 4];
    for (i, row) in r.iter_mut().enumerate() {
        for (j, v) in row.iter_mut().enumerate() {
            *v = (0..4).map(|k| a[i][k] * b[k][j]).sum();
        }
    }
    r
}

/// A blend tool with its blend triangles as planar facets: the export of
/// a fillet call with `$fn` set. Each triangle is its own plane record;
/// reconstruction merges the two of each quad of the arc.
fn faceted_blend(t: &meshbrep::blend::Tool) -> TaggedMesh {
    let mut m = t.mesh.clone();
    for k in 0..m.triangles.len() {
        if !t.blend.contains(&m.triangle_surface[k]) {
            continue;
        }
        let [a, b, c] = m.triangles[k].map(|i| m.positions[i as usize]);
        let u = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
        let v = [c[0] - a[0], c[1] - a[1], c[2] - a[2]];
        let n = [
            u[1] * v[2] - u[2] * v[1],
            u[2] * v[0] - u[0] * v[2],
            u[0] * v[1] - u[1] * v[0],
        ];
        m.surfaces.push(Surface::Plane {
            origin: a,
            normal: unit(n),
        });
        m.triangle_surface[k] = (m.surfaces.len() - 1) as u32;
    }
    m
}

/// A transform node whose matrix is finite but flattens space (a zero
/// scale): its children have no volume however they are built.
fn flattening(n: &Node) -> bool {
    match &n.kind {
        NodeKind::Transform { matrix, .. } => {
            matrix.iter().flatten().all(|v| v.is_finite()) && det3(matrix) == 0.0
        }
        _ => false,
    }
}

fn det3(m: &Matrix) -> f64 {
    m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1])
        - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
        + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0])
}

fn apply(m: &Matrix, p: [f64; 3]) -> [f64; 3] {
    [0, 1, 2].map(|i| m[i][0] * p[0] + m[i][1] * p[1] + m[i][2] * p[2] + m[i][3])
}

/// The inverse of an affine matrix, `None` when it flattens.
fn invert(m: &Matrix) -> Option<Matrix> {
    let d = det3(m);
    if d == 0.0 || !d.is_finite() {
        return None;
    }
    let a = |i: usize, j: usize| m[i][j];
    // The 3 x 3 inverse by cofactors, then the translation.
    let mut r = [[0.0; 4]; 4];
    r[0][0] = (a(1, 1) * a(2, 2) - a(1, 2) * a(2, 1)) / d;
    r[0][1] = (a(0, 2) * a(2, 1) - a(0, 1) * a(2, 2)) / d;
    r[0][2] = (a(0, 1) * a(1, 2) - a(0, 2) * a(1, 1)) / d;
    r[1][0] = (a(1, 2) * a(2, 0) - a(1, 0) * a(2, 2)) / d;
    r[1][1] = (a(0, 0) * a(2, 2) - a(0, 2) * a(2, 0)) / d;
    r[1][2] = (a(0, 2) * a(1, 0) - a(0, 0) * a(1, 2)) / d;
    r[2][0] = (a(1, 0) * a(2, 1) - a(1, 1) * a(2, 0)) / d;
    r[2][1] = (a(0, 1) * a(2, 0) - a(0, 0) * a(2, 1)) / d;
    r[2][2] = (a(0, 0) * a(1, 1) - a(0, 1) * a(1, 0)) / d;
    for row in r.iter_mut().take(3) {
        row[3] = -(row[0] * a(0, 3) + row[1] * a(1, 3) + row[2] * a(2, 3));
    }
    r[3][3] = 1.0;
    Some(r)
}

fn linear(m: &Matrix, d: [f64; 3]) -> [f64; 3] {
    [0, 1, 2].map(|i| m[i][0] * d[0] + m[i][1] * d[1] + m[i][2] * d[2])
}

fn norm(v: [f64; 3]) -> f64 {
    (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt()
}

fn unit(v: [f64; 3]) -> [f64; 3] {
    let l = norm(v);
    v.map(|x| x / l)
}

/// The scale `s` when the linear part of `m` is `s` times an orthogonal
/// matrix (a rotation, possibly with a mirror): the transforms under which
/// circles stay circles. `None` for a non-uniform scale or a shear, which
/// turn cylinders and spheres into elliptic ones that STEP export cannot
/// write exactly yet.
fn similarity_scale(m: &Matrix) -> Option<f64> {
    let col = |j: usize| [m[0][j], m[1][j], m[2][j]];
    let c = [col(0), col(1), col(2)];
    let dot = |a: [f64; 3], b: [f64; 3]| a[0] * b[0] + a[1] * b[1] + a[2] * b[2];
    let s2 = (dot(c[0], c[0]) + dot(c[1], c[1]) + dot(c[2], c[2])) / 3.0;
    if !(s2 > 0.0 && s2.is_finite()) {
        return None;
    }
    // Rounding in a chain of rotations leaves about 1e-16 relative; the
    // surface merge tolerance downstream is 1e-9 of the model's size.
    let tol = 1e-10 * s2;
    for i in 0..3 {
        for j in 0..3 {
            let want = if i == j { s2 } else { 0.0 };
            if (dot(c[i], c[j]) - want).abs() > tol {
                return None;
            }
        }
    }
    Some(s2.sqrt())
}

/// The image of a surface record under `m`. Planes map under any affine
/// matrix (their normal by the cofactor matrix); curved surfaces only
/// under a similarity of scale `s`.
fn transform_surface(s: &Surface, m: &Matrix, scale: Option<f64>) -> Surface {
    match (s, scale) {
        (Surface::Plane { origin, normal }, _) => {
            let c = |i: usize, j: usize| {
                let (i1, i2) = ((i + 1) % 3, (i + 2) % 3);
                let (j1, j2) = ((j + 1) % 3, (j + 2) % 3);
                m[i1][j1] * m[i2][j2] - m[i1][j2] * m[i2][j1]
            };
            // The cofactor matrix is det(L) L^-T, which maps normals.
            let n = [0, 1, 2].map(|i| (0..3).map(|j| c(i, j) * normal[j]).sum::<f64>());
            Surface::Plane {
                origin: apply(m, *origin),
                normal: unit(n),
            }
        }
        (
            Surface::Cylinder {
                origin,
                axis,
                radius,
            },
            Some(k),
        ) => Surface::Cylinder {
            origin: apply(m, *origin),
            axis: unit(linear(m, *axis)),
            radius: radius * k,
        },
        (Surface::Cone { apex, axis, slope }, Some(_)) => Surface::Cone {
            apex: apply(m, *apex),
            axis: unit(linear(m, *axis)),
            slope: *slope,
        },
        (Surface::Sphere { center, radius }, Some(k)) => Surface::Sphere {
            center: apply(m, *center),
            radius: radius * k,
        },
        (
            Surface::Torus {
                center,
                axis,
                major_radius,
                minor_radius,
            },
            Some(k),
        ) => Surface::Torus {
            center: apply(m, *center),
            axis: unit(linear(m, *axis)),
            major_radius: major_radius * k,
            minor_radius: minor_radius * k,
        },
        // A B-spline patch (a blend between curved faces) is the same
        // patch of the mapped control points under any affine map, a
        // non-uniform scale included: rational ones too, weights kept.
        (Surface::BSpline(b), _) => {
            let mut b = b.clone();
            for row in &mut b.control {
                for q in row.iter_mut() {
                    *q = apply(m, *q);
                }
            }
            Surface::BSpline(b)
        }
        _ => Surface::Faceted,
    }
}

impl Walk<'_> {
    fn stopped(&self) -> bool {
        self.opts
            .interrupt
            .as_ref()
            .is_some_and(|f| f.load(Ordering::Relaxed))
            || self.opts.guard.as_ref().is_some_and(|g| g.stopped())
    }

    fn note(&mut self, kind: SubstitutionKind, n: &Node, detail: String) {
        let module = module_name(&n.kind);
        let loc = loc_of(n);
        let key: SubKey = (
            kind,
            module,
            detail.clone(),
            loc.as_ref()
                .map(|l| (l.unit, l.span.file.0, l.span.start, l.span.end)),
        );
        if let Some(&i) = self.sub_index.get(&key) {
            self.subs[i].count += 1;
            return;
        }
        self.sub_index.insert(key, self.subs.len());
        self.subs.push(Substitution {
            kind,
            module,
            detail,
            loc,
            count: 1,
        });
    }

    /// The export render of `top` under `m`.
    ///
    /// The walk keeps the nodes waiting for their children on a heap
    /// stack rather than recursing once per level, as the normal render
    /// does (`evaluate.rs`, `Ctx::walk`): trees from recursive modules are
    /// as deep as the evaluator allows (100,000 levels), and BOSL2 nests
    /// groups deeply, so a recursive walk would overflow the native stack
    /// (and far sooner a wasm worker's) after the normal render succeeded.
    fn node(&mut self, top: &Node, m: &Matrix) -> Result<Res, Unsupported> {
        struct Waiting<'n> {
            n: &'n Node,
            m: Matrix,
            op: OpType,
            kids: Vec<Res>,
            next: usize,
            /// A recurring subtree's first instance: kept when done.
            memo: Option<memo::Start>,
        }
        let mut stack: Vec<Waiting<'_>> = Vec::new();
        let mut visit: Option<(&Node, Matrix)> = Some((top, *m));
        loop {
            let mut done = None;
            if let Some((n, m)) = visit.take() {
                if self.stopped() {
                    return Err(Unsupported::interrupted());
                }
                let key = self.memo_plan.get(&memo::node_id(n)).copied();
                let placed = match key {
                    Some(k) => self.place_memo(k, &m),
                    None => None,
                };
                match self.branch(n, &m) {
                    _ if placed.is_some() => done = placed,
                    Some((op, child_m)) => {
                        let memo = key
                            .filter(|k| !self.memo.contains_key(k))
                            .map(|k| self.memo_start(k, &m));
                        stack.push(Waiting {
                            n,
                            m: child_m,
                            op,
                            kids: Vec::with_capacity(n.children.len()),
                            next: 0,
                            memo,
                        })
                    }
                    None => {
                        if self.trace {
                            // The waiting nodes are the leaf's ancestors:
                            // the bottom one is the top, whose `next`
                            // has just moved past the child being built.
                            let child = stack.first().map(|w| (w.next - 1) as u32);
                            let part = stack.iter().rev().find_map(|w| match &w.n.kind {
                                NodeKind::Part { name } => Some(name.clone()),
                                _ => None,
                            });
                            self.here = (child, part);
                        }
                        done = Some(self.leaf_traced(n, &m)?)
                    }
                }
            }
            if let Some(r) = done {
                match stack.last_mut() {
                    None => return Ok(r),
                    Some(w) => w.kids.push(r),
                }
            }
            let Some(w) = stack.last_mut() else {
                unreachable!("the walk ends when the top node is done")
            };
            // The next child that is not a background (`%`) one.
            while w.next < w.n.children.len() && is_background(&w.n.children[w.next]) {
                w.next += 1;
            }
            if let Some(c) = w.n.children.get(w.next) {
                w.next += 1;
                visit = Some((c, w.m));
                continue;
            }
            let w = stack.pop().expect("a waiting node");
            let mut r = self.combine(w.op, w.kids)?;
            // A fillet's blends, except on the node a traced walk selects
            // for: that walk is of its child (with its first pass, for a
            // second pass's selection).
            let at_top = self.trace && std::ptr::eq(w.n, top);
            if matches!(w.n.kind, NodeKind::Fillet(_)) && (!at_top || self.first_pass.is_some()) {
                let alone;
                let passes: &[crate::fillet::Pass] = match self.first_pass {
                    Some(s) if at_top => {
                        alone = [crate::fillet::Pass::FirstAlone(s)];
                        &alone
                    }
                    _ => &[crate::fillet::Pass::First, crate::fillet::Pass::Second],
                };
                if self.trace {
                    let child = stack.first().map(|p| (p.next - 1) as u32);
                    let part = stack.iter().rev().find_map(|p| match &p.n.kind {
                        NodeKind::Part { name } => Some(name.clone()),
                        _ => None,
                    });
                    self.here = (child, part);
                }
                r = self.fillet(w.n, &w.m, r, passes)?;
            }
            if let Some(start) = w.memo {
                self.memo_keep(start, &r);
            }
            // Then on with the parent's next child.
            match stack.last_mut() {
                None => return Ok(r),
                Some(p) => p.kids.push(r),
            }
        }
    }

    /// For a node whose result is an operation on its children: the
    /// operation and the matrix the children are placed under.
    fn branch(&self, n: &Node, m: &Matrix) -> Option<(OpType, Matrix)> {
        match &n.kind {
            NodeKind::Root
            | NodeKind::Group { .. }
            | NodeKind::Render { .. }
            | NodeKind::Color { .. }
            | NodeKind::Part { .. }
            | NodeKind::Csg(CsgOp::Union) => Some((OpType::Add, *m)),
            NodeKind::IntersectionFor | NodeKind::Csg(CsgOp::Intersection) => {
                Some((OpType::Intersect, *m))
            }
            NodeKind::Csg(CsgOp::Difference) => Some((OpType::Subtract, *m)),
            // A fillet: its children's union, then its blend tools
            // ([`Walk::fillet`]), tagged with their exact surfaces.
            NodeKind::Fillet(_) => Some((OpType::Add, *m)),
            NodeKind::Transform { matrix, .. } => {
                let next = mul(m, matrix);
                let d = det3(&next);
                // A matrix that removes the object, or flattens it, is
                // left to the normal render ([`Walk::leaf`]).
                let bad =
                    matrix.iter().flatten().any(|v| !v.is_finite()) || d == 0.0 || !d.is_finite();
                (!bad).then_some((OpType::Add, next))
            }
            _ => None,
        }
    }

    /// A fillet node's blends (`docs/fillets.md`, section 6.4) on its
    /// children's union `r`, under `m`: the same plan's tools as the
    /// normal render's, with arcs of [`Walk::mult`] times the segments and
    /// every triangle tagged with its exact surface (cylinder, plane,
    /// sphere), so the export reconstructs true blends. With `$fn` set on
    /// the call, the arcs are kept as the polygon, as for any curve.
    fn fillet(
        &mut self,
        n: &Node,
        m: &Matrix,
        r: Res,
        passes: &[crate::fillet::Pass],
    ) -> Result<Res, Unsupported> {
        let Res::Solid(mut child) = r else {
            return Ok(r);
        };
        if child.is_empty() {
            return Ok(Res::Solid(child));
        }
        let scale = similarity_scale(m);
        let inv = invert(m);
        let mut tools = 0;
        let mut explicit_fn = false;
        let mut sagitta: f64 = 0.0;
        let mut area = 0.0;
        // A two-pass call (`docs/fillets.md`, section 15.6) applies its
        // second pass to what its first made, as a nested call would.
        for &pass in passes {
            // The solid's triangles back in the node's own coordinates,
            // where the plan is: a circular edge's tool takes its sections
            // at the vertices of the polygon beside it, as the walk (or
            // the first pass) tessellated it.
            let local = inv.map(|inv| {
                let gl = child.get_mesh_gl64(-1);
                let np = (gl.num_prop as usize).max(3);
                let at = |i: u64| {
                    let k = i as usize * np;
                    apply(
                        &inv,
                        [
                            gl.vert_properties[k],
                            gl.vert_properties[k + 1],
                            gl.vert_properties[k + 2],
                        ],
                    )
                };
                gl.tri_verts
                    .chunks(3)
                    .map(|c| [at(c[0]), at(c[1]), at(c[2])])
                    .collect::<Vec<_>>()
            });
            let blends = match crate::fillet::blends(
                self.renderer,
                n,
                self.keys,
                self.opts,
                self.mult,
                local.as_deref(),
                false,
                pass,
            ) {
                Ok(Some(b)) => b,
                Ok(None) => break,
                Err(_) => return Err(Unsupported::interrupted()),
            };
            let base = self.surfaces.len();
            let mut adds = vec![Res::Solid(child)];
            let mut subs = Vec::new();
            for t in &blends.tools {
                let placed = if blends.explicit_fn {
                    self.place(&faceted_blend(t), m, scale)?
                } else {
                    self.place(&t.mesh, m, scale)?
                };
                if t.add {
                    adds.push(placed);
                } else {
                    subs.push(placed);
                }
            }
            self.record(n, base);
            // The fit scales with the placement (a similarity's factor;
            // the largest stretch of any other affine map).
            let stretch = scale.unwrap_or_else(|| {
                (0..3)
                    .map(|j| {
                        let c = [m[0][j], m[1][j], m[2][j]];
                        (c[0] * c[0] + c[1] * c[1] + c[2] * c[2]).sqrt()
                    })
                    .fold(0.0, f64::max)
            });
            for t in &blends.tools {
                self.surface_fit = self.surface_fit.max(t.fit * stretch);
            }
            tools += blends.tools.len();
            explicit_fn |= blends.explicit_fn;
            sagitta = sagitta.max(blends.sagitta);
            area += blends.area;
            let joined = self.combine(OpType::Add, adds)?;
            let mut parts = vec![joined];
            parts.extend(subs);
            match self.combine(OpType::Subtract, parts)? {
                Res::Solid(s) => child = s,
                other => return Ok(other),
            }
        }
        if tools == 0 {
            return Ok(Res::Solid(child));
        }
        if explicit_fn {
            self.note(
                SubstitutionKind::Polygon,
                n,
                format!(
                    "keeps its blends' arcs as polygons because $fn is set ({tools} tool{})",
                    if tools == 1 { "" } else { "s" }
                ),
            );
        } else if let Some(s) = scale {
            self.normal_sagitta = self.normal_sagitta.max(sagitta * s);
            self.normal_volume_bound += area * sagitta * s * s * s;
            self.note(
                SubstitutionKind::Exact,
                n,
                format!(
                    "is exported with exact blends ({tools} tool{}), not the polygonal arcs of the mesh ($fn is not set)",
                    if tools == 1 { "" } else { "s" }
                ),
            );
        } else {
            self.note(
                SubstitutionKind::Faceted,
                n,
                "exports its blends as planar facets: a non-uniform scale or shear makes them elliptic".into(),
            );
        }
        Ok(Res::Solid(child))
    }

    /// [`Walk::leaf`], recording `n` as the origin of every surface it
    /// adds to the table.
    fn leaf_traced(&mut self, n: &Node, m: &Matrix) -> Result<Res, Unsupported> {
        let base = self.surfaces.len();
        let r = self.leaf(n, m);
        // Marks for a primitive that was delegated after all are void.
        self.pending_polygons.clear();
        let r = r?;
        self.record(n, base);
        Ok(r)
    }

    /// Records `n` as the origin of the surfaces from `base` on (and, in a
    /// traced walk, where they lie: [`Walk::here`]).
    fn record(&mut self, n: &Node, base: usize) {
        if self.trace {
            let (child, part) = self.here.clone();
            let leaf = self.leaves;
            self.leaves += 1;
            for s in base..self.surfaces.len() {
                self.provenance.push(Provenance {
                    child,
                    part: part.clone(),
                    leaf,
                    polygon: self.polygon_of.get(&s).copied(),
                });
            }
        }
        if self.surface_origin.len() < self.surfaces.len() {
            let module = module_name(&n.kind);
            let loc = loc_of(n);
            let key = (
                module,
                loc.as_ref()
                    .map(|l| (l.unit, l.span.file.0, l.span.start, l.span.end)),
            );
            let next = self.origins.len() as u32;
            let o = *self.origin_index.entry(key).or_insert(next);
            if o == next {
                self.origins.push((module, loc));
            }
            self.surface_origin.resize(self.surfaces.len(), o);
        }
    }

    /// A node that is not an operation on its children (a primitive, or
    /// something the normal render builds), under `m`.
    fn leaf(&mut self, n: &Node, m: &Matrix) -> Result<Res, Unsupported> {
        match &n.kind {
            NodeKind::Transform { .. } => self.delegate(n, m, None),
            NodeKind::Cube { size, center } => {
                if size.iter().any(|&s| s <= 0.0 || !s.is_finite()) {
                    return self.delegate(n, m, None);
                }
                let local = if *center {
                    Transform::translate(size.map(|s| -s / 2.0))
                } else {
                    Transform::IDENTITY
                };
                let t = primitives::cuboid(*size, &local);
                self.place(&t, m, None)
            }
            NodeKind::Cylinder {
                h,
                r1,
                r2,
                center,
                disc,
            } => {
                let (h, r1, r2) = (*h, *r1, *r2);
                let bad = h <= 0.0
                    || !h.is_finite()
                    || r1 < 0.0
                    || !r1.is_finite()
                    || r2 < 0.0
                    || !r2.is_finite()
                    || (r1 <= 0.0 && r2 <= 0.0);
                let fragments = fragments::circular_segments(disc, r1.max(r2));
                if bad || fragments.is_none() {
                    return self.delegate(n, m, None);
                }
                let f = fragments.unwrap_or(3).max(3) as u32;
                if fn_is_explicit(disc) {
                    self.note(
                        SubstitutionKind::Polygon,
                        n,
                        format!(
                            "keeps its {f}-sided polygon because $fn is set; leave $fn unset (use $fa and $fs) to export a true {}",
                            if r1 == r2 { "cylinder" } else { "cone" }
                        ),
                    );
                    let ps = crate::primitives::cylinder(h, r1, r2, *center, disc);
                    return self.polyset_planes(n, &ps, m);
                }
                let Some(s) = similarity_scale(m) else {
                    return self.delegate(
                        n,
                        m,
                        Some(format!(
                            "is exported as planar facets: a non-uniform scale or shear makes it an elliptic {}, which STEP export cannot write exactly yet",
                            if r1 == r2 { "cylinder" } else { "cone" }
                        )),
                    );
                };
                let segs = primitives::aligned_segments(f.saturating_mul(self.mult));
                let local = if *center {
                    Transform::translate([0.0, 0.0, -h / 2.0])
                } else {
                    Transform::IDENTITY
                };
                let t = primitives::frustum(h, r1, r2, segs, &local);
                let sag = r1.max(r2) * (1.0 - (std::f64::consts::PI / f64::from(f)).cos());
                let slant = (h * h + (r1 - r2) * (r1 - r2)).sqrt();
                let area = std::f64::consts::PI * (r1 + r2) * slant;
                self.normal_sagitta = self.normal_sagitta.max(sag * s);
                self.normal_volume_bound += area * sag * s * s * s;
                self.note(
                    SubstitutionKind::Exact,
                    n,
                    format!(
                        "is exported as an exact {}, not the {f}-sided polygon of the mesh ($fn is not set)",
                        if r1 == r2 { "cylinder" } else { "cone" }
                    ),
                );
                self.place(&t, m, Some(s))
            }
            NodeKind::Sphere { r, disc } => {
                let r = *r;
                let fragments = fragments::circular_segments(disc, r);
                if r <= 0.0 || !r.is_finite() || fragments.is_none() {
                    return self.delegate(n, m, None);
                }
                let f = fragments.unwrap_or(3).max(3) as u32;
                if fn_is_explicit(disc) {
                    self.note(
                        SubstitutionKind::Polygon,
                        n,
                        format!(
                            "keeps its {f}-fragment polyhedron because $fn is set; leave $fn unset (use $fa and $fs) to export a true sphere"
                        ),
                    );
                    let ps = crate::primitives::sphere(r, disc);
                    return self.polyset_planes(n, &ps, m);
                }
                let Some(s) = similarity_scale(m) else {
                    return self.delegate(
                        n,
                        m,
                        Some(
                            "is exported as planar facets: a non-uniform scale or shear makes it an ellipsoid, which STEP export cannot write exactly yet"
                                .into(),
                        ),
                    );
                };
                // At least 16: with 8 (OpenSCAD's 5 fragments, aligned) a
                // sphere is three rings, and the volume cross-check
                // against such a mesh is too loose to catch anything.
                let segs = primitives::aligned_segments(f.saturating_mul(self.mult).max(16));
                let t = primitives::sphere(r, segs, &Transform::IDENTITY);
                // OpenSCAD's sphere is rings of quads, half a ring step
                // from each pole (`primitives.cc`). A quad's middle is
                // inside the sphere by both its chords' sagittas at once.
                let rings = f64::from(f.div_ceil(2));
                let sag = r
                    * (1.0
                        - (std::f64::consts::PI / f64::from(f)).cos()
                            * (std::f64::consts::PI / (2.0 * rings)).cos());
                self.normal_sagitta = self.normal_sagitta.max(sag * s);
                self.normal_volume_bound += 4.0 * std::f64::consts::PI * r * r * sag * s * s * s;
                self.note(
                    SubstitutionKind::Exact,
                    n,
                    format!(
                        "is exported as an exact sphere, not the {f}-fragment polyhedron of the mesh ($fn is not set)"
                    ),
                );
                self.place(&t, m, Some(s))
            }
            NodeKind::Polyhedron { .. } => self.delegate(
                n,
                m,
                Some(
                    "is exported as planar facets (a polyhedron has no curved surfaces to recover)"
                        .into(),
                ),
            ),
            NodeKind::Square { .. }
            | NodeKind::Circle { .. }
            | NodeKind::Polygon { .. }
            | NodeKind::Sketch(_) => Ok(Res::TwoD),
            NodeKind::LinearExtrude(_) | NodeKind::RotateExtrude { .. }
                if self.extrusions == Extrusions::Faceted =>
            {
                self.delegate(
                    n,
                    m,
                    Some(
                        "is exported as planar facets: the model did not reconstruct with its extrusions exact"
                            .into(),
                    ),
                )
            }
            NodeKind::LinearExtrude(e) => {
                self.exact_extrusions += 1;
                self.linear_extrude(n, e, m)
            }
            NodeKind::RotateExtrude {
                angle, start, disc, ..
            } => {
                self.exact_extrusions += 1;
                self.rotate_extrude(n, *angle, *start, disc, m)
            }
            _ => {
                let module = module_name(&n.kind);
                self.delegate(
                    n,
                    m,
                    Some(format!(
                        "is exported as planar facets: {module}() has no exact surfaces in STEP export yet"
                    )),
                )
            }
        }
    }

    /// `applyToChildren` for 3D over the children's results: OpenSCAD's
    /// rules for 2D children, empty children and empty operands, with the
    /// boolean on Manifold's CSG tree as the normal render does it.
    fn combine(&mut self, op: OpType, kids: Vec<Res>) -> Result<Res, Unsupported> {
        // The first child with geometry sets the dimension.
        let dim = kids
            .iter()
            .find_map(|r| match r {
                Res::Nothing => None,
                Res::Solid(_) => Some(3),
                Res::TwoD => Some(2),
            })
            .unwrap_or(0);
        if dim == 2 {
            return Ok(Res::TwoD);
        }
        // 2D children of a 3D operation are ignored (`collectChildren3D`).
        let mut solids: Vec<Option<Manifold>> = kids
            .into_iter()
            .map(|r| match r {
                Res::Solid(s) => Some(s),
                Res::Nothing | Res::TwoD => None,
            })
            .collect();
        if solids.is_empty() {
            return Ok(Res::Nothing);
        }
        if solids.len() == 1 {
            return Ok(solids.pop().flatten().map_or(Res::Nothing, Res::Solid));
        }
        if op == OpType::Add {
            solids.retain(|s| s.as_ref().is_some_and(|s| !s.is_empty()));
            match solids.len() {
                0 => return Ok(Res::Nothing),
                1 => return Ok(solids.pop().flatten().map_or(Res::Nothing, Res::Solid)),
                _ => {}
            }
        }
        let mut parts = Vec::with_capacity(solids.len());
        for s in solids {
            match s.filter(|s| !s.is_empty()) {
                Some(s) => parts.push(s),
                None => {
                    if op == OpType::Intersect || (op == OpType::Subtract && parts.is_empty()) {
                        return Ok(Res::Nothing);
                    }
                }
            }
        }
        if parts.len() == 1 {
            return Ok(parts.pop().map_or(Res::Nothing, Res::Solid));
        }
        let leaves = parts
            .into_iter()
            .map(|p| CsgNode::leaf(p.into_impl()))
            .collect();
        let out =
            Manifold::from_impl(CsgNode::op_n(op, leaves).evaluate_with_token(self.token.as_ref()));
        if out.status() == KernelError::Cancelled {
            return Err(Unsupported::interrupted());
        }
        Ok(Res::Solid(out))
    }

    /// A tagged primitive, placed under `m`, as one new original.
    fn place(
        &mut self,
        t: &TaggedMesh,
        m: &Matrix,
        scale: Option<f64>,
    ) -> Result<Res, Unsupported> {
        let base = self.surfaces.len() as u64;
        self.surfaces
            .extend(t.surfaces.iter().map(|s| transform_surface(s, m, scale)));
        for (s, p) in std::mem::take(&mut self.pending_polygons) {
            self.polygon_of.insert(base as usize + s as usize, p);
        }
        let flip = det3(m) < 0.0;
        let positions: Vec<f64> = t.positions.iter().flat_map(|&p| apply(m, p)).collect();
        let tri_verts: Vec<u64> = t
            .triangles
            .iter()
            .flat_map(|&[a, b, c]| if flip { [a, c, b] } else { [a, b, c] })
            .map(u64::from)
            .collect();
        let face_id: Vec<u64> = t
            .triangle_surface
            .iter()
            .map(|&s| u64::from(s) + base)
            .collect();
        Ok(self.solid(positions, tri_verts, face_id))
    }

    /// What the walk has before the first instance of a recurring subtree
    /// ([`memo`]) is built under `m`. The sagitta is a maximum, so it is
    /// started afresh and the subtree's own read at the end; the volume
    /// bound goes on accumulating in tree order as it always did (the
    /// first instance adds exactly what it added before), and its share
    /// is the difference.
    fn memo_start(&mut self, key: memo::MemoKey, m: &Matrix) -> memo::Start {
        let start = memo::Start {
            key,
            m: *m,
            base: self.surfaces.len(),
            subs: self.subs.iter().map(|s| s.count).collect(),
            sagitta: self.normal_sagitta,
            volume_bound: self.normal_volume_bound,
            exact_extrusions: self.exact_extrusions,
        };
        self.normal_sagitta = 0.0;
        start
    }

    /// Keeps the first instance of a recurring subtree, built as `r`.
    fn memo_keep(&mut self, start: memo::Start, r: &Res) {
        let sagitta = self.normal_sagitta;
        self.normal_sagitta = start.sagitta.max(sagitta);
        let end = self.surfaces.len();
        let result = match r {
            // Too small to be worth placing rather than building (see
            // `memo::MIN_TRIANGLES`): later instances are built in place.
            Res::Solid(s) if s.num_tri() < memo::MIN_TRIANGLES => return,
            Res::Nothing | Res::TwoD => return,
            Res::Solid(s) => {
                let gl = s.get_mesh_gl64(-1);
                let np = (gl.num_prop as usize).max(3);
                // Its triangles name only the records it added (each leaf
                // adds its own); anything else and it is not kept.
                let range = start.base as u64..end as u64;
                if !gl.face_id.iter().all(|f| range.contains(f))
                    || gl.face_id.len() * 3 != gl.tri_verts.len()
                {
                    return;
                }
                memo::Mesh {
                    positions: gl
                        .vert_properties
                        .chunks(np)
                        .flat_map(|c| [c[0], c[1], c[2]])
                        .collect(),
                    tri_verts: gl.tri_verts,
                    face_id: gl.face_id,
                }
            }
        };
        if self.surface_origin.len() != end {
            return;
        }
        let subs = self
            .subs
            .iter()
            .enumerate()
            .filter_map(|(i, s)| {
                let added = s.count - start.subs.get(i).copied().unwrap_or(0);
                (added > 0).then_some((i, added))
            })
            .collect();
        let entry = memo::Entry {
            m: start.m,
            result,
            surfaces: self.surfaces[start.base..].to_vec(),
            base: start.base,
            origins: self.surface_origin[start.base..].to_vec(),
            subs,
            sagitta,
            volume_bound: self.normal_volume_bound - start.volume_bound,
            exact_extrusions: self.exact_extrusions - start.exact_extrusions,
        };
        self.memo.insert(start.key, entry);
    }

    /// Another instance of a recurring subtree, under `m`: the first
    /// instance moved by the similarity between their placements, with
    /// fresh surface records and its reports counted again. `None` when
    /// there is no first instance yet, or the placements differ by more
    /// than a similarity (the subtree is then built as any other).
    fn place_memo(&mut self, key: memo::MemoKey, m: &Matrix) -> Option<Res> {
        let entry = self.memo.get(&key)?;
        let rel = mul(m, &invert(&entry.m)?);
        let scale = similarity_scale(&rel)?;
        let mesh = match &entry.result {
            memo::Mesh { tri_verts, .. } if tri_verts.is_empty() => {
                Err(Res::Solid(Manifold::empty()))
            }
            memo::Mesh {
                positions,
                tri_verts,
                face_id,
            } => {
                let flip = det3(&rel) < 0.0;
                let positions: Vec<f64> = positions
                    .chunks(3)
                    .flat_map(|c| apply(&rel, [c[0], c[1], c[2]]))
                    .collect();
                let tri_verts: Vec<u64> = tri_verts
                    .chunks(3)
                    .flat_map(|c| {
                        if flip {
                            [c[0], c[2], c[1]]
                        } else {
                            [c[0], c[1], c[2]]
                        }
                    })
                    .collect();
                let base = self.surfaces.len() as u64;
                let first = entry.base as u64;
                let face_id: Vec<u64> = face_id.iter().map(|f| f - first + base).collect();
                Ok((positions, tri_verts, face_id))
            }
        };
        let result = match mesh {
            Err(r) => r,
            Ok((positions, tri_verts, face_id)) => {
                match self.solid(positions, tri_verts, face_id) {
                    // Placed, it is no longer a valid mesh (rounding closed a
                    // gap): this instance is built in place instead.
                    Res::Solid(s) if s.is_empty() => return None,
                    r => r,
                }
            }
        };
        let entry = &self.memo[&key];
        self.surfaces.extend(
            entry
                .surfaces
                .iter()
                .map(|s| transform_surface(s, &rel, Some(scale))),
        );
        self.surface_origin.extend_from_slice(&entry.origins);
        for &(i, added) in &entry.subs {
            self.subs[i].count += added;
        }
        self.normal_sagitta = self.normal_sagitta.max(entry.sagitta * scale);
        self.normal_volume_bound += entry.volume_bound * scale * scale * scale;
        self.exact_extrusions += entry.exact_extrusions;
        self.placed_copies += 1;
        Some(result)
    }

    fn solid(&mut self, positions: Vec<f64>, tri_verts: Vec<u64>, face_id: Vec<u64>) -> Res {
        let id = self.next_id;
        self.next_id += 1;
        let n = tri_verts.len() as u64;
        let mesh = MeshGL64 {
            num_prop: 3,
            vert_properties: positions,
            tri_verts,
            face_id,
            run_index: vec![0, n],
            run_original_id: vec![id],
            ..Default::default()
        };
        let m = Manifold::from_mesh_gl64(&mesh);
        if m.status() != KernelError::NoError {
            return Res::Solid(Manifold::empty());
        }
        Res::Solid(m)
    }

    /// A primitive with an explicit `$fn`: OpenSCAD's own polygons, each a
    /// plane of its own (they are planar: rings of a cylinder or a
    /// sphere). Falls back to facets if the mesh does not close.
    fn polyset_planes(&mut self, n: &Node, ps: &PolySet, m: &Matrix) -> Result<Res, Unsupported> {
        if ps.is_empty() {
            return Ok(Res::Solid(Manifold::empty()));
        }
        let flip = det3(m) < 0.0;
        let pts: Vec<[f64; 3]> = ps.vertices.iter().map(|&p| apply(m, p)).collect();
        let base = self.surfaces.len();
        let mut tri_verts = Vec::new();
        let mut face_id = Vec::new();
        // The polygon's facets, for fillet selection: a sphere's every
        // face, a cylinder's sides but not its caps (whose rims are
        // edges of the modelled shape). A cap is the face whose normal,
        // before the transform, is the cylinder's axis.
        let polygon = self.polygons;
        self.polygons += 1;
        let cylinder = matches!(n.kind, NodeKind::Cylinder { .. });
        let scale = pts
            .iter()
            .flat_map(|p| p.iter().map(|x| x.abs()))
            .fold(0.0, f64::max)
            .max(1e-300);
        for f in &ps.faces {
            if f.len() < 3 {
                continue;
            }
            let poly: Vec<[f64; 3]> = f.iter().map(|&i| pts[i as usize]).collect();
            let nrm = crate::polyset::newell(&poly);
            let l = norm(nrm);
            let id = (self.surfaces.len()) as u64;
            if self.trace {
                let cap = cylinder && {
                    let local: Vec<[f64; 3]> = f.iter().map(|&i| ps.vertices[i as usize]).collect();
                    let ln = crate::polyset::newell(&local);
                    let ll = norm(ln);
                    ll > 0.0 && ln[2].abs() >= ll * (1.0 - 1e-12)
                };
                if !cap {
                    self.polygon_of.insert(self.surfaces.len(), polygon);
                }
            }
            if l > 0.0 {
                let nrm = nrm.map(|x| x / l);
                let o = poly[0];
                let planar = poly.iter().all(|p| {
                    ((p[0] - o[0]) * nrm[0] + (p[1] - o[1]) * nrm[1] + (p[2] - o[2]) * nrm[2]).abs()
                        <= 1e-9 * scale
                });
                self.surfaces.push(if planar {
                    Surface::Plane {
                        origin: o,
                        normal: nrm,
                    }
                } else {
                    Surface::Faceted
                });
            } else {
                self.surfaces.push(Surface::Faceted);
            }
            for i in 1..f.len() - 1 {
                let (a, b, c) = (f[0], f[i], f[i + 1]);
                let t = if flip { [a, c, b] } else { [a, b, c] };
                tri_verts.extend(t.map(u64::from));
                face_id.push(id);
            }
        }
        let positions: Vec<f64> = pts.iter().flatten().copied().collect();
        match self.solid(positions, tri_verts, face_id) {
            Res::Solid(s) if !s.is_empty() => Ok(Res::Solid(s)),
            _ => {
                // Not a closed mesh as it stands (shared positions under
                // different indices): the normal render's conversion
                // repairs that.
                self.surfaces.truncate(base);
                self.delegate(n, m, Some("is exported as planar facets".into()))
            }
        }
    }

    /// The node as the normal render builds it, placed under `m`, its
    /// triangles tagged [`Surface::Faceted`]. `why` is the substitution to
    /// report when it is non-empty 3D geometry.
    fn delegate(&mut self, n: &Node, m: &Matrix, why: Option<String>) -> Result<Res, Unsupported> {
        let r = self.renderer.render(n, self.keys, self.opts.clone())?;
        let nonempty_3d = match &r.geometry {
            Some(Geometry::PolySet(ps)) => !ps.is_empty(),
            Some(Geometry::Manifold(m)) => !m.is_empty(),
            _ => false,
        };
        // Reported before the conversion, so that a failure below is
        // still listed with what fell back (a broken polyhedron is a
        // mesh-only construct, not a fault of the exact surfaces).
        if nonempty_3d && let Some(why) = why {
            self.note(SubstitutionKind::Faceted, n, why);
        }
        let solid = match r.geometry {
            None => return Ok(Res::Nothing),
            Some(Geometry::Polygon2d(_)) => return Ok(Res::TwoD),
            Some(Geometry::PolySet(ps)) => {
                if ps.is_empty() {
                    return Ok(Res::Solid(Manifold::empty()));
                }
                let mut w = Vec::new();
                let mut e = Vec::new();
                let m = ManifoldGeometry::from_polyset(&ps, &GlobalIds, &mut w, &mut e).manifold;
                if m.is_empty() && flattening(n) {
                    // A transform that flattens its children (`scale([1,
                    // 0, 0])`, `issue4522.scad`) leaves a mesh with no
                    // volume. The normal render drops it from the solid,
                    // so the export does too; the volume and box checks
                    // against the normal render still hold it to that.
                    return Ok(Res::Solid(Manifold::empty()));
                }
                if m.is_empty() {
                    // A mesh export writes such a mesh as it is (OpenSCAD
                    // does too), but it encloses no solid, so it has no
                    // B-rep: say which node, rather than "empty".
                    return Err(Unsupported {
                        what: module_name(&n.kind),
                        loc: loc_of(n),
                    });
                }
                m
            }
            Some(Geometry::Manifold(mg)) => Arc::unwrap_or_clone(mg).manifold,
        };
        if solid.is_empty() {
            return Ok(Res::Solid(Manifold::empty()));
        }
        let gl = solid.get_mesh_gl64(-1);
        let np = (gl.num_prop as usize).max(3);
        let flip = det3(m) < 0.0;
        let positions: Vec<f64> = gl
            .vert_properties
            .chunks(np)
            .flat_map(|c| apply(m, [c[0], c[1], c[2]]))
            .collect();
        let tri_verts: Vec<u64> = gl
            .tri_verts
            .chunks(3)
            .flat_map(|c| {
                if flip {
                    [c[0], c[2], c[1]]
                } else {
                    [c[0], c[1], c[2]]
                }
            })
            .collect();
        let id = self.surfaces.len() as u64;
        self.surfaces.push(Surface::Faceted);
        let face_id = vec![id; tri_verts.len() / 3];
        match self.solid(positions, tri_verts, face_id) {
            Res::Solid(s) if s.is_empty() => {
                // The normal render kept it as a triangle soup (its repair
                // path for closed but non-manifold meshes); no B-rep can
                // be built from that.
                Err(Unsupported {
                    what: module_name(&n.kind),
                    loc: loc_of(n),
                })
            }
            r => Ok(r),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn similarity_is_recognised() {
        let mut m = crate::IDENTITY;
        assert_eq!(similarity_scale(&m), Some(1.0));
        m[0][0] = -2.0;
        m[1][1] = 2.0;
        m[2][2] = 2.0;
        assert_eq!(similarity_scale(&m), Some(2.0));
        m[2][2] = 3.0;
        assert_eq!(similarity_scale(&m), None);
        // A rotation by 30 degrees about z, scaled by 0.5.
        let (s, c) = (
            0.5 * 30f64.to_radians().sin(),
            0.5 * 30f64.to_radians().cos(),
        );
        let r = [
            [c, -s, 0.0, 1.0],
            [s, c, 0.0, 2.0],
            [0.0, 0.0, 0.5, 3.0],
            [0.0, 0.0, 0.0, 1.0],
        ];
        let k = similarity_scale(&r).unwrap();
        assert!((k - 0.5).abs() < 1e-15);
    }

    #[test]
    fn planes_map_under_any_affine_matrix() {
        // x = 1 under a shear x' = x + y stays a plane: x' - y' = 1.
        let mut m = crate::IDENTITY;
        m[0][1] = 1.0;
        let p = Surface::Plane {
            origin: [1.0, 0.0, 0.0],
            normal: [1.0, 0.0, 0.0],
        };
        let Surface::Plane { origin, normal } = transform_surface(&p, &m, None) else {
            panic!()
        };
        let h = std::f64::consts::FRAC_1_SQRT_2;
        assert!((normal[0] - h).abs() < 1e-15 && (normal[1] + h).abs() < 1e-15);
        assert_eq!(origin, [1.0, 0.0, 0.0]);
        // A sphere under a non-uniform scale is not representable.
        let s = Surface::Sphere {
            center: [0.0; 3],
            radius: 1.0,
        };
        assert_eq!(transform_surface(&s, &m, None), Surface::Faceted);
    }
}
