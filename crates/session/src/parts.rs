//! Named parts: neoscad's `part("name") { ... }` extension
//! (`eval::node::NodeKind::Part`, on with `--enable part`).
//!
//! A part is a union like any group, but its identity survives rendering
//! in two ways, and `check`, `measure` and `snapshot` use both:
//!
//! - **faces:** every face of the rendered model knows the part it came
//!   from, through Manifold's original IDs
//!   (`geom::manifold_geom::ManifoldGeometry::part_of`). That is how a thin
//!   wall or an overhang found on the model is attributed to a part, and
//!   how a snapshot colours or ghosts parts.
//! - **solids:** each part's own solid ([`Part`]), rendered on its own and
//!   placed where the model places it. That is what per-part volumes,
//!   distances and intersections are measured on, because in the model
//!   two touching parts are one solid and overlapping ones have lost the
//!   faces inside each other.
//!
//! A part's solid is its subtree's geometry: when an operation above it
//! changes what reaches the model (a `difference()` that subtracts it, an
//! `intersection()`, a `hull()`), the solid is still the part's own and
//! [`Part::context`] names that operation.

use std::sync::Arc;

use eval::node::{CsgOp, Matrix, Node, NodeKind};
use geom::Geometry;
use geom::manifold_geom::{ManifoldGeometry, OpType};

/// A part node found in the tree, with where the model puts it.
#[derive(Debug, Clone)]
pub struct Found<'n> {
    pub node: &'n Node,
    /// The full dotted name.
    pub name: &'n str,
    /// The product of the transforms above it.
    pub matrix: Matrix,
    /// The first operation above it that is not a plain union (see
    /// [`Part::context`]).
    pub context: Option<&'static str>,
}

/// One named part: every instance of the name, united, in model
/// coordinates.
#[derive(Debug, Clone)]
pub struct Part {
    /// The full dotted name (`lid.hinge`).
    pub name: String,
    /// How many `part()` calls had this name (more than one warns when
    /// the model is evaluated).
    pub instances: usize,
    /// The part's solid; `None` for a 2D part (parts are tracked in 3D
    /// only) or one with no geometry.
    pub solid: Option<ManifoldGeometry>,
    /// `None` when the part's geometry reaches the model as it is (under
    /// groups, unions, transforms, colours, `render()` and as the first
    /// child of a `difference()`, which cuts it). Otherwise the operation
    /// above it that changes it: `difference` (subtracted), `intersection`,
    /// `hull`, `minkowski`, `resize`, or `2d` (projected or extruded).
    pub context: Option<&'static str>,
}

impl Part {
    /// Whether `other` is this part or nested in it.
    pub fn contains(&self, other: &str) -> bool {
        is_within(other, &self.name)
    }
}

/// Whether part `name` is `outer` or nested in it (`lid.hinge` is within
/// `lid`; `lidx` is not).
pub fn is_within(name: &str, outer: &str) -> bool {
    name == outer
        || (name.len() > outer.len()
            && name.starts_with(outer)
            && name.as_bytes()[outer.len()] == b'.')
}

/// The product of two transforms (`a` then applied after `b`).
pub(crate) fn mul(a: &Matrix, b: &Matrix) -> Matrix {
    let mut m = [[0.0; 4]; 4];
    for (i, row) in m.iter_mut().enumerate() {
        for (j, x) in row.iter_mut().enumerate() {
            *x = (0..4).map(|k| a[i][k] * b[k][j]).sum();
        }
    }
    m
}

/// Every part node under `top`, in tree order. Background (`%`) subtrees
/// are skipped: they are not part of the rendered model.
pub fn find(top: &Node) -> Vec<Found<'_>> {
    // A pre-order walk from an explicit stack, children pushed in reverse
    // so they come off in order: a recursive module makes a tree as deep
    // as the evaluator allows, and a walk that recursed per level could
    // overflow after the evaluation itself had succeeded.
    let mut out = Vec::new();
    let mut stack: Vec<(&Node, Matrix, Option<&'static str>)> =
        vec![(top, eval::node::IDENTITY, None)];
    while let Some((n, m, ctx)) = stack.pop() {
        if n.origin.as_ref().is_some_and(|o| o.tag_background) {
            continue;
        }
        let mut m = m;
        match &n.kind {
            NodeKind::Part { name } => out.push(Found {
                node: n,
                name,
                matrix: m,
                context: ctx,
            }),
            NodeKind::Transform { matrix, .. } => m = mul(&m, matrix),
            _ => {}
        }
        // The context the `i`-th child is in: the innermost operation
        // that makes a part inside it no longer a part of the model.
        let child_ctx = |i: usize| match &n.kind {
            NodeKind::Csg(CsgOp::Difference) if i == 0 => ctx,
            NodeKind::Csg(CsgOp::Difference) => ctx.or(Some("difference")),
            NodeKind::Csg(CsgOp::Intersection) | NodeKind::IntersectionFor => {
                ctx.or(Some("intersection"))
            }
            NodeKind::Hull => ctx.or(Some("hull")),
            NodeKind::Minkowski { .. } => ctx.or(Some("minkowski")),
            NodeKind::Resize { .. } => ctx.or(Some("resize")),
            NodeKind::Projection { .. }
            | NodeKind::LinearExtrude(_)
            | NodeKind::RotateExtrude { .. }
            | NodeKind::Offset { .. } => ctx.or(Some("2d")),
            _ => ctx,
        };
        for (i, c) in n.children.iter().enumerate().rev() {
            stack.push((c, m, child_ctx(i)));
        }
    }
    out
}

/// The parts from their nodes and each node's rendered geometry (in the
/// same order): instances of one name united, in order of first
/// appearance.
pub fn assemble(found: &[Found<'_>], built: Vec<geom::Rendered>) -> Vec<Part> {
    let mut parts: Vec<Part> = Vec::new();
    let mut solids: Vec<Vec<ManifoldGeometry>> = Vec::new();
    for (f, r) in found.iter().zip(built) {
        let i = match parts.iter().position(|p| p.name == f.name) {
            Some(i) => {
                parts[i].instances += 1;
                i
            }
            None => {
                parts.push(Part {
                    name: f.name.to_string(),
                    instances: 1,
                    solid: None,
                    context: f.context,
                });
                solids.push(Vec::new());
                parts.len() - 1
            }
        };
        let solid = match r.geometry {
            Some(Geometry::Manifold(m)) => Some(Arc::unwrap_or_clone(m)),
            Some(g @ Geometry::PolySet(_)) => Some(crate::stats::solid(&g)),
            _ => None,
        };
        if let Some(mut s) = solid.filter(|s| !s.is_empty()) {
            s.transform(&f.matrix);
            solids[i].push(s);
        }
    }
    for (p, s) in parts.iter_mut().zip(solids) {
        p.solid = ManifoldGeometry::batch(OpType::Add, s);
    }
    parts
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nesting_is_by_dotted_prefix() {
        assert!(is_within("lid", "lid"));
        assert!(is_within("lid.hinge", "lid"));
        assert!(!is_within("lidx", "lid"));
        assert!(!is_within("lid", "lid.hinge"));
    }
}
