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
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use eval::dump::Keys;
use eval::node::{CsgOp, Node, NodeKind};
use lang::diag::Severity;
use lang::loader::{FileSystem, StdFs};
use lang::source::Span;
use manifold_rust::manifold::Manifold;
use manifold_rust::types::OpType;
use sha2::{Digest, Sha256};

use eval::node::OffsetJoin;
use eval::trig::cos_degrees;

use crate::color::{Color, Scheme};
use crate::manifold_geom::{IdSource, ManifoldGeometry};
use crate::polygon2d::Polygon2d;
use crate::polyset::PolySet;
use crate::{Geometry, clipper, extrude, fragments, primitives};

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
    /// `None` for a plain `LOG(...)` line with no `WARNING:`-style prefix
    /// (e.g. "Reading 3MF with title ...").
    pub severity: Option<Severity>,
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
#[derive(Clone)]
pub struct RenderOptions {
    /// The render colour scheme's face colours, which reach exported meshes.
    pub scheme: Scheme,
    /// `--render=force`: convert a mesh result to a Manifold solid, as
    /// OpenSCAD's `RenderType::BACKEND_SPECIFIC` does (`openscad.cc:495-509`).
    pub force: bool,
    /// Where `import()` and `surface()` read their files.
    pub fs: Arc<dyn FileSystem + Send + Sync>,
    /// The document's directory: the working directory OpenSCAD runs in,
    /// which some import messages print file names relative to.
    pub doc_dir: PathBuf,
}

impl Default for RenderOptions {
    fn default() -> Self {
        RenderOptions { scheme: crate::color::CORNFIELD, force: false, fs: Arc::new(StdFs), doc_dir: PathBuf::new() }
    }
}

impl std::fmt::Debug for RenderOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RenderOptions").field("scheme", &self.scheme).field("force", &self.force).field("doc_dir", &self.doc_dir).finish()
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
        Geometry::Polygon2d(p) => p.outlines.iter().map(|o| 24 + 16 * o.vertices.len()).sum(),
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

/// Consecutive ranges of one block, for several conversions under one
/// node; past the block's end, fresh IDs from Manifold's counter.
struct Seq {
    block: Block,
    used: std::cell::Cell<u32>,
}

impl IdSource for Seq {
    fn reserve(&self, count: u32) -> u32 {
        let used = self.used.get();
        if used + count <= BLOCK {
            self.used.set(used + count);
            self.block.0 + used
        } else {
            Manifold::reserve_ids(count)
        }
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
    opts: &'a RenderOptions,
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
    Msg { severity: Some(Severity::Warning), text: text.into(), loc: loc_of(n) }
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
        let mut ctx = Ctx { r: self, opts: &opts, hashes: vec![0; len], first: vec![false; len], blocks: HashMap::new() };
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
            msgs.extend(w.into_iter().map(|t| Msg { severity: Some(Severity::Warning), text: t, loc: None }));
            msgs.extend(e.into_iter().map(|t| Msg { severity: Some(Severity::Error), text: t, loc: None }));
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
            NodeKind::Square { size, center } => leaf(leaf_2d(primitives::square(*size, *center))),
            NodeKind::Circle { r, disc } => leaf(leaf_2d(primitives::circle2d(*r, disc))),
            NodeKind::Polygon { points, paths, .. } => leaf(leaf_2d(primitives::polygon(points, paths))),
            NodeKind::Root | NodeKind::Group { .. } | NodeKind::Render { .. } => self.apply(n, Op::Union),
            NodeKind::IntersectionFor => self.apply(n, Op::Intersection),
            NodeKind::Csg(CsgOp::Union) => self.apply(n, Op::Union),
            NodeKind::Csg(CsgOp::Intersection) => self.apply(n, Op::Intersection),
            NodeKind::Csg(CsgOp::Difference) => self.apply(n, Op::Difference),
            NodeKind::Fill => self.apply(n, Op::Fill),
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
                out.geom = out.geom.map(|g| transform(g, matrix, &mut out.msgs));
                Ok(out)
            }
            NodeKind::Offset { delta, join, disc, .. } => {
                let (poly, msgs) = self.children_2d_union(n)?;
                let geom = poly.map(|p| {
                    // "The formula for the number of steps in a full circular
                    // arc is ... Pi / acos(1 - arc_tolerance / abs(delta))"
                    // (`GeometryEvaluator.cc:617-621`): the tolerance that
                    // makes Clipper step like a circle of `|delta|` would.
                    let steps = f64::from(fragments::circular_segments(disc, delta.abs()).unwrap_or(3));
                    let tolerance = delta.abs() * (1.0 - cos_degrees(180.0 / steps));
                    let join = match join {
                        OffsetJoin::Round => clipper::Join::Round,
                        OffsetJoin::Miter => clipper::Join::Miter,
                        OffsetJoin::Square => clipper::Join::Square,
                    };
                    // `OffsetNode::miter_limit`, "fixed high value to disable
                    // chamfers with jtMiter".
                    Geometry::Polygon2d(Arc::new(clipper::offset(&p, *delta, join, 1_000_000.0, tolerance)))
                });
                Ok(Out { geom, msgs })
            }
            NodeKind::LinearExtrude(e) => {
                let (poly, msgs) = self.children_2d_union(n)?;
                let geom = poly.map(|p| Geometry::PolySet(Arc::new(extrude::linear_extrude(e, &p))));
                Ok(Out { geom, msgs })
            }
            NodeKind::RotateExtrude { angle, start, disc, .. } => {
                let (poly, mut msgs) = self.children_2d_union(n)?;
                let geom = match poly.map(|p| extrude::rotate_extrude(*angle, *start, disc, &p)) {
                    Some(Ok(ps)) => ps.map(|ps| Geometry::PolySet(Arc::new(ps))),
                    Some(Err(text)) => {
                        msgs.push(Msg { severity: Some(Severity::Error), text, loc: None });
                        None
                    }
                    None => None,
                };
                Ok(Out { geom, msgs })
            }
            NodeKind::Projection { cut, .. } => self.projection(n, *cut),
            NodeKind::Minkowski { .. } => unsupported("minkowski"),
            NodeKind::Hull => unsupported("hull"),
            NodeKind::Resize { .. } => unsupported("resize"),
            NodeKind::Surface { file, center, invert, .. } => Ok(self.surface(n, file, *center, *invert)),
            NodeKind::Import(i) if i.kind == "nef3" => unsupported("import"),
            NodeKind::Import(i) => Ok(self.import(n, i)),
            NodeKind::Text(_) => unsupported("text"),
        }
    }

    /// Messages from a reader, located at the node when OpenSCAD logs them
    /// with the call's location.
    fn read_msgs(n: &Node, msgs: Vec<io::Message>) -> Vec<Msg> {
        msgs.into_iter().map(|m| Msg { severity: m.severity, text: m.text, loc: if m.located { loc_of(n) } else { None } }).collect()
    }

    fn import(&self, n: &Node, i: &eval::node::Import) -> Out {
        let line = loc_of(n).map_or(0, |l| l.line);
        let union = |meshes: Vec<PolySet>| -> PolySet {
            // `ManifoldUtils::applyOperator3DManifold(children, UNION)`, then
            // `getGeometryAsPolySet`. Each conversion takes fresh IDs; they
            // come from this node's own block, in order, so the result does
            // not depend on scheduling.
            let ids = Seq { block: self.block(n, OWN), used: std::cell::Cell::new(0) };
            let mut parts = Vec::with_capacity(meshes.len());
            let mut w = Vec::new();
            let mut e = Vec::new();
            for ps in &meshes {
                let m = ManifoldGeometry::from_polyset(ps, &ids, &mut w, &mut e);
                if !m.is_empty() {
                    parts.push(m);
                }
            }
            ManifoldGeometry::batch(Op::Union.manifold(), parts).map(|m| m.to_polyset(&self.opts.scheme)).unwrap_or_default()
        };
        let (geom, msgs) = crate::import::import(self.opts, i, line, &union);
        Out { geom: Some(geom), msgs: Self::read_msgs(n, msgs) }
    }

    fn surface(&self, n: &Node, file: &str, center: bool, invert: bool) -> Out {
        let (geom, msgs) = crate::import::surface(self.opts, file, center, invert);
        Out { geom: Some(geom), msgs: Self::read_msgs(n, msgs) }
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
            // `Polygon2d` does not override `Geometry::setColor`, so a colour
            // on 2D geometry is dropped: a render shows 2D in the scheme's
            // colour, and an extrusion of a coloured shape is uncoloured.
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
            2 => self.apply_2d(&items, op, &mut msgs),
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
        if op == Op::Fill {
            for (_, c, _) in &children {
                msgs.push(warn(c, "fill() not yet implemented for 3D"));
            }
        }
        if children.len() == 1 {
            return children.pop().and_then(|(_, _, g)| g);
        }
        if op == Op::Fill {
            // `applyOperator3DManifold` has no case for FILL: the first
            // solid is kept and every later one is an error
            // (`manifold-applyops.cc`, "Unsupported CGAL operator", FILL
            // being 5 in `OpenSCADOperator`).
            let mut first = None;
            for (i, _, g) in children {
                let Some(m) = g.and_then(|g| self.to_manifold(n, i, g, msgs)).filter(|m| !m.is_empty()) else { continue };
                if first.is_none() {
                    first = Some(m);
                } else {
                    msgs.push(Msg { severity: Some(Severity::Error), text: "Unsupported CGAL operator: 5".into(), loc: None });
                }
            }
            return first.map(|m| Geometry::Manifold(Arc::new(m)));
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
                msgs.extend(w.into_iter().map(|t| Msg { severity: Some(Severity::Warning), text: t, loc: None }));
                msgs.extend(e.into_iter().map(|t| Msg { severity: Some(Severity::Error), text: t, loc: None }));
                Some(m)
            }
            Geometry::Polygon2d(_) => None,
        }
    }

    /// `collectChildren2D` (`GeometryEvaluator.cc:302-336`): one entry per
    /// non-background child, `None` for nothing, empty or 3D (which warns).
    fn collect_2d(&self, items: &[(&Node, Option<Geometry>)], msgs: &mut Vec<Msg>) -> Vec<Option<Arc<Polygon2d>>> {
        let mut out = Vec::with_capacity(items.len());
        for (c, g) in items {
            if is_background(c) {
                continue;
            }
            match g {
                Some(g) if g.dimension() == 3 => {
                    msgs.push(warn(c, "Ignoring 3D child object for 2D operation"));
                    out.push(None);
                }
                Some(Geometry::Polygon2d(p)) if !p.is_empty() => out.push(Some(p.clone())),
                _ => out.push(None),
            }
        }
        out
    }

    /// `applyToChildren2D` (`GeometryEvaluator.cc:416-454`). One child
    /// passes through untouched; more go through Clipper.
    fn apply_2d(&self, items: &[(&Node, Option<Geometry>)], op: Op, msgs: &mut Vec<Msg>) -> Option<Geometry> {
        let children = self.collect_2d(items, msgs);
        let refs: Vec<Option<&Polygon2d>> = children.iter().map(|c| c.as_deref()).collect();
        if op == Op::Fill {
            return Some(Geometry::Polygon2d(Arc::new(clipper::fill(&refs))));
        }
        match children.len() {
            0 => None,
            1 => children.into_iter().next().flatten().map(Geometry::Polygon2d),
            _ => {
                let op = match op {
                    Op::Union => clipper::Op2::Union,
                    Op::Intersection => clipper::Op2::Intersection,
                    Op::Difference => clipper::Op2::Difference,
                    Op::Fill => unreachable!("handled above"),
                };
                Some(Geometry::Polygon2d(Arc::new(clipper::apply(&refs, op))))
            }
        }
    }

    /// The children of a 2D-only operation (offset, the extrusions) as one
    /// shape: `applyToChildren2D(node, UNION)` called directly, so there is
    /// no mixing check, only a warning per 3D child.
    fn children_2d_union(&self, n: &Node) -> Result<(Option<Polygon2d>, Vec<Msg>), Unsupported> {
        let results = self.children(n)?;
        let mut msgs = Vec::new();
        let mut items: Vec<(&Node, Option<Geometry>)> = Vec::with_capacity(results.len());
        for (c, o) in n.children.iter().zip(results) {
            msgs.extend(o.msgs);
            items.push((c, o.geom));
        }
        let geom = self.apply_2d(&items, Op::Union, &mut msgs);
        let poly = match geom {
            Some(Geometry::Polygon2d(p)) => Some(Arc::unwrap_or_clone(p)),
            _ => None,
        };
        Ok((poly, msgs))
    }

    /// `projectionCut` / `projectionNoCut` for the Manifold backend
    /// (`GeometryEvaluator.cc:845-907`): union the 3D children, then slice
    /// at z = 0 or take the outline from above, and sanitize. With no 3D
    /// geometry a cut gives nothing and a projection an empty shape.
    fn projection(&self, n: &Node, cut: bool) -> Result<Out, Unsupported> {
        let results = self.children(n)?;
        let mut msgs = Vec::new();
        let mut items: Vec<(&Node, Option<Geometry>)> = Vec::with_capacity(results.len());
        for (c, o) in n.children.iter().zip(results) {
            msgs.extend(o.msgs);
            items.push((c, o.geom));
        }
        let solid = self.apply_3d(n, &items, Op::Union, &mut msgs);
        let Some(solid) = solid else {
            let geom = (!cut).then(|| Geometry::Polygon2d(Arc::new(Polygon2d::default())));
            return Ok(Out { geom, msgs });
        };
        // `createManifoldFromGeometry` converts a mesh with a fresh set of
        // IDs; the slot past the children is this node's own.
        let had_faces = !solid.is_empty();
        let m = self.to_manifold(n, OWN, solid, &mut msgs);
        if had_faces && m.as_ref().is_some_and(ManifoldGeometry::is_empty) {
            // The conversion failed (a mesh that is not closed even after
            // repair), which OpenSCAD reports as a null solid and answers
            // with its non-Manifold paths (`GeometryEvaluator.cc:859-906`).
            if cut {
                // CGAL cannot build a Nef polyhedron from it either.
                msgs.push(Msg {
                    severity: Some(Severity::Error),
                    text: "The given mesh is not closed! Unable to convert to CGALNefGeometry.".into(),
                    loc: None,
                });
                return Ok(Out { geom: None, msgs });
            }
            // Each child's faces projected and unioned with Clipper.
            let faces: Vec<Polygon2d> = items
                .iter()
                .filter(|(c, _)| !is_background(c))
                .filter_map(|(_, g)| g.as_ref().and_then(|g| crate::export::as_polyset(g, &self.opts.scheme)))
                .map(|ps| Polygon2d {
                    outlines: ps.faces.iter().map(|f| crate::polygon2d::Outline::new(f.iter().map(|&v| [ps.vertices[v as usize][0], ps.vertices[v as usize][1]]).collect())).collect(),
                    sanitized: false,
                })
                .collect();
            let geom = clipper::project_union(&faces).map(|p| Geometry::Polygon2d(Arc::new(p)));
            return Ok(Out { geom, msgs });
        }
        let geom = m.map(|m| {
            let flat = if cut { m.slice() } else { m.project() };
            Geometry::Polygon2d(Arc::new(clipper::sanitize(&flat)))
        });
        Ok(Out { geom, msgs })
    }
}

/// A 2D leaf as `visit(LeafNode)` stores it: sanitized unless the
/// primitive already guarantees it (`GeometryEvaluator.cc:672-675`).
fn leaf_2d(p: Polygon2d) -> Geometry {
    Geometry::Polygon2d(Arc::new(if p.sanitized { p } else { clipper::sanitize(&p) }))
}

/// Transform a result: 2D keeps the 2D part of the matrix, 3D takes it all.
fn transform(g: Geometry, m: &crate::Matrix, msgs: &mut Vec<Msg>) -> Geometry {
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
            let m2 = Polygon2d::matrix_2d(m);
            if let Some(w) = p.transform(&m2) {
                msgs.push(Msg { severity: Some(Severity::Warning), text: w.into(), loc: None });
            }
            // A mirror reverses every outline, so a sanitized shape would
            // have clockwise outers and counter-clockwise holes; Clipper
            // puts them right (`GeometryEvaluator.cc:759-765`).
            if p.sanitized && crate::polygon2d::det3(&m2) <= 0.0 {
                p = clipper::sanitize(&p);
            }
            Geometry::Polygon2d(Arc::new(p))
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Op {
    Union,
    Intersection,
    Difference,
    /// `fill()`: a 2D operation; 3D children only warn.
    Fill,
}

impl Op {
    fn manifold(self) -> OpType {
        match self {
            Op::Union => OpType::Add,
            Op::Intersection => OpType::Intersect,
            Op::Difference => OpType::Subtract,
            // Not a Manifold operation; `apply_3d` handles it first.
            Op::Fill => OpType::Add,
        }
    }
}
