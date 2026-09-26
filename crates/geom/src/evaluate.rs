//! Turning the node tree into geometry: OpenSCAD's `GeometryEvaluator`
//! (`src/geometry/GeometryEvaluator.cc`) for the Manifold backend.
//!
//! The rules ported here, per node:
//!
//! - groups, the root, `render()` and a transform's or colour's children
//!   are an implicit union;
//! - background (`%`) children are skipped everywhere; highlighted (`#`)
//!   and disabled (`*`) need nothing here (the first renders normally, the
//!   second never reaches the tree);
//! - 2D and 3D do not mix: the first child with geometry sets the
//!   dimension, a later non-empty child of the other dimension warns
//!   "Mixing 2D and 3D objects is not supported", and a 3D operation warns
//!   "Ignoring 2D child object for 3D operation" for each 2D child;
//! - leaves stay `PolySet`s, transforms and colours apply to whatever they
//!   get, and meshes become Manifold solids only when a boolean needs them
//!   (or at the end, for `--render=force`).
//!
//! Results are cached by the subtree's canonical key ([`eval::dump::Keys`],
//! hashed), which is exactly OpenSCAD's cache discipline with an exact key.
//! The cache outlives one render, so a long-lived process re-renders an edit
//! by recomputing only the subtrees whose keys changed.
//!
//! With the `parallel` feature, a node's children are evaluated on rayon's
//! pool. Everything that could depend on scheduling is fixed up front:
//! original IDs come from blocks reserved in tree order, one per child
//! slot of each subtree key
//! (see [`crate::manifold_geom::IdSource`]), and messages travel with the
//! results and are concatenated in child order, so the output is the same
//! as a serial run's.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use eval::dump::Keys;
use eval::node::{CsgOp, Node, NodeKind};
use lang::diag::Severity;
use lang::source::Span;
use manifold_rust::manifold::Manifold;
use manifold_rust::types::OpType;
use sha2::{Digest, Sha256};

use crate::color::{Color, Scheme};
use crate::manifold_geom::{IdSource, ManifoldGeometry};
use crate::polygon2d::Polygon2d;
use crate::polyset::PolySet;
use crate::{Geometry, primitives};

/// Where a message points: the instantiation that produced the node.
#[derive(Debug, Clone, PartialEq)]
pub struct MsgLoc {
    pub unit: u32,
    pub span: Span,
    pub line: u32,
}

/// A message from rendering, with OpenSCAD's text.
#[derive(Debug, Clone, PartialEq)]
pub struct Msg {
    pub severity: Severity,
    pub text: String,
    pub loc: Option<MsgLoc>,
}

/// A node kind this phase cannot build yet.
#[derive(Debug, Clone, PartialEq)]
pub struct Unsupported {
    /// The module name as OpenSCAD spells it, e.g. `linear_extrude`.
    pub what: &'static str,
    pub loc: Option<MsgLoc>,
}

/// Rendering settings from the command line.
#[derive(Debug, Clone, Copy)]
pub struct RenderOptions {
    /// The render colour scheme's face colours, which reach exported meshes.
    pub scheme: Scheme,
    /// `--render=force`: convert a mesh result to a Manifold solid, as
    /// OpenSCAD's `RenderType::BACKEND_SPECIFIC` does (`openscad.cc:495-509`).
    pub force: bool,
}

impl Default for RenderOptions {
    fn default() -> Self {
        RenderOptions { scheme: crate::color::CORNFIELD, force: false }
    }
}

/// The result of a render.
#[derive(Debug)]
pub struct Rendered {
    /// `None` when the tree produced nothing at all.
    pub geometry: Option<Geometry>,
    /// Messages in OpenSCAD's order.
    pub messages: Vec<Msg>,
    /// Entries in the geometry cache after the render.
    pub cache_entries: usize,
}

type Key = u128;

/// The per-process state of rendering: the geometry cache and the ID
/// blocks. One `Renderer` can render many trees; subtrees it has seen are
/// not recomputed.
#[derive(Debug, Default)]
pub struct Renderer {
    cache: Mutex<Cache>,
    ids: Mutex<HashMap<(Key, u32), u32>>,
    /// Worker threads with the evaluator's stack size: the tree walk
    /// recurses once per level, and trees from recursive modules are as
    /// deep as the evaluator allowed, far beyond a default 2 MiB stack.
    #[cfg(feature = "parallel")]
    pool: std::sync::OnceLock<rayon::ThreadPool>,
}

/// The geometry cache: least recently used entries go first once the
/// estimated size passes the budget. OpenSCAD bounds its caches the same
/// way (100 MiB each for geometry and for backend solids by default);
/// without a bound, a deep chain of nested unions keeps every intermediate
/// result and memory grows with the square of the depth.
#[derive(Debug)]
struct Cache {
    entries: HashMap<Key, (Option<Geometry>, usize, u64)>,
    /// Use stamp -> key, oldest first.
    order: std::collections::BTreeMap<u64, Key>,
    bytes: usize,
    budget: usize,
    clock: u64,
}

impl Default for Cache {
    fn default() -> Self {
        Cache { entries: HashMap::new(), order: Default::default(), bytes: 0, budget: CACHE_BUDGET, clock: 0 }
    }
}

/// Default cache budget, the sum of OpenSCAD's two default cache sizes.
const CACHE_BUDGET: usize = 200 << 20;

impl Cache {
    fn get(&mut self, k: Key) -> Option<Option<Geometry>> {
        let (g, _, stamp) = self.entries.get_mut(&k)?;
        self.order.remove(stamp);
        self.clock += 1;
        *stamp = self.clock;
        self.order.insert(self.clock, k);
        Some(g.clone())
    }

    fn insert(&mut self, k: Key, g: Option<Geometry>) {
        let cost = g.as_ref().map_or(0, cost_of) + 64;
        if let Some((_, c, stamp)) = self.entries.remove(&k) {
            self.bytes -= c;
            self.order.remove(&stamp);
        }
        self.clock += 1;
        self.entries.insert(k, (g, cost, self.clock));
        self.order.insert(self.clock, k);
        self.bytes += cost;
        while self.bytes > self.budget && self.entries.len() > 1 {
            let Some((_, old)) = self.order.pop_first() else { break };
            if let Some((_, c, _)) = self.entries.remove(&old) {
                self.bytes -= c;
            }
        }
    }
}

/// Rough memory of a geometry: coordinates and indices, plus Manifold's
/// halfedges, normals and triangle references for a solid.
fn cost_of(g: &Geometry) -> usize {
    match g {
        Geometry::PolySet(p) => p.vertices.len() * 24 + p.faces.iter().map(|f| 24 + 4 * f.len()).sum::<usize>(),
        Geometry::Manifold(m) => m.manifold.num_vert() * 48 + m.manifold.num_tri() * 112,
        Geometry::Polygon2d(p) => p.outlines.iter().map(|o| 24 + 16 * o.len()).sum(),
    }
}

/// IDs per block: enough for one conversion of a mesh with this many
/// colours. A mesh with more takes fresh IDs from Manifold's counter.
const BLOCK: u32 = 64;

/// The slot of a node's own ID block (children use their index).
const OWN: u32 = u32::MAX;

#[derive(Debug, Clone, Copy)]
struct Block(u32);

impl IdSource for Block {
    fn reserve(&self, count: u32) -> u32 {
        if count <= BLOCK { self.0 } else { Manifold::reserve_ids(count) }
    }
}

/// What one subtree produced.
struct Out {
    geom: Option<Geometry>,
    msgs: Vec<Msg>,
}

/// Per-render context.
struct Ctx<'a> {
    r: &'a Renderer,
    /// Node index → key hash.
    hashes: Vec<Key>,
    /// Node index → this is the first node with its key in tree order, so
    /// its messages are printed (later copies hit OpenSCAD's cache).
    first: Vec<bool>,
    /// ID blocks by (subtree key, slot): slot `i` for the conversion of
    /// child `i`, [`OWN`] for the node's own use (colouring a solid).
    blocks: HashMap<(Key, u32), u32>,
}

fn hash_key(s: &str) -> Key {
    let d = Sha256::digest(s.as_bytes());
    u128::from_le_bytes(d[..16].try_into().expect("16 bytes"))
}

fn loc_of(n: &Node) -> Option<MsgLoc> {
    n.origin.as_ref().map(|o| MsgLoc { unit: o.unit, span: o.span, line: o.line })
}

fn is_background(n: &Node) -> bool {
    n.origin.as_ref().is_some_and(|o| o.tag_background)
}

fn warn(n: &Node, text: &str) -> Msg {
    Msg { severity: Severity::Warning, text: text.into(), loc: loc_of(n) }
}

impl Renderer {
    pub fn new() -> Renderer {
        Renderer::default()
    }

    /// Render `top` (the root, or the node a `!` selected), whose keys are
    /// in `keys`. Returns the node that stopped the render if it uses a
    /// feature of a later phase.
    pub fn render(&self, top: &Node, keys: &Keys, opts: RenderOptions) -> Result<Rendered, Unsupported> {
        fn max_index(n: &Node) -> usize {
            n.children.iter().map(max_index).fold(n.index, usize::max)
        }
        let len = max_index(top) + 1;
        let mut ctx = Ctx { r: self, hashes: vec![0; len], first: vec![false; len], blocks: HashMap::new() };
        // Tree order pass: hashes, first occurrences and ID blocks, all
        // decided before anything runs in parallel.
        {
            let mut seen = HashSet::new();
            let mut ids = self.ids.lock().expect("id registry");
            // (node, its parent's key and first-occurrence flag)
            let mut stack: Vec<(&Node, Option<(Key, bool)>)> = vec![(top, None)];
            while let Some((n, parent)) = stack.pop() {
                let h = hash_key(keys.get(n));
                ctx.hashes[n.index] = h;
                // A group with one child that has content shares that
                // child's key (`Keys`): it is the same computation, so the
                // child inherits the group's claim to be first.
                let first = match parent {
                    Some((ph, pf)) if ph == h => pf,
                    _ => seen.insert(h),
                };
                seen.insert(h);
                ctx.first[n.index] = first;
                for slot in std::iter::once(OWN).chain(0..n.children.len() as u32) {
                    let b = *ids.entry((h, slot)).or_insert_with(|| Manifold::reserve_ids(BLOCK));
                    ctx.blocks.insert((h, slot), b);
                }
                stack.extend(n.children.iter().rev().map(|c| (c, Some((h, first)))));
            }
        }
        #[cfg(feature = "parallel")]
        let out = {
            let pool = self.pool.get_or_init(|| {
                rayon::ThreadPoolBuilder::new()
                    .stack_size(eval::DEFAULT_THREAD_STACK)
                    .thread_name(|i| format!("geom-{i}"))
                    .build()
                    .expect("geometry thread pool")
            });
            pool.install(|| ctx.node(top))?
        };
        #[cfg(not(feature = "parallel"))]
        let out = ctx.node(top)?;
        let mut geom = out.geom;
        let mut msgs = out.msgs;
        if opts.force
            && let Some(Geometry::PolySet(ps)) = &geom
        {
            // `getBackendSpecificGeometry` (`GeometryUtils.cc:529-545`).
            let mut w = Vec::new();
            let mut e = Vec::new();
            let m = ManifoldGeometry::from_polyset(ps, &ctx.block(top, OWN), &mut w, &mut e);
            msgs.extend(w.into_iter().map(|t| Msg { severity: Severity::Warning, text: t, loc: None }));
            msgs.extend(e.into_iter().map(|t| Msg { severity: Severity::Error, text: t, loc: None }));
            geom = Some(Geometry::Manifold(Arc::new(m)));
        }
        let cache_entries = self.cache.lock().expect("cache").entries.len();
        Ok(Rendered { geometry: geom, messages: msgs, cache_entries })
    }

    /// Forget every cached geometry.
    pub fn clear(&self) {
        *self.cache.lock().expect("cache") = Cache::default();
    }
}

impl Ctx<'_> {
    fn block(&self, n: &Node, slot: u32) -> Block {
        Block(self.blocks[&(self.hashes[n.index], slot)])
    }

    /// Evaluate one node, from the cache when possible.
    fn node(&self, n: &Node) -> Result<Out, Unsupported> {
        let h = self.hashes[n.index];
        if let Some(g) = self.r.cache.lock().expect("cache").get(h) {
            return Ok(Out { geom: g, msgs: Vec::new() });
        }
        let mut out = self.compute(n)?;
        if !self.first[n.index] {
            out.msgs.clear();
        }
        self.r.cache.lock().expect("cache").insert(h, out.geom.clone());
        Ok(out)
    }

    /// Children's results in order, evaluated in parallel when enabled.
    fn children(&self, n: &Node) -> Result<Vec<Out>, Unsupported> {
        #[cfg(feature = "parallel")]
        if n.children.len() > 1 {
            use rayon::prelude::*;
            return n.children.par_iter().map(|c| self.node(c)).collect();
        }
        n.children.iter().map(|c| self.node(c)).collect()
    }

    fn compute(&self, n: &Node) -> Result<Out, Unsupported> {
        let unsupported = |what: &'static str| Err(Unsupported { what, loc: loc_of(n) });
        let leaf = |g: Geometry| Ok(Out { geom: Some(g), msgs: Vec::new() });
        match &n.kind {
            NodeKind::Cube { size, center } => leaf(Geometry::PolySet(Arc::new(primitives::cube(*size, *center)))),
            NodeKind::Sphere { r, disc } => leaf(Geometry::PolySet(Arc::new(primitives::sphere(*r, disc)))),
            NodeKind::Cylinder { h, r1, r2, center, disc } => {
                leaf(Geometry::PolySet(Arc::new(primitives::cylinder(*h, *r1, *r2, *center, disc))))
            }
            NodeKind::Polyhedron { points, faces, .. } => leaf(Geometry::PolySet(Arc::new(primitives::polyhedron(points, faces)))),
            NodeKind::Square { size, center } => leaf(Geometry::Polygon2d(Arc::new(primitives::square(*size, *center)))),
            NodeKind::Circle { r, disc } => leaf(Geometry::Polygon2d(Arc::new(primitives::circle2d(*r, disc)))),
            NodeKind::Polygon { points, paths, .. } => leaf(Geometry::Polygon2d(Arc::new(primitives::polygon(points, paths)))),
            NodeKind::Root | NodeKind::Group { .. } | NodeKind::Render { .. } => self.apply(n, Op::Union),
            NodeKind::IntersectionFor => self.apply(n, Op::Intersection),
            NodeKind::Csg(CsgOp::Union) => self.apply(n, Op::Union),
            NodeKind::Csg(CsgOp::Intersection) => self.apply(n, Op::Intersection),
            NodeKind::Csg(CsgOp::Difference) => self.apply(n, Op::Difference),
            NodeKind::Color { rgba } => {
                let mut out = self.apply(n, Op::Union)?;
                out.geom = out.geom.map(|g| self.color(n, g, Color(*rgba)));
                Ok(out)
            }
            NodeKind::Transform { matrix, .. } => {
                if matrix.iter().flatten().any(|v| !v.is_finite()) {
                    // The children are still evaluated (and report their own
                    // messages) before the transform gives up on them.
                    let mut msgs: Vec<Msg> = self.children(n)?.into_iter().flat_map(|o| o.msgs).collect();
                    msgs.push(warn(n, "Transformation matrix contains Not-a-Number and/or Infinity - removing object."));
                    return Ok(Out { geom: None, msgs });
                }
                let mut out = self.apply(n, Op::Union)?;
                out.geom = out.geom.map(|g| transform(g, matrix));
                Ok(out)
            }
            NodeKind::Projection { .. } => unsupported("projection"),
            NodeKind::Minkowski { .. } => unsupported("minkowski"),
            NodeKind::Hull => unsupported("hull"),
            NodeKind::Fill => unsupported("fill"),
            NodeKind::Resize { .. } => unsupported("resize"),
            NodeKind::Offset { .. } => unsupported("offset"),
            NodeKind::LinearExtrude(_) => unsupported("linear_extrude"),
            NodeKind::RotateExtrude { .. } => unsupported("rotate_extrude"),
            NodeKind::Surface { .. } => unsupported("surface"),
            NodeKind::Import(_) => unsupported("import"),
            NodeKind::Text(_) => unsupported("text"),
        }
    }

    fn color(&self, n: &Node, g: Geometry, c: Color) -> Geometry {
        match g {
            Geometry::PolySet(ps) => {
                let mut ps = Arc::unwrap_or_clone(ps);
                ps.set_color(c);
                Geometry::PolySet(Arc::new(ps))
            }
            Geometry::Manifold(m) => {
                let mut m = Arc::unwrap_or_clone(m);
                m.set_color(c, &self.block(n, OWN));
                Geometry::Manifold(Arc::new(m))
            }
            // `Polygon2d::setColor` is a no-op for geometry in this phase.
            g @ Geometry::Polygon2d(_) => g,
        }
    }

    /// `applyToChildren` (`GeometryEvaluator.cc:128-139`).
    fn apply(&self, n: &Node, op: Op) -> Result<Out, Unsupported> {
        let results = self.children(n)?;
        let mut msgs = Vec::new();
        let mut items: Vec<(&Node, Option<Geometry>)> = Vec::with_capacity(results.len());
        for (c, o) in n.children.iter().zip(results) {
            msgs.extend(o.msgs);
            items.push((c, o.geom));
        }
        // `isValidDim`: the first child with geometry sets the dimension.
        let mut dim = 0;
        for (c, g) in &items {
            if is_background(c) {
                continue;
            }
            let Some(g) = g else { continue };
            if dim == 0 {
                dim = g.dimension();
            } else if dim != g.dimension() && !g.is_empty() {
                msgs.push(warn(c, "Mixing 2D and 3D objects is not supported"));
                break;
            }
        }
        let geom = match dim {
            2 => self.apply_2d(&items, &mut msgs),
            3 => self.apply_3d(n, &items, op, &mut msgs),
            _ => None,
        };
        Ok(Out { geom, msgs })
    }

    /// `applyToChildren3D` (`GeometryEvaluator.cc:146-209`) with
    /// `collectChildren3D` (`:386-411`) and `applyOperator3DManifold`
    /// (`manifold-applyops.cc`).
    fn apply_3d(&self, n: &Node, items: &[(&Node, Option<Geometry>)], op: Op, msgs: &mut Vec<Msg>) -> Option<Geometry> {
        // (child index, node, geometry)
        let mut children: Vec<(u32, &Node, Option<Geometry>)> = Vec::new();
        for (i, (c, g)) in items.iter().enumerate() {
            let i = i as u32;
            if is_background(c) {
                continue;
            }
            match g {
                Some(g) if g.dimension() == 2 => {
                    msgs.push(warn(c, "Ignoring 2D child object for 3D operation"));
                    children.push((i, c, None));
                }
                g => children.push((i, c, g.clone())),
            }
        }
        if children.is_empty() {
            return None;
        }
        if children.len() == 1 {
            return children.pop().and_then(|(_, _, g)| g);
        }
        let children: Vec<(u32, &Node, Option<Geometry>)> = if op == Op::Union {
            let actual: Vec<_> = children.into_iter().filter(|(_, _, g)| g.as_ref().is_some_and(|g| !g.is_empty())).collect();
            match actual.len() {
                0 => return None,
                1 => return actual.into_iter().next().and_then(|(_, _, g)| g),
                _ => actual,
            }
        } else {
            children
        };
        let mut parts: Vec<ManifoldGeometry> = Vec::with_capacity(children.len());
        for (i, _, g) in children {
            let m = g.and_then(|g| self.to_manifold(n, i, g, msgs));
            let Some(m) = m.filter(|m| !m.is_empty()) else {
                // Intersecting with nothing is nothing, and so is
                // subtracting from nothing.
                if op == Op::Intersection || (op == Op::Difference && parts.is_empty()) {
                    return None;
                }
                continue;
            };
            parts.push(m);
        }
        ManifoldGeometry::batch(op.manifold(), parts).map(|m| Geometry::Manifold(Arc::new(m)))
    }

    /// `createManifoldFromGeometry`, for child `slot` of `n`. OpenSCAD
    /// reserves fresh IDs for every conversion, so the same mesh converted
    /// under two different parents gets two sets of IDs (and a sphere cut
    /// out in one place is not painted as a cut face in another).
    fn to_manifold(&self, n: &Node, slot: u32, g: Geometry, msgs: &mut Vec<Msg>) -> Option<ManifoldGeometry> {
        match g {
            Geometry::Manifold(m) => Some(Arc::unwrap_or_clone(m)),
            Geometry::PolySet(ps) => {
                let mut w = Vec::new();
                let mut e = Vec::new();
                let m = ManifoldGeometry::from_polyset(&ps, &self.block(n, slot), &mut w, &mut e);
                msgs.extend(w.into_iter().map(|t| Msg { severity: Severity::Warning, text: t, loc: None }));
                msgs.extend(e.into_iter().map(|t| Msg { severity: Severity::Error, text: t, loc: None }));
                Some(m)
            }
            Geometry::Polygon2d(_) => None,
        }
    }

    /// `applyToChildren2D` without a 2D kernel (phase 5b): one child passes
    /// through; several are concatenated and marked approximate.
    fn apply_2d(&self, items: &[(&Node, Option<Geometry>)], msgs: &mut Vec<Msg>) -> Option<Geometry> {
        let mut polys: Vec<Arc<Polygon2d>> = Vec::new();
        let mut count = 0;
        for (c, g) in items {
            if is_background(c) {
                continue;
            }
            count += 1;
            match g {
                Some(g) if g.dimension() == 3 => msgs.push(warn(c, "Ignoring 3D child object for 2D operation")),
                Some(Geometry::Polygon2d(p)) if !p.is_empty() => polys.push(p.clone()),
                _ => {}
            }
        }
        match (count, polys.len()) {
            (0, _) => None,
            (1, 1) => polys.pop().map(Geometry::Polygon2d),
            (1, _) => None,
            _ => {
                let mut p = Polygon2d { approximate: true, ..Default::default() };
                for q in polys {
                    p.outlines.extend(q.outlines.iter().cloned());
                }
                Some(Geometry::Polygon2d(Arc::new(p)))
            }
        }
    }
}

/// Transform a result: 2D keeps the 2D part of the matrix, 3D takes it all.
fn transform(g: Geometry, m: &crate::Matrix) -> Geometry {
    match g {
        Geometry::PolySet(ps) => {
            let mut ps: PolySet = Arc::unwrap_or_clone(ps);
            ps.transform(m);
            Geometry::PolySet(Arc::new(ps))
        }
        Geometry::Manifold(mg) => {
            let mut mg = Arc::unwrap_or_clone(mg);
            mg.transform(m);
            Geometry::Manifold(Arc::new(mg))
        }
        Geometry::Polygon2d(p) => {
            let mut p = Arc::unwrap_or_clone(p);
            p.transform(m);
            Geometry::Polygon2d(Arc::new(p))
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Op {
    Union,
    Intersection,
    Difference,
}

impl Op {
    fn manifold(self) -> OpType {
        match self {
            Op::Union => OpType::Add,
            Op::Intersection => OpType::Intersect,
            Op::Difference => OpType::Subtract,
        }
    }
}
