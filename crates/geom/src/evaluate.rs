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
//! a Merkle hash), which is exactly OpenSCAD's cache discipline with an exact key.
//! The cache outlives one render, so a long-lived process re-renders an edit
//! by recomputing only the subtrees whose keys changed.
//!
//! With the `parallel` feature, on native targets, a node's children are
//! evaluated on rayon's pool. Everything that could depend on scheduling
//! is fixed up front: original IDs come from blocks reserved in tree
//! order, one per child
//! slot of each subtree key (see [`crate::manifold_geom::IdSource`]; a
//! block that turns out too small makes the render start again with a
//! bigger one, see [`Overflow`]), and messages travel with the results and
//! are concatenated in child order, so the output is the same as a serial
//! run's. IDs that Manifold draws from its global counter on its own (mesh
//! IDs, and the IDs of freshly built hulls) never decide an order in the
//! output: runs sharing an original ID are ordered by geometry
//! (`canonical_mesh`), and built hulls are retagged from a block.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use eval::dump::Keys;
use eval::node::{CsgOp, Node, NodeKind};
use lang::diag::{PathBase, Severity};
use lang::loader::{FileSystem, StdFs};
use lang::source::Span;
use manifold_rust::manifold::Manifold;
use manifold_rust::types::OpType;

use eval::node::OffsetJoin;
use eval::trig::cos_degrees;

use crate::color::{Color, Scheme};
use crate::manifold_geom::{IdSource, ManifoldGeometry};
use crate::polygon2d::Polygon2d;
use crate::polyset::PolySet;
use crate::{Geometry, clipper, extrude, fragments, hull, minkowski, primitives};

/// Where a message points: the instantiation that produced the node.
#[derive(Debug, Clone, PartialEq)]
pub struct MsgLoc {
    pub unit: u32,
    pub span: Span,
    pub line: u32,
    /// What the file name prints relative to: OpenSCAD's geometry
    /// evaluator logs with the main file's directory, its file readers
    /// with the working directory.
    pub base: PathBase,
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

/// A node kind this phase cannot build yet, or a render that
/// [`RenderOptions::interrupt`] stopped ([`Unsupported::interrupted`]).
#[derive(Debug, Clone, PartialEq)]
pub struct Unsupported {
    /// The module name as OpenSCAD spells it, e.g. `linear_extrude`, or
    /// [`INTERRUPTED`].
    pub what: &'static str,
    pub loc: Option<MsgLoc>,
}

/// [`Unsupported::what`] of a render stopped by its interrupt flag. It is
/// not a module name, so no caller can mistake it for one.
pub const INTERRUPTED: &str = "(interrupted)";

impl Unsupported {
    /// The error a render returns when its interrupt flag was set.
    pub fn interrupted() -> Unsupported {
        Unsupported {
            what: INTERRUPTED,
            loc: None,
        }
    }

    /// Whether this is [`Unsupported::interrupted`] rather than a missing
    /// feature.
    pub fn is_interrupted(&self) -> bool {
        self.what == INTERRUPTED
    }
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
    /// The working directory, which file names in import messages are
    /// relative to (`Filename`'s `operator<<`, `Value.cc:195-201`). Empty
    /// leaves them as they are.
    pub work_dir: PathBuf,
    /// The fonts `text()` can use.
    pub fonts: Arc<text::FontDb>,
    /// Checked before each node is computed: when set, the render stops
    /// with [`Unsupported::interrupted`]. Results finished before that stay
    /// cached, so a long-lived host that cancels a stale render keeps what
    /// it already paid for. A single kernel operation is not interrupted.
    pub interrupt: Option<Arc<AtomicBool>>,
    /// The request's resource limits (`eval::limits`); `None` is
    /// unlimited, as OpenSCAD is. Primitives and extrusions are checked
    /// against the fragment, slice and triangle limits before they are
    /// built, every result against the triangle and memory limits after,
    /// and the time limit before every node and inside long loops. A limit
    /// passed stops the render as [`RenderOptions::interrupt`] does, with
    /// the limit recorded on the guard.
    pub guard: Option<Arc<eval::limits::Guard>>,
    /// What a node answered from the cache prints. `None` is OpenSCAD's
    /// rule: nothing, as its geometry cache answers silently. That is what
    /// the command line wants: `--animate` frames share one cache, and the
    /// nightly prints a subtree's warnings in the first frame only.
    ///
    /// `Some(epoch)` makes a warm render print what a fresh one would,
    /// for a long-lived host whose every render should read like a
    /// command-line run: a cached node that is the first with its key
    /// replays the messages its first computation printed. They carry
    /// source locations, and a node's key ignores where it came from, so
    /// they are replayed only in a render of the same `epoch` (derived by
    /// the host from the sources' contents); with another, a cached node
    /// that had messages is computed again, its children still from the
    /// cache.
    pub replay: Option<u64>,
}

impl Default for RenderOptions {
    fn default() -> Self {
        RenderOptions {
            scheme: crate::color::CORNFIELD,
            force: false,
            fs: Arc::new(StdFs),
            work_dir: PathBuf::new(),
            fonts: Arc::new(text::FontDb::new()),
            interrupt: None,
            guard: None,
            replay: None,
        }
    }
}

impl std::fmt::Debug for RenderOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RenderOptions")
            .field("scheme", &self.scheme)
            .field("force", &self.force)
            .field("work_dir", &self.work_dir)
            .field("replay", &self.replay)
            .finish()
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
    /// Lookups answered from the cache, and nodes computed.
    hits: AtomicU64,
    misses: AtomicU64,
    /// ID blocks by (subtree key, slot): first ID and size.
    ids: Mutex<HashMap<(Key, u32), (u32, u32)>>,
    /// IDs each block turned out to need beyond [`BLOCK`], so the next tree
    /// pass reserves enough (see [`Overflow`]).
    needs: Mutex<Needs>,
    /// Worker threads with the evaluator's stack size: the tree walk
    /// recurses once per level, and trees from recursive modules are as
    /// deep as the evaluator allowed, far beyond a default 2 MiB stack.
    #[cfg(all(feature = "parallel", not(target_arch = "wasm32")))]
    pool: std::sync::OnceLock<rayon::ThreadPool>,
}

/// The geometry cache: least recently used entries go first once the
/// estimated size passes the budget. OpenSCAD bounds its caches the same
/// way (100 MiB each for geometry and for backend solids by default);
/// without a bound, a deep chain of nested unions keeps every intermediate
/// result and memory grows with the square of the depth.
#[derive(Debug)]
struct Cache {
    entries: HashMap<Key, Entry>,
    /// Use stamp -> key, oldest first.
    order: std::collections::BTreeMap<u64, Key>,
    bytes: usize,
    budget: usize,
    clock: u64,
    evictions: u64,
}

/// One cached result.
#[derive(Debug)]
struct Entry {
    geom: Option<Geometry>,
    cost: usize,
    stamp: u64,
    replay: Replay,
    /// Where each original ID in `geom` came from; see [`IdRef`].
    ids: IdTable,
}

/// One original ID of a cached solid and the block it was drawn from:
/// (subtree key, slot) and the offset in the block. `None` would be an ID
/// from Manifold's global counter, which no result keeps today (a 3D
/// `minkowski` draws its hulls' IDs there but gives its sum a fresh ID from
/// its own block, and a block that overflows empties the cache); such an
/// ID is left as it is.
///
/// The same subtree can be given different blocks in different renders
/// (see [`Renderer::prepare`]), so a hit's IDs are rebased onto this
/// render's blocks before it is used; this table says how. Without it a
/// warm session's output depended on what it had rendered before: IDs
/// decide the order of a solid's triangle runs, and a block kept from an
/// earlier variant sat out of tree order, so exporting the same file warm
/// and cold gave the same triangles in a different order (the T2 pilot).
#[derive(Debug, Clone, Copy)]
struct IdRef {
    id: u32,
    block: Option<((Key, u32), u32)>,
}

type IdTable = Arc<[IdRef]>;

/// The messages a cached node printed when it was computed as the first
/// node with its key, so that a later render in which it is again the
/// first prints them again, as a fresh render would. They are only valid
/// for renders whose nodes below it are first or not in the same pattern
/// (a node that is not first prints nothing, so its messages are missing
/// from its parent's), and whose sources are the same
/// ([`RenderOptions::replay`]).
#[derive(Debug, Clone)]
struct Replay {
    /// `None` when the node was computed while not first: its messages
    /// then lack those of its children and cannot be replayed.
    msgs: Option<Arc<Vec<Msg>>>,
    /// [`Ctx::pattern`] of the node when it was computed.
    pattern: u64,
    epoch: u64,
    /// The most the subtree asked of the count limits while it was
    /// computed; `None` when it was computed without limits (the checks
    /// that measure it are skipped then). See [`Demand`].
    demand: Option<Demand>,
    /// The entry is a preview product drawn in image space
    /// ([`KeptProduct::Image`]), which has no mesh; always false for a
    /// node's result.
    image: bool,
}

/// The largest fragment, slice and triangle counts a subtree asked for
/// (a primitive's rings, an extrusion's slices, any result's triangles).
///
/// A cache hit costs no work, but it must not let a request pass that the
/// same request with a cold cache refuses: after the limits are lowered
/// (an agent checking whether a model fits a budget, or the app's own
/// limits changing), a warm cache would otherwise answer with results no
/// longer allowed, and whether a model passes would depend on what ran
/// before it. So a hit is used only when its recorded demand is within
/// the request's limits; otherwise the node is computed again, and the
/// check that refuses it fails at the node that asked too much, with the
/// message and location a cold render gives. Memory and time are not
/// re-checked: a hit allocates nothing and takes no time.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
struct Demand {
    fragments: f64,
    slices: f64,
    triangles: f64,
}

impl Demand {
    fn record(&mut self, l: eval::limits::Limit, asked: f64) {
        use eval::limits::Limit;
        let slot = match l {
            Limit::Fragments => &mut self.fragments,
            Limit::Slices => &mut self.slices,
            Limit::Triangles => &mut self.triangles,
            _ => return,
        };
        if asked > *slot {
            *slot = asked;
        }
    }

    fn merge(&mut self, o: &Demand) {
        self.fragments = self.fragments.max(o.fragments);
        self.slices = self.slices.max(o.slices);
        self.triangles = self.triangles.max(o.triangles);
    }

    /// Whether every count is within `g`'s limits.
    fn allowed(&self, g: &eval::limits::Guard) -> bool {
        use eval::limits::Limit;
        [
            (Limit::Fragments, self.fragments),
            (Limit::Slices, self.slices),
            (Limit::Triangles, self.triangles),
        ]
        .into_iter()
        .all(|(l, asked)| g.exceeds(l, asked, "").is_none())
    }
}

impl Default for Cache {
    fn default() -> Self {
        Cache {
            entries: HashMap::new(),
            order: Default::default(),
            bytes: 0,
            budget: CACHE_BUDGET,
            clock: 0,
            evictions: 0,
        }
    }
}

/// Default cache budget, the sum of OpenSCAD's two default cache sizes.
pub const CACHE_BUDGET: usize = 200 << 20;

/// What the geometry cache holds, for hosts that report it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CacheStats {
    pub entries: usize,
    /// Estimated memory of the cached geometry.
    pub bytes: usize,
    /// The budget: least recently used entries go once `bytes` passes it.
    pub budget: usize,
    /// Node lookups answered from the cache, and nodes computed, since the
    /// renderer was made.
    pub hits: u64,
    pub misses: u64,
    /// Entries dropped to stay within the budget.
    pub evictions: u64,
}

impl Cache {
    fn get(&mut self, k: Key) -> Option<(Option<Geometry>, Replay, IdTable)> {
        let e = self.entries.get_mut(&k)?;
        self.order.remove(&e.stamp);
        self.clock += 1;
        e.stamp = self.clock;
        self.order.insert(self.clock, k);
        Some((e.geom.clone(), e.replay.clone(), e.ids.clone()))
    }

    /// Store a hit's geometry rebased onto the current blocks, so the next
    /// render that keeps those blocks uses it as it is. IDs map one for
    /// one, so the cost is unchanged.
    fn rebased(&mut self, k: Key, g: Option<Geometry>, ids: IdTable) {
        if let Some(e) = self.entries.get_mut(&k) {
            e.geom = g;
            e.ids = ids;
        }
    }

    fn insert(&mut self, k: Key, g: Option<Geometry>, mut replay: Replay, ids: IdTable) {
        let cost = g.as_ref().map_or(0, cost_of) + 64 + ids.len() * size_of::<IdRef>();
        if let Some(old) = self.entries.remove(&k) {
            self.bytes -= old.cost;
            self.order.remove(&old.stamp);
            // Two threads can compute the same key in one render, only one
            // of them as the first node; keep the messages that one found.
            if replay.msgs.is_none() && old.replay.msgs.is_some() {
                replay = old.replay;
            }
        }
        self.clock += 1;
        self.entries.insert(
            k,
            Entry {
                geom: g,
                cost,
                stamp: self.clock,
                replay,
                ids,
            },
        );
        self.order.insert(self.clock, k);
        self.bytes += cost;
        self.shrink();
    }

    fn shrink(&mut self) {
        while self.bytes > self.budget && self.entries.len() > 1 {
            let Some((_, old)) = self.order.pop_first() else {
                break;
            };
            if let Some(e) = self.entries.remove(&old) {
                self.bytes -= e.cost;
                self.evictions += 1;
            }
        }
    }
}

/// Rough memory of a geometry: coordinates and indices, plus Manifold's
/// halfedges, normals and triangle references for a solid.
fn cost_of(g: &Geometry) -> usize {
    match g {
        Geometry::PolySet(p) => {
            p.vertices.len() * 24 + p.faces.iter().map(|f| 24 + 4 * f.len()).sum::<usize>()
        }
        Geometry::Manifold(m) => solid_cost(m),
        Geometry::Polygon2d(p) => p.outlines.iter().map(|o| 24 + 16 * o.vertices.len()).sum(),
    }
}

/// [`cost_of`] a solid.
pub(crate) fn solid_cost(m: &ManifoldGeometry) -> usize {
    m.manifold.num_vert() * 48 + m.manifold.num_tri() * 112
}

/// IDs per block by default: enough for one conversion of a mesh with
/// this many colours, or this many conversions under one node.
const BLOCK: u32 = 64;

/// The slot of a node's own ID block (children use their index).
const OWN: u32 = u32::MAX;

/// Where blocks report running out.
///
/// An ID drawn from Manifold's global counter while subtrees are evaluated
/// in parallel gets a value that depends on which thread drew first, and
/// original IDs decide the order of a solid's triangle runs, so such an ID
/// in a result makes the output depend on scheduling (two siblings that
/// each drew one would swap places in their parent's union from run to
/// run). So a block that runs out still hands out global IDs, to finish the
/// render, but records how many it needed; [`Renderer::render`] then drops
/// that render's results and renders again with blocks that size, reserved
/// in tree order like the rest. Only a mesh with more than [`BLOCK`]
/// colours, or a node converting more than that many meshes, pays for the
/// second pass.
type Overflow = Mutex<Needs>;

/// IDs needed by (subtree key, slot), for blocks that ran out.
type Needs = HashMap<(Key, u32), u32>;

#[derive(Clone, Copy)]
struct Block<'a> {
    first: u32,
    size: u32,
    key: (Key, u32),
    overflow: &'a Overflow,
}

impl Block<'_> {
    /// Record that this block needed `n` IDs, and draw them globally.
    fn overflow(&self, n: u32, count: u32) -> u32 {
        let mut o = self
            .overflow
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let need = o.entry(self.key).or_insert(0);
        *need = (*need).max(n);
        Manifold::reserve_ids(count)
    }
}

impl std::fmt::Debug for Block<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Block({}, {})", self.first, self.size)
    }
}

impl IdSource for Block<'_> {
    /// The block's start every time: each call is a separate conversion of
    /// the same child, whose IDs may repeat (as OpenSCAD's would not, but
    /// they never meet in one solid).
    fn reserve(&self, count: u32) -> u32 {
        if count <= self.size {
            self.first
        } else {
            self.overflow(count, count)
        }
    }
}

/// Consecutive ranges of one block, for several conversions under one
/// node.
struct Seq<'a> {
    block: Block<'a>,
    used: std::cell::Cell<u32>,
}

impl IdSource for Seq<'_> {
    fn reserve(&self, count: u32) -> u32 {
        let used = self.used.get();
        self.used.set(used + count);
        if used + count <= self.block.size {
            self.block.first + used
        } else {
            self.block.overflow(used + count, count)
        }
    }
}

/// A node's children with their results.
type Items<'n> = Vec<(&'n Node, Option<Geometry>)>;

/// What one subtree produced.
struct Out {
    geom: Option<Geometry>,
    msgs: Vec<Msg>,
}

/// Nodes a tree may have and still be walked on the calling thread: few
/// enough that the walk's recursion (one level per node) fits any thread's
/// stack, where a deep tree needs the pool's [`eval::DEFAULT_THREAD_STACK`].
#[cfg(all(feature = "parallel", not(target_arch = "wasm32")))]
const CHAIN_NODES: usize = 32;

/// Whether `top` is a short chain: at most [`CHAIN_NODES`] nodes, none of
/// them with more than one child (`cube(1)`, `rotate(..) linear_extrude(..)
/// square(..)`). Such a tree has no two subtrees to run side by side, so
/// its walk runs on the calling thread without starting the pool. The
/// kernels' own data-parallel loops still run on rayon (on its global
/// pool, started only if one of them is large enough to split), and each
/// of those gives the same result at any thread count, so the output is
/// the same either way; a build without the `parallel` feature walks every
/// tree like this.
#[cfg(all(feature = "parallel", not(target_arch = "wasm32")))]
fn is_chain(top: &Node) -> bool {
    let mut n = top;
    for _ in 0..CHAIN_NODES {
        match n.children.as_slice() {
            [] => return true,
            [c] => n = c,
            _ => return false,
        }
    }
    false
}

/// How many parallel splits of the render walk may nest. A split runs
/// its children's walks inside rayon's `map` on the stack of the thread
/// that split, so a tree that branches at every level (a recursive module
/// that adds a leaf beside its recursive call) nested once per level and
/// could overflow even the pool's large stacks. Past this many splits the
/// children are walked in order, which gives the same result: the output
/// is the same at any thread count. By then the pool has long had more
/// tasks than threads.
#[cfg(all(feature = "parallel", not(target_arch = "wasm32")))]
const PARALLEL_MAX_NESTING: u32 = 64;

#[cfg(all(test, feature = "parallel", not(target_arch = "wasm32")))]
#[path = "evaluate/small_stack_tests.rs"]
mod small_stack_tests;

/// Whether computing `n` takes its children's results. The primitives
/// ignore any children they were given, and so does the walk: evaluating
/// them could fail or warn where OpenSCAD does neither. The match is
/// exhaustive so a new kind has to be placed on one side.
fn uses_children(n: &Node) -> bool {
    match &n.kind {
        NodeKind::Cube { .. }
        | NodeKind::Sphere { .. }
        | NodeKind::Cylinder { .. }
        | NodeKind::Polyhedron { .. }
        | NodeKind::Square { .. }
        | NodeKind::Circle { .. }
        | NodeKind::Polygon { .. }
        | NodeKind::Surface { .. }
        | NodeKind::Import(_)
        | NodeKind::Text(_) => false,
        NodeKind::Root
        | NodeKind::Group { .. }
        | NodeKind::Render { .. }
        | NodeKind::IntersectionFor
        | NodeKind::Csg(_)
        | NodeKind::Fill
        | NodeKind::Color { .. }
        | NodeKind::Transform { .. }
        | NodeKind::Offset { .. }
        | NodeKind::LinearExtrude(_)
        | NodeKind::RotateExtrude { .. }
        | NodeKind::Projection { .. }
        | NodeKind::Minkowski { .. }
        | NodeKind::Hull
        | NodeKind::Resize { .. }
        | NodeKind::Part { .. } => true,
    }
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
    /// Node index → a hash of the `first` flags of its subtree in tree
    /// order. A node's messages include those of its first descendants
    /// only, so cached messages are replayed only under the same pattern
    /// (see [`Replay`]).
    pattern: Vec<u64>,
    /// ID blocks by (subtree key, slot): slot `i` for the conversion of
    /// child `i`, [`OWN`] for the node's own use (colouring a solid).
    blocks: HashMap<(Key, u32), (u32, u32)>,
    /// The same blocks as (first ID, (subtree key, slot), size), in
    /// ascending order of first ID, which is the order they were placed.
    owners: Vec<(u32, (Key, u32), u32)>,
    /// Blocks that ran out during this render.
    overflow: Overflow,
    /// With resource limits: the bytes each computed node's result was
    /// charged to the guard's memory estimate (by node index), until its
    /// parent has used it.
    charged: Mutex<HashMap<usize, u64>>,
    /// With resource limits: each node's [`Demand`] in this render (by
    /// node index), its own checks and its children's merged when it is
    /// cached.
    demand: Mutex<HashMap<usize, Demand>>,
    /// Whether a node's children run side by side on the pool (with the
    /// `parallel` feature, off wasm32). A test turns it off to walk a tree
    /// the way wasm32 does, entirely on its own small stack.
    parallel: bool,
    /// The token the kernel's booleans run under
    /// ([`crate::manifold_geom::kernel_token`]), `None` without an
    /// interrupt flag or limits.
    token: Option<manifold_rust::cancel::CancelToken>,
}

/// A result's weight in the memory limit's estimate, as a multiple of
/// its [`cost_of`] (the geometry cache's measure of what it holds): the
/// kernel's working copies (a mesh converted to a solid, a boolean's
/// operands and intermediate results, Manifold's collider and relations)
/// are not results but take memory while a node computes. Calibrated on
/// five benchmark models, whose peak RSS ran 4 to 10 times the
/// unweighted estimate (BOSL2's fractal_tree: 1.96 GB against under 256
/// MiB); weighted, the estimate is within about 1x to 4x of the peak.
pub(crate) const KERNEL_FACTOR: u64 = 6;

/// A node as a limit message names it: `sphere()`, `linear_extrude()`.
fn node_what(n: &Node) -> String {
    match &n.origin {
        Some(o) if !o.name.is_empty() => format!("{}()", o.name),
        _ => "the model".into(),
    }
}

fn loc_of(n: &Node) -> Option<MsgLoc> {
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

/// The key a node's result is cached under: its [`Keys`] key, except for a
/// group that `Keys` makes transparent but the evaluator does not.
///
/// `Keys` gives a group with at most one child that has content its
/// child's key, since OpenSCAD's `getIdString` leaves such groups out. Its
/// other children are empty groups, and the 2D union still counts them:
/// `applyToChildren2D` sends `[nothing, X]` through Clipper, which snaps
/// `X` to Clipper's grid, while `X` alone passes through
/// (`GeometryEvaluator.cc:317-333,434-441`). So `group() { group(); X }`
/// and `X` share a key but not a result, and whichever the cache held last
/// answered the other. Parallel siblings made that a race: in BOSL2's
/// `torx_mask2d` the hulled tip circles are such groups, and the drive
/// recess came out differently in most runs of `bosl_screws__001`.
///
/// Such a group gets a key of its own instead, derived from its content
/// child's, so it computes and caches what it computes: the first copy's
/// result, as in OpenSCAD, for every copy. (OpenSCAD's own later copies
/// hit its cache entry for `X`, so its output depends on its cache; the
/// first occurrence is the one a cold render always computes.) The 3D
/// union drops empty children before deciding to pass one through, so
/// 3D groups did not need this; they get their own key too, which costs a
/// cache entry sharing the child's geometry and nothing more.
///
/// [`result_key`] is the same key for callers outside this module.
fn cache_key(n: &Node, keys: &Keys, memo: &mut [Option<Key>]) -> Key {
    // The chain of transparent groups from `n` down to the node with
    // content, walked iteratively: BOSL2 nests groups deeply.
    let mut chain: Vec<&Node> = Vec::new();
    let mut cur = n;
    let mut key = loop {
        if let Some(k) = memo[cur.index] {
            break k;
        }
        let h = keys.get(cur);
        let group = matches!(cur.kind, NodeKind::Root | NodeKind::Group { .. });
        // A transparent group's key is its content child's (`Keys` hashes
        // any other group from its own label), so the child with an equal
        // key is the content.
        match group
            .then(|| cur.children.iter().find(|c| keys.get(c) == h))
            .flatten()
        {
            Some(c) => {
                chain.push(cur);
                cur = c;
            }
            None => {
                memo[cur.index] = Some(h);
                break h;
            }
        }
    };
    for g in chain.into_iter().rev() {
        if g.children.iter().filter(|c| !is_background(c)).count() > 1 {
            // Any fixed bijection works: keys are SHA-256 prefixes, so
            // the result meets another key only by collision. Each level
            // applies it again, since `group() { group(); group() {
            // group(); X } }` goes through Clipper twice.
            key = key
                .wrapping_mul(0x2545_f491_4f6c_dd1d_9e37_79b9_7f4a_7c15)
                .wrapping_add(0x6a09_e667_f3bc_c908);
        }
        memo[g.index] = Some(key);
    }
    key
}

/// The key [`Renderer::render`] caches `top`'s result under (see
/// `cache_key`). A cache of whole results kept outside the renderer must
/// key on this rather than on `keys.get(top)`, or a top-level
/// `group(); X` would be answered with the result of a top-level `X`.
pub fn result_key(top: &Node, keys: &Keys) -> u128 {
    // `cache_key` memoises by node index; only the chain of groups under
    // `top` is visited, so the memo needs to reach that chain's indices.
    let mut len = 0;
    let mut n = top;
    loop {
        len = len.max(n.index + 1);
        match n.children.iter().find(|c| keys.get(c) == keys.get(n)) {
            Some(c) => n = c,
            None => break,
        }
    }
    cache_key(top, keys, &mut vec![None; len])
}

fn warn(n: &Node, text: &str) -> Msg {
    Msg {
        severity: Some(Severity::Warning),
        text: text.into(),
        loc: loc_of(n),
    }
}

impl Renderer {
    pub fn new() -> Renderer {
        Renderer::default()
    }

    /// A renderer whose geometry cache holds about `bytes` (estimated)
    /// before it evicts the least recently used entries.
    pub fn with_budget(bytes: usize) -> Renderer {
        let r = Renderer::default();
        r.cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .budget = bytes;
        r
    }

    /// The cached result of a leaf node (a primitive or an import) of a
    /// tree rendered with `keys`, if the cache still holds it; nothing is
    /// computed and the entry's age is not touched. For a host that wants
    /// to look at what the render read (an imported mesh) without reading
    /// it again. Groups are not leaves: their key is not their result's
    /// (see `cache_key`).
    pub fn cached_leaf(&self, n: &Node, keys: &Keys) -> Option<Geometry> {
        if !n.children.is_empty() || matches!(n.kind, NodeKind::Root | NodeKind::Group { .. }) {
            return None;
        }
        self.cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .entries
            .get(&keys.get(n))
            .and_then(|e| e.geom.clone())
    }

    /// Change the cache budget, evicting at once if it is now over.
    pub fn set_budget(&self, bytes: usize) {
        let mut c = self
            .cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        c.budget = bytes;
        c.shrink();
    }

    /// The cache's size, budget and counters.
    pub fn stats(&self) -> CacheStats {
        let c = self
            .cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        CacheStats {
            entries: c.entries.len(),
            bytes: c.bytes,
            budget: c.budget,
            hits: self.hits.load(Ordering::Relaxed),
            misses: self.misses.load(Ordering::Relaxed),
            evictions: c.evictions,
        }
    }

    /// Render `top` (the root, or the node a `!` selected), whose keys are
    /// in `keys`. Returns the node that stopped the render if it uses a
    /// feature of a later phase.
    pub fn render(
        &self,
        top: &Node,
        keys: &Keys,
        opts: RenderOptions,
    ) -> Result<Rendered, Unsupported> {
        loop {
            let (out, overflow) = self.render_once(top, keys, &opts)?;
            if overflow.is_empty() {
                return Ok(out);
            }
            // Some block was too small, so this render's results hold IDs
            // drawn in scheduling order (see [`Overflow`]). Size those
            // blocks for what they needed and start again from nothing;
            // the results cached on the way may hold such IDs too. The
            // needs only grow, so this ends after one retry.
            let mut needs = self
                .needs
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            for (k, n) in overflow {
                let e = needs.entry(k).or_insert(0);
                *e = (*e).max(n);
            }
            drop(needs);
            self.clear();
        }
    }

    /// The geometry of several nodes of one tree, each as [`Renderer::render`]
    /// would give it (without `force`), in the order given: the leaves a
    /// preview draws (`CSGTreeEvaluator` asks for each leaf's geometry on
    /// its own). The nodes may nest; one tree-order pass assigns keys and ID
    /// blocks to all of them before anything runs, so the results are the
    /// same at any thread count. Nodes are evaluated deepest first, those
    /// at one depth in parallel, so a nested node is computed once and an
    /// enclosing one finds it in the cache (and does not repeat its
    /// messages).
    pub fn render_many(
        &self,
        tops: &[&Node],
        keys: &Keys,
        opts: RenderOptions,
    ) -> Result<Vec<Rendered>, Unsupported> {
        let opts = RenderOptions {
            force: false,
            ..opts
        };
        loop {
            let (out, overflow) = self.render_tops(tops, keys, &opts)?;
            if overflow.is_empty() {
                return Ok(out);
            }
            let mut needs = self
                .needs
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            for (k, n) in overflow {
                let e = needs.entry(k).or_insert(0);
                *e = (*e).max(n);
            }
            drop(needs);
            self.clear();
        }
    }

    fn render_once(
        &self,
        top: &Node,
        keys: &Keys,
        opts: &RenderOptions,
    ) -> Result<(Rendered, Needs), Unsupported> {
        let ctx = self.prepare(&[top], keys, opts);
        #[cfg(all(feature = "parallel", not(target_arch = "wasm32")))]
        let out = if is_chain(top) {
            ctx.node(top)?
        } else {
            self.pool().install(|| ctx.node(top))?
        };
        #[cfg(not(all(feature = "parallel", not(target_arch = "wasm32"))))]
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
            msgs.extend(w.into_iter().map(|t| Msg {
                severity: Some(Severity::Warning),
                text: t,
                loc: None,
            }));
            msgs.extend(e.into_iter().map(|t| Msg {
                severity: Some(Severity::Error),
                text: t,
                loc: None,
            }));
            geom = Some(Geometry::Manifold(Arc::new(m)));
        }
        let cache_entries = self
            .cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .entries
            .len();
        let overflow = std::mem::take(
            &mut *ctx
                .overflow
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        );
        Ok((
            Rendered {
                geometry: geom,
                messages: msgs,
                cache_entries,
            },
            overflow,
        ))
    }

    /// [`Renderer::render_many`]'s single pass.
    fn render_tops(
        &self,
        tops: &[&Node],
        keys: &Keys,
        opts: &RenderOptions,
    ) -> Result<(Vec<Rendered>, Needs), Unsupported> {
        let ctx = self.prepare(tops, keys, opts);
        // Depth of each top: how many other tops enclose it.
        let mut depth = vec![0usize; tops.len()];
        {
            let mut owner: HashMap<usize, usize> = HashMap::new();
            for (i, t) in tops.iter().enumerate() {
                owner.insert(t.index, i);
            }
            // Iterative, as every walk of the tree here is (see
            // `Ctx::node`): only the maximum matters, not the order.
            let mut stack: Vec<(&Node, usize)> = tops.iter().map(|t| (*t, 0)).collect();
            while let Some((n, mut enclosing)) = stack.pop() {
                if let Some(&i) = owner.get(&n.index) {
                    depth[i] = depth[i].max(enclosing);
                    enclosing += 1;
                }
                stack.extend(n.children.iter().map(|c| (c, enclosing)));
            }
        }
        let mut outs: Vec<Option<Out>> = (0..tops.len()).map(|_| None).collect();
        let max_depth = depth.iter().copied().max().unwrap_or(0);
        for d in (0..=max_depth).rev() {
            let at: Vec<usize> = (0..tops.len()).filter(|&i| depth[i] == d).collect();
            #[cfg(all(feature = "parallel", not(target_arch = "wasm32")))]
            let results: Vec<Result<Out, Unsupported>> = if let [i] = at[..]
                && is_chain(tops[i])
            {
                vec![ctx.node(tops[i])]
            } else {
                use rayon::prelude::*;
                self.pool()
                    .install(|| at.par_iter().map(|&i| ctx.node(tops[i])).collect())
            };
            #[cfg(not(all(feature = "parallel", not(target_arch = "wasm32"))))]
            let results: Vec<Result<Out, Unsupported>> =
                at.iter().map(|&i| ctx.node(tops[i])).collect();
            for (i, r) in at.into_iter().zip(results) {
                outs[i] = Some(r?);
            }
        }
        let cache_entries = self
            .cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .entries
            .len();
        let overflow = std::mem::take(
            &mut *ctx
                .overflow
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        );
        let rendered = outs
            .into_iter()
            .map(|o| {
                let o = o.expect("every top is evaluated at its depth");
                Rendered {
                    geometry: o.geom,
                    messages: o.msgs,
                    cache_entries,
                }
            })
            .collect();
        Ok((rendered, overflow))
    }

    /// The pool, started on first use. Starting it spawns every worker
    /// thread at once, a measurable share of a whole `cube(1)` run from
    /// the command line; [`is_chain`] keeps trees with nothing to run side
    /// by side from starting it at all.
    ///
    /// Its threads get the evaluator's stack, as the thread that walks a
    /// chain does. The walk no longer needs it (it nests at most
    /// [`PARALLEL_MAX_NESTING`] splits deep): with 128 KiB a thread every
    /// conformance test passes and every bench model's STL is the same
    /// (release build, macOS arm64), where 64 KiB crashed BOSL2's
    /// `fractal_tree` and the `module_recursion` tests.
    /// The kernels do: building and reading a Clipper2 polytree recurses
    /// once per level of polygon nesting (`from_tree`'s walk, Clipper2's
    /// `recursive_check_owners`), so the union of 3,000 concentric rings
    /// needed over 512 KiB, 6,000 over 1 MiB and 12,000 (19 s to compute)
    /// over 2 MiB. With the evaluator's 80 MiB that nesting is limited by
    /// time rather than by a crash, the same on a pool thread as on the
    /// walking thread; and the size is only reserved address space, of
    /// which a thread touches what it uses.
    #[cfg(all(feature = "parallel", not(target_arch = "wasm32")))]
    fn pool(&self) -> &rayon::ThreadPool {
        self.pool.get_or_init(|| {
            rayon::ThreadPoolBuilder::new()
                .stack_size(eval::DEFAULT_THREAD_STACK)
                .thread_name(|i| format!("geom-{i}"))
                .build()
                .expect("geometry thread pool")
        })
    }

    /// The tree-order pass: hashes, first occurrences and ID blocks for
    /// every node under `tops`, all decided before anything runs in
    /// parallel. A node reached again under a later top (a nested top) is
    /// not revisited, so it keeps the flags of its first, tree-order visit.
    fn prepare<'a>(&'a self, tops: &[&Node], keys: &Keys, opts: &'a RenderOptions) -> Ctx<'a> {
        // The walks here are iterative, as `Ctx::node` is: a recursive
        // one needs stack in proportion to the tree's depth.
        fn max_index(top: &Node) -> usize {
            let mut max = 0;
            let mut stack = vec![top];
            while let Some(n) = stack.pop() {
                max = max.max(n.index);
                stack.extend(&n.children);
            }
            max
        }
        let len = tops.iter().map(|t| max_index(t)).max().unwrap_or(0) + 1;
        let mut ctx = Ctx {
            r: self,
            opts,
            hashes: vec![0; len],
            first: vec![false; len],
            pattern: vec![0; len],
            blocks: HashMap::new(),
            owners: Vec::new(),
            overflow: Mutex::new(HashMap::new()),
            charged: Mutex::new(HashMap::new()),
            demand: Mutex::new(HashMap::new()),
            parallel: cfg!(all(feature = "parallel", not(target_arch = "wasm32"))),
            token: crate::manifold_geom::kernel_token(opts.interrupt.as_ref(), opts.guard.as_ref()),
        };
        {
            let mut seen = HashSet::new();
            // One past the last ID of the blocks placed so far.
            let mut end = 0;
            let mut visited = vec![false; len];
            let mut memo = vec![None; len];
            let mut ids = self
                .ids
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let needs = self
                .needs
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            // (node, its parent's key and first-occurrence flag)
            let mut stack: Vec<(&Node, Option<(Key, bool)>)> =
                tops.iter().rev().map(|t| (*t, None)).collect();
            while let Some((n, parent)) = stack.pop() {
                if std::mem::replace(&mut visited[n.index], true) {
                    continue;
                }
                let h = cache_key(n, keys, &mut memo);
                ctx.hashes[n.index] = h;
                // A group with one child that has content, and nothing
                // else the evaluator counts, shares that child's key
                // (`Keys`, see `cache_key`): it is the same computation,
                // so the child inherits the group's claim to be first.
                let first = match parent {
                    Some((ph, pf)) if ph == h => pf,
                    _ => seen.insert(h),
                };
                seen.insert(h);
                ctx.first[n.index] = first;
                for slot in std::iter::once(OWN).chain(0..n.children.len() as u32) {
                    let k = (h, slot);
                    if ctx.blocks.contains_key(&k) {
                        // A second node with this key: the same computation,
                        // the same IDs.
                        continue;
                    }
                    let size = needs.get(&k).map_or(BLOCK, |&n| n.max(BLOCK));
                    let b = ids.entry(k).or_insert((0, 0));
                    // A block from an earlier render is kept only if it
                    // comes after every block placed so far in this one.
                    // A fresh render reserves its blocks in this order, so
                    // its IDs rise in tree order; a block kept out of that
                    // order (a subtree that sat later in an earlier
                    // variant) would put its runs before its tree-order
                    // predecessors' and change the output's triangle order.
                    if b.1 < size || b.0 < end {
                        *b = (Manifold::reserve_ids(size), size);
                    }
                    end = b.0 + b.1;
                    ctx.blocks.insert(k, *b);
                    ctx.owners.push((b.0, k, b.1));
                }
                stack.extend(n.children.iter().rev().map(|c| (c, Some((h, first)))));
            }
        }
        // Each node's flag hashed with its children's patterns, in
        // post-order: a node is seen once on the way down, and again
        // (`ready`) when its children are done.
        fn pattern(top: &Node, first: &[bool], out: &mut [u64], done: &mut [bool]) {
            let mut stack = vec![(top, false)];
            while let Some((n, ready)) = stack.pop() {
                if done[n.index] {
                    continue;
                }
                if !ready {
                    stack.push((n, true));
                    stack.extend(n.children.iter().rev().map(|c| (c, false)));
                    continue;
                }
                let mut h = std::collections::hash_map::DefaultHasher::new();
                first[n.index].hash(&mut h);
                for c in &n.children {
                    out[c.index].hash(&mut h);
                }
                out[n.index] = h.finish();
                done[n.index] = true;
            }
        }
        let mut done = vec![false; len];
        for t in tops {
            pattern(t, &ctx.first, &mut ctx.pattern, &mut done);
        }
        ctx
    }

    /// Forget every cached geometry (the budget stays).
    pub fn clear(&self) {
        let mut c = self
            .cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let budget = c.budget;
        *c = Cache {
            budget,
            ..Cache::default()
        };
    }

    /// A preview product kept by [`Renderer::keep_product`] or
    /// [`Renderer::keep_image_product`] under `key`
    /// ([`crate::csg::product_key`]), if the cache still holds it. A hit
    /// counts as a use for the least-recently-used order.
    pub fn product(&self, key: u128) -> Option<KeptProduct> {
        let (geom, replay, _) = self
            .cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(key)?;
        if replay.image {
            return Some(KeptProduct::Image);
        }
        match geom {
            None => Some(KeptProduct::Mesh(None)),
            Some(Geometry::PolySet(ps)) => Some(KeptProduct::Mesh(Some(ps))),
            // Product keys are hashed apart from node keys, so no node's
            // result is ever found here; if one were, it is not a product.
            Some(_) => None,
        }
    }

    /// Keep a preview product's mesh under `key` for the next preview.
    ///
    /// Product meshes share the geometry cache's budget and its least
    /// recently used order with the subtrees renders cache, so the memory
    /// a session keeps for a document is bounded by the one budget
    /// (`session::Config::geometry_budget`) however it is split between
    /// renders and previews, and a host's eviction under memory pressure
    /// (`set_budget`, `clear`) drops them too. Without that, previewing
    /// one large model after another would keep every product it ever
    /// computed.
    pub fn keep_product(&self, key: u128, mesh: Option<Arc<PolySet>>) {
        self.keep(key, mesh.map(Geometry::PolySet), false);
    }

    /// Keep under `key` that the preview product it names is drawn in
    /// image space because a leaf does not bound a solid. Finding that out
    /// means checking the leaves (`PolySet::is_outward_solid`), which for
    /// the BOSL2 gearbox example took about 20 ms of every preview,
    /// unchanged or not, when there is no boolean to save. The key names
    /// the leaves' meshes, so the verdict holds as long as the key does.
    pub fn keep_image_product(&self, key: u128) {
        self.keep(key, None, true);
    }

    fn keep(&self, key: u128, geom: Option<Geometry>, image: bool) {
        let replay = Replay {
            msgs: None,
            pattern: 0,
            epoch: 0,
            demand: None,
            image,
        };
        self.cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(key, geom, replay, Arc::from([]));
    }
}

/// A preview product an earlier preview kept ([`Renderer::product`]).
#[derive(Debug, Clone)]
pub enum KeptProduct {
    /// The product's boolean, `None` when it came out empty.
    Mesh(Option<Arc<PolySet>>),
    /// A leaf of the product does not bound a solid, so it is drawn in
    /// image space from its leaves, as OpenCSG draws it.
    Image,
}

impl Ctx<'_> {
    /// Whether the render should stop: cancelled, a limit passed, or out
    /// of time. Cheap enough for every node and every ring of a sphere.
    fn stopped(&self) -> bool {
        self.opts
            .interrupt
            .as_ref()
            .is_some_and(|f| f.load(Ordering::Relaxed))
            || self.opts.guard.as_ref().is_some_and(|g| g.stopped())
    }

    /// Limit `l` against what node `n` (`what`) is about to build; a limit
    /// passed is recorded, located at the node, and stops the render.
    fn limit(
        &self,
        n: &Node,
        l: eval::limits::Limit,
        asked: f64,
        what: &str,
    ) -> Result<(), Unsupported> {
        let Some(g) = &self.opts.guard else {
            return Ok(());
        };
        self.demand
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .entry(n.index)
            .or_default()
            .record(l, asked);
        match g.exceeds(l, asked, what) {
            None => Ok(()),
            Some(e) => Err(self.trip(n, g, e)),
        }
    }

    fn trip(
        &self,
        n: &Node,
        g: &eval::limits::Guard,
        mut e: eval::limits::Exceeded,
    ) -> Unsupported {
        e.at = n.origin.as_ref().map(|o| eval::limits::At {
            unit: o.unit,
            span: o.span,
            line: o.line,
        });
        g.trip(e);
        Unsupported {
            what: INTERRUPTED,
            loc: loc_of(n),
        }
    }

    /// A computed result against the triangle limit and the request's
    /// geometry memory. The memory estimate is what is alive: results
    /// computed and not yet used by their parent. Once a node is computed
    /// its children's results are its parent's input no longer (the
    /// cache, with its own budget, may still hold them), so they leave
    /// the estimate; counting every result ever made refused BOSL2's
    /// fractal_tree (1.96 GB peak) at 4 GiB.
    fn check_result(
        &self,
        n: &Node,
        g: &eval::limits::Guard,
        geom: Option<&Geometry>,
    ) -> Result<(), Unsupported> {
        let what = node_what(n);
        let mut bytes = 0;
        if let Some(geom) = geom {
            let tris = match geom {
                Geometry::PolySet(p) => p.faces.iter().map(|f| f.len().saturating_sub(2)).sum(),
                Geometry::Manifold(m) => m.manifold.num_tri(),
                Geometry::Polygon2d(p) => p.outlines.iter().map(|o| o.vertices.len()).sum(),
            };
            self.limit(n, eval::limits::Limit::Triangles, tris as f64, &what)?;
            bytes = KERNEL_FACTOR * cost_of(geom) as u64;
        }
        let charged = g.charge_geometry(bytes, &what);
        let freed: u64 = {
            let mut c = self
                .charged
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            c.insert(n.index, bytes);
            n.children.iter().filter_map(|ch| c.remove(&ch.index)).sum()
        };
        g.credit_geometry(freed);
        if let Err(e) = charged {
            return Err(self.trip(n, g, e));
        }
        Ok(())
    }

    fn block(&self, n: &Node, slot: u32) -> Block<'_> {
        let key = (self.hashes[n.index], slot);
        let (first, size) = self.blocks[&key];
        Block {
            first,
            size,
            key,
            overflow: &self.overflow,
        }
    }

    /// Evaluate one node, from the cache when possible.
    ///
    /// The walk keeps the nodes waiting for their children on a heap
    /// stack instead of recursing once per level. Each level of a
    /// recursive walk cost about 2 KiB of native stack, and far more as
    /// wasm under JavaScriptCore, whose frames for these large functions
    /// are much bigger than V8's: Safari's worker overflowed rendering a
    /// BOSL2 gear whose tree is only 77 levels deep, where Chromium and
    /// Firefox rendered it. Now the stack the walk needs does not depend on
    /// the tree's depth. The order everything happens in is the
    /// recursion's: a node is looked up in the cache on the way down, its
    /// children are evaluated in order (in parallel where
    /// [`Ctx::kids_in_parallel`] says so), and the first error ends the
    /// walk.
    fn node(&self, top: &Node) -> Result<Out, Unsupported> {
        self.walk(top, 0)
    }

    /// [`Ctx::node`] inside `nesting` parallel splits (see
    /// [`PARALLEL_MAX_NESTING`]).
    fn walk(&self, top: &Node, nesting: u32) -> Result<Out, Unsupported> {
        /// A node whose children are being evaluated, with the results
        /// of those done so far.
        struct Waiting<'n> {
            n: &'n Node,
            outs: Vec<Out>,
        }
        let mut waiting: Vec<Waiting<'_>> = Vec::new();
        let mut next = top;
        loop {
            let mut done = match self.lookup(next)? {
                Some(out) => out,
                None if !uses_children(next) || next.children.is_empty() => {
                    self.finish(next, Vec::new())?
                }
                None => match self.kids_in_parallel(next, nesting) {
                    Some(kids) => self.finish(next, kids?)?,
                    None => {
                        waiting.push(Waiting {
                            n: next,
                            outs: Vec::with_capacity(next.children.len()),
                        });
                        next = &next.children[0];
                        continue;
                    }
                },
            };
            // Hand the result to its parent; a parent with all its
            // children done is finished in turn, up to one that still has a
            // child to start, or the top.
            loop {
                let Some(w) = waiting.last_mut() else {
                    return Ok(done);
                };
                w.outs.push(done);
                if let Some(c) = w.n.children.get(w.outs.len()) {
                    next = c;
                    break;
                }
                let w = waiting.pop().expect("the parent just updated");
                done = self.finish(w.n, w.outs)?;
            }
        }
    }

    /// The children's results of a node with more than one child,
    /// evaluated side by side on the pool, unless `nesting` splits already
    /// enclose this one. Each child's own walk is [`Ctx::walk`]'s, so the
    /// stack grows only at the splits, and the pool's threads have
    /// [`eval::DEFAULT_THREAD_STACK`].
    #[cfg(all(feature = "parallel", not(target_arch = "wasm32")))]
    fn kids_in_parallel(&self, n: &Node, nesting: u32) -> Option<Result<Vec<Out>, Unsupported>> {
        use rayon::prelude::*;
        (self.parallel && n.children.len() > 1 && nesting < PARALLEL_MAX_NESTING).then(|| {
            n.children
                .par_iter()
                .map(|c| self.walk(c, nesting + 1))
                .collect()
        })
    }

    /// Without the pool, children are always evaluated in order by the
    /// walk itself.
    #[cfg(not(all(feature = "parallel", not(target_arch = "wasm32"))))]
    fn kids_in_parallel(&self, _n: &Node, _nesting: u32) -> Option<Result<Vec<Out>, Unsupported>> {
        let _ = self.parallel;
        None
    }

    /// The start of evaluating `n`: its result from the cache, or `None`
    /// when it must be computed (after its children).
    fn lookup(&self, n: &Node) -> Result<Option<Out>, Unsupported> {
        let h = self.hashes[n.index];
        let first = self.first[n.index];
        let pattern = self.pattern[n.index];
        let cached = self
            .r
            .cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(h);
        // A hit whose demand the request's limits no longer allow (or that
        // was computed without limits, so its demand is unknown) is
        // computed again; see [`Demand`].
        let cached = match (&self.opts.guard, cached) {
            (Some(g), Some((geom, replay, ids))) => match replay.demand {
                Some(d) if d.allowed(g) => {
                    self.demand
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .insert(n.index, d);
                    Some((geom, replay, ids))
                }
                _ => None,
            },
            (_, c) => c,
        };
        if let Some((geom, replay, ids)) = cached {
            if !first || self.opts.replay.is_none() {
                // A later copy, or a host that keeps OpenSCAD's rule: the
                // cache answers silently.
                self.r.hits.fetch_add(1, Ordering::Relaxed);
                return Ok(Some(Out {
                    geom: self.rebase(h, geom, &ids),
                    msgs: Vec::new(),
                }));
            }
            if let Some(msgs) = replay.msgs
                && replay.pattern == pattern
                && (msgs.is_empty() || Some(replay.epoch) == self.opts.replay)
            {
                self.r.hits.fetch_add(1, Ordering::Relaxed);
                return Ok(Some(Out {
                    geom: self.rebase(h, geom, &ids),
                    msgs: msgs.to_vec(),
                }));
            }
            // The messages a fresh render would print here are not known:
            // compute the node again, from its children's cached results.
        }
        if self.stopped() {
            return Err(Unsupported::interrupted());
        }
        self.r.misses.fetch_add(1, Ordering::Relaxed);
        Ok(None)
    }

    /// The end of evaluating `n`, which [`Ctx::lookup`] did not find in
    /// the cache: compute it from its children's results `kids` (empty for
    /// a node that does not use its children), check it and cache it.
    fn finish(&self, n: &Node, kids: Vec<Out>) -> Result<Out, Unsupported> {
        let h = self.hashes[n.index];
        let first = self.first[n.index];
        let pattern = self.pattern[n.index];
        let mut out = self.compute(n, kids)?;
        // A kernel operation the token cancelled returned an empty solid,
        // not its answer; cached, it would be the answer of every later
        // render of this subtree. The flag is sticky, so a result computed
        // as the request stopped is dropped too, which costs nothing: the
        // request is over.
        if self.token.as_ref().is_some_and(|t| t.is_cancelled()) {
            return Err(Unsupported {
                what: INTERRUPTED,
                loc: loc_of(n),
            });
        }
        if let Some(g) = &self.opts.guard {
            self.check_result(n, g, out.geom.as_ref())?;
        }
        let demand = self.opts.guard.as_ref().map(|_| {
            let mut d = self
                .demand
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let mut own = d.get(&n.index).copied().unwrap_or_default();
            for c in &n.children {
                if let Some(cd) = d.get(&c.index) {
                    own.merge(cd);
                }
            }
            d.insert(n.index, own);
            own
        });
        let msgs = if first {
            Some(Arc::new(out.msgs.clone()))
        } else {
            out.msgs.clear();
            None
        };
        let ids = self.id_table(out.geom.as_ref());
        self.r
            .cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(
                h,
                out.geom.clone(),
                Replay {
                    msgs,
                    pattern,
                    epoch: self.opts.replay.unwrap_or(0),
                    demand,
                    image: false,
                },
                ids,
            );
        Ok(out)
    }

    /// Where each original ID of a result computed in this render came
    /// from: every ID drawn for it is in one of this render's blocks (a
    /// child from the cache was rebased onto them first).
    fn id_table(&self, geom: Option<&Geometry>) -> IdTable {
        let Some(Geometry::Manifold(m)) = geom else {
            return Arc::new([]);
        };
        m.all_ids()
            .into_iter()
            .map(|id| {
                let at = self.owners.partition_point(|o| o.0 <= id);
                IdRef {
                    id,
                    block: at
                        .checked_sub(1)
                        .map(|i| self.owners[i])
                        .filter(|&(first, _, size)| id - first < size)
                        .map(|(first, k, _)| (k, id - first)),
                }
            })
            .collect()
    }

    /// A cached result with its IDs moved onto this render's blocks: each
    /// to the same offset in the block its (subtree key, slot) has now,
    /// which is the ID a fresh render gives it. The output, which follows
    /// the IDs' order, is then the fresh render's. A result whose blocks
    /// are unchanged (a render of the same model, or the part of an edited
    /// one before the edit) is returned as it is.
    fn rebase(&self, h: Key, geom: Option<Geometry>, ids: &IdTable) -> Option<Geometry> {
        let Some(Geometry::Manifold(m)) = &geom else {
            return geom;
        };
        let map: BTreeMap<u32, u32> = ids
            .iter()
            .filter_map(|r| {
                let (k, off) = r.block?;
                let &(first, size) = self.blocks.get(&k)?;
                (off < size && first + off != r.id).then_some((r.id, first + off))
            })
            .collect();
        if map.is_empty() {
            return geom;
        }
        let mut m = ManifoldGeometry::clone(m);
        m.relabel(&map);
        let table: IdTable = ids
            .iter()
            .map(|r| IdRef {
                id: map.get(&r.id).copied().unwrap_or(r.id),
                block: r.block,
            })
            .collect();
        let geom = Some(Geometry::Manifold(Arc::new(m)));
        self.r
            .cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .rebased(h, geom.clone(), table);
        geom
    }

    fn compute(&self, n: &Node, kids: Vec<Out>) -> Result<Out, Unsupported> {
        let leaf = |g: Geometry| {
            Ok(Out {
                geom: Some(g),
                msgs: Vec::new(),
            })
        };
        use eval::limits::Limit;
        let stop = || self.stopped();
        let stopped = || Unsupported {
            what: INTERRUPTED,
            loc: loc_of(n),
        };
        // Fragments of a circle of radius `r`, checked against the
        // fragment limit, and `tris` of them against the triangle limit,
        // before anything is allocated.
        let fragments =
            |disc: &eval::node::Discretizer, r: f64, tris: &dyn Fn(f64) -> f64, what: &str| {
                if self.opts.guard.is_none() || !(r > 0.0 && r.is_finite()) {
                    return Ok(());
                }
                let f = f64::from(fragments::circular_segments(disc, r).unwrap_or(3));
                self.limit(n, Limit::Fragments, f, what)?;
                self.limit(n, Limit::Triangles, tris(f), what)
            };
        match &n.kind {
            NodeKind::Cube { size, center } => leaf(Geometry::PolySet(Arc::new(primitives::cube(
                *size, *center,
            )))),
            NodeKind::Sphere { r, disc } => {
                // (f + 1) / 2 rings of f quads.
                fragments(disc, *r, &|f| f * (f + 1.0), "sphere()")?;
                let s = primitives::sphere_with(*r, disc, &stop).ok_or_else(stopped)?;
                leaf(Geometry::PolySet(Arc::new(s)))
            }
            NodeKind::Cylinder {
                h,
                r1,
                r2,
                center,
                disc,
            } => {
                fragments(disc, r1.max(*r2), &|f| 4.0 * f, "cylinder()")?;
                let c = primitives::cylinder_with(*h, *r1, *r2, *center, disc, &stop)
                    .ok_or_else(stopped)?;
                leaf(Geometry::PolySet(Arc::new(c)))
            }
            NodeKind::Polyhedron { points, faces, .. } => leaf(Geometry::PolySet(Arc::new(
                primitives::polyhedron(points, faces),
            ))),
            NodeKind::Square { size, center } => leaf(leaf_2d(primitives::square(*size, *center))),
            NodeKind::Circle { r, disc } => {
                fragments(disc, *r, &|f| f, "circle()")?;
                let c = primitives::circle2d_with(*r, disc, &stop).ok_or_else(stopped)?;
                leaf(leaf_2d(c))
            }
            NodeKind::Polygon { points, paths, .. } => {
                leaf(leaf_2d(primitives::polygon(points, paths)))
            }
            NodeKind::Root | NodeKind::Group { .. } | NodeKind::Render { .. } => {
                self.apply(n, Op::Union, kids)
            }
            NodeKind::IntersectionFor => self.apply(n, Op::Intersection, kids),
            NodeKind::Csg(CsgOp::Union) => self.apply(n, Op::Union, kids),
            NodeKind::Csg(CsgOp::Intersection) => self.apply(n, Op::Intersection, kids),
            NodeKind::Csg(CsgOp::Difference) => self.apply(n, Op::Difference, kids),
            NodeKind::Fill => self.apply(n, Op::Fill, kids),
            NodeKind::Color { rgba } => {
                let mut out = self.apply(n, Op::Union, kids)?;
                out.geom = out.geom.map(|g| self.color(n, g, Color(*rgba)));
                Ok(out)
            }
            NodeKind::Transform { matrix, .. } => {
                if matrix.iter().flatten().any(|v| !v.is_finite()) {
                    // The children are still evaluated (and report their own
                    // messages) before the transform gives up on them.
                    let mut msgs: Vec<Msg> = kids.into_iter().flat_map(|o| o.msgs).collect();
                    msgs.push(warn(n, "Transformation matrix contains Not-a-Number and/or Infinity - removing object."));
                    return Ok(Out { geom: None, msgs });
                }
                let mut out = self.apply(n, Op::Union, kids)?;
                out.geom = out.geom.map(|g| transform(g, matrix, &mut out.msgs));
                Ok(out)
            }
            NodeKind::Offset {
                delta, join, disc, ..
            } => {
                let (poly, msgs) = self.children_2d_union(n, kids)?;
                if *join == OffsetJoin::Round && poly.is_some() {
                    fragments(disc, delta.abs(), &|f| f, "offset()")?;
                }
                let geom = poly.map(|p| {
                    // "The formula for the number of steps in a full circular
                    // arc is ... Pi / acos(1 - arc_tolerance / abs(delta))"
                    // (`GeometryEvaluator.cc:617-621`): the tolerance that
                    // makes Clipper step like a circle of `|delta|` would.
                    let steps =
                        f64::from(fragments::circular_segments(disc, delta.abs()).unwrap_or(3));
                    let tolerance = delta.abs() * (1.0 - cos_degrees(180.0 / steps));
                    let join = match join {
                        OffsetJoin::Round => clipper::Join::Round,
                        OffsetJoin::Miter => clipper::Join::Miter,
                        OffsetJoin::Square => clipper::Join::Square,
                    };
                    // `OffsetNode::miter_limit`, "fixed high value to disable
                    // chamfers with jtMiter".
                    Geometry::Polygon2d(Arc::new(clipper::offset(
                        &p,
                        *delta,
                        join,
                        1_000_000.0,
                        tolerance,
                    )))
                });
                Ok(Out { geom, msgs })
            }
            NodeKind::LinearExtrude(e) => {
                let (poly, msgs) = self.children_2d_union(n, kids)?;
                let geom = match poly {
                    Some(p) => {
                        if self.opts.guard.is_some() && e.height[2] > 0.0 {
                            let slices = f64::from(extrude::num_slices(e, &p));
                            let ring: usize = p.outlines.iter().map(|o| o.vertices.len()).sum();
                            self.limit(n, Limit::Slices, slices, "linear_extrude()")?;
                            self.limit(
                                n,
                                Limit::Triangles,
                                2.0 * slices * ring as f64,
                                "linear_extrude()",
                            )?;
                        }
                        let ps = extrude::linear_extrude_with(e, &p, &stop).ok_or_else(stopped)?;
                        Some(Geometry::PolySet(Arc::new(ps)))
                    }
                    None => None,
                };
                Ok(Out { geom, msgs })
            }
            NodeKind::RotateExtrude {
                angle, start, disc, ..
            } => {
                let (poly, mut msgs) = self.children_2d_union(n, kids)?;
                if let Some(p) = &poly
                    && self.opts.guard.is_some()
                    && *angle != 0.0
                {
                    let f = f64::from(extrude::rotate_sections(*angle, disc, p));
                    let ring: usize = p.outlines.iter().map(|o| o.vertices.len()).sum();
                    self.limit(n, Limit::Fragments, f, "rotate_extrude()")?;
                    self.limit(
                        n,
                        Limit::Triangles,
                        2.0 * f * ring as f64,
                        "rotate_extrude()",
                    )?;
                }
                let rotated = match poly {
                    Some(p) => Some(
                        extrude::rotate_extrude_with(*angle, *start, disc, &p, &stop)
                            .ok_or_else(stopped)?,
                    ),
                    None => None,
                };
                let geom = match rotated {
                    Some(Ok(ps)) => ps.map(|ps| Geometry::PolySet(Arc::new(ps))),
                    Some(Err(text)) => {
                        msgs.push(Msg {
                            severity: Some(Severity::Error),
                            text,
                            loc: None,
                        });
                        None
                    }
                    None => None,
                };
                Ok(Out { geom, msgs })
            }
            NodeKind::Projection { cut, .. } => self.projection(n, *cut, kids),
            NodeKind::Minkowski { .. } => self.minkowski(n, kids),
            NodeKind::Hull => self.hull(n, kids),
            NodeKind::Resize {
                newsize, autosize, ..
            } => {
                let mut out = self.apply(n, Op::Union, kids)?;
                out.geom = out
                    .geom
                    .map(|g| resize(g, *newsize, *autosize, &mut out.msgs));
                Ok(out)
            }
            NodeKind::Surface {
                file,
                center,
                invert,
                ..
            } => Ok(self.surface(n, file, *center, *invert)),
            NodeKind::Part { name } => {
                let mut out = self.apply(n, Op::Union, kids)?;
                out.geom = out.geom.map(|g| self.part(n, g, name, &mut out.msgs));
                Ok(out)
            }
            NodeKind::Import(i) => Ok(self.import(n, i)),
            NodeKind::Text(t) => Ok(self.text(n, t)),
        }
    }

    /// Messages from a reader, located at the node when OpenSCAD logs them
    /// with the call's location.
    fn read_msgs(n: &Node, msgs: Vec<io::Message>) -> Vec<Msg> {
        msgs.into_iter()
            .map(|m| Msg {
                severity: m.severity,
                text: m.text,
                // The readers log with an empty document path, so the
                // file prints relative to the working directory
                // (`import_stl.cc:201` and the like), where messages of
                // the geometry evaluator use the main file's directory.
                loc: if m.located {
                    loc_of(n).map(|l| MsgLoc {
                        base: PathBase::WorkingDir,
                        ..l
                    })
                } else {
                    None
                },
            })
            .collect()
    }

    fn import(&self, n: &Node, i: &eval::node::Import) -> Out {
        let line = loc_of(n).map_or(0, |l| l.line);
        let union = |meshes: Vec<PolySet>| -> PolySet {
            // `ManifoldUtils::applyOperator3DManifold(children, UNION)`, then
            // `getGeometryAsPolySet`. Each conversion takes fresh IDs; they
            // come from this node's own block, in order, so the result does
            // not depend on scheduling.
            let ids = Seq {
                block: self.block(n, OWN),
                used: std::cell::Cell::new(0),
            };
            let mut parts = Vec::with_capacity(meshes.len());
            let mut w = Vec::new();
            let mut e = Vec::new();
            for ps in &meshes {
                let m = ManifoldGeometry::from_polyset(ps, &ids, &mut w, &mut e);
                if !m.is_empty() {
                    parts.push(m);
                }
            }
            ManifoldGeometry::batch_until(Op::Union.manifold(), parts, self.token.as_ref())
                .map(|m| m.to_polyset(&self.opts.scheme))
                .unwrap_or_default()
        };
        let (geom, msgs) = crate::import::import(self.opts, i, line, &union);
        Out {
            geom: Some(geom),
            msgs: Self::read_msgs(n, msgs),
        }
    }

    /// `GeometryEvaluator::visit(TextNode)`: the glyphs, each a sanitized
    /// polygon whose contours keep the font's winding, unioned with the
    /// non-zero rule (`ClipperUtils::apply(polygons, Union)`), which fills
    /// overlapping contours and combining marks rather than cutting holes.
    fn text(&self, n: &Node, t: &eval::node::Text) -> Out {
        let (script, direction) = eval::text_props::resolve(t);
        let segments = text::segments_for(fragments::circular_segments(&t.disc, t.size));
        let r = text::render(
            &self.opts.fonts,
            &text::Params {
                text: &t.text,
                size: t.size,
                spacing: t.spacing,
                font: &t.font,
                direction,
                language: &t.language,
                script: &script,
                halign: &t.halign,
                valign: &t.valign,
                segments,
            },
        );
        let msgs = r
            .messages
            .into_iter()
            .map(|m| match m.level {
                text::Level::FontWarning => Msg {
                    severity: None,
                    text: format!("FONT-WARNING: {}", m.text),
                    loc: None,
                },
                text::Level::Warning => warn(n, &m.text),
            })
            .collect();
        let polys: Vec<Polygon2d> = r
            .glyphs
            .into_iter()
            .map(|g| Polygon2d {
                outlines: g
                    .into_iter()
                    .map(|vertices| crate::polygon2d::Outline {
                        vertices,
                        positive: true,
                    })
                    .collect(),
                sanitized: true,
            })
            .collect();
        let refs: Vec<Option<&Polygon2d>> = polys.iter().map(Some).collect();
        Out {
            geom: Some(Geometry::Polygon2d(Arc::new(clipper::apply(
                &refs,
                clipper::Op2::Union,
            )))),
            msgs,
        }
    }

    fn surface(&self, n: &Node, file: &str, center: bool, invert: bool) -> Out {
        let (geom, msgs) = crate::import::surface(self.opts, file, center, invert);
        Out {
            geom: Some(geom),
            msgs: Self::read_msgs(n, msgs),
        }
    }

    /// A `part()`'s union, made a solid whose faces carry the part's name
    /// (`ManifoldGeometry::tag_part`). A mesh is converted here rather
    /// than at the first boolean above, because that conversion would draw
    /// IDs no part claims. 2D parts stay as they are: parts are tracked in
    /// 3D only.
    fn part(&self, n: &Node, g: Geometry, name: &str, msgs: &mut Vec<Msg>) -> Geometry {
        let name: Arc<str> = Arc::from(name);
        match g {
            Geometry::PolySet(_) => match self.to_manifold(n, 0, g, msgs) {
                Some(mut m) => {
                    m.claim_part(&name);
                    Geometry::Manifold(Arc::new(m))
                }
                None => Geometry::Manifold(Arc::new(ManifoldGeometry::default())),
            },
            Geometry::Manifold(m) => {
                let mut m = Arc::unwrap_or_clone(m);
                m.tag_part(&name, &self.block(n, OWN));
                Geometry::Manifold(Arc::new(m))
            }
            g @ Geometry::Polygon2d(_) => g,
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
            // `Polygon2d` does not override `Geometry::setColor`, so a colour
            // on 2D geometry is dropped: a render shows 2D in the scheme's
            // colour, and an extrusion of a coloured shape is uncoloured.
            g @ Geometry::Polygon2d(_) => g,
        }
    }

    /// The children's results paired with their nodes, and their messages
    /// in child order.
    fn items<'n>(&self, n: &'n Node, results: Vec<Out>) -> (Items<'n>, Vec<Msg>) {
        let mut msgs = Vec::new();
        let mut items: Items<'n> = Vec::with_capacity(results.len());
        for (c, o) in n.children.iter().zip(results) {
            msgs.extend(o.msgs);
            items.push((c, o.geom));
        }
        (items, msgs)
    }

    /// `isValidDim` over the children (`GeometryEvaluator.cc:114-125`): the
    /// first child with geometry sets the dimension, and the first later
    /// non-empty child of the other dimension warns and ends the scan.
    fn dim(items: &[(&Node, Option<Geometry>)], msgs: &mut Vec<Msg>) -> u32 {
        let mut dim = 0;
        for (c, g) in items {
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
        dim
    }

    /// `applyToChildren` (`GeometryEvaluator.cc:128-139`).
    fn apply(&self, n: &Node, op: Op, kids: Vec<Out>) -> Result<Out, Unsupported> {
        let (items, mut msgs) = self.items(n, kids);
        let geom = match Self::dim(&items, &mut msgs) {
            2 => self.apply_2d(&items, op, &mut msgs),
            3 => self.apply_3d(n, &items, op, &mut msgs),
            _ => None,
        };
        Ok(Out { geom, msgs })
    }

    /// `collectChildren3D` (`GeometryEvaluator.cc:386-411`): one entry per
    /// non-background child as (child index, node, geometry), with 2D
    /// geometry replaced by nothing and a warning.
    fn collect_3d<'n>(
        items: &[(&'n Node, Option<Geometry>)],
        msgs: &mut Vec<Msg>,
    ) -> Vec<(u32, &'n Node, Option<Geometry>)> {
        let mut children = Vec::new();
        for (i, (c, g)) in items.iter().enumerate() {
            let i = i as u32;
            if is_background(c) {
                continue;
            }
            match g {
                Some(g) if g.dimension() == 2 => {
                    msgs.push(warn(c, "Ignoring 2D child object for 3D operation"));
                    children.push((i, *c, None));
                }
                g => children.push((i, *c, g.clone())),
            }
        }
        children
    }

    /// `applyToChildren3D` (`GeometryEvaluator.cc:146-209`) with
    /// `applyOperator3DManifold` (`manifold-applyops.cc`).
    fn apply_3d(
        &self,
        n: &Node,
        items: &[(&Node, Option<Geometry>)],
        op: Op,
        msgs: &mut Vec<Msg>,
    ) -> Option<Geometry> {
        let mut children = Self::collect_3d(items, msgs);
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
                let Some(m) = g
                    .and_then(|g| self.to_manifold(n, i, g, msgs))
                    .filter(|m| !m.is_empty())
                else {
                    continue;
                };
                if first.is_none() {
                    first = Some(m);
                } else {
                    msgs.push(Msg {
                        severity: Some(Severity::Error),
                        text: "Unsupported CGAL operator: 5".into(),
                        loc: None,
                    });
                }
            }
            return first.map(|m| Geometry::Manifold(Arc::new(m)));
        }
        let children: Vec<(u32, &Node, Option<Geometry>)> = if op == Op::Union {
            let actual: Vec<_> = children
                .into_iter()
                .filter(|(_, _, g)| g.as_ref().is_some_and(|g| !g.is_empty()))
                .collect();
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
        ManifoldGeometry::batch_until(op.manifold(), parts, self.token.as_ref())
            .map(|m| Geometry::Manifold(Arc::new(m)))
    }

    /// `createManifoldFromGeometry`, for child `slot` of `n`. OpenSCAD
    /// reserves fresh IDs for every conversion, so the same mesh converted
    /// under two different parents gets two sets of IDs (and a sphere cut
    /// out in one place is not painted as a cut face in another).
    fn to_manifold(
        &self,
        n: &Node,
        slot: u32,
        g: Geometry,
        msgs: &mut Vec<Msg>,
    ) -> Option<ManifoldGeometry> {
        match g {
            Geometry::Manifold(m) => Some(Arc::unwrap_or_clone(m)),
            Geometry::PolySet(ps) => {
                let mut w = Vec::new();
                let mut e = Vec::new();
                let m = ManifoldGeometry::from_polyset(&ps, &self.block(n, slot), &mut w, &mut e);
                msgs.extend(w.into_iter().map(|t| Msg {
                    severity: Some(Severity::Warning),
                    text: t,
                    loc: None,
                }));
                msgs.extend(e.into_iter().map(|t| Msg {
                    severity: Some(Severity::Error),
                    text: t,
                    loc: None,
                }));
                Some(m)
            }
            Geometry::Polygon2d(_) => None,
        }
    }

    /// `collectChildren2D` (`GeometryEvaluator.cc:302-336`): one entry per
    /// non-background child, `None` for nothing, empty or 3D (which warns).
    fn collect_2d(
        &self,
        items: &[(&Node, Option<Geometry>)],
        msgs: &mut Vec<Msg>,
    ) -> Vec<Option<Arc<Polygon2d>>> {
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
    fn apply_2d(
        &self,
        items: &[(&Node, Option<Geometry>)],
        op: Op,
        msgs: &mut Vec<Msg>,
    ) -> Option<Geometry> {
        let children = self.collect_2d(items, msgs);
        let refs: Vec<Option<&Polygon2d>> = children.iter().map(|c| c.as_deref()).collect();
        if op == Op::Fill {
            return Some(Geometry::Polygon2d(Arc::new(clipper::fill(&refs))));
        }
        match children.len() {
            0 => None,
            1 => children
                .into_iter()
                .next()
                .flatten()
                .map(Geometry::Polygon2d),
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
    fn children_2d_union(
        &self,
        n: &Node,
        kids: Vec<Out>,
    ) -> Result<(Option<Polygon2d>, Vec<Msg>), Unsupported> {
        let (items, mut msgs) = self.items(n, kids);
        let geom = self.apply_2d(&items, Op::Union, &mut msgs);
        let poly = match geom {
            Some(Geometry::Polygon2d(p)) => Some(Arc::unwrap_or_clone(p)),
            _ => None,
        };
        Ok((poly, msgs))
    }

    /// `hull()`: `applyHull2D`, or `applyHull3D` for the Manifold backend,
    /// which hulls even a single child (`GeometryEvaluator.cc:150-154`).
    fn hull(&self, n: &Node, kids: Vec<Out>) -> Result<Out, Unsupported> {
        let (items, mut msgs) = self.items(n, kids);
        let geom = match Self::dim(&items, &mut msgs) {
            2 => {
                let children = self.collect_2d(&items, &mut msgs);
                let refs: Vec<Option<&Polygon2d>> = children.iter().map(|c| c.as_deref()).collect();
                Some(Geometry::Polygon2d(Arc::new(hull::hull_2d(&refs))))
            }
            3 => {
                let children: Vec<Geometry> = Self::collect_3d(&items, &mut msgs)
                    .into_iter()
                    .filter_map(|(_, _, g)| g)
                    .collect();
                let mut points = Vec::new();
                hull::hull_points(&children, &mut points);
                // No points: `applyOperator3DManifold` returns null.
                (!points.is_empty()).then(|| {
                    let imp = hull::hull_3d(&points);
                    let id = self.block(n, OWN).reserve(1);
                    Geometry::Manifold(Arc::new(ManifoldGeometry::from_built(imp, id)))
                })
            }
            _ => None,
        };
        Ok(Out { geom, msgs })
    }

    /// `minkowski()`: `applyMinkowski2D`, or the MINKOWSKI case of
    /// `applyToChildren3D` (`GeometryEvaluator.cc:164-174`), where one child
    /// passes through before empty children are dropped, and one non-empty
    /// child passes through after.
    fn minkowski(&self, n: &Node, kids: Vec<Out>) -> Result<Out, Unsupported> {
        let (items, mut msgs) = self.items(n, kids);
        let geom = match Self::dim(&items, &mut msgs) {
            2 => {
                let children = self.collect_2d(&items, &mut msgs);
                if children.is_empty() {
                    None
                } else {
                    let refs: Vec<Option<&Polygon2d>> =
                        children.iter().map(|c| c.as_deref()).collect();
                    minkowski::minkowski_2d(&refs).map(|p| Geometry::Polygon2d(Arc::new(p)))
                }
            }
            3 => {
                let mut children = Self::collect_3d(&items, &mut msgs);
                if children.len() <= 1 {
                    children.pop().and_then(|(_, _, g)| g)
                } else {
                    let actual: Vec<(u32, Geometry)> = children
                        .into_iter()
                        .filter_map(|(i, _, g)| g.filter(|g| !g.is_empty()).map(|g| (i, g)))
                        .collect();
                    match actual.len() {
                        0 => None,
                        1 => actual.into_iter().next().map(|(_, g)| g),
                        _ => {
                            let (slots, geoms): (Vec<u32>, Vec<Geometry>) =
                                actual.into_iter().unzip();
                            let conv = |k: usize| -> Box<dyn IdSource + '_> {
                                Box::new(self.block(n, slots[k]))
                            };
                            let own = Seq {
                                block: self.block(n, OWN),
                                used: std::cell::Cell::new(0),
                            };
                            let mut w = Vec::new();
                            let mut e = Vec::new();
                            let m = minkowski::minkowski_3d(&geoms, &conv, &own, &mut w, &mut e);
                            msgs.extend(w.into_iter().map(|t| Msg {
                                severity: Some(Severity::Warning),
                                text: t,
                                loc: None,
                            }));
                            msgs.extend(e.into_iter().map(|t| Msg {
                                severity: Some(Severity::Error),
                                text: t,
                                loc: None,
                            }));
                            m.map(|m| Geometry::Manifold(Arc::new(m)))
                        }
                    }
                }
            }
            _ => None,
        };
        Ok(Out { geom, msgs })
    }

    /// `projectionCut` / `projectionNoCut` for the Manifold backend
    /// (`GeometryEvaluator.cc:845-907`): union the 3D children, then slice
    /// at z = 0 or take the outline from above, and sanitize. With no 3D
    /// geometry a cut gives nothing and a projection an empty shape.
    fn projection(&self, n: &Node, cut: bool, kids: Vec<Out>) -> Result<Out, Unsupported> {
        let (items, mut msgs) = self.items(n, kids);
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
                    text: "The given mesh is not closed! Unable to convert to CGALNefGeometry."
                        .into(),
                    loc: None,
                });
                return Ok(Out { geom: None, msgs });
            }
            // Each child's faces projected and unioned with Clipper.
            let faces: Vec<Polygon2d> = items
                .iter()
                .filter(|(c, _)| !is_background(c))
                .filter_map(|(_, g)| {
                    g.as_ref()
                        .and_then(|g| crate::export::as_polyset(g, &self.opts.scheme))
                })
                .map(|ps| Polygon2d {
                    outlines: ps
                        .faces
                        .iter()
                        .map(|f| {
                            crate::polygon2d::Outline::new(
                                f.iter()
                                    .map(|&v| {
                                        [ps.vertices[v as usize][0], ps.vertices[v as usize][1]]
                                    })
                                    .collect(),
                            )
                        })
                        .collect(),
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
    Geometry::Polygon2d(Arc::new(if p.sanitized {
        p
    } else {
        clipper::sanitize(&p)
    }))
}

/// `resize()`: the scale `Polygon2d::resize` (`Polygon2d.cc:105-125`) or
/// `GeometryUtils::getResizeTransform` (`GeometryUtils.cc:502-524`) works
/// out from the bounding box, applied as a transform. The two differ: in 2D
/// the largest requested size picks the auto-scale only when it is positive,
/// in 3D the largest one is taken as is. A zero 2D scale (an empty shape,
/// whose Eigen bounding box has size -inf) removes the shape with the
/// transform's warning.
fn resize(g: Geometry, newsize: [f64; 3], autosize: [bool; 3], msgs: &mut Vec<Msg>) -> Geometry {
    let scale = if g.dimension() == 2 {
        let Geometry::Polygon2d(p) = &g else {
            unreachable!("2D geometry is a Polygon2d")
        };
        let size = p.bounds().map_or([f64::NEG_INFINITY; 2], |(lo, hi)| {
            [hi[0] - lo[0], hi[1] - lo[1]]
        });
        // `newsize[1] && newsize[1] > newsize[0]`: a NaN counts as set.
        let maxdim = usize::from(newsize[1] != 0.0 && newsize[1] > newsize[0]);
        let scale: [f64; 2] = std::array::from_fn(|i| {
            if newsize[i] > 0.0 {
                newsize[i] / size[i]
            } else {
                1.0
            }
        });
        let auto = if newsize[maxdim] > 0.0 {
            newsize[maxdim] / size[maxdim]
        } else {
            1.0
        };
        let s: [f64; 2] = std::array::from_fn(|i| {
            if !autosize[i] || newsize[i] > 0.0 {
                scale[i]
            } else {
                auto
            }
        });
        [s[0], s[1], 1.0]
    } else {
        let bounds = match &g {
            // `PolySet::getBoundingBox` covers every vertex.
            Geometry::PolySet(ps) => ps.vertices.first().map(|&v0| {
                ps.vertices.iter().fold((v0, v0), |(lo, hi), v| {
                    (
                        std::array::from_fn(|k| lo[k].min(v[k])),
                        std::array::from_fn(|k| hi[k].max(v[k])),
                    )
                })
            }),
            Geometry::Manifold(m) => m.bounds(),
            Geometry::Polygon2d(_) => None,
        };
        // An empty solid stays empty whatever the scale.
        let Some((lo, hi)) = bounds else { return g };
        let size: [f64; 3] = std::array::from_fn(|i| hi[i] - lo[i]);
        let mut maxdim = 0;
        for i in 1..3 {
            if newsize[i] > newsize[maxdim] {
                maxdim = i;
            }
        }
        let scale: [f64; 3] = std::array::from_fn(|i| {
            if newsize[i] > 0.0 {
                newsize[i] / size[i]
            } else {
                1.0
            }
        });
        let auto = scale[maxdim];
        std::array::from_fn(|i| {
            if !autosize[i] || newsize[i] > 0.0 {
                scale[i]
            } else {
                auto
            }
        })
    };
    let m = [
        [scale[0], 0.0, 0.0, 0.0],
        [0.0, scale[1], 0.0, 0.0],
        [0.0, 0.0, scale[2], 0.0],
        [0.0, 0.0, 0.0, 1.0],
    ];
    transform(g, &m, msgs)
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
                msgs.push(Msg {
                    severity: Some(Severity::Warning),
                    text: w.into(),
                    loc: None,
                });
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
