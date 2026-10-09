//! The export render's memo: a subtree that recurs is built once and
//! placed again wherever else it appears.
//!
//! The normal render caches each subtree's geometry by its key, so a
//! module instantiated 400 times is built once. The export render cannot
//! use that cache for exact solids: their surface records are numbered in
//! tree order and their primitives are built in place under their
//! ancestors' matrices. Without a memo of its own, the BOSL2 fractal tree
//! (each level two rotated copies of the level below) spent 9 s in the
//! export render's unions against half a second of normal render.
//!
//! Here the first instance of a recurring subtree is built as before, in
//! place, and kept: its triangles, its surface records, and what building
//! it added to the walk's reports. Another instance whose placement
//! differs from the first's by a similarity (a rotation, a mirror, a
//! uniform scale, a translation) is that solid with its positions and
//! records mapped by the difference, its triangles tagged with fresh
//! records numbered where the instance stands in tree order. Every
//! choice the walk makes for a leaf (an exact surface, a kept polygon, a
//! faceted region) depends on the leaf's matrix only through whether it
//! is a similarity, which a similarity between the placements preserves,
//! so the second instance is the first one's choices, moved.
//!
//! A model with no large recurring subtree is built exactly as before.
//! In one with some, the copies differ from in-place builds by the
//! rounding of one more matrix product (about 1e-16 of the model's size,
//! far below the 1e-9 within which reconstruction merges surfaces). Where
//! faces are flush or touch, that rounding can still decide whether
//! Manifold's mesh reconstructs, so only subtrees large enough to be
//! worth it are kept ([`MIN_TRIANGLES`]), and a model refused with copies
//! is exported again with none (`super::super::export_step`).

use std::collections::HashMap;

use eval::dump::Keys;
use eval::node::{Node, NodeKind};
use meshbrep::Surface;

use crate::Matrix;

/// A subtree's identity for the memo: its key (its content), and a hash of
/// the source locations in it. Two subtrees of one content written at two
/// places in the source report their substitutions at their own places,
/// so they are built separately.
pub(super) type MemoKey = (u128, u64);

/// Which nodes of a tree (by [`node_id`]) to build once and place again:
/// the operations whose subtree recurs (the same [`MemoKey`] at two or
/// more nodes) and has a boolean in it, so that placing it is cheaper
/// than building it.
/// Subtrees with a `fillet()` are left out: its blends are planned on the
/// solid in the call's own frame.
pub(super) fn plan(top: &Node, keys: &Keys) -> HashMap<usize, MemoKey> {
    // A post-order walk on the heap (trees are as deep as the evaluator
    // allows): per node, its location hash, whether its subtree has a
    // fillet, and whether it has a boolean of two or more children.
    struct Info {
        loc: u64,
        fillet: bool,
        boolean: bool,
    }
    // The finished subtrees' infos, in order: a node's children's are the
    // last `children.len()` when it is finished.
    let mut done_infos: Vec<Info> = Vec::new();
    let mut candidates: Vec<(usize, MemoKey)> = Vec::new();
    let mut stack: Vec<(&Node, bool)> = vec![(top, false)];
    while let Some((n, done)) = stack.pop() {
        if !done {
            stack.push((n, true));
            for c in n.children.iter().rev() {
                stack.push((c, false));
            }
            continue;
        }
        let mut loc = mix(0x6d65_6d6f, u64::from(n.children.len() as u32));
        if let Some(o) = &n.origin {
            loc = mix(loc, u64::from(o.unit));
            loc = mix(loc, u64::from(o.span.file.0));
            loc = mix(loc, u64::from(o.span.start));
            loc = mix(loc, u64::from(o.span.end));
            loc = mix(loc, u64::from(o.line));
        }
        let mut fillet = matches!(n.kind, NodeKind::Fillet(_));
        let mut boolean = n.children.len() >= 2;
        let first = done_infos.len() - n.children.len();
        for ci in done_infos.drain(first..) {
            loc = mix(loc, ci.loc);
            fillet |= ci.fillet;
            boolean |= ci.boolean;
        }
        let operation = matches!(
            n.kind,
            NodeKind::Group { .. }
                | NodeKind::Render { .. }
                | NodeKind::Color { .. }
                | NodeKind::Part { .. }
                | NodeKind::Csg(_)
                | NodeKind::IntersectionFor
                | NodeKind::Transform { .. }
        );
        if operation && boolean && !fillet {
            candidates.push((node_id(n), (keys.get(n), loc)));
        }
        done_infos.push(Info {
            loc,
            fillet,
            boolean,
        });
    }
    let mut count: HashMap<MemoKey, u32> = HashMap::new();
    for (_, k) in &candidates {
        *count.entry(*k).or_default() += 1;
    }
    let mut planned: HashMap<usize, MemoKey> = candidates
        .into_iter()
        .filter(|(_, k)| count[k] >= 2)
        .collect();
    // A planned node's only child (and its only child, ...) builds the
    // same solid: placing the outer one is what saves the work, and
    // keeping a copy at every level of such a chain (BOSL2 wraps each
    // module in a dozen groups) added 700 MB to the export of the BOSL2
    // fractal tree.
    let mut stack: Vec<(&Node, bool)> = vec![(top, false)];
    while let Some((n, in_chain)) = stack.pop() {
        let id = node_id(n);
        let here = planned.contains_key(&id);
        if here && in_chain {
            planned.remove(&id);
        }
        let chain = (here || in_chain) && n.children.len() == 1;
        for c in &n.children {
            stack.push((c, chain));
        }
    }
    planned
}

/// A node's identity in [`plan`]'s map: its address, which is unique
/// while the tree lives (creation indices need not be, across subtrees an
/// evaluator reused). Only looked up, never iterated, so the walk does not
/// depend on it.
pub(super) fn node_id(n: &Node) -> usize {
    std::ptr::from_ref(n) as usize
}

/// A 64-bit mixing step (SplitMix64's finaliser on the sum): fixed, so
/// the plan is the same on every platform.
fn mix(h: u64, x: u64) -> u64 {
    let mut z = h
        .rotate_left(5)
        .wrapping_add(x)
        .wrapping_add(0x9e37_79b9_7f4a_7c15);
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

/// A subtree as its first instance built it.
pub(super) struct Entry {
    /// The matrix it was built under.
    pub m: Matrix,
    pub result: Mesh,
    /// The surface records it added, from `base` in the walk's table.
    pub surfaces: Vec<Surface>,
    pub base: usize,
    /// Each record's origin ([`super::ExportMesh::surface_origin`]).
    pub origins: Vec<u32>,
    /// The substitutions it reported: index in the walk's list, and how
    /// many times.
    pub subs: Vec<(usize, u32)>,
    /// Its share of the walk's sagitta (a maximum) and volume bound (a
    /// sum), in its own placement's scale.
    pub sagitta: f64,
    pub volume_bound: f64,
    pub exact_extrusions: u32,
}

/// The solid a memoised subtree made: its triangles, `face_id` indexing
/// the walk's surface table.
pub(super) struct Mesh {
    pub positions: Vec<f64>,
    pub tri_verts: Vec<u64>,
    pub face_id: Vec<u64>,
}

/// The fewest triangles a subtree's solid must have to be kept and placed
/// again. Placing a copy differs from building it in place by rounding,
/// and in a model of flush or touching faces rounding can decide whether
/// Manifold's mesh reconstructs: kept for every recurring subtree, the
/// memo turned four BOSL2 examples that exported (`show_anchors()`'s
/// arrows, BOSL2's sliders) into refusals and one refusal into an export.
/// Small subtrees are cheap to build anyway; the ones that cost seconds
/// (the levels of a recursive tree) are far larger than this.
pub(super) const MIN_TRIANGLES: usize = 4096;

/// Where a memoised subtree's first instance started: what the walk had
/// before it, so that what building it added can be told apart.
pub(super) struct Start {
    pub key: MemoKey,
    pub m: Matrix,
    pub base: usize,
    pub subs: Vec<u32>,
    pub sagitta: f64,
    pub volume_bound: f64,
    pub exact_extrusions: u32,
}
