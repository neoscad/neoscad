//! The plan of a `fillet_edges()`/`chamfer_edges()` call without its
//! geometry (`docs/fillets.md`, stage F1): the B-rep of the call's child,
//! the facts of each of its edges, and which of them the call's selectors
//! pick, with the diagnostics a host reports.
//!
//! The child's B-rep is built as the STEP export builds one: an export
//! render of the children ([`crate::exact::walk::export_render_traced`],
//! which also records where each surface came from), then
//! `meshbrep::reconstruct`. That costs a render and a reconstruction, so
//! the facts are cached on the [`Renderer`] by the children's keys: a
//! second request, another selector on the same child, or the same child
//! in a loop reuses them. The facts are a pure function of the subtree,
//! so a warm request answers exactly as a cold one.
//!
//! Selection is cheap and runs on every request ([`plan`]). Edges are
//! numbered in one canonical order (class, curve, centre, length) before
//! anything uses their order, so reports and drawings do not depend on
//! how the reconstruction happened to number them.

mod curve;
mod select;

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::Arc;
use std::sync::atomic::Ordering;

use eval::dump::Keys;
pub use eval::fillet::FilletKind;
use eval::fillet::FilletNode;
pub use eval::fillet::selector::Curve as CurveKind;
use eval::fillet::selector::{Item, Selector};
use eval::node::{Node, NodeKind};
use lang::diag::{DiagCode, Severity};
use meshbrep::{Brep, Curve, Surface};

use crate::evaluate::{MsgLoc, RenderOptions, Renderer};
use crate::exact::walk;
use curve::{V, cross, dot, norm, sub, unit};

/// How the material meets at an edge (build123d's `Convexity`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Sense {
    /// The material angle is under 180°: a fillet removes material.
    Convex,
    /// Over 180°: a fillet adds material.
    Concave,
    /// The faces are tangent along the whole edge.
    Smooth,
    /// Convex in places and concave in others.
    Saddle,
}

impl Sense {
    pub fn name(self) -> &'static str {
        match self {
            Sense::Convex => "convex",
            Sense::Concave => "concave",
            Sense::Smooth => "smooth",
            Sense::Saddle => "saddle",
        }
    }
}

/// Which blend construction an edge needs (`docs/fillets.md`, 6.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Class {
    /// A line whose faces are planes containing it or cylinders parallel
    /// to it: a 2D fillet swept along the line.
    Translational,
    /// A circle whose faces are surfaces of revolution about its axis: a
    /// 2D fillet revolved about it.
    Rotational,
    /// Anything else: not blended in v1.
    Other,
}

impl Class {
    pub fn name(self) -> &'static str {
        match self {
            Class::Translational => "translational",
            Class::Rotational => "rotational",
            Class::Other => "other",
        }
    }
}

/// Why an edge is never filleted (`docs/fillets.md`, 5.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Skip {
    /// Between two facets of one `$fn` polygon.
    Seam,
    /// Between tangent faces.
    Tangent,
    /// A face on either side is a faceted region (`hull`, `polyhedron`,
    /// an import), whose edges are its tessellation's.
    Faceted,
}

impl Skip {
    pub fn name(self) -> &'static str {
        match self {
            Skip::Seam => "polygon seam",
            Skip::Tangent => "tangent",
            Skip::Faceted => "faceted",
        }
    }
}

/// One edge of the child's B-rep and what selection asks of it.
#[derive(Debug, Clone, PartialEq)]
pub struct EdgeFact {
    pub curve: CurveKind,
    pub sense: Sense,
    /// The angle the material fills at the middle of the edge, in
    /// degrees: 90 at a box's edge, 270 in an inside corner.
    pub angle: f64,
    pub class: Class,
    pub length: f64,
    /// Its centre of mass, which `>z` and `>>z[i]` compare.
    pub center: V,
    pub from: V,
    pub to: V,
    /// A closed curve (a full circle), whose `from` is its `to`.
    pub closed: bool,
    /// A line's unit direction.
    pub direction: Option<V>,
    /// A circle's unit axis.
    pub axis: Option<V>,
    pub radius: Option<f64>,
    /// The surface kinds of its two faces (`"plane"`, `"cylinder"`, ...,
    /// or `"faceted"`).
    pub faces: [&'static str; 2],
    /// Per face, the children of the call its surface came from.
    pub children: [Vec<u32>; 2],
    /// Per face, the full names of the parts around its surface.
    pub parts: [Vec<String>; 2],
    /// Per face, the leaf instances its surface came from.
    pub leaves: [Vec<u32>; 2],
    pub skip: Option<Skip>,
    /// For a seam or a faceted edge, the leaf node it belongs to (an index
    /// into [`Facts::origins`]).
    pub origin: Option<u32>,
    /// Points along it, for drawing, for `box(...)` and for anchors.
    pub path: Vec<V>,
}

/// Everything selection needs to know about a fillet's child.
#[derive(Debug, Clone, PartialEq)]
pub struct Facts {
    /// The B-rep's edges (its periodic surfaces' seams left out), in the
    /// canonical order.
    pub edges: Vec<EdgeFact>,
    /// The child's bounding box.
    pub bbox: Option<(V, V)>,
    /// Positions compare equal within this: 1e-6 of the box's diagonal.
    pub tolerance: f64,
    /// The leaves that made surfaces: module and location.
    pub origins: Vec<(&'static str, Option<MsgLoc>)>,
    /// The export render's segment multiplier the B-rep was built at (1,
    /// or 2 or 4 after a topology mismatch).
    pub mult: u32,
}

/// Why a child has no facts.
#[derive(Debug, Clone, PartialEq)]
pub enum Unavailable {
    /// The children render to nothing.
    Empty,
    /// The children are 2D.
    TwoD,
    /// No B-rep: the reconstruction's reason.
    NoBrep(String),
    /// The request was cancelled or a limit stopped it.
    Interrupted,
}

type FactsKey = Vec<(u128, bool)>;

/// Recent children's facts ([`Renderer`]'s), most recently used last.
#[derive(Debug, Default)]
pub struct FactsCache {
    entries: VecDeque<(FactsKey, Result<Arc<Facts>, Unavailable>)>,
}

/// Facts kept per renderer: a model has few fillet calls, and an edit
/// changes one or two of them.
const FACTS_KEPT: usize = 32;

impl FactsCache {
    fn get(&mut self, k: &FactsKey) -> Option<Result<Arc<Facts>, Unavailable>> {
        let i = self.entries.iter().position(|(key, _)| key == k)?;
        let e = self.entries.remove(i)?;
        let r = e.1.clone();
        self.entries.push_back(e);
        Some(r)
    }

    fn put(&mut self, k: FactsKey, r: Result<Arc<Facts>, Unavailable>) {
        if self.entries.len() >= FACTS_KEPT {
            self.entries.pop_front();
        }
        self.entries.push_back((k, r));
    }

    pub(crate) fn clear(&mut self) {
        self.entries.clear();
    }
}

fn is_background(n: &Node) -> bool {
    n.origin.as_ref().is_some_and(|o| o.tag_background)
}

/// The facts of fillet node `node`'s child (the union of its children),
/// from the renderer's cache or built now.
pub fn facts(
    renderer: &Renderer,
    node: &Node,
    keys: &Keys,
    opts: &RenderOptions,
) -> Result<Arc<Facts>, Unavailable> {
    let key: FactsKey = node
        .children
        .iter()
        .map(|c| (keys.get(c), is_background(c)))
        .collect();
    let hit = renderer
        .fillet_facts
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(&key);
    if let Some(r) = hit {
        return r;
    }
    let r = compute(renderer, node, keys, opts).map(Arc::new);
    // A stopped request says nothing about the child.
    if r != Err(Unavailable::Interrupted) {
        renderer
            .fillet_facts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .put(key, r.clone());
    }
    r
}

fn stopped(opts: &RenderOptions) -> bool {
    opts.interrupt
        .as_ref()
        .is_some_and(|f| f.load(Ordering::Relaxed))
        || opts.guard.as_ref().is_some_and(|g| g.stopped())
}

fn compute(
    renderer: &Renderer,
    node: &Node,
    keys: &Keys,
    opts: &RenderOptions,
) -> Result<Facts, Unavailable> {
    // The children's dimension, from the normal render (cached: the
    // request rendered this node already).
    match renderer.render(node, keys, opts.clone()) {
        Ok(r) => match r.geometry {
            None => return Err(Unavailable::Empty),
            Some(g) if g.dimension() == 2 => return Err(Unavailable::TwoD),
            Some(g) if g.is_empty() => return Err(Unavailable::Empty),
            Some(_) => {}
        },
        Err(u) if u.is_interrupted() => return Err(Unavailable::Interrupted),
        Err(u) => {
            return Err(Unavailable::NoBrep(format!(
                "{}() could not be rendered",
                u.what
            )));
        }
    }
    let should_stop: meshbrep::StopFn = {
        let interrupt = opts.interrupt.clone();
        let guard = opts.guard.clone();
        Arc::new(move || {
            interrupt
                .as_ref()
                .is_some_and(|f| f.load(Ordering::Relaxed))
                || guard.as_ref().is_some_and(|g| g.stopped())
        })
    };
    let options = meshbrep::Options {
        should_stop: Some(should_stop),
        ..meshbrep::Options::default()
    };
    // As the STEP export does: a topology mismatch (slivers at a
    // near-tangency) usually goes away at a finer tessellation.
    let mut last = String::new();
    for mult in [1u32, 2, 4] {
        let em = match walk::export_render_traced(renderer, node, keys, opts, mult) {
            Ok(m) => m,
            Err((u, _)) if u.is_interrupted() => return Err(Unavailable::Interrupted),
            Err((u, _)) => {
                let at = u
                    .loc
                    .map(|l| format!(" at line {}", l.line))
                    .unwrap_or_default();
                return Err(Unavailable::NoBrep(format!(
                    "{}(){at} does not make a closed solid",
                    u.what
                )));
            }
        };
        if em.mesh.triangles.is_empty() {
            return Err(Unavailable::Empty);
        }
        match meshbrep::reconstruct_located(&em.mesh, &options) {
            Ok(b) => return Ok(build(&b, &em, mult)),
            Err(f) if f.error == meshbrep::Error::Stopped => {
                return Err(Unavailable::Interrupted);
            }
            Err(f) => {
                last = f.error.to_string();
                if !matches!(f.error, meshbrep::Error::TopologyMismatch(_)) {
                    break;
                }
            }
        }
        if stopped(opts) {
            return Err(Unavailable::Interrupted);
        }
    }
    Err(Unavailable::NoBrep(last))
}

/// `sin` of the turn between two faces' normals below which they are
/// tangent. The normals come from exact surfaces at exact points, so
/// rounding is far below this.
const TANGENT: f64 = 1e-7;

fn build(b: &Brep, em: &walk::ExportMesh, mult: u32) -> Facts {
    let mesh = &em.mesh;
    // Each face's surface records, from the triangles it was built from.
    let records: Vec<Vec<u32>> = b
        .report
        .face_triangles
        .iter()
        .map(|ts| {
            let s: BTreeSet<u32> = ts
                .iter()
                .filter_map(|&t| mesh.triangle_surface.get(t as usize).copied())
                .collect();
            s.into_iter().collect()
        })
        .collect();
    // The faces using each edge, and in which direction.
    let mut uses: Vec<Vec<(usize, bool)>> = vec![Vec::new(); b.edges.len()];
    for (fi, f) in b.faces.iter().enumerate() {
        for lp in &f.loops {
            for c in &lp.coedges {
                if let Some(u) = uses.get_mut(c.edge as usize) {
                    u.push((fi, c.forward));
                }
            }
        }
    }
    let mut lo = [f64::INFINITY; 3];
    let mut hi = [f64::NEG_INFINITY; 3];
    for p in &mesh.positions {
        for k in 0..3 {
            lo[k] = lo[k].min(p[k]);
            hi[k] = hi[k].max(p[k]);
        }
    }
    let bbox = (lo[0] <= hi[0]).then_some((lo, hi));
    let diag = bbox.map_or(0.0, |(l, h)| norm(sub(h, l)));
    let tolerance = (1e-6 * diag).max(1e-12);
    let place_tol = (1e-7 * diag).max(1e-12);
    let mut edges = Vec::new();
    for (ei, e) in b.edges.iter().enumerate() {
        if e.seam {
            continue;
        }
        let u = &uses[ei];
        if u.len() != 2 {
            continue;
        }
        // Face A is the one whose loop runs along the edge.
        let (a, b_) = if u[0].1 || !u[1].1 {
            (u[0], u[1])
        } else {
            (u[1], u[0])
        };
        let (fa, fb) = (&b.faces[a.0], &b.faces[b_.0]);
        let [t0, t1] = e.range;
        let mut convex = 0;
        let mut concave = 0;
        let mut mid_turn = 0.0;
        let mut mid_sign = 0.0;
        for (k, frac) in [0.1, 0.3, 0.5, 0.7, 0.9].into_iter().enumerate() {
            let t = t0 + (t1 - t0) * frac;
            let p = curve::point(&e.curve, t);
            let tan = curve::tangent(&e.curve, t, e.range);
            let d = if a.1 { tan } else { curve::mul(tan, -1.0) };
            let na = curve::outward(fa, p);
            let nb = curve::outward(fb, p);
            let x = cross(na, nb);
            let s = norm(x);
            let turn = s.atan2(dot(na, nb));
            let sign = dot(x, d);
            if k == 2 {
                mid_turn = turn;
                mid_sign = sign;
            }
            if s <= TANGENT && dot(na, nb) > 0.0 {
                continue;
            }
            if sign > 0.0 {
                convex += 1;
            } else {
                concave += 1;
            }
        }
        let sense = match (convex, concave) {
            (0, 0) => Sense::Smooth,
            (_, 0) => Sense::Convex,
            (0, _) => Sense::Concave,
            _ => Sense::Saddle,
        };
        let deg = mid_turn.to_degrees();
        let angle = match sense {
            Sense::Convex => 180.0 - deg,
            Sense::Concave => 180.0 + deg,
            Sense::Smooth => 180.0,
            Sense::Saddle => {
                if mid_sign > 0.0 {
                    180.0 - deg
                } else {
                    180.0 + deg
                }
            }
        };
        let (kind, direction, axis, radius) = match &e.curve {
            Curve::Line { direction, .. } => (CurveKind::Line, Some(unit(*direction)), None, None),
            Curve::Circle { normal, radius, .. } => {
                (CurveKind::Circle, None, Some(unit(*normal)), Some(*radius))
            }
            Curve::Ellipse { normal, .. } => (CurveKind::Ellipse, None, Some(unit(*normal)), None),
            Curve::BSpline(_) => (CurveKind::BSpline, None, None, None),
        };
        let class = class_of(&e.curve, &fa.surface, &fb.surface, place_tol);
        let (ra, rb) = (&records[a.0], &records[b_.0]);
        let prov = |rs: &[u32]| {
            let mut ch = BTreeSet::new();
            let mut parts = BTreeSet::new();
            let mut leaves = BTreeSet::new();
            for &r in rs {
                if let Some(p) = em.provenance.get(r as usize) {
                    if let Some(c) = p.child {
                        ch.insert(c);
                    }
                    if let Some(n) = &p.part {
                        parts.insert(n.clone());
                    }
                    leaves.insert(p.leaf);
                }
            }
            (
                ch.into_iter().collect::<Vec<_>>(),
                parts.into_iter().collect::<Vec<_>>(),
                leaves.into_iter().collect::<Vec<_>>(),
            )
        };
        let (ca, pa, la) = prov(ra);
        let (cb, pb, lb) = prov(rb);
        let polygon = |rs: &[u32]| -> BTreeMap<u32, u32> {
            rs.iter()
                .filter_map(|&r| {
                    em.provenance
                        .get(r as usize)
                        .and_then(|p| p.polygon)
                        .map(|g| (g, r))
                })
                .collect()
        };
        let (ga, gb) = (polygon(ra), polygon(rb));
        let shared = ga.iter().find(|(g, _)| gb.contains_key(g)).map(|(_, &r)| r);
        let faceted_record = ra
            .iter()
            .chain(rb)
            .find(|&&r| matches!(mesh.surfaces.get(r as usize), Some(Surface::Faceted)))
            .copied();
        let (skip, origin_record) = if fa.faceted || fb.faceted {
            (Some(Skip::Faceted), faceted_record.or(ra.first().copied()))
        } else if let Some(r) = shared {
            (Some(Skip::Seam), Some(r))
        } else if sense == Sense::Smooth {
            (Some(Skip::Tangent), None)
        } else {
            (None, None)
        };
        let origin = origin_record.and_then(|r| em.surface_origin.get(r as usize).copied());
        let face_kind = |f: &meshbrep::Face| {
            if f.faceted {
                "faceted"
            } else {
                f.surface.kind()
            }
        };
        let path = curve::samples(e, curve::sample_count(e));
        edges.push(EdgeFact {
            curve: kind,
            sense,
            angle,
            class,
            length: curve::length(e),
            center: curve::centre(e),
            from: path[0],
            to: path[path.len() - 1],
            closed: e.start == e.end,
            direction,
            axis,
            radius,
            faces: [face_kind(fa), face_kind(fb)],
            children: [ca, cb],
            parts: [pa, pb],
            leaves: [la, lb],
            skip,
            origin,
            path,
        });
    }
    let order = |c: CurveKind| match c {
        CurveKind::Line => 0,
        CurveKind::Circle => 1,
        CurveKind::Ellipse => 2,
        CurveKind::BSpline => 3,
    };
    edges.sort_by(|x, y| {
        x.class
            .cmp(&y.class)
            .then(order(x.curve).cmp(&order(y.curve)))
            .then(x.center[0].total_cmp(&y.center[0]))
            .then(x.center[1].total_cmp(&y.center[1]))
            .then(x.center[2].total_cmp(&y.center[2]))
            .then(x.length.total_cmp(&y.length))
    });
    Facts {
        edges,
        bbox,
        tolerance,
        origins: em.origins.clone(),
        mult,
    }
}

fn parallel(a: V, b: V) -> bool {
    norm(cross(unit(a), unit(b))) <= 1e-9
}

/// The distance of `p` from the line through `o` along unit `d`.
fn off_line(p: V, o: V, d: V) -> f64 {
    let v = sub(p, o);
    norm(sub(v, curve::mul(d, dot(v, d))))
}

/// The edge's blend class (`docs/fillets.md`, 6.1).
fn class_of(c: &Curve, a: &Surface, b: &Surface, tol: f64) -> Class {
    match c {
        Curve::Line { direction, .. } => {
            let ok = |s: &Surface| match s {
                Surface::Plane { .. } => true,
                Surface::Cylinder { axis, .. } => parallel(*axis, *direction),
                _ => false,
            };
            if ok(a) && ok(b) {
                Class::Translational
            } else {
                Class::Other
            }
        }
        Curve::Circle { center, normal, .. } => {
            let n = unit(*normal);
            let ok = |s: &Surface| match s {
                Surface::Plane { normal: pn, .. } => parallel(*pn, n),
                Surface::Cylinder { origin, axis, .. } => {
                    parallel(*axis, n) && off_line(*origin, *center, n) <= tol
                }
                Surface::Cone { apex, axis, .. } => {
                    parallel(*axis, n) && off_line(*apex, *center, n) <= tol
                }
                Surface::Sphere { center: sc, .. } => off_line(*sc, *center, n) <= tol,
                Surface::Torus {
                    center: tc, axis, ..
                } => parallel(*axis, n) && off_line(*tc, *center, n) <= tol,
                _ => false,
            };
            if ok(a) && ok(b) {
                Class::Rotational
            } else {
                Class::Other
            }
        }
        _ => Class::Other,
    }
}

/// What became of a call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// Edges selected, all of a supported kind.
    Selected,
    /// Nothing matched.
    NoEdges,
    /// The count differs from `expect`.
    Count,
    /// Some selected edges are of a kind the blends do not cover.
    Unsupported,
    /// The child has no B-rep.
    NoBrep,
    /// The children are 2D.
    TwoD,
    /// The children are empty.
    Empty,
    /// The request stopped first.
    Interrupted,
}

impl Status {
    pub fn name(self) -> &'static str {
        match self {
            Status::Selected => "selected",
            Status::NoEdges => "no-edges",
            Status::Count => "count",
            Status::Unsupported => "unsupported",
            Status::NoBrep => "no-brep",
            Status::TwoD => "2d",
            Status::Empty => "empty",
            Status::Interrupted => "interrupted",
        }
    }
}

/// One diagnostic of a plan, at the call.
#[derive(Debug, Clone, PartialEq)]
pub struct PlanDiag {
    pub severity: Severity,
    pub code: DiagCode,
    pub message: String,
    pub hints: Vec<String>,
}

/// A fillet call's selection on its child.
#[derive(Debug, Clone)]
pub struct Plan {
    pub kind: FilletKind,
    pub size: f64,
    /// The selectors as written in canonical form.
    pub edges_text: String,
    pub except_text: Option<String>,
    pub expect: Option<u32>,
    pub status: Status,
    pub facts: Option<Arc<Facts>>,
    /// The selected edges (indices into `facts.edges`), in the canonical
    /// order: report edge `k` (from 1) is `selected[k - 1]`.
    pub selected: Vec<usize>,
    /// Edges the selector named that are never filleted.
    pub skipped: Vec<usize>,
    /// Selected edges of a kind the blends do not cover.
    pub unsupported: Vec<usize>,
    pub diags: Vec<PlanDiag>,
}

/// How many edges a diagnostic lists before "and N more".
const LISTED: usize = 8;

/// A selector in the canonical form the `.csg` prints.
pub fn selector_text(s: &Selector) -> String {
    let one = |i: &Item| match i {
        Item::Expr(e) => format!("\"{e}\""),
        Item::Descriptor(d) => format!("[{}, {}, {}]", d[0], d[1], d[2]),
    };
    match s.items.as_slice() {
        [i] => one(i),
        items => format!("[{}]", items.iter().map(one).collect::<Vec<_>>().join(", ")),
    }
}

/// A number as reports print it: at most 4 decimals, no `-0`.
pub fn round4(x: f64) -> f64 {
    let r = (x * 1e4).round() / 1e4;
    if r == 0.0 { 0.0 } else { r }
}

fn point_text(p: V) -> String {
    let f = |x: f64| {
        let s = format!("{:.4}", round4(x));
        let s = s.trim_end_matches('0').trim_end_matches('.').to_string();
        if s == "-0" { "0".to_string() } else { s }
    };
    format!("[{}, {}, {}]", f(p[0]), f(p[1]), f(p[2]))
}

/// One edge in words: "line (convex, 90°) at [20, 0, 10], 40 long".
pub fn edge_text(e: &EdgeFact) -> String {
    let len = format!("{:.4}", round4(e.length));
    let len = len.trim_end_matches('0').trim_end_matches('.');
    format!(
        "{} ({}, {}°) at {}, {} long",
        e.curve.name(),
        e.sense.name(),
        round4(e.angle).round(),
        point_text(e.center),
        len
    )
}

/// The plan of fillet node `node`: its child's facts, the selection, and
/// what to report.
pub fn plan(renderer: &Renderer, node: &Node, keys: &Keys, opts: &RenderOptions) -> Option<Plan> {
    let NodeKind::Fillet(f) = &node.kind else {
        return None;
    };
    Some(plan_with(f, facts(renderer, node, keys, opts)))
}

/// [`plan`] from facts already found.
pub fn plan_with(f: &FilletNode, facts: Result<Arc<Facts>, Unavailable>) -> Plan {
    let m = f.kind.module();
    let edges_text = selector_text(&f.edges);
    let mut p = Plan {
        kind: f.kind,
        size: f.size,
        edges_text: edges_text.clone(),
        except_text: f.except.as_ref().map(selector_text),
        expect: f.expect,
        status: Status::Selected,
        facts: None,
        selected: Vec::new(),
        skipped: Vec::new(),
        unsupported: Vec::new(),
        diags: Vec::new(),
    };
    let facts = match facts {
        Ok(x) => x,
        Err(u) => {
            let (status, diag) = match u {
                Unavailable::Empty => (Status::Empty, None),
                Unavailable::Interrupted => (Status::Interrupted, None),
                Unavailable::TwoD => (
                    Status::TwoD,
                    Some(PlanDiag {
                        severity: Severity::Error,
                        code: DiagCode::Fillet2d,
                        message: format!("{m}() needs a 3D child; its children are 2D"),
                        hints: vec![
                            "round a 2D outline with offset(r = r) offset(delta = -r), or with a sketch's fillet()".into(),
                        ],
                    }),
                ),
                Unavailable::NoBrep(why) => (
                    Status::NoBrep,
                    Some(PlanDiag {
                        severity: Severity::Error,
                        code: DiagCode::FilletNoBrep,
                        message: format!(
                            "{m}(): the child could not be reconstructed as a B-rep to select edges on: {why}"
                        ),
                        hints: vec![
                            "bodies that only touch along an edge or at a point are the usual cause: overlap them".into(),
                        ],
                    }),
                ),
            };
            p.status = status;
            p.diags.extend(diag);
            return p;
        }
    };
    let matched = select::matches(&facts, &f.edges, f);
    let except = f
        .except
        .as_ref()
        .map(|s| select::matches(&facts, s, f))
        .unwrap_or_else(|| vec![false; facts.edges.len()]);
    for (i, e) in facts.edges.iter().enumerate() {
        if !matched[i] || except[i] {
            continue;
        }
        if e.skip.is_some() {
            p.skipped.push(i);
        } else {
            p.selected.push(i);
            if !select::supported(e) {
                p.unsupported.push(i);
            }
        }
    }
    let quote = |e: &EdgeFact| edge_text(e);
    if !p.skipped.is_empty() {
        // By reason and the leaf that made them.
        let mut groups: BTreeMap<(Skip, Option<u32>), usize> = BTreeMap::new();
        for &i in &p.skipped {
            let e = &facts.edges[i];
            *groups
                .entry((e.skip.unwrap_or(Skip::Tangent), e.origin))
                .or_default() += 1;
        }
        let parts: Vec<String> = groups
            .iter()
            .map(|(&(skip, origin), &n)| {
                let what = match (skip, n) {
                    (Skip::Seam, 1) => "polygon seam".to_string(),
                    (Skip::Seam, _) => "polygon seams".to_string(),
                    (Skip::Tangent, 1) => "tangent edge".to_string(),
                    (Skip::Tangent, _) => "tangent edges".to_string(),
                    (Skip::Faceted, 1) => "edge of a faceted region".to_string(),
                    (Skip::Faceted, _) => "edges of a faceted region".to_string(),
                };
                let of = origin
                    .and_then(|o| facts.origins.get(o as usize))
                    .map(|(module, loc)| match loc {
                        Some(l) => format!(" of {module}() at line {}", l.line),
                        None => format!(" of {module}()"),
                    })
                    .unwrap_or_default();
                format!("{n} {what}{of}")
            })
            .collect();
        let n = p.skipped.len();
        p.diags.push(PlanDiag {
            severity: Severity::Info,
            code: DiagCode::FilletSkipped,
            message: format!(
                "{m}(): {n} edge{} named by the selector {} skipped: {}",
                if n == 1 { "" } else { "s" },
                if n == 1 { "was" } else { "were" },
                parts.join(", ")
            ),
            hints: vec![
                "polygon seams belong to a $fn polygon (leave $fn unset to get a true curve); tangent edges and the edges of hull(), polyhedron() and imports are never rounded".into(),
            ],
        });
    }
    let n = p.selected.len();
    if let Some(want) = f.expect
        && n != want as usize
    {
        p.status = Status::Count;
        let mut list: Vec<String> = p
            .selected
            .iter()
            .take(LISTED)
            .enumerate()
            .map(|(k, &i)| format!("{}. {}", k + 1, quote(&facts.edges[i])))
            .collect();
        if n > LISTED {
            list.push(format!("and {} more", n - LISTED));
        }
        let what = if list.is_empty() {
            String::new()
        } else {
            format!(": {}", list.join("; "))
        };
        p.diags.push(PlanDiag {
            severity: Severity::Error,
            code: DiagCode::FilletCount,
            message: format!(
                "{m}(): edges = {edges_text} matched {n} edge{}, expect = {want}{what}",
                if n == 1 { "" } else { "s" }
            ),
            hints: vec![format!(
                "if these are the edges you mean, write expect = {n}; otherwise narrow or widen the selector"
            )],
        });
    } else if n == 0 && f.expect.is_none() {
        p.status = Status::NoEdges;
        p.diags.push(PlanDiag {
            severity: Severity::Warning,
            code: DiagCode::FilletNoEdges,
            message: format!("{m}(): edges = {edges_text} matched no edge; the child is unchanged"),
            hints: vec![
                "`snapshot --fillet` draws the child's edges numbered, and `measure --fillet` lists them"
                    .into(),
            ],
        });
    }
    if !p.unsupported.is_empty() {
        if p.status == Status::Selected {
            p.status = Status::Unsupported;
        }
        let k = p.unsupported.len();
        let mut list: Vec<String> = p
            .unsupported
            .iter()
            .take(LISTED)
            .map(|&i| {
                let e = &facts.edges[i];
                let at = p.selected.iter().position(|&s| s == i).map_or(0, |x| x + 1);
                let why = if matches!(e.sense, Sense::Saddle) {
                    "convex in places and concave in others".to_string()
                } else {
                    format!(
                        "{} between a {} and a {}",
                        e.curve.name(),
                        e.faces[0],
                        e.faces[1]
                    )
                };
                format!("edge {at} ({why})")
            })
            .collect();
        if k > LISTED {
            list.push(format!("and {} more", k - LISTED));
        }
        let default = f.edges.is_all();
        p.diags.push(PlanDiag {
            severity: if default {
                Severity::Warning
            } else {
                Severity::Error
            },
            code: DiagCode::FilletUnsupportedEdge,
            message: format!(
                "{m}(): {} {} not of a kind this version blends: {}; only lines between planes (or cylinders parallel to them) and circles about one axis of revolution are",
                if k == 1 { "1 selected edge" } else { "selected edges" },
                if k == 1 { "is" } else { "are" },
                list.join(", ")
            ),
            hints: vec![
                "select fewer edges, or round the profile before the boolean".into(),
            ],
        });
    }
    p.facts = Some(facts);
    p
}
