//! OpenSCAD's preview model: the node tree as a CSG expression over leaf
//! meshes (`CSGTreeEvaluator`, `src/core/CSGTreeEvaluator.cc`), put in
//! sum-of-products form (`CSGTreeNormalizer`,
//! `src/glview/preview/CSGTreeNormalizer.cc`) and split into products
//! (`CSGProducts::import`, `src/core/CSGNode.cc`).
//!
//! OpenSCAD's preview never computes the booleans: OpenCSG draws each
//! product (an intersection of leaves minus a union of leaves) in image
//! space, and a union is just several products drawn into one depth
//! buffer. The renderer here draws the same products, but gets each
//! product's visible surface from real Manifold booleans ([`product_mesh`])
//! with every face coloured by the leaf it came from, so the colours come
//! out as OpenCSG's do (a subtracted leaf's faces in the cut-out colour or
//! its own). A product of one leaf needs no boolean at all, so a union of
//! many objects (the common case) costs nothing beyond its leaves.
//!
//! Which nodes are leaves follows the evaluator: every primitive, import,
//! extrusion, `offset`, `projection`, `text`, `render()`, and the
//! operations CGAL evaluates (`minkowski`, `hull`, `fill`, `resize`). Only
//! groups, `union`/`difference`/`intersection`, transforms and colours are
//! CSG structure. A 2D leaf becomes a slab one unit thick
//! (`polygon2dToPolySet`), which is how previews show 2D shapes.

use std::cell::Cell;
use std::collections::{BTreeMap, HashMap};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use eval::dump::Keys;
use eval::node::{CsgOp, Node, NodeKind};
use lang::diag::Severity;
use manifold_rust::cancel::CancelToken;
use manifold_rust::types::OpType;

use crate::color::{Color, Scheme};
use crate::manifold_geom::{IdSource, ManifoldGeometry};
use crate::polygon2d::Polygon2d;
use crate::polyset::{PolySet, apply};
use crate::{Geometry, Matrix, Msg, RenderOptions, Renderer, Unsupported};

/// An axis-aligned box, `None` when empty (Eigen's null `AlignedBox3d`).
pub type BoundingBox = Option<([f64; 3], [f64; 3])>;

/// `CSGNode::FLAG_BACKGROUND` (`%`).
pub const FLAG_BACKGROUND: u8 = 1;
/// `CSGNode::FLAG_HIGHLIGHT` (`#`).
pub const FLAG_HIGHLIGHT: u8 = 2;

/// The colour of an uncoloured leaf: every component unset.
pub const NO_COLOR: Color = Color([-1.0; 4]);

/// `CSGTreeNormalizer`'s default limit (`RenderSettings::openCSGTermLimit`).
pub const DEFAULT_TERM_LIMIT: usize = 100_000;

/// The most leaves the preview puts through real booleans (the leaves of
/// every product of more than one leaf); past it the whole preview is
/// drawn thrown together, with a warning ([`CsgTree::booleans`]).
///
/// OpenSCAD never computes a product: OpenCSG draws it in image space, so
/// its preview only gives up past `openCSGLimit` elements (100,000, where
/// the GUI switches to thrown together). A boolean costs far more, and
/// grows with its result rather than its leaf count: the Menger sponge
/// example at depth 5 is one product of 14,045 leaves, which OpenSCAD
/// previews in 20 s, but whose boolean is the depth-5 sponge itself:
/// minutes and 12 GB natively, and past the web demo's memory. At depth 4
/// (1,757 leaves) it takes 4 s natively and 30 s in the web demo, and is
/// still drawn.
pub const BOOLEAN_LIMIT: usize = 10_000;

/// One leaf of the CSG expression (`CSGLeaf`).
#[derive(Debug)]
pub struct Leaf {
    /// The leaf's mesh in its own coordinates, or `None` for the empty
    /// set. Faces are polygons (convex ones are kept whole, as OpenSCAD
    /// keeps them for drawing) and may carry colours.
    pub mesh: Option<Arc<PolySet>>,
    /// 2 for a 2D shape drawn as a slab.
    pub dim: u32,
    /// The accumulated transform from the leaf to the model.
    pub matrix: Matrix,
    /// The outermost `color()` above the leaf that set a valid colour
    /// (`CSGTreeEvaluator` keeps the first one it meets on the way down).
    /// [`NO_COLOR`] when none applies.
    pub color: Color,
    /// The node's index, OpenSCAD's `CSGLeaf::index`.
    pub index: usize,
    /// The mesh's bounding box moved by `matrix` (its eight corners).
    pub bbox: BoundingBox,
    /// The leaf's place in the tree: its own link and its ancestors'
    /// (`None` for the empty set). A preview product uses it to compute
    /// a union of negatives that a repeated subtree contributes once.
    pub chain: Option<Arc<Chain>>,
}

/// One node on the way from the top of the tree to a leaf: shared by
/// every leaf below it, so a tree costs one link per node.
#[derive(Debug)]
pub struct Chain {
    /// `Node::index`.
    pub index: usize,
    /// The node's subtree key (`Keys::get`): equal keys, equal subtrees.
    pub key: u128,
    /// The node's position among its parent's children.
    pub pos: u32,
    /// The node's own transform (the identity for anything but a
    /// transform): the subtree's geometry in its parent's coordinates is
    /// this times its children's.
    pub own: Matrix,
    pub parent: Option<Arc<Chain>>,
}

impl Drop for Chain {
    // Iterative, as `TermNode`'s: a recursive drop of a deep chain takes a
    // frame per link, and BOSL2 nests nodes deeply.
    fn drop(&mut self) {
        let mut next = self.parent.take();
        while let Some(p) = next {
            match Arc::try_unwrap(p) {
                Ok(mut c) => next = c.parent.take(),
                Err(_) => break,
            }
        }
    }
}

impl Leaf {
    fn empty() -> Leaf {
        Leaf {
            mesh: None,
            dim: 3,
            matrix: crate::IDENTITY,
            color: NO_COLOR,
            index: 0,
            bbox: None,
            chain: None,
        }
    }

    fn is_empty_set(&self) -> bool {
        self.mesh.as_ref().is_none_or(|m| m.is_empty())
    }
}

/// A leaf with the flags accumulated on the way to it (`CSGChainObject`).
#[derive(Debug, Clone)]
pub struct ChainObject {
    pub leaf: Arc<Leaf>,
    pub flags: u8,
}

/// `CSGProduct`: the intersection of `intersections` minus the union of
/// `subtractions`.
#[derive(Debug, Clone, Default)]
pub struct Product {
    pub intersections: Vec<ChainObject>,
    pub subtractions: Vec<ChainObject>,
}

impl Product {
    /// `CSGProduct::getBoundingBox`: the intersection of the positive
    /// leaves' boxes, or with `throwntogether` the union of every leaf's.
    pub fn bounding_box(&self, throwntogether: bool) -> BoundingBox {
        let mut it = self.intersections.iter();
        let first = it.next()?.leaf.bbox;
        if throwntogether {
            let b = it.fold(first, |a, c| merged(a, c.leaf.bbox));
            self.subtractions
                .iter()
                .fold(b, |a, c| merged(a, c.leaf.bbox))
        } else {
            it.fold(first, |a, c| intersection(a, c.leaf.bbox))
        }
    }
}

/// `CSGProducts`: products drawn into one depth buffer, whose union is the
/// shape.
#[derive(Debug, Clone)]
pub struct Products {
    pub products: Vec<Product>,
}

impl Products {
    fn new() -> Products {
        Products {
            products: vec![Product::default()],
        }
    }

    /// `CSGProducts::getBoundingBox`.
    pub fn bounding_box(&self, throwntogether: bool) -> BoundingBox {
        self.products
            .iter()
            .fold(None, |a, p| merged(a, p.bounding_box(throwntogether)))
    }

    /// `CSGProducts::import`: walk a normalised term, starting a product
    /// at each leaf reached through a union once the current one has
    /// positive leaves.
    fn import(&mut self, term: &Term) {
        #[derive(Clone, Copy, PartialEq)]
        enum Kind {
            Union,
            Intersection,
            Difference,
        }
        let mut current_is_sub = false;
        let mut stack: Vec<(Term, Kind, u8)> = vec![(term.clone(), Kind::Union, 0)];
        while let Some((node, kind, flags)) = stack.pop() {
            let flags = node.flags.get() | flags;
            match &node.kind {
                TermKind::Leaf(leaf) => {
                    let cur = self.products.last().expect("a product is always open");
                    match kind {
                        Kind::Union if !cur.intersections.is_empty() => {
                            self.products.push(Product::default());
                            current_is_sub = false;
                        }
                        Kind::Difference => current_is_sub = true,
                        Kind::Intersection => current_is_sub = false,
                        Kind::Union => {}
                    }
                    let cur = self.products.last_mut().expect("a product is always open");
                    let obj = ChainObject {
                        leaf: leaf.clone(),
                        flags,
                    };
                    if current_is_sub {
                        cur.subtractions.push(obj);
                    } else {
                        cur.intersections.push(obj);
                    }
                }
                TermKind::Op(op, l, r) => {
                    let k = match op {
                        CsgOp::Union => Kind::Union,
                        CsgOp::Intersection => Kind::Intersection,
                        CsgOp::Difference => Kind::Difference,
                    };
                    stack.push((r.clone(), k, flags));
                    stack.push((l.clone(), kind, flags));
                }
            }
        }
    }

    /// Leaves in all products (`CSGProducts::size`).
    pub fn len(&self) -> usize {
        self.products
            .iter()
            .map(|p| p.intersections.len() + p.subtractions.len())
            .sum()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// A node of the CSG expression (`CSGNode`): a leaf or a binary operation,
/// with its box and flags. Terms are shared (normalisation duplicates
/// subtrees), and flags are set on shared terms as OpenSCAD sets them.
#[derive(Debug)]
struct TermNode {
    kind: TermKind,
    flags: Cell<u8>,
    bbox: BoundingBox,
}

#[derive(Debug)]
enum TermKind {
    Leaf(Arc<Leaf>),
    Op(CsgOp, Term, Term),
}

type Term = Rc<TermNode>;

fn leaf_term(leaf: Arc<Leaf>) -> Term {
    let bbox = leaf.bbox;
    Rc::new(TermNode {
        kind: TermKind::Leaf(leaf),
        flags: Cell::new(0),
        bbox,
    })
}

/// `CSGNode::createEmptySet`.
fn empty_set() -> Term {
    leaf_term(Arc::new(Leaf::empty()))
}

impl TermNode {
    fn is_empty_set(&self) -> bool {
        match &self.kind {
            TermKind::Leaf(l) => l.is_empty_set(),
            TermKind::Op(..) => false,
        }
    }
    fn is_leaf(&self) -> bool {
        matches!(self.kind, TermKind::Leaf(_))
    }
    fn has(&self, flag: u8) -> bool {
        self.flags.get() & flag != 0
    }
    fn set(&self, flag: u8) {
        self.flags.set(self.flags.get() | flag);
    }
}

thread_local! {
    /// What [`TermNode`]'s drop leaves in an operand it has taken out:
    /// shared, so taking one costs a reference count, not an allocation.
    static TAKEN: Term = empty_set();
}

/// Terms are chains as long as the model has operands: a union of the
/// children of a `for` of thousands, or a difference of one solid and
/// thousands of holes, which normalisation turns into
/// `((x - a) - b) - ...`. The drop the compiler writes recurses once per
/// link, and freeing the Menger sponge example at depth 5 (a chain of
/// 14,044) overflowed V8's 1 MB stack in the web demo. This one takes the
/// operands only it still owns onto a heap stack and frees them from
/// there, so freeing a term needs the same stack at any length.
impl Drop for TermNode {
    fn drop(&mut self) {
        fn take(slot: &mut Term, pending: &mut Vec<Term>) {
            // A shared operand is not freed here, so it does not recurse.
            if !slot.is_leaf() && Rc::strong_count(slot) == 1 && Rc::weak_count(slot) == 0 {
                pending.push(std::mem::replace(slot, TAKEN.with(Rc::clone)));
            }
        }
        let TermKind::Op(_, l, r) = &mut self.kind else {
            return;
        };
        let mut pending = Vec::new();
        take(l, &mut pending);
        take(r, &mut pending);
        while let Some(mut t) = pending.pop() {
            if let Some(node) = Rc::get_mut(&mut t)
                && let TermKind::Op(_, l, r) = &mut node.kind
            {
                take(l, &mut pending);
                take(r, &mut pending);
            }
            // `t`'s operands are now shared placeholders or terms other
            // owners keep, so its own drop returns at once.
        }
    }
}

fn merged(a: BoundingBox, b: BoundingBox) -> BoundingBox {
    match (a, b) {
        (None, b) => b,
        (a, None) => a,
        (Some((al, ah)), Some((bl, bh))) => Some((
            std::array::from_fn(|k| al[k].min(bl[k])),
            std::array::from_fn(|k| ah[k].max(bh[k])),
        )),
    }
}

/// Eigen's `AlignedBox::intersection`, which may be empty (min > max).
fn intersection(a: BoundingBox, b: BoundingBox) -> BoundingBox {
    let ((al, ah), (bl, bh)) = (a?, b?);
    let lo: [f64; 3] = std::array::from_fn(|k| al[k].max(bl[k]));
    let hi: [f64; 3] = std::array::from_fn(|k| ah[k].min(bh[k]));
    (0..3).all(|k| lo[k] <= hi[k]).then_some((lo, hi))
}

/// `CSGOperation::createCSGNode`, with its pruning: an empty operand
/// decides the result, an intersection of disjoint boxes is empty, and a
/// difference whose negative box misses the positive one drops the
/// negative.
fn create(op: CsgOp, left: Option<Term>, right: Option<Term>) -> Term {
    let (left, right) = match (left, right) {
        (None, None) => return empty_set(),
        (None, Some(r)) => return r,
        (Some(l), None) => return l,
        (Some(l), Some(r)) => (l, r),
    };
    if right.is_empty_set() {
        return if matches!(op, CsgOp::Union | CsgOp::Difference) {
            left
        } else {
            right
        };
    }
    if left.is_empty_set() {
        return if op == CsgOp::Union { right } else { left };
    }
    match op {
        CsgOp::Intersection => {
            if intersection(left.bbox, right.bbox).is_none() {
                return empty_set();
            }
        }
        CsgOp::Difference => {
            if intersection(left.bbox, right.bbox).is_none() {
                return left;
            }
        }
        CsgOp::Union => {}
    }
    // `CSGOperation::initBoundingBox`.
    let bbox = match op {
        CsgOp::Union => merged(left.bbox, right.bbox),
        CsgOp::Intersection => intersection(left.bbox, right.bbox),
        CsgOp::Difference => left.bbox,
    };
    Rc::new(TermNode {
        kind: TermKind::Op(op, left, right),
        flags: Cell::new(0),
        bbox,
    })
}

/// The preview's CSG expression: the model's products, and the highlighted
/// (`#`) and background (`%`) terms drawn over it, each as products.
#[derive(Debug)]
pub struct CsgTree {
    /// `None` when there is nothing to draw, or normalisation gave up.
    pub root: Option<Products>,
    pub highlights: Option<Products>,
    pub background: Option<Products>,
    /// Geometry messages of the leaves and the normaliser's, in order.
    pub messages: Vec<Msg>,
    /// Whether the preview may draw products from booleans: false past
    /// [`BOOLEAN_LIMIT`], when it is drawn thrown together instead.
    pub booleans: bool,
}

impl CsgTree {
    /// `CsgInfo::compile_products` for the tree under `top` (the root, or
    /// the node a `!` selected): each leaf's geometry from `renderer` (so
    /// from its cache when a render already computed it), the terms, and
    /// their products, normalised with at most `limit` operations per term
    /// (OpenSCAD's `--csglimit`).
    pub fn build(
        top: &Node,
        renderer: &Renderer,
        keys: &Keys,
        opts: RenderOptions,
        limit: usize,
    ) -> Result<CsgTree, Unsupported> {
        let scheme = opts.scheme;
        let mut leaves = Vec::new();
        collect_leaves(top, &mut leaves);
        let rendered = renderer.render_many(&leaves, keys, opts)?;
        let mut messages = Vec::new();
        let mut geometry: HashMap<usize, Option<Geometry>> = HashMap::with_capacity(leaves.len());
        for (n, r) in leaves.iter().zip(rendered) {
            messages.extend(r.messages);
            geometry.insert(n.index, r.geometry);
        }
        let mut ev = TreeEvaluator {
            geometry: &geometry,
            keys,
            scheme,
            highlights: Vec::new(),
            background: Vec::new(),
            messages: Vec::new(),
        };
        let state = State {
            matrix: crate::IDENTITY,
            color: NO_COLOR,
            chain: None,
        };
        // `buildCSGTree`.
        let mut root = match ev.visit(top, &state, 0) {
            Visited::Pruned => None,
            Visited::Term(t) => t,
        };
        if let Some(t) = &root {
            if t.has(FLAG_HIGHLIGHT) {
                ev.highlights.push(t.clone());
            }
            if t.has(FLAG_BACKGROUND) {
                ev.background.push(t.clone());
                root = None;
            }
        }
        messages.append(&mut ev.messages);

        let mut normalizer = Normalizer {
            limit,
            count: 0,
            aborted: false,
        };
        // OpenSCAD logs the abort where it happens, so it comes before
        // the empty tree it leaves, and once per abandoned term.
        let abort_msg = || Msg {
            severity: Some(Severity::Warning),
            text: format!(
                "Normalized tree is growing past {limit} elements. Aborting normalization.\n"
            ),
            loc: None,
        };
        let root = root.and_then(|t| {
            let n = normalizer.normalize(&t);
            if normalizer.aborted {
                messages.push(abort_msg());
            }
            if n.is_none() {
                messages.push(Msg {
                    severity: Some(Severity::Warning),
                    text: "CSG normalization resulted in an empty tree".into(),
                    loc: None,
                });
            }
            n.map(|n| {
                let mut p = Products::new();
                p.import(&n);
                p
            })
        });
        let mut compile = |terms: Vec<Term>| -> Option<Products> {
            if terms.is_empty() {
                return None;
            }
            let mut p = Products::new();
            for t in terms {
                // A term normalised to nothing is skipped (`import` of a
                // null term would crash OpenSCAD; it never happens there).
                let n = normalizer.normalize(&t);
                if normalizer.aborted {
                    messages.push(abort_msg());
                }
                if let Some(n) = n {
                    p.import(&n);
                }
            }
            Some(p)
        };
        let highlights = compile(std::mem::take(&mut ev.highlights));
        let background = compile(std::mem::take(&mut ev.background));
        // The leaves a boolean would take: a product of one leaf is drawn
        // as it is.
        let boolean_leaves: usize = [&root, &highlights, &background]
            .into_iter()
            .flatten()
            .flat_map(|p| &p.products)
            .map(|p| p.intersections.len() + p.subtractions.len())
            .filter(|&n| n > 1)
            .sum();
        let booleans = boolean_leaves <= BOOLEAN_LIMIT;
        if !booleans {
            messages.push(Msg {
                severity: Some(Severity::Warning),
                text: format!(
                    "The CSG products have {boolean_leaves} elements to combine, more than the \
                     {BOOLEAN_LIMIT} a preview computes; drawing them thrown together. Render \
                     to see the result."
                ),
                loc: None,
            });
        }
        Ok(CsgTree {
            root,
            highlights,
            background,
            messages,
            booleans,
        })
    }

    /// `OpenCSGRenderer::getBoundingBox` (or with `throwntogether`,
    /// `ThrownTogetherRenderer`'s): what `--viewall` fits.
    pub fn bounding_box(&self, throwntogether: bool) -> BoundingBox {
        [&self.root, &self.highlights, &self.background]
            .into_iter()
            .flatten()
            .fold(None, |a, p| merged(a, p.bounding_box(throwntogether)))
    }
}

/// Which kind of node a node is to the CSG evaluator.
enum Role {
    /// Children combined with an operation (groups, CSG, transforms,
    /// colours).
    Op(CsgOp),
    /// An `AbstractPolyNode` or `render()`: a leaf; its children are
    /// visited but their terms dropped.
    Leaf,
    /// A `CgalAdvNode` (`minkowski`, `hull`, `fill`, `resize`): a leaf
    /// whose highlighted and background children are still drawn.
    AdvLeaf,
}

fn role(n: &Node) -> Role {
    match &n.kind {
        NodeKind::Root
        | NodeKind::Group { .. }
        | NodeKind::Part { .. }
        | NodeKind::Transform { .. }
        | NodeKind::Color { .. } => Role::Op(CsgOp::Union),
        NodeKind::IntersectionFor => Role::Op(CsgOp::Intersection),
        NodeKind::Csg(op) => Role::Op(*op),
        NodeKind::Minkowski { .. } | NodeKind::Hull | NodeKind::Fill | NodeKind::Resize { .. } => {
            Role::AdvLeaf
        }
        _ => Role::Leaf,
    }
}

fn prunes(n: &Node) -> bool {
    matches!(&n.kind, NodeKind::Transform { matrix, .. } if matrix.iter().flatten().any(|v| !v.is_finite()))
}

/// Every node the evaluator asks the geometry evaluator about, children
/// before parents (the traversal's postfix order). The walk keeps its
/// pending nodes on a heap stack, as [`TreeEvaluator::visit`] does.
fn collect_leaves<'n>(top: &'n Node, out: &mut Vec<&'n Node>) {
    // A node, and whether its children are collected already.
    let mut stack: Vec<(&Node, bool)> = vec![(top, false)];
    while let Some((n, done)) = stack.pop() {
        if done {
            if !matches!(role(n), Role::Op(_)) {
                out.push(n);
            }
        } else if !prunes(n) {
            stack.push((n, true));
            stack.extend(n.children.iter().rev().map(|c| (c, false)));
        }
    }
}

#[derive(Clone)]
struct State {
    matrix: Matrix,
    color: Color,
    /// The parent's link.
    chain: Option<Arc<Chain>>,
}

enum Visited {
    /// Not added to the parent (a transform with NaN or infinity).
    Pruned,
    Term(Option<Term>),
}

struct TreeEvaluator<'a> {
    geometry: &'a HashMap<usize, Option<Geometry>>,
    keys: &'a Keys,
    scheme: Scheme,
    highlights: Vec<Term>,
    background: Vec<Term>,
    messages: Vec<Msg>,
}

fn mul(a: &Matrix, b: &Matrix) -> Matrix {
    std::array::from_fn(|r| std::array::from_fn(|c| (0..4).map(|k| a[r][k] * b[k][c]).sum()))
}

impl TreeEvaluator<'_> {
    /// `buildCSGTree`'s walk over the subtree under `top`, the `pos`-th
    /// child of a node whose state is `state`.
    ///
    /// The nodes waiting for their children's terms are kept on a heap
    /// stack rather than recursing per level: a recursive module makes a
    /// tree as deep as the evaluator allows, and the preview walked it on
    /// whatever stack the evaluation had left. The order of everything is
    /// the recursion's: a node is entered (its state, or the warning that
    /// prunes it) before its children, and finished after them.
    fn visit(&mut self, top: &Node, state: &State, pos: u32) -> Visited {
        /// A node whose children are being visited: its state, the index
        /// of the next child, and the terms of those not pruned.
        struct Waiting<'n> {
            n: &'n Node,
            state: State,
            next: usize,
            kids: Vec<Option<Term>>,
        }
        let mut waiting: Vec<Waiting<'_>> = Vec::new();
        let mut enter = Some((top, pos));
        loop {
            if let Some((n, pos)) = enter.take() {
                let parent = waiting.last().map_or(state, |w| &w.state);
                match self.enter(n, parent, pos) {
                    Some(state) => waiting.push(Waiting {
                        n,
                        state,
                        next: 0,
                        kids: Vec::with_capacity(n.children.len()),
                    }),
                    // A pruned node is left out of its parent's terms.
                    None if waiting.is_empty() => return Visited::Pruned,
                    None => {}
                }
            }
            let w = waiting.last_mut().expect("a node is waiting");
            if let Some(c) = w.n.children.get(w.next) {
                enter = Some((c, w.next as u32));
                w.next += 1;
                continue;
            }
            let w = waiting.pop().expect("just looked at it");
            let t = self.finish(w.n, &w.state, w.kids);
            match waiting.last_mut() {
                Some(parent) => parent.kids.push(t),
                None => return Visited::Term(t),
            }
        }
    }

    /// The start of visiting `n`, the `pos`-th child of a node whose state
    /// is `state`: `n`'s own state, or `None` (and the warning) for a
    /// transform that prunes it.
    fn enter(&mut self, n: &Node, state: &State, pos: u32) -> Option<State> {
        let mut state = state.clone();
        let own = match &n.kind {
            NodeKind::Transform { matrix, .. } => *matrix,
            _ => crate::IDENTITY,
        };
        state.chain = Some(Arc::new(Chain {
            index: n.index,
            key: self.keys.get(n),
            pos,
            own,
            parent: state.chain.take(),
        }));
        match &n.kind {
            NodeKind::Transform { matrix, .. } => {
                if prunes(n) {
                    self.messages.push(Msg {
                        severity: Some(Severity::Warning),
                        text: "Transformation matrix contains Not-a-Number and/or Infinity - removing object.".into(),
                        loc: n.origin.as_ref().map(|o| crate::MsgLoc {
                            unit: o.unit,
                            span: o.span,
                            line: o.line,
                            base: lang::diag::PathBase::MainFileDir,
                        }),
                    });
                    return None;
                }
                state.matrix = mul(&state.matrix, matrix);
            }
            // The outermost colour wins: an inner `color()` only applies
            // where no valid colour is set yet.
            NodeKind::Color { rgba } if !state.color.is_valid() => {
                state.color = Color(*rgba);
            }
            _ => {}
        }
        Some(state)
    }

    /// The end of visiting `n`, with its state and the terms of its
    /// children that were not pruned.
    fn finish(&mut self, n: &Node, state: &State, children: Vec<Option<Term>>) -> Option<Term> {
        let (highlight, background) = n
            .origin
            .as_ref()
            .map_or((false, false), |o| (o.tag_highlight, o.tag_background));
        match role(n) {
            Role::Op(op) => self.apply_to_children(children, op, highlight, background),
            Role::Leaf | Role::AdvLeaf => {
                if matches!(role(n), Role::AdvLeaf) {
                    // `applyBackgroundAndHighlight`.
                    for t in children.into_iter().flatten() {
                        if t.has(FLAG_BACKGROUND) {
                            self.background.push(t.clone());
                        }
                        if t.has(FLAG_HIGHLIGHT) {
                            self.highlights.push(t);
                        }
                    }
                }
                let t = match self.geometry.get(&n.index) {
                    Some(Some(g)) => leaf_term(Arc::new(self.leaf(n, g, state))),
                    _ => empty_set(),
                };
                if highlight {
                    t.set(FLAG_HIGHLIGHT);
                }
                if background {
                    t.set(FLAG_BACKGROUND);
                }
                Some(t)
            }
        }
    }

    /// `evaluateCSGNodeFromGeometry`: the leaf's mesh as the preview draws
    /// it.
    fn leaf(&self, n: &Node, g: &Geometry, state: &State) -> Leaf {
        let (mesh, dim) = if g.is_empty() {
            (None, g.dimension())
        } else {
            match g {
                Geometry::Polygon2d(p) => (Some(Arc::new(slab(p))), 2),
                Geometry::PolySet(ps) => {
                    // `evaluateGeometry(node, false)`: faces of a mesh not
                    // known to be convex are split, since OpenGL draws
                    // only convex polygons.
                    let ps = if ps.triangular || ps.convex == Some(true) {
                        ps.clone()
                    } else {
                        Arc::new(ps.tessellate(&mut Vec::new()))
                    };
                    (Some(ps), 3)
                }
                Geometry::Manifold(m) => (Some(Arc::new(m.to_polyset(&self.scheme))), 3),
            }
        };
        let bbox = mesh
            .as_ref()
            .and_then(|m| transformed_box(&state.matrix, &all_vertices_box(m)));
        Leaf {
            mesh,
            dim,
            matrix: state.matrix,
            color: state.color,
            index: n.index,
            bbox,
            chain: state.chain.clone(),
        }
    }

    /// `CSGTreeEvaluator::applyToChildren`, with its handling of `%` and
    /// `#` children: a background child leaves the expression for the
    /// background list; a highlighted one is also drawn in the highlight
    /// colour, and a highlighted operand of a union leaves the expression.
    fn apply_to_children(
        &mut self,
        children: Vec<Option<Term>>,
        op: CsgOp,
        highlight: bool,
        background: bool,
    ) -> Option<Term> {
        if children.is_empty() {
            return Some(empty_set());
        }
        let mut t1: Option<Term> = None;
        for t2 in children {
            let Some(t2) = t2 else { continue };
            let Some(a) = t1.clone() else {
                t1 = Some(t2);
                continue;
            };
            let mut t = if t2.has(FLAG_BACKGROUND) {
                self.background.push(t2.clone());
                a.clone()
            } else if a.has(FLAG_BACKGROUND) {
                self.background.push(a.clone());
                t2.clone()
            } else {
                create(op, Some(a.clone()), Some(t2.clone()))
            };
            let is = |x: &Term, y: &Term| Rc::ptr_eq(x, y);
            match op {
                CsgOp::Difference => {
                    if !is(&t, &a) && a.has(FLAG_HIGHLIGHT) {
                        t.set(FLAG_HIGHLIGHT);
                    } else if !is(&t, &t2) && t2.has(FLAG_HIGHLIGHT) {
                        self.highlights.push(t2.clone());
                    }
                }
                CsgOp::Intersection => {
                    if !t.is_empty_set()
                        && !is(&t, &a)
                        && !is(&t, &t2)
                        && a.has(FLAG_HIGHLIGHT)
                        && t2.has(FLAG_HIGHLIGHT)
                    {
                        t.set(FLAG_HIGHLIGHT);
                    } else {
                        if !is(&t, &a) && a.has(FLAG_HIGHLIGHT) {
                            self.highlights.push(a.clone());
                        }
                        if !is(&t, &t2) && t2.has(FLAG_HIGHLIGHT) {
                            self.highlights.push(t2.clone());
                        }
                    }
                }
                CsgOp::Union => {
                    if !is(&t, &a)
                        && !is(&t, &t2)
                        && a.has(FLAG_HIGHLIGHT)
                        && t2.has(FLAG_HIGHLIGHT)
                    {
                        t.set(FLAG_HIGHLIGHT);
                    } else if !is(&t, &a) && a.has(FLAG_HIGHLIGHT) {
                        self.highlights.push(a.clone());
                        t = t2.clone();
                    } else if !is(&t, &t2) && t2.has(FLAG_HIGHLIGHT) {
                        self.highlights.push(t2.clone());
                        t = a.clone();
                    }
                }
            }
            t1 = Some(t);
        }
        if let Some(t) = &t1 {
            if background {
                t.set(FLAG_BACKGROUND);
            }
            if highlight {
                t.set(FLAG_HIGHLIGHT);
            }
        }
        t1
    }
}

/// `PolySet::getBoundingBox`: every vertex, used or not.
fn all_vertices_box(ps: &PolySet) -> BoundingBox {
    let mut it = ps.vertices.iter();
    let first = *it.next()?;
    Some(it.fold((first, first), |(lo, hi), v| {
        (
            std::array::from_fn(|k| lo[k].min(v[k])),
            std::array::from_fn(|k| hi[k].max(v[k])),
        )
    }))
}

/// `operator*(Transform3d, BoundingBox)`: the box of the eight moved
/// corners.
fn transformed_box(m: &Matrix, b: &BoundingBox) -> BoundingBox {
    let (lo, hi) = (*b)?;
    let mut out: BoundingBox = None;
    for z in [lo[2], hi[2]] {
        for y in [lo[1], hi[1]] {
            for x in [lo[0], hi[0]] {
                let p = apply(m, [x, y, z]);
                out = merged(out, Some((p, p)));
            }
        }
    }
    out
}

/// `polygon2dToPolySet`: a 2D shape as a slab from z = -0.5 to 0.5: the
/// triangulation reversed at the bottom and as it is at the top, and a
/// quad down each outline edge.
pub fn slab(p: &Polygon2d) -> PolySet {
    // `Polygon2d::tessellate` keeps one vertex per outline vertex, in
    // order, so the bottom copy of vertex `i` is `i` and the top copy
    // `n + i`, and the sides can share them: the slab is a closed mesh,
    // which the product booleans need.
    let tri = p.tessellate();
    let n = tri.vertices.len() as u32;
    let mut ps = PolySet {
        convex: Some(p.is_convex()),
        ..Default::default()
    };
    ps.vertices
        .extend(tri.vertices.iter().map(|v| [v[0], v[1], v[2] - 0.5]));
    ps.vertices
        .extend(tri.vertices.iter().map(|v| [v[0], v[1], v[2] + 0.5]));
    for f in &tri.faces {
        ps.faces.push(f.iter().rev().copied().collect());
    }
    for f in &tri.faces {
        ps.faces.push(f.iter().map(|&i| n + i).collect());
    }
    let mut start = 0u32;
    for o in &p.outlines {
        let len = o.vertices.len() as u32;
        for i in 0..len {
            let (a, b) = (start + i, start + (i + 1) % len);
            ps.faces.push(vec![a, b, n + b, n + a]);
        }
        start += len;
    }
    ps
}

/// `CSGTreeNormalizer`: rewrites a term into a union of products
/// (Goldfeather et al., "Near Real-Time CSG Rendering Using Tree
/// Normalization and Geometric Pruning", 1989) with OpenSCAD's rule order,
/// giving up (and yielding nothing) past `limit` operations.
struct Normalizer {
    limit: usize,
    count: usize,
    aborted: bool,
}

fn op_parts(t: &Term) -> Option<(CsgOp, &Term, &Term)> {
    match &t.kind {
        TermKind::Op(op, l, r) => Some((*op, l, r)),
        TermKind::Leaf(_) => None,
    }
}

fn is_union(t: &Term) -> bool {
    matches!(op_parts(t), Some((CsgOp::Union, _, _)))
}

/// The same operation with new children, keeping the node's flags and
/// its box: the C++ assigns the children in place, and a node's box is
/// computed once, when it is created, so later pruning sees the box from
/// before normalisation.
fn with_children(t: &Term, l: Term, r: Term) -> Term {
    let (op, _, _) = op_parts(t).expect("an operation");
    Rc::new(TermNode {
        kind: TermKind::Op(op, l, r),
        flags: Cell::new(t.flags.get()),
        bbox: t.bbox,
    })
}

impl Normalizer {
    fn normalize(&mut self, root: &Term) -> Option<Term> {
        self.aborted = false;
        self.count = 0;
        self.pass(root.clone())
    }

    /// `normalizePass`: rewrite the top until no rule applies, normalise
    /// the left operand, and repeat while the node is not a union and
    /// still has an operation on the right or a union on the left; then
    /// normalise the right operand. `None` once the limit is passed:
    /// OpenSCAD then abandons the whole term.
    ///
    /// The recursion is kept on a heap stack, as OpenSCAD's is: the left
    /// operand of a normalised difference of `n` holes is a chain `n`
    /// long, and recursing down it overflowed V8's 1 MB stack in the web
    /// demo on the Menger sponge at depth 5 (14,043 holes), well inside
    /// the limit. The order of the work, and so the count, is the
    /// recursive one's.
    fn pass(&mut self, root: Term) -> Option<Term> {
        /// A node waiting for the pass over one of its operands.
        enum Waiting {
            /// Its left operand; then it may be rewritten again.
            Left(Term),
            /// Its right operand; then it is done.
            Right(Term),
        }
        enum Step {
            /// Normalise this term.
            Enter(Term),
            /// Rewrite this operation at the top and descend to its left.
            Top(Term),
            /// A normalised term, for the node waiting on it.
            Done(Term),
        }
        let mut waiting: Vec<Waiting> = Vec::new();
        let mut step = Step::Enter(root);
        loop {
            step = match step {
                Step::Enter(node) if node.is_leaf() => Step::Done(node),
                Step::Enter(node) => Step::Top(node),
                Step::Top(mut node) => {
                    while let Some(n) = match_and_replace(&node) {
                        node = n;
                    }
                    self.count += 1;
                    if self.count > self.limit {
                        self.aborted = true;
                        return None;
                    }
                    match op_parts(&node) {
                        None => Step::Done(node),
                        Some((_, l, _)) => {
                            let l = l.clone();
                            waiting.push(Waiting::Left(node));
                            Step::Enter(l)
                        }
                    }
                }
                Step::Done(value) => match waiting.pop() {
                    None => return Some(value),
                    Some(Waiting::Left(node)) => {
                        let (_, _, r) = op_parts(&node).expect("an operation");
                        let node = with_children(&node, value, r.clone());
                        let (_, l, r) = op_parts(&node).expect("an operation");
                        if is_union(&node) || !(!r.is_leaf() || is_union(l)) {
                            let r = r.clone();
                            waiting.push(Waiting::Right(node));
                            Step::Enter(r)
                        } else {
                            Step::Top(node)
                        }
                    }
                    Some(Waiting::Right(node)) => {
                        let (_, l, _) = op_parts(&node).expect("an operation");
                        Step::Done(with_children(&node, l.clone(), value))
                    }
                },
            };
        }
    }
}

/// `CSGTreeNormalizer::match_and_replace`: one rewrite at the top of
/// `node`, if a rule applies.
fn match_and_replace(node: &Term) -> Option<Term> {
    use CsgOp::{Difference as D, Intersection as I, Union as U};
    let (op, left, right) = op_parts(node)?;
    if op == U {
        return None;
    }
    let c = |op, l: &Term, r: &Term| create(op, Some(l.clone()), Some(r.clone()));
    if let Some((rop, y, z)) = op_parts(right) {
        let x = left;
        return Some(match (op, rop) {
            // 1. x - (y + z) -> (x - y) - z
            (D, U) => create(D, Some(c(D, x, y)), Some(z.clone())),
            // 2. x * (y + z) -> (x * y) + (x * z)
            (I, U) => create(U, Some(c(I, x, y)), Some(c(I, x, z))),
            // 3. x - (y * z) -> (x - y) + (x - z)
            (D, I) => create(U, Some(c(D, x, y)), Some(c(D, x, z))),
            // 4. x * (y * z) -> (x * y) * z
            (I, I) => create(I, Some(c(I, x, y)), Some(z.clone())),
            // 5. x - (y - z) -> (x - y) + (x * z)
            (D, D) => create(U, Some(c(D, x, y)), Some(c(I, x, z))),
            // 6. x * (y - z) -> (x * y) - z
            (I, D) => create(D, Some(c(I, x, y)), Some(z.clone())),
            (U, _) => unreachable!("unions return early"),
        });
    }
    if let Some((lop, x, y)) = op_parts(left) {
        let z = right;
        return match (lop, op) {
            // 7. (x - y) * z -> (x * z) - y
            (D, I) => Some(create(D, Some(c(I, x, z)), Some(y.clone()))),
            // 8. (x + y) - z -> (x - z) + (y - z)
            (U, D) => Some(create(U, Some(c(D, x, z)), Some(c(D, y, z)))),
            // 9. (x + y) * z -> (x * z) + (y * z)
            (U, I) => Some(create(U, Some(c(I, x, z)), Some(c(I, y, z)))),
            _ => None,
        };
    }
    None
}

/// One product for [`product_meshes`]: its positive leaves as meshes in
/// model coordinates and its negative leaves placed, every face already in
/// the colour it should be drawn in.
#[derive(Debug, Clone, Default)]
pub struct ProductJob {
    pub positives: Vec<PolySet>,
    pub negatives: Vec<Negative>,
}

/// A positive leaf of a product, by name, for [`product_key`]: the mesh
/// of the leaf whose subtree key is `key` (its geometry in the renderer
/// that built the tree, [`CsgTree::build`]), coloured `color` (every face
/// when `force`, otherwise the faces without a valid colour of their own)
/// and moved by `matrix`. `mesh` is the leaf's mesh as the tree has it;
/// only its size is read.
#[derive(Debug, Clone)]
pub struct Source {
    pub key: u128,
    pub mesh: Arc<PolySet>,
    pub matrix: Matrix,
    pub color: Color,
    pub force: bool,
}

/// The key under which a preview keeps a product's mesh
/// ([`crate::Renderer::keep_product`]): the product whose [`ProductJob`]
/// has `positives` as its positives and `negatives`, each coloured in its
/// tint, as its negatives. The negatives' meshes may be the leaves'
/// uncoloured ones: only their sizes are read. `None` when a negative has
/// no [`Chain`], which names it.
///
/// [`product_meshes`] gives the product a mesh that is a function of what
/// is hashed here, so equal keys mean byte-identical meshes:
///
/// - each leaf's mesh, named by its subtree key as the render cache names
///   it (within one renderer, which is where the products are kept), with
///   its colour and placement. Hashing the meshes themselves would cost
///   much of what a hit saves: the threaded-ring example's 36 channels
///   are 36 MB of vertices. Their vertex and face counts are hashed too,
///   as a cheap check of the name;
/// - for a union of negatives by subtree ([`crate::shared`]), the plan:
///   which unions it computes and how each part is placed. Not the chains
///   themselves, whose top is the root: its key changes with any edit
///   anywhere in the file, and so would every product's;
/// - `scheme`, which colours faces a repair left uncoloured.
///
/// Not hashed, because the mesh does not depend on them: the IDs the
/// conversions draw (a product's run order depends only on their order
/// within its own range), and which other products, computed with it,
/// share a negative (a shared conversion gives what the product's own
/// would, [`SharedNegatives`]).
pub fn product_key(positives: &[Source], negatives: &[Negative], scheme: &Scheme) -> Option<u128> {
    use sha2::{Digest as _, Sha256};
    let mut h = Sha256::new();
    let count = |h: &mut Sha256, x: usize| h.update((x as u64).to_le_bytes());
    let color = |h: &mut Sha256, c: Color| {
        for x in c.0 {
            h.update(x.to_bits().to_le_bytes());
        }
    };
    h.update(b"neoscad preview product\0");
    color(&mut h, scheme.face_front);
    color(&mut h, scheme.face_back);
    count(&mut h, positives.len());
    for s in positives {
        h.update(s.key.to_le_bytes());
        hash_matrix(&mut h, &s.matrix);
        color(&mut h, s.color);
        h.update([u8::from(s.force)]);
        count(&mut h, s.mesh.vertices.len());
        count(&mut h, s.mesh.faces.len());
    }
    count(&mut h, negatives.len());
    for n in negatives {
        h.update(n.chain.as_ref()?.key.to_le_bytes());
        match &n.matrix {
            Some(m) => {
                h.update([1]);
                hash_matrix(&mut h, m);
            }
            None => h.update([0]),
        }
        color(&mut h, n.tint);
        h.update([u8::from(n.slab)]);
        count(&mut h, n.mesh.vertices.len());
        count(&mut h, n.mesh.faces.len());
    }
    match crate::shared::Plan::new(negatives) {
        Some(plan) => {
            h.update([1]);
            plan.hash_into(&mut h);
        }
        None => h.update([0]),
    }
    let d = h.finalize();
    Some(u128::from_le_bytes(d[..16].try_into().expect("32 bytes")))
}

/// A matrix bit for bit into a product key.
pub(crate) fn hash_matrix(h: &mut sha2::Sha256, m: &Matrix) {
    use sha2::Digest as _;
    for x in m.as_flattened() {
        h.update(x.to_bits().to_le_bytes());
    }
}

/// A negative leaf of a product: its mesh where the leaf is, and where
/// it came from in the tree, so copies of a repeated subtree can share
/// one union ([`crate::shared`]).
#[derive(Debug, Clone)]
pub struct Negative {
    /// The mesh, coloured, in the leaf's own coordinates.
    pub mesh: Arc<PolySet>,
    /// Leaf to model coordinates (`None`: `mesh` is in model coordinates).
    pub matrix: Option<Matrix>,
    /// The colour every face was given (negatives are drawn in one).
    pub tint: Color,
    /// A 2D leaf's slab, stretched in z by 1.1 (`matrix` includes it).
    pub slab: bool,
    /// [`Leaf::chain`]; `None` keeps this product's union flat.
    pub chain: Option<Arc<Chain>>,
}

impl From<PolySet> for Negative {
    /// A mesh in model coordinates, with no place in a tree.
    fn from(ps: PolySet) -> Negative {
        Negative {
            mesh: Arc::new(ps),
            matrix: None,
            tint: NO_COLOR,
            slab: false,
            chain: None,
        }
    }
}

/// Whether [`product_meshes`] unions `negatives` by the subtrees they came
/// from, computing a repeated one once, rather than flat.
pub fn shares_subtrees(negatives: &[Negative]) -> bool {
    crate::shared::Plan::new(negatives).is_some()
}

impl Negative {
    /// The mesh in model coordinates.
    pub fn placed(&self) -> PolySet {
        let mut ps = PolySet::clone(&self.mesh);
        if let Some(m) = &self.matrix {
            ps.transform(m);
        }
        ps
    }
}

/// What stops a preview's booleans early: the request's interrupt flag (a
/// cancel or a superseding edit) and its limits' guard (the time and
/// memory limits). The default stops nothing.
///
/// A product under [`BOOLEAN_LIMIT`] can still take minutes: before that
/// limit, the Menger example at depth 5 ran past 235 s and 2.5 GB in the
/// web core under a 60 s time limit, because nothing looked at the clock
/// once the products' booleans began. With a `Stop`, the booleans look
/// between kernel operations, so a request overruns by at most one of them.
#[derive(Clone, Default, Debug)]
pub struct Stop {
    pub interrupt: Option<Arc<AtomicBool>>,
    pub guard: Option<Arc<eval::limits::Guard>>,
}

impl Stop {
    /// Whether to stop: cancelled, a limit passed, or out of time (which
    /// trips the guard's time limit, as the render stage does).
    pub fn stopped(&self) -> bool {
        self.interrupt
            .as_ref()
            .is_some_and(|f| f.load(Ordering::Relaxed))
            || self.guard.as_ref().is_some_and(|g| g.stopped())
    }

    /// The limit that stopped the preview, if one did; `None` after a
    /// stop means it was cancelled.
    pub fn exceeded(&self) -> Option<eval::limits::Exceeded> {
        self.guard.as_ref().and_then(|g| g.exceeded())
    }

    pub(crate) fn check(&self) -> Result<(), Unsupported> {
        if self.stopped() {
            Err(Unsupported::interrupted())
        } else {
            Ok(())
        }
    }

    /// The token for the kernel operations themselves
    /// ([`crate::manifold_geom::kernel_token`]), so one long boolean stops
    /// inside, not only before the next.
    fn token(&self) -> Option<CancelToken> {
        crate::manifold_geom::kernel_token(self.interrupt.as_ref(), self.guard.as_ref())
    }

    /// `geom` held ([`Stop::hold`]), unless its operation was cancelled.
    fn hold_result(&self, geom: ManifoldGeometry) -> Result<Held<'_>, Unsupported> {
        if geom.is_cancelled() {
            return Err(Unsupported::interrupted());
        }
        self.hold(geom)
    }

    /// `geom`, counted against the memory limit while it is alive: the
    /// leaves converted for a product and the partial unions are the
    /// preview's working memory, which the render stage's estimate never
    /// sees. Passing the limit trips the guard and stops the preview.
    pub(crate) fn hold(&self, geom: ManifoldGeometry) -> Result<Held<'_>, Unsupported> {
        self.hold_weighted(geom, crate::evaluate::KERNEL_FACTOR)
    }

    /// [`Stop::hold`] with the solid's estimated size times `factor`
    /// rather than [`crate::evaluate::KERNEL_FACTOR`], which covers the
    /// kernel's working copies while a solid is converted or operated on.
    fn hold_weighted(&self, geom: ManifoldGeometry, factor: u64) -> Result<Held<'_>, Unsupported> {
        let bytes = match &self.guard {
            Some(g) if g.limits().memory.is_some() => {
                factor * crate::evaluate::solid_cost(&geom) as u64
            }
            _ => 0,
        };
        // Built first, so the charge is credited back however this ends.
        let held = Held {
            geom: Some(geom),
            bytes,
            stop: self,
        };
        if let Some(g) = &self.guard
            && bytes > 0
            && g.charge_geometry(bytes, "the preview's booleans").is_err()
        {
            return Err(Unsupported::interrupted());
        }
        Ok(held)
    }
}

/// A solid charged to the memory limit until it is dropped.
pub(crate) struct Held<'s> {
    geom: Option<ManifoldGeometry>,
    bytes: u64,
    stop: &'s Stop,
}

impl Held<'_> {
    /// The solid, still held.
    pub(crate) fn get(&self) -> Option<&ManifoldGeometry> {
        self.geom.as_ref()
    }

    /// The solid, for an operation; the charge stays until `self` drops,
    /// so an operation's operands count while it runs.
    pub(crate) fn take(&mut self) -> ManifoldGeometry {
        self.geom.take().unwrap_or_default()
    }
}

impl Drop for Held<'_> {
    fn drop(&mut self) {
        if let Some(g) = &self.stop.guard
            && self.bytes > 0
        {
            g.credit_geometry(self.bytes);
        }
    }
}

/// The visible solid of each product: the intersection of its positives
/// minus the union of its negatives, as a triangle mesh whose faces keep
/// the colour of the leaf face they came from (Manifold's original IDs
/// carry them through the booleans). Products are solved in parallel;
/// every conversion's IDs come from a range reserved in product order
/// beforehand, so the meshes are the same at any thread count. `scheme`
/// colours only faces that lost their colour to a mesh repair.
pub fn product_meshes(jobs: Vec<ProductJob>, scheme: &Scheme) -> Vec<Option<PolySet>> {
    // Nothing sets a default `Stop`, so this never stops.
    product_meshes_until(jobs, scheme, &Stop::default()).unwrap_or_default()
}

/// [`product_meshes`] that gives up with [`Unsupported::interrupted`] once
/// `stop` says so (checked before each kernel operation), leaving the
/// reason on `stop`'s guard ([`Stop::exceeded`]). The meshes it does
/// return are [`product_meshes`]'s.
pub fn product_meshes_until(
    jobs: Vec<ProductJob>,
    scheme: &Scheme,
    stop: &Stop,
) -> Result<Vec<Option<PolySet>>, Unsupported> {
    // A conversion takes one ID per colour group and one for a repair.
    let firsts: Vec<u32> = jobs
        .iter()
        .map(|j| {
            let n: u32 = j.positives.iter().map(need).sum::<u32>()
                + j.negatives.iter().map(|n| need(&n.mesh)).sum::<u32>();
            manifold_rust::manifold::Manifold::reserve_ids(n.max(1))
        })
        .collect();
    let token = stop.token();
    let shared = SharedNegatives::new(&jobs, stop)?;
    let solve = |(i, (job, first)): (usize, (ProductJob, u32))| {
        product_mesh(job, i, first, scheme, &shared, stop, token.as_ref())
    };
    let work: Vec<(usize, (ProductJob, u32))> = jobs.into_iter().zip(firsts).enumerate().collect();
    #[cfg(all(feature = "parallel", not(target_arch = "wasm32")))]
    {
        use rayon::prelude::*;
        work.into_par_iter().map(solve).collect()
    }
    #[cfg(not(all(feature = "parallel", not(target_arch = "wasm32"))))]
    work.into_iter().map(solve).collect()
}

/// The IDs converting `ps` takes: one per colour group and one for a
/// repair.
fn need(ps: &PolySet) -> u32 {
    let mut colours: Vec<[u32; 4]> = ps
        .color_indices
        .iter()
        .filter_map(|&ci| usize::try_from(ci).ok())
        .filter_map(|ci| ps.colors.get(ci))
        .map(Color::key)
        .collect();
    colours.sort_unstable();
    colours.dedup();
    colours.len() as u32 + 2
}

/// IDs handed out in order from a reserved range.
pub(crate) struct Range(pub(crate) Cell<u32>);

impl IdSource for Range {
    fn reserve(&self, count: u32) -> u32 {
        let first = self.0.get();
        self.0.set(first + count);
        first
    }
}

/// The one colour every face of `ps` has (`None`: no colour), or `None`
/// when its faces differ. Colours are compared as a conversion groups
/// them ([`Color::key`]).
fn uniform_color(ps: &PolySet) -> Option<Option<Color>> {
    let color = |i: usize| {
        let ci = ps.color_indices.get(i).copied().unwrap_or(-1);
        usize::try_from(ci).ok().and_then(|ci| ps.colors.get(ci))
    };
    let first = color(0);
    let key = first.map(Color::key);
    (1..ps.faces.len())
        .all(|i| color(i).map(Color::key) == key)
        .then_some(first.copied())
}

/// A matrix bit for bit, for comparing and hashing.
fn matrix_bits(m: &Option<Matrix>) -> Option<[[u64; 4]; 4]> {
    m.map(|m| m.map(|row| row.map(f64::to_bits)))
}

/// Whether two negatives are the same solid in the same place, whatever
/// their colours: what a conversion's geometry depends on.
fn same_solid(a: &Negative, b: &Negative) -> bool {
    matrix_bits(&a.matrix) == matrix_bits(&b.matrix) && a.mesh.same_shape(&b.mesh)
}

/// Negatives that more than one product subtracts by a flat union,
/// converted once each, and which of them each product's negatives are.
///
/// One solid subtracted from many is common: a `difference()` whose first
/// child is a union (a `for` of pieces) normalises to one product per
/// piece, each minus every later child. The threaded-ring example is 36
/// coloured wedges minus one 39,000-vertex channel. The channel is
/// evaluated once per wedge and takes each wedge's colour, so its copies
/// are equal meshes in different colours, not one mesh. Converting it 36
/// times was 28 ms each natively and 60% of the preview's booleans; in the
/// web core, which runs the products one after another, it was over a
/// second of the time the preview took after it said "Previewed in".
///
/// Copies are matched by content: the same vertices, faces and
/// placement, bit for bit, with every face in one colour (a subtracted
/// leaf is drawn in one, so all are). A product takes a copy of the shared
/// solid, moves its IDs onto those it would have drawn converting the mesh
/// itself (`ManifoldGeometry::relabel`, as a render's cached subtree is
/// rebased) and gives it its own colour. Its result, which Manifold orders
/// by original ID, is then what converting its own mesh gave, at any
/// thread count.
struct SharedNegatives<'s> {
    /// Each shared solid, with the IDs its conversion took (`0..count`).
    solids: Vec<(Held<'s>, u32)>,
    /// Per job, per negative: the shared solid it is a copy of.
    of: Vec<Vec<Option<usize>>>,
}

impl<'s> SharedNegatives<'s> {
    fn new(jobs: &[ProductJob], stop: &'s Stop) -> Result<SharedNegatives<'s>, Unsupported> {
        // Classes of equal solids: a representative, whether its faces
        // are coloured (an uncoloured conversion has no colour to
        // replace), and how many negatives are copies of it.
        struct Class<'j> {
            rep: &'j Negative,
            colored: bool,
            uses: usize,
        }
        let mut classes: Vec<Class<'_>> = Vec::new();
        let mut by_hash: HashMap<u64, Vec<usize>> = HashMap::new();
        let mut of: Vec<Vec<Option<usize>>> = Vec::with_capacity(jobs.len());
        for job in jobs {
            // Only the flat unions convert each negative where it is; a
            // product with a plan (`crate::shared`) converts its own.
            if crate::shared::Plan::new(&job.negatives).is_some() {
                of.push(vec![None; job.negatives.len()]);
                continue;
            }
            let mut mine = Vec::with_capacity(job.negatives.len());
            for n in &job.negatives {
                let Some(color) = uniform_color(&n.mesh) else {
                    mine.push(None);
                    continue;
                };
                let colored = color.is_some();
                // Candidates by shape; placements are compared after.
                let same = by_hash.entry(n.mesh.shape_hash()).or_default();
                let found = same
                    .iter()
                    .copied()
                    .find(|&c| classes[c].colored == colored && same_solid(classes[c].rep, n));
                let c = found.unwrap_or_else(|| {
                    classes.push(Class {
                        rep: n,
                        colored,
                        uses: 0,
                    });
                    same.push(classes.len() - 1);
                    classes.len() - 1
                });
                classes[c].uses += 1;
                mine.push(Some(c));
            }
            of.push(mine);
        }
        // Only solids used more than once are converted here.
        let mut index = vec![None; classes.len()];
        let mut repeated = Vec::new();
        for (c, class) in classes.iter().enumerate() {
            if class.uses > 1 {
                index[c] = Some(repeated.len());
                repeated.push(class.rep);
            }
        }
        for mine in &mut of {
            for s in mine.iter_mut() {
                *s = s.and_then(|c| index[c]);
            }
        }
        let convert = |n: &&Negative| {
            stop.check()?;
            let ids = Range(Cell::new(0));
            let (mut warnings, mut errors) = (Vec::new(), Vec::new());
            let geom =
                ManifoldGeometry::from_polyset(&n.placed(), &ids, &mut warnings, &mut errors);
            // Charged as the finished solid it is: the kernel's working
            // memory is charged on each product's copy, as it was on the
            // product's own conversion.
            Ok((stop.hold_weighted(geom, 1)?, ids.0.get()))
        };
        #[cfg(all(feature = "parallel", not(target_arch = "wasm32")))]
        let solids = {
            use rayon::prelude::*;
            repeated
                .par_iter()
                .map(convert)
                .collect::<Result<_, Unsupported>>()?
        };
        #[cfg(not(all(feature = "parallel", not(target_arch = "wasm32"))))]
        let solids = repeated
            .iter()
            .map(convert)
            .collect::<Result<_, Unsupported>>()?;
        Ok(SharedNegatives { solids, of })
    }

    /// Negative `k` of job `job`, converted with IDs from `ids`, if it is
    /// a copy of a shared solid.
    fn get(
        &self,
        job: usize,
        k: usize,
        negative: &Negative,
        ids: &Range,
    ) -> Option<ManifoldGeometry> {
        let (held, count) = &self.solids[self.of[job][k]?];
        let first = ids.reserve(*count);
        let mut geom = held.get()?.clone();
        let map: BTreeMap<u32, u32> = (0..*count).map(|i| (i, first + i)).collect();
        geom.relabel(&map);
        if let Some(Some(c)) = uniform_color(&negative.mesh) {
            geom.recolor_uniform(c);
        }
        Some(geom)
    }
}

fn product_mesh(
    job: ProductJob,
    index: usize,
    first: u32,
    scheme: &Scheme,
    shared: &SharedNegatives<'_>,
    stop: &Stop,
    token: Option<&CancelToken>,
) -> Result<Option<PolySet>, Unsupported> {
    stop.check()?;
    let ids = Range(Cell::new(first));
    let mut warnings = Vec::new();
    let polled = || {
        // A conversion is cheap next to a boolean; the clock is read
        // every 1,024th (`Guard::poll`).
        if stop
            .interrupt
            .as_ref()
            .is_some_and(|f| f.load(Ordering::Relaxed))
            || stop.guard.as_ref().is_some_and(|g| g.poll())
        {
            return Err(Unsupported::interrupted());
        }
        Ok(())
    };
    let mut convert = |ps: &PolySet| {
        polled()?;
        let mut errors = Vec::new();
        stop.hold(ManifoldGeometry::from_polyset(
            ps,
            &ids,
            &mut warnings,
            &mut errors,
        ))
    };
    let positives = job
        .positives
        .iter()
        .map(&mut convert)
        .collect::<Result<Vec<_>, _>>()?;
    // Copies of a repeated subtree (the Menger sponge's 1,755 negatives
    // are 585 leaves of one subtree, three times) are unioned once and
    // moved, as a render reuses its cached subtree; anything else is one
    // flat union, as before.
    let negatives = match crate::shared::Plan::new(&job.negatives) {
        Some(plan) => {
            // Within the product's range, after the positives: a union
            // converts only its own meshes, so the copies take none.
            let firsts: Vec<u32> = plan
                .needs(&job.negatives, need)
                .into_iter()
                .map(|n| ids.reserve(n))
                .collect();
            plan.union(&job.negatives, &firsts, stop, token)?
                .into_iter()
                .collect()
        }

        None => job
            .negatives
            .iter()
            .enumerate()
            .map(|(k, n)| match shared.get(index, k, n, &ids) {
                Some(geom) => {
                    polled()?;
                    stop.hold(geom)
                }
                None => convert(&n.placed()),
            })
            .collect::<Result<Vec<_>, _>>()?,
    };
    let Some(mut pos) = batch(OpType::Intersect, positives, stop, token)? else {
        return Ok(None);
    };
    let solid = match union_tree(negatives, stop, token)? {
        None => pos,
        Some(mut neg) => {
            stop.check()?;
            let d = pos
                .take()
                .boolean_until(&neg.take(), OpType::Subtract, token);
            stop.hold_result(d)?
        }
    };
    let solid = solid.geom.as_ref().filter(|s| !s.is_empty());
    Ok(solid.map(|s| s.to_polyset(scheme)))
}

/// [`ManifoldGeometry::batch`] over held solids, after a check.
pub(crate) fn batch<'s>(
    op: OpType,
    mut parts: Vec<Held<'s>>,
    stop: &'s Stop,
    token: Option<&CancelToken>,
) -> Result<Option<Held<'s>>, Unsupported> {
    if parts.len() == 1 {
        return Ok(parts.pop());
    }
    stop.check()?;
    let geoms = parts.iter_mut().map(Held::take).collect();
    ManifoldGeometry::batch_until(op, geoms, token)
        .map(|g| stop.hold_result(g))
        .transpose()
}

/// Operands a product's union of negatives handles in one batch; more are
/// split in two halves, unioned in parallel, and joined.
const UNION_LEAF: usize = 16;

/// The union of `parts` by a tree of fixed shape (halves, down to
/// [`UNION_LEAF`] operands), the halves in parallel. A product can hold
/// hundreds of negatives (a difference of one solid and a `for` loop of
/// holes normalises to one), and one batch unions them with Manifold's
/// pairwise rounds one after another, where a render unions each subtree
/// of the model in parallel. The tree's shape depends only on the count,
/// so the result does not depend on the thread count. `stop` is checked
/// before every batch and join.
pub(crate) fn union_tree<'s>(
    mut parts: Vec<Held<'s>>,
    stop: &'s Stop,
    token: Option<&CancelToken>,
) -> Result<Option<Held<'s>>, Unsupported> {
    if parts.len() <= UNION_LEAF {
        return batch(OpType::Add, parts, stop, token);
    }
    let right = parts.split_off(parts.len() / 2);
    #[cfg(all(feature = "parallel", not(target_arch = "wasm32")))]
    let (a, b) = rayon::join(
        || union_tree(parts, stop, token),
        || union_tree(right, stop, token),
    );
    #[cfg(not(all(feature = "parallel", not(target_arch = "wasm32"))))]
    let (a, b) = (
        union_tree(parts, stop, token),
        union_tree(right, stop, token),
    );
    match (a?, b?) {
        (Some(mut a), Some(mut b)) => {
            stop.check()?;
            let u = a.take().boolean_until(&b.take(), OpType::Add, token);
            Ok(Some(stop.hold_result(u)?))
        }

        (a, b) => Ok(a.or(b)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cube_leaf(lo: f64, hi: f64) -> Term {
        let ps = crate::primitives::cube([hi - lo; 3], false);
        let m: Matrix = [
            [1.0, 0.0, 0.0, lo],
            [0.0, 1.0, 0.0, lo],
            [0.0, 0.0, 1.0, lo],
            [0.0, 0.0, 0.0, 1.0],
        ];
        let bbox = transformed_box(&m, &all_vertices_box(&ps));
        leaf_term(Arc::new(Leaf {
            mesh: Some(Arc::new(ps)),
            dim: 3,
            matrix: m,
            color: NO_COLOR,
            index: 0,
            bbox,
            chain: None,
        }))
    }

    fn products(t: &Term) -> Vec<(usize, usize)> {
        let mut n = Normalizer {
            limit: DEFAULT_TERM_LIMIT,
            count: 0,
            aborted: false,
        };
        let t = n.normalize(t).unwrap();
        let mut p = Products::new();
        p.import(&t);
        p.products
            .iter()
            .map(|p| (p.intersections.len(), p.subtractions.len()))
            .collect()
    }

    #[test]
    fn union_minus_one_becomes_two_products() {
        // (a + b) - c -> (a - c) + (b - c)
        let (a, b, c) = (
            cube_leaf(0.0, 2.0),
            cube_leaf(1.0, 3.0),
            cube_leaf(1.5, 2.5),
        );
        let t = create(
            CsgOp::Difference,
            Some(create(CsgOp::Union, Some(a), Some(b))),
            Some(c),
        );
        assert_eq!(products(&t), vec![(1, 1), (1, 1)]);
    }

    #[test]
    fn disjoint_negative_is_pruned() {
        let (a, c) = (cube_leaf(0.0, 1.0), cube_leaf(5.0, 6.0));
        let t = create(CsgOp::Difference, Some(a.clone()), Some(c));
        assert!(Rc::ptr_eq(&t, &a));
        let t = create(CsgOp::Intersection, Some(a), Some(cube_leaf(5.0, 6.0)));
        assert!(t.is_empty_set());
    }

    #[test]
    fn nested_difference_turns_into_an_intersection() {
        // x - (y - z) -> (x - y) + (x * z)
        let (x, y, z) = (
            cube_leaf(0.0, 4.0),
            cube_leaf(1.0, 3.0),
            cube_leaf(1.5, 2.5),
        );
        let t = create(
            CsgOp::Difference,
            Some(x),
            Some(create(CsgOp::Difference, Some(y), Some(z))),
        );
        assert_eq!(products(&t), vec![(1, 1), (2, 0)]);
    }

    #[test]
    fn slab_is_one_unit_thick() {
        let p = Polygon2d::from_outline(vec![[0.0, 0.0], [2.0, 0.0], [2.0, 1.0], [0.0, 1.0]]);
        let s = slab(&p);
        assert_eq!(
            all_vertices_box(&s),
            Some(([0.0, 0.0, -0.5], [2.0, 1.0, 0.5]))
        );
        // Two triangles each at the bottom and top, four sides.
        assert_eq!(s.faces.len(), 8);
    }

    #[test]
    fn product_mesh_keeps_face_colours() {
        let red = Color([1.0, 0.0, 0.0, 1.0]);
        let green = Color([0.0, 1.0, 0.0, 1.0]);
        let mut a = crate::primitives::cube([2.0; 3], false);
        a.set_color(red);
        let mut b = crate::primitives::cube([1.0; 3], false);
        b.transform(&[
            [1.0, 0.0, 0.0, 1.5],
            [0.0, 1.0, 0.0, 0.5],
            [0.0, 0.0, 1.0, 0.5],
            [0.0, 0.0, 0.0, 1.0],
        ]);
        b.set_color(green);
        let out = product_meshes(
            vec![ProductJob {
                positives: vec![a],
                negatives: vec![b.into()],
            }],
            &crate::color::CORNFIELD,
        );
        let ps = out[0].as_ref().unwrap();
        let used: std::collections::BTreeSet<[u32; 4]> = ps
            .color_indices
            .iter()
            .map(|&i| ps.colors[i as usize].key())
            .collect();
        assert_eq!(used.len(), 2, "outer faces red, cut faces green");
    }

    fn moved(mut ps: PolySet, x: f64, y: f64, z: f64) -> PolySet {
        ps.transform(&[
            [1.0, 0.0, 0.0, x],
            [0.0, 1.0, 0.0, y],
            [0.0, 0.0, 1.0, z],
            [0.0, 0.0, 0.0, 1.0],
        ]);
        ps
    }

    /// A cutter several products subtract, as equal meshes in different
    /// colours (a `for` of coloured pieces minus one solid), is converted
    /// once; each product still comes out exactly as when its own copy is
    /// converted, which is what a product alone does. An uncoloured copy
    /// is not merged with the coloured ones.
    #[test]
    fn a_negative_many_products_subtract_is_converted_once() {
        let colors = [
            Color([1.0, 0.0, 0.0, 1.0]),
            Color([0.0, 1.0, 0.0, 1.0]),
            Color([0.0, 0.0, 1.0, 1.0]),
        ];
        let cutter = || {
            moved(
                crate::primitives::cube([12.0, 1.0, 1.0], false),
                -1.0,
                0.5,
                0.5,
            )
        };
        let mut jobs: Vec<ProductJob> = (0..3)
            .map(|i| {
                let mut a = moved(
                    crate::primitives::cube([2.0; 3], false),
                    3.0 * i as f64,
                    0.0,
                    0.0,
                );
                a.set_color(colors[i]);
                let mut c = cutter();
                c.set_color(colors[(i + 1) % 3]);
                ProductJob {
                    positives: vec![a],
                    negatives: vec![c.into()],
                }
            })
            .collect();
        jobs.push(ProductJob {
            positives: vec![moved(
                crate::primitives::cube([2.0; 3], false),
                9.0,
                0.0,
                0.0,
            )],
            negatives: vec![cutter().into()],
        });
        let stop = Stop::default();
        let shared = SharedNegatives::new(&jobs, &stop).unwrap();
        assert_eq!(shared.solids.len(), 1);
        assert_eq!(
            shared.of,
            vec![vec![Some(0)], vec![Some(0)], vec![Some(0)], vec![None]]
        );
        let scheme = crate::color::CORNFIELD;
        let together = format!("{:?}", product_meshes(jobs.clone(), &scheme));
        let alone: Vec<Option<PolySet>> = jobs
            .into_iter()
            .flat_map(|j| product_meshes(vec![j], &scheme))
            .collect();
        assert_eq!(together, format!("{alone:?}"));
    }
}
