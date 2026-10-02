//! `check`'s findings about `difference()`s that do not do what their
//! author wrote: an object of the first child that the subtracted children
//! remove entirely (`cut-away`), and a subtracted child that removes
//! nothing (`cuts-nothing`).
//!
//! The first is the commonest silent mistake in agents' enclosure models:
//! standoff posts put in a `union()` with the shell, and the same
//! `difference()` then subtracts the cavity through them, so they vanish
//! from the result while the source, and the agent's report, still say
//! "four posts". Nothing else in a check sees it: the result is a valid,
//! printable solid, just without the posts.
//!
//! How it works, on the evaluated node tree and the render's cache:
//!
//! - **which differences:** each `difference()` written in the user's own
//!   files (the main file's directory and below), not in a library.
//!   Library internals (BOSL2's `diff()`, MCAD) subtract what they like,
//!   and their author is not the reader. Background (`%`) children are not
//!   operands, as in OpenSCAD.
//! - **the objects:** the first child is taken apart through unions,
//!   groups, module calls, `for` loops, colours, `render()` and
//!   transforms, as long as what is inside was written in the user's files
//!   too; each piece below that is one object, in the difference's frame.
//! - **cut away:** the subtracted children that reach an object are taken
//!   from it, most overlapping first, and it is cut away when they took at
//!   least a tenth of it and left under 1%. Or they left more, but only a
//!   stub that lies inside the other objects: a post sunk into the floor,
//!   whose cavity leaves its foot in the floor. A remnant as long and wide
//!   as the object (a ring left by a bore) is not a stub: that object is
//!   missing because another one buries it, not because of the cut.
//! - **cuts nothing:** a subtracted child that does not reach the objects,
//!   or whose overlap with them is empty or inside what the other
//!   subtracted children remove, in every instance of the call that made
//!   it.
//!
//! The cost is bounded: geometry comes from the render's cache and is
//! converted or moved only when a boolean needs it; with one object, or
//! one subtracted child, the volumes of the object and the result answer
//! without a boolean; booleans are capped in operand triangles
//! ([`MAX_TRIANGLES`], [`MAX_OPERANDS`]), and the differences and objects
//! looked at are capped too. A finding that would cost more is not made.

use eval::node::{CsgOp, IDENTITY, Matrix, Node, NodeKind, Origin};
use geom::Geometry;
use geom::manifold_geom::{ManifoldGeometry, OpType};

use crate::check::{Finding, Level};
use crate::mesh::Aabb;
use crate::parts::mul;

/// Distinct differences looked at; the rest are skipped.
const MAX_CUTS: usize = 64;
/// Differences collected from the tree before duplicates are dropped: a
/// loop of thousands of identical differences is not walked into
/// thousands of times.
const MAX_FOUND: usize = 1024;
/// Objects in one difference's first child; a difference with more (a
/// grid of hundreds of pins) is skipped whole rather than half-checked.
const MAX_PIECES: usize = 64;
/// Nodes visited while taking one first child apart.
const MAX_VISIT: usize = 512;
/// Operand triangles (both sides, summed over every boolean) per check.
const MAX_TRIANGLES: usize = 200_000;
/// Operands bigger than this (triangles, both sides) are not compared: a
/// boolean that large costs more than the finding is worth on every check.
const MAX_OPERANDS: usize = 50_000;
/// What an object may keep and still count as cut away.
const LEFT: f64 = 0.01;
/// What the subtraction must take from an object for it to be blamed.
const TAKEN: f64 = 0.1;
/// What the cut may leave of an object, along some axis, and still have
/// taken it across its whole section (see `cut_away`).
const STUB: f64 = 0.75;
/// Objects thinner than this (mm) are ignored: the 0.01 mm slivers that
/// keep faces from being coplanar are not things a reader misses.
const MIN_EXTENT: f64 = 0.05;

/// A `difference()` to look at.
pub(crate) struct Cut<'n> {
    pub node: &'n Node,
    /// The difference's frame in model coordinates.
    pub matrix: Matrix,
    /// The objects of its first child, each with its transform in the
    /// difference's frame.
    pub pieces: Vec<(&'n Node, Matrix)>,
    /// The children it subtracts.
    pub subtracted: Vec<&'n Node>,
}

/// Whether a node is a background (`%`) one, which a difference leaves
/// out of its operands: `difference() { sphere(10); %cylinder(...); }`
/// subtracts nothing, and the cylinder is not a mistake to report.
fn background(n: &Node) -> bool {
    n.origin.as_ref().is_some_and(|o| o.tag_background)
}

/// Whether a node just unites its children (so its children are the
/// objects, not it).
fn unites(n: &Node) -> bool {
    matches!(
        n.kind,
        NodeKind::Root
            | NodeKind::Group { .. }
            | NodeKind::Csg(CsgOp::Union)
            | NodeKind::Color { .. }
            | NodeKind::Render { .. }
            | NodeKind::Part { .. }
            | NodeKind::Transform { .. }
    )
}

/// The differences under `top` to look at, in tree order; `user` says
/// whether a node was written in the user's files. Background (`%`)
/// subtrees are not part of the model and are skipped.
pub(crate) fn find<'n>(top: &'n Node, user: &dyn Fn(&Origin) -> bool) -> Vec<Cut<'n>> {
    let is_user = |n: &Node| n.origin.as_deref().is_some_and(user);
    let mut out = Vec::new();
    // An explicit stack, as `parts::find`: a recursive module's tree can
    // be deeper than a recursive walk's native stack.
    let mut stack: Vec<(&Node, Matrix)> = vec![(top, IDENTITY)];
    while let Some((n, m)) = stack.pop() {
        if out.len() == MAX_FOUND {
            break;
        }
        if n.origin.as_ref().is_some_and(|o| o.tag_background) {
            continue;
        }
        let m = match &n.kind {
            NodeKind::Transform { matrix, .. } => mul(&m, matrix),
            _ => m,
        };
        // The operands: the children that are not background, the first
        // of them the one subtracted from.
        let mut operands = n.children.iter().filter(|c| !background(c));
        if matches!(n.kind, NodeKind::Csg(CsgOp::Difference))
            && is_user(n)
            && let Some(first) = operands.next()
            && let Some(pieces) = pieces(first, &is_user)
        {
            let subtracted: Vec<&Node> = operands.collect();
            if !subtracted.is_empty() {
                out.push(Cut {
                    node: n,
                    matrix: m,
                    pieces,
                    subtracted,
                });
            }
        }
        for c in n.children.iter().rev() {
            stack.push((c, m));
        }
    }
    out
}

/// The objects of a first child, or `None` when there are too many. A
/// uniting node is taken apart when all its children are the user's: a
/// call of a library module stays one object, at the user's call.
fn pieces<'n>(first: &'n Node, is_user: &dyn Fn(&Node) -> bool) -> Option<Vec<(&'n Node, Matrix)>> {
    let mut out = Vec::new();
    let mut stack: Vec<(&Node, Matrix)> = vec![(first, IDENTITY)];
    let mut visited = 0;
    while let Some((n, m)) = stack.pop() {
        visited += 1;
        if visited > MAX_VISIT || out.len() > MAX_PIECES {
            return None;
        }
        if n.origin.as_ref().is_some_and(|o| o.tag_background) {
            continue;
        }
        let open = unites(n) && !n.children.is_empty() && n.children.iter().all(is_user);
        if !open {
            out.push((n, m));
            continue;
        }
        let m = match &n.kind {
            NodeKind::Transform { matrix, .. } => mul(&m, matrix),
            _ => m,
        };
        for c in n.children.iter().rev() {
            stack.push((c, m));
        }
    }
    (out.len() <= MAX_PIECES).then_some(out)
}

fn bounds(s: &ManifoldGeometry) -> Aabb {
    s.bounds().map_or(Aabb::EMPTY, |(lo, hi)| Aabb { lo, hi })
}

/// The box of `b` moved by `m`.
fn moved(b: &Aabb, m: &Matrix) -> Aabb {
    if b.is_empty() {
        return Aabb::EMPTY;
    }
    let mut out = Aabb::EMPTY;
    for i in 0..8 {
        let p = [
            if i & 1 == 0 { b.lo[0] } else { b.hi[0] },
            if i & 2 == 0 { b.lo[1] } else { b.hi[1] },
            if i & 4 == 0 { b.lo[2] } else { b.hi[2] },
        ];
        let q = [0, 1, 2].map(|r| m[r][0] * p[0] + m[r][1] * p[1] + m[r][2] * p[2] + m[r][3]);
        out.grow(q);
    }
    out
}

fn triangles(s: &ManifoldGeometry) -> usize {
    s.manifold.num_tri()
}

/// A rendered node, placed in a difference's frame, as a solid only when a
/// boolean needs one. Converting a mesh (a `polyhedron()`, an import) to
/// a solid, or copying a cached solid to move it, is most of the cost
/// otherwise: the /try threaded ring subtracts the same swept polyhedra in
/// 36 differences, and converting them for each took a second, with no
/// boolean run at all.
pub(crate) struct Lazy {
    geom: Geometry,
    matrix: Option<Matrix>,
    solid: std::cell::OnceCell<Option<std::sync::Arc<ManifoldGeometry>>>,
    bbox: std::cell::OnceCell<Aabb>,
    volume: std::cell::OnceCell<f64>,
}

impl Lazy {
    /// `None` for 2D or nothing.
    pub fn new(geom: Option<Geometry>) -> Option<Lazy> {
        let geom = geom.filter(|g| g.dimension() == 3 && !g.is_empty())?;
        Some(Lazy {
            geom,
            matrix: None,
            solid: std::cell::OnceCell::new(),
            bbox: std::cell::OnceCell::new(),
            volume: std::cell::OnceCell::new(),
        })
    }

    /// This node's geometry moved by `m`.
    pub fn moved(&self, m: Matrix) -> Lazy {
        Lazy {
            geom: self.geom.clone(),
            matrix: Some(m),
            solid: std::cell::OnceCell::new(),
            bbox: std::cell::OnceCell::new(),
            volume: std::cell::OnceCell::new(),
        }
    }

    /// The box, from the mesh as it is (moved: a box around the moved box).
    fn bbox(&self) -> Aabb {
        *self.bbox.get_or_init(|| self.find_bbox())
    }

    /// The volume. A mesh's is summed from its faces (a closed mesh's
    /// signed tetrahedra), which spares converting it: an extrusion is a
    /// mesh, and converting the 36 wedges of the /try threaded ring only
    /// to weigh them was most of what this check cost there.
    fn volume(&self) -> f64 {
        *self.volume.get_or_init(|| match &self.geom {
            Geometry::PolySet(p) => {
                let v = &p.vertices;
                let mut six = 0.0;
                for f in &p.faces {
                    let Some(&a) = f.first() else { continue };
                    let a = v[a as usize];
                    for w in f[1..].windows(2) {
                        let (b, c) = (v[w[0] as usize], v[w[1] as usize]);
                        six += a[0] * (b[1] * c[2] - b[2] * c[1])
                            - a[1] * (b[0] * c[2] - b[2] * c[0])
                            + a[2] * (b[0] * c[1] - b[1] * c[0]);
                    }
                }
                let det = self.matrix.map_or(1.0, |m| {
                    m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1])
                        - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
                        + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0])
                });
                (six / 6.0 * det).abs()
            }
            _ => self.get().map_or(0.0, |s| s.manifold.volume()),
        })
    }

    fn find_bbox(&self) -> Aabb {
        let b = match &self.geom {
            Geometry::Manifold(m) => m.bounds(),
            Geometry::PolySet(p) => p.bounds(),
            Geometry::Polygon2d(_) => None,
        }
        .map_or(Aabb::EMPTY, |(lo, hi)| Aabb { lo, hi });
        match &self.matrix {
            Some(m) => moved(&b, m),
            None => b,
        }
    }

    /// Whether every vertex passes `f`, in this frame, without converting.
    fn all_vertices(&self, f: impl Fn([f64; 3]) -> bool) -> bool {
        let at = |p: [f64; 3]| match &self.matrix {
            Some(m) => {
                [0, 1, 2].map(|r| m[r][0] * p[0] + m[r][1] * p[1] + m[r][2] * p[2] + m[r][3])
            }
            None => p,
        };
        match &self.geom {
            Geometry::Manifold(m) => m
                .manifold
                .as_impl()
                .vert_pos
                .iter()
                .all(|p| f(at([p.x, p.y, p.z]))),
            Geometry::PolySet(p) => p.vertices.iter().all(|&p| f(at(p))),
            Geometry::Polygon2d(_) => false,
        }
    }

    /// Triangles, without converting.
    fn triangles(&self) -> usize {
        match &self.geom {
            Geometry::Manifold(m) => m.manifold.num_tri(),
            Geometry::PolySet(p) => p.faces.iter().map(|f| f.len().saturating_sub(2)).sum(),
            Geometry::Polygon2d(_) => 0,
        }
    }

    fn get(&self) -> Option<&ManifoldGeometry> {
        self.solid
            .get_or_init(|| {
                let s = match (&self.geom, &self.matrix) {
                    (Geometry::Manifold(m), None) => m.clone(),
                    (g, m) => {
                        let mut s = crate::stats::solid(g);
                        if let Some(m) = m {
                            s.transform(m);
                        }
                        std::sync::Arc::new(s)
                    }
                };
                (!s.is_empty()).then_some(s)
            })
            .as_deref()
    }
}

/// What the booleans may still spend, in operand triangles. Booleans cost
/// about in proportion to their operands' triangles, so this bounds the
/// time the findings add to a check.
pub(crate) struct Budget {
    left: usize,
}

impl Budget {
    pub fn new() -> Budget {
        Budget {
            left: MAX_TRIANGLES,
        }
    }

    /// Whether a boolean of operands of `a` and `b` triangles may run.
    fn take(&mut self, a: usize, b: usize) -> bool {
        let n = a + b;
        if n > MAX_OPERANDS || n > self.left {
            return false;
        }
        self.left -= n;
        true
    }
}

/// One difference's geometry, in its frame: its objects and subtracted
/// children (`None`: 2D, empty, or not rendered) and its result. Not its
/// first child as a whole: a render need not keep a `union()` that only
/// feeds a difference, and building it again cost the /try gearbox a
/// fifth of a second.
pub(crate) struct Solids<'a> {
    pub pieces: Vec<Option<Held<'a>>>,
    pub subtracted: Vec<Option<&'a Lazy>>,
    pub result: Option<&'a Lazy>,
}

/// An object's geometry: the node's own, shared by every difference that
/// has it, or moved into this difference's frame.
pub(crate) enum Held<'a> {
    Shared(&'a Lazy),
    Own(Box<Lazy>),
}

impl std::ops::Deref for Held<'_> {
    type Target = Lazy;
    fn deref(&self) -> &Lazy {
        match self {
            Held::Shared(l) => l,
            Held::Own(l) => l,
        }
    }
}

impl Solids<'_> {
    fn subs(&self) -> Vec<(usize, &Lazy, Aabb)> {
        self.subtracted
            .iter()
            .enumerate()
            .filter_map(|(i, g)| g.map(|g| (i, g, g.bbox())))
            .collect()
    }

    /// The only object, when there is one: then the first child is it.
    fn only(&self) -> Option<&Lazy> {
        let mut it = self.pieces.iter().flatten();
        let one = it.next()?;
        it.next().is_none().then_some(&**one)
    }

    /// With one object, its volume and the result's: what the render
    /// already has, and the whole answer.
    fn volumes(&self) -> Option<(f64, f64)> {
        let one = self.only()?;
        Some((one.volume(), self.result.map_or(0.0, Lazy::volume)))
    }
}

/// An object cut away: its index in [`Cut::pieces`], its volume, what is
/// left of it, and the subtracted child that took the most of it.
pub(crate) struct Away {
    pub piece: usize,
    pub volume: f64,
    pub left: f64,
    pub by: usize,
}

/// What one difference does wrong: objects cut away, and the indices of
/// subtracted children that touch nothing (`misses`) or touch only what
/// the others remove already (`covered`).
#[derive(Default)]
pub(crate) struct Verdict {
    pub away: Vec<Away>,
    pub misses: Vec<usize>,
    pub covered: Vec<usize>,
    /// Subtracted children that were compared (a miss counts only against
    /// them; the rest ran out of budget).
    pub compared: Vec<usize>,
    /// The boxes of the objects and of the subtracted children, in the
    /// difference's frame (empty for those with no solid).
    pub piece_boxes: Vec<Aabb>,
    pub sub_boxes: Vec<Aabb>,
}

impl Verdict {
    pub fn new(s: &Solids<'_>) -> Verdict {
        Verdict {
            piece_boxes: s
                .pieces
                .iter()
                .map(|p| p.as_ref().map_or(Aabb::EMPTY, |p| p.bbox()))
                .collect(),
            sub_boxes: s
                .subtracted
                .iter()
                .map(|p| p.map_or(Aabb::EMPTY, Lazy::bbox))
                .collect(),
            ..Verdict::default()
        }
    }
}

/// The objects of `s` that its subtracted children cut away (into `v`).
pub(crate) fn cut_away(s: &Solids<'_>, v: &mut Verdict, budget: &mut Budget) {
    let subs = s.subs();
    if subs.is_empty() {
        return;
    }
    let boxes = v.piece_boxes.clone();
    for (j, piece) in s.pieces.iter().enumerate() {
        let Some(piece) = piece else { continue };
        let b = &boxes[j];
        if b.is_empty() || b.size().iter().any(|&x| x < MIN_EXTENT) {
            continue;
        }
        // The subtracted children that reach it, the most overlapping
        // first: for a post in a cavity the first is the cavity, and the
        // post is gone after one boolean.
        let mut reach: Vec<(usize, &Lazy, f64)> = subs
            .iter()
            .filter(|(_, _, sb)| sb.overlaps(b, 0.0))
            .map(|(i, g, sb)| (*i, *g, shared(b, sb)))
            .collect();
        if reach.is_empty() {
            continue;
        }
        reach.sort_by(|x, y| y.2.total_cmp(&x.2));
        // The only object: the result is what is left of it, and the
        // render has it already.
        if let Some((before, after)) = s.volumes() {
            if before > 1e-9 && after <= LEFT * before && before - after >= TAKEN * before {
                v.away.push(Away {
                    piece: j,
                    volume: before,
                    left: after.max(0.0),
                    by: reach[0].0,
                });
            }
            continue;
        }
        // Cheap necessary conditions before any boolean: the boxes it
        // shares with the children that reach it hold at least a tenth of
        // its volume, and every vertex of it is inside one of their boxes
        // or another object's (a vertex outside all of them keeps some of
        // the object).
        let pad = 1e-6 * (1.0 + b.size().iter().fold(0.0_f64, |a, &x| a.max(x)));
        let covers = |p: [f64; 3]| {
            let at = Aabb::point(p);
            reach
                .iter()
                .any(|(i, _, _)| v.sub_boxes[*i].overlaps(&at, pad))
                || boxes
                    .iter()
                    .enumerate()
                    .any(|(i, ob)| i != j && ob.overlaps(&at, pad))
        };
        let volume = piece.volume();
        if volume <= 1e-9 || reach.iter().map(|r| r.2).sum::<f64>() < TAKEN * volume {
            continue;
        }
        if !piece.all_vertices(covers) {
            continue;
        }
        let Some(solid) = piece.get() else { continue };
        let mut rest = solid.clone();
        let mut left = volume;
        let mut by = (reach[0].0, 0.0);
        let mut spent = false;
        for (i, g, _) in &reach {
            if left <= LEFT * volume {
                break;
            }
            if !budget.take(triangles(&rest), g.triangles()) {
                spent = true;
                break;
            }
            let Some(g) = g.get() else { continue };
            rest = rest.boolean(g, OpType::Subtract);
            let now = rest.manifold.volume();
            if left - now > by.1 {
                by = (*i, left - now);
            }
            left = now;
        }
        if spent || left > (1.0 - TAKEN) * volume {
            continue;
        }
        // What survives the cut may be inside the other objects (a post
        // sunk into a floor): take those away too. That blames the cut
        // only when it took the object across its whole section, leaving
        // a stub (shorter than the object along some axis by a quarter or
        // more): a lead-in taper drawn inside the barb stem it should
        // have stuck out of, with a bore through both, is left a ring as
        // long and wide as the taper, and it is missing because of the
        // stem, not the bore (two agents' hose adapters; saying the bore
        // removed it, and to add it after the subtraction, was wrong
        // advice). A post standing in a solid block from z = 0, with the
        // cavity cut above the floor, is left a stub in the floor.
        if left > LEFT * volume {
            let (rs, ps) = (bounds(&rest).size(), b.size());
            if !(0..3).any(|k| rs[k] <= STUB * ps[k]) {
                continue;
            }
            let rb = bounds(&rest);
            for (i, other) in s.pieces.iter().enumerate() {
                let Some(other) = other else { continue };
                if i == j || !boxes[i].overlaps(&rb, 0.0) {
                    continue;
                }
                if !budget.take(triangles(&rest), other.triangles()) {
                    left = f64::INFINITY;
                    break;
                }
                let Some(other) = other.get() else { continue };
                rest = rest.boolean(other, OpType::Subtract);
                left = rest.manifold.volume();
                if left <= LEFT * volume {
                    break;
                }
            }
        }
        if left > LEFT * volume {
            continue;
        }
        v.away.push(Away {
            piece: j,
            volume,
            left: left.max(0.0),
            by: by.0,
        });
    }
}

/// The subtracted children of `s` that touch nothing of its first child
/// (into `v`). Run after every difference's [`cut_away`], the finding
/// worth the budget first.
pub(crate) fn misses(s: &Solids<'_>, v: &mut Verdict, budget: &mut Budget) {
    let subs = s.subs();
    if subs.is_empty() {
        return;
    }
    let first = v.piece_boxes.iter().fold(Aabb::EMPTY, |a, b| a.union(b));
    // With one object, whether the result is as big as it: then nothing
    // was cut, by any of them.
    let nothing_cut = s
        .volumes()
        .map(|(before, after)| before - after <= 1e-9 * before.max(1.0));
    let none = |g: &ManifoldGeometry, of: &Lazy| g.manifold.volume() <= 1e-9 * of.volume().max(1.0);
    'subs: for (i, g, b) in &subs {
        let (touches, effective) = if !b.overlaps(&first, 0.0) || nothing_cut == Some(true) {
            (false, false)
        } else if nothing_cut == Some(false) && subs.len() == 1 {
            // The one subtracted child, and the result is smaller.
            (true, true)
        } else {
            // What it takes from each object it reaches, less what the
            // other subtracted children take: a cutout meant to go through
            // a wall but placed in the cavity takes only what the cavity
            // takes already.
            let mut touches = false;
            let mut effective = false;
            for (j, piece) in s.pieces.iter().enumerate() {
                let Some(piece) = piece else { continue };
                if !v.piece_boxes[j].overlaps(b, 0.0) {
                    continue;
                }
                if !budget.take(piece.triangles(), g.triangles()) {
                    continue 'subs;
                }
                let (Some(p), Some(gs)) = (piece.get(), g.get()) else {
                    continue;
                };
                let mut rest = p.boolean(gs, OpType::Intersect);
                if none(&rest, g) {
                    continue;
                }
                touches = true;
                for (k, other, ob) in &subs {
                    if k == i || !ob.overlaps(&bounds(&rest), 0.0) {
                        continue;
                    }
                    if !budget.take(triangles(&rest), other.triangles()) {
                        continue 'subs;
                    }
                    let Some(other) = other.get() else { continue };
                    rest = rest.boolean(other, OpType::Subtract);
                    if none(&rest, g) {
                        break;
                    }
                }
                if !none(&rest, g) {
                    effective = true;
                    break;
                }
            }
            (touches, effective)
        };
        v.compared.push(*i);
        if !touches {
            v.misses.push(*i);
        } else if !effective {
            v.covered.push(*i);
        }
    }
}

/// The volume two boxes share.
fn shared(a: &Aabb, b: &Aabb) -> f64 {
    (0..3)
        .map(|k| (a.hi[k].min(b.hi[k]) - a.lo[k].max(b.lo[k])).max(0.0))
        .product()
}

/// Where a call is, for messages: `base.scad:31`.
pub(crate) type Locate<'a> = dyn Fn(&Origin) -> String + 'a;

/// The node to name for a subtracted child: through transforms, colours
/// and `render()` with one child, to the shape or module call they place,
/// so a message says "the cylinder() at m.scad:4" rather than the
/// `translate()` around it.
fn shape(mut n: &Node) -> &Node {
    while matches!(
        n.kind,
        NodeKind::Transform { .. } | NodeKind::Color { .. } | NodeKind::Render { .. }
    ) && n.children.len() == 1
        && n.children[0].origin.is_some()
    {
        n = &n.children[0];
    }
    n
}

/// The name of the call that made a node: `cylinder()`, `posts()`.
fn call(n: &Node) -> String {
    let name = n.origin.as_ref().map_or("", |o| o.name.as_str());
    let name = name.strip_prefix("module ").unwrap_or(name);
    format!("{name}()")
}

/// The findings for the verdicts of `cuts` (in the same order), grouped by
/// the call that made each object or subtracted child: four posts from one
/// `for` loop are one finding.
pub(crate) fn findings(cuts: &[Cut<'_>], verdicts: &[Verdict], at: &Locate<'_>) -> Vec<Finding> {
    // Objects cut away, by the call that made them.
    struct Group {
        what: String,
        count: usize,
        volume: f64,
        left: f64,
        bbox: Aabb,
        diff: String,
        by: String,
    }
    let mut away: Vec<((u32, u32, u32), Group)> = Vec::new();
    for (cut, v) in cuts.iter().zip(verdicts) {
        for a in &v.away {
            let (node, _) = &cut.pieces[a.piece];
            let Some(o) = node.origin.as_deref() else {
                continue;
            };
            let key = (o.unit, o.span.file.0, o.span.start);
            let by_node = shape(cut.subtracted[a.by]);
            let by = by_node.origin.as_deref().map_or(String::new(), |bo| {
                format!("the {} at {}", call(by_node), at(bo))
            });
            let diff = cut.node.origin.as_deref().map_or(String::new(), at);
            let i = match away.iter().position(|(k, _)| *k == key) {
                Some(i) => i,
                None => {
                    away.push((
                        key,
                        Group {
                            what: format!("{} at {}", call(node), at(o)),
                            count: 0,
                            volume: 0.0,
                            left: 0.0,
                            bbox: Aabb::EMPTY,
                            diff,
                            by,
                        },
                    ));
                    away.len() - 1
                }
            };
            let g = &mut away[i].1;
            g.count += 1;
            g.volume += a.volume;
            g.left += a.left;
            g.bbox = g.bbox.union(&moved(&v.piece_boxes[a.piece], &cut.matrix));
        }
    }
    let mut out = Vec::new();
    for (_, g) in away {
        // Pronouns: one object, or the several one call made.
        let (subject, they, them, lie) = if g.count == 1 {
            (format!("the {}", g.what), "it", "it", "lies")
        } else {
            (
                format!("the {} objects made by {}", g.count, g.what),
                "they",
                "them",
                "lie",
            )
        };
        let left = if g.left <= 1e-9 {
            format!("nothing of {them} is left in the result")
        } else {
            format!(
                "{:.1}% of {them} is left in the result",
                100.0 * g.left / g.volume
            )
        };
        let by = if g.by.is_empty() {
            String::new()
        } else {
            format!(" ({})", g.by)
        };
        out.push(Finding {
            level: Level::Warning,
            code: "cut-away",
            message: format!("the difference() at {} removes {subject}: {left}", g.diff),
            point: g.bbox.center(),
            bbox: g.bbox,
            part: None,
            fix: format!(
                "{they} {lie} inside what the difference() subtracts{by}, so the model does \
                 not have {them}; if {they} belong in it, add {them} after the subtraction, \
                 union() {{ difference() {{ ... }} <{them}> }}, not in its first child"
            ),
            value: Some(g.left),
            limit: Some(LEFT * g.volume),
        });
    }

    // Subtracted children that cut nothing, by the call that made them: a
    // call reported only when every instance of it that was compared cut
    // nothing (a hole in a loop that misses at one end of a range and cuts
    // elsewhere is not a mistake).
    struct Call<'n> {
        key: (u32, u32, u32),
        node: &'n Node,
        diff: String,
        nothing: bool,
        /// Some instance touched the solid, inside what others remove.
        covered: bool,
        bbox: Aabb,
    }
    let mut calls: Vec<Call<'_>> = Vec::new();
    for (cut, v) in cuts.iter().zip(verdicts) {
        for &i in &v.compared {
            let n = shape(cut.subtracted[i]);
            let Some(o) = n.origin.as_deref() else {
                continue;
            };
            let key = (o.unit, o.span.file.0, o.span.start);
            let covered = v.covered.contains(&i);
            let nothing = covered || v.misses.contains(&i);
            let b = moved(&v.sub_boxes[i], &cut.matrix);
            match calls.iter_mut().find(|c| c.key == key) {
                Some(c) => {
                    c.nothing &= nothing;
                    c.covered |= covered;
                    c.bbox = c.bbox.union(&b);
                }
                None => calls.push(Call {
                    key,
                    node: n,
                    diff: cut.node.origin.as_deref().map_or(String::new(), at),
                    nothing,
                    covered,
                    bbox: b,
                }),
            }
        }
    }
    for c in calls {
        if !c.nothing {
            continue;
        }
        let (n, diff, b) = (c.node, &c.diff, c.bbox);
        let o = n.origin.as_deref().expect("kept only with an origin");
        let why = if c.covered {
            "everything it reaches is removed by the other subtracted children already"
        } else {
            "it does not touch the first child"
        };
        out.push(Finding {
            level: Level::Warning,
            code: "cuts-nothing",
            message: format!(
                "the {} at {} subtracted by the difference() at {diff} removes nothing: {why}",
                call(n),
                at(o)
            ),
            point: b.center(),
            bbox: b,
            part: None,
            fix: "move it to where it should cut (a hole or cutout must overlap the solid, and \
                  go a little past each face it opens, e.g. through a wall rather than into the \
                  cavity), or remove it"
                .into(),
            value: Some(0.0),
            limit: None,
        });
    }
    out
}

/// The findings of a check's cut stage, and the milliseconds it took.
#[derive(Debug, Default)]
pub(crate) struct Cuts {
    pub findings: Vec<Finding>,
    pub ms: f64,
}

/// Where a difference's nodes are in the list rendered for the findings.
struct Plan {
    pieces: Vec<usize>,
    subtracted: Vec<usize>,
    result: usize,
}

impl crate::Session {
    /// The `cut-away` and `cuts-nothing` findings of the model under `top`,
    /// from the renderer that built it (see the module's notes).
    pub(crate) fn cut_findings(
        &self,
        pipe: &mut crate::Pipe,
        loaded: &crate::Loaded,
        top: &Node,
        keys: &eval::dump::Keys,
        scheme: &geom::color::Scheme,
        job: &crate::JobGuard<'_>,
    ) -> Result<Vec<Finding>, crate::Stop> {
        let cwd = pipe.paths.cwd.clone();
        let main_dir = crate::docfs::normal(&pipe.paths.main_dir);
        let file_of = |o: &Origin| {
            loaded
                .unit_sources(o.unit)
                .filter(|s| (o.span.file.0 as usize) < s.len())
                .map(|s| s.path(o.span.file).to_path_buf())
        };
        let user = |o: &Origin| {
            file_of(o).is_some_and(|p| crate::docfs::normal(&cwd.join(p)).starts_with(&main_dir))
        };
        let mut cuts = find(top, &user);
        if cuts.is_empty() {
            return Ok(Vec::new());
        }
        // One look per distinct difference: a module called four times
        // with the same arguments is one subtree, judged once.
        let mut seen = Vec::new();
        cuts.retain(|c| {
            let k = keys.get(c.node);
            let new = !seen.contains(&k);
            seen.push(k);
            new
        });
        cuts.truncate(MAX_CUTS);

        // Every node whose geometry is needed, once each.
        let mut nodes: Vec<&Node> = Vec::new();
        // By key: the channel a loop subtracts in 36 differences is one
        // subtree, so one entry.
        fn slot<'n>(
            n: &'n Node,
            keys: &eval::dump::Keys,
            at: &mut std::collections::HashMap<u128, usize>,
            nodes: &mut Vec<&'n Node>,
        ) -> usize {
            *at.entry(keys.get(n)).or_insert_with(|| {
                nodes.push(n);
                nodes.len() - 1
            })
        }
        let mut at = std::collections::HashMap::new();
        let plans: Vec<Plan> = cuts
            .iter()
            .map(|c| Plan {
                pieces: c
                    .pieces
                    .iter()
                    .map(|(n, _)| slot(n, keys, &mut at, &mut nodes))
                    .collect(),
                subtracted: c
                    .subtracted
                    .iter()
                    .map(|n| slot(n, keys, &mut at, &mut nodes))
                    .collect(),
                result: slot(c.node, keys, &mut at, &mut nodes),
            })
            .collect();
        let (font_sig, fonts) = self.fonts_for(&loaded.used(), &*pipe.fs);
        let (_, renderer) = self.renderer_for(scheme, font_sig);
        let opts = geom::RenderOptions {
            scheme: *scheme,
            force: false,
            fs: pipe.fs.clone(),
            work_dir: pipe.paths.cwd.clone(),
            fonts,
            interrupt: Some(job.flag.clone()),
            guard: job.limits.clone(),
            // The model's render printed every message already.
            replay: None,
        };
        let built = match renderer.render_many(&nodes, keys, opts) {
            Ok(b) => b,
            Err(u) if u.is_interrupted() => return Err(self.interrupted(pipe, loaded, job)),
            // The model rendered, so these do too; if one somehow does
            // not, the check goes without these findings.
            Err(_) => return Ok(Vec::new()),
        };
        if job.stopped() {
            return Err(self.interrupted(pipe, loaded, job));
        }
        // Nothing is converted or copied until a boolean needs it, and a
        // node's box, volume and solid are found once for every difference
        // that has it.
        let lazies: Vec<Option<Lazy>> = built.into_iter().map(|r| Lazy::new(r.geometry)).collect();
        // `None` for a 2D difference, which is not a print's business here.
        let solids = |c: &Cut<'_>, plan: &Plan| -> Option<Solids<'_>> {
            let pieces: Vec<Option<Held<'_>>> = c
                .pieces
                .iter()
                .zip(&plan.pieces)
                .map(|((_, m), &i)| {
                    let l = lazies[i].as_ref()?;
                    Some(if *m == IDENTITY {
                        Held::Shared(l)
                    } else {
                        Held::Own(Box::new(l.moved(*m)))
                    })
                })
                .collect();
            pieces.iter().any(Option::is_some).then(|| Solids {
                pieces,
                subtracted: plan
                    .subtracted
                    .iter()
                    .map(|&i| lazies[i].as_ref())
                    .collect(),
                result: lazies[plan.result].as_ref(),
            })
        };
        let mut budget = Budget::new();
        let mut verdicts = Vec::new();
        for (c, plan) in cuts.iter().zip(&plans) {
            let Some(s) = solids(c, plan) else {
                verdicts.push(Verdict::default());
                continue;
            };
            let mut v = Verdict::new(&s);
            cut_away(&s, &mut v, &mut budget);
            verdicts.push(v);
            if job.stopped() {
                return Err(self.interrupted(pipe, loaded, job));
            }
        }
        for ((c, plan), v) in cuts.iter().zip(&plans).zip(&mut verdicts) {
            if let Some(s) = solids(c, plan) {
                misses(&s, v, &mut budget);
            }
            if job.stopped() {
                return Err(self.interrupted(pipe, loaded, job));
            }
        }
        let at = |o: &Origin| {
            let file = file_of(o).map_or(String::new(), |p| {
                p.file_name().map_or_else(
                    || p.to_string_lossy().into_owned(),
                    |n| n.to_string_lossy().into_owned(),
                )
            });
            format!("{file}:{}", o.line)
        };
        Ok(findings(&cuts, &verdicts, &at))
    }
}
