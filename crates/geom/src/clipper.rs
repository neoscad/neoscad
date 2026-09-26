//! The 2D kernel: OpenSCAD's `ClipperUtils` (`src/geometry/ClipperUtils.cc`)
//! on `clipper2-rust`, a pure-Rust port of Clipper2.
//!
//! Every operation works on integer coordinates: doubles are scaled by
//! 2^27 (`scaleBitsFromPrecision(8)`, `ilogb(1e8) + 1`) and rounded, and
//! results are scaled back. Results are read out of a polytree
//! depth-first, each outline simplified with Clipper's `SimplifyPath` at
//! OpenSCAD's epsilon and marked positive or hole by its signed area.
//!
//! The rules that shape results, as OpenSCAD applies them:
//!
//! - an unsanitized shape (a `polygon()`, a mirrored shape) has each
//!   outline turned counter-clockwise and is unioned with the even-odd
//!   rule, so every nested path alternates between filled and hole
//!   whatever its direction;
//! - booleans between sanitized shapes use the non-zero rule;
//! - intersections run as a chain of binary operations;
//! - offsets use Round (`r`), Miter with limit 1e6 (`delta`) or Square
//!   (`chamfer`) joins, with OpenSCAD's arc tolerance.

use clipper2_rust::{
    ClipType, Clipper64, ClipperOffset, EndType, FillRule, JoinType, Path64, Paths64, Point64, PolyTree64, is_positive,
    poly_tree_to_paths64, simplify_path,
};

use crate::polygon2d::{Outline, Polygon2d};

/// `scaleBitsFromPrecision()` with `DEFAULT_PRECISION = 8`
/// (`ClipperUtils.h:12`): `ilogb(10^8) + 1`.
pub const SCALE_BITS: i32 = 27;

/// The epsilon `toPolygon2d` simplifies outlines with, in scaled units
/// ("taken from Clipper1's default", `ClipperUtils.cc:207`).
const SIMPLIFY_EPSILON: f64 = 1.1415;

/// The 2D operators.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op2 {
    Union,
    Intersection,
    Difference,
}

impl Op2 {
    fn clip_type(self) -> ClipType {
        match self {
            Op2::Union => ClipType::Union,
            Op2::Intersection => ClipType::Intersection,
            Op2::Difference => ClipType::Difference,
        }
    }
}

fn scale() -> f64 {
    2f64.powi(SCALE_BITS)
}

/// `fromPolygon2d`: scale and round (`Point64(double, double)` rounds with
/// `std::round`). Unsanitized outlines are turned counter-clockwise, since
/// their direction carries no meaning yet.
pub fn to_paths(poly: &Polygon2d) -> Paths64 {
    let s = scale();
    poly.outlines
        .iter()
        .map(|o| {
            let mut p: Path64 = o.vertices.iter().map(|v| Point64::new((v[0] * s).round() as i64, (v[1] * s).round() as i64)).collect();
            if !poly.sanitized && !is_positive(&p) {
                p.reverse();
            }
            p
        })
        .collect()
}

/// `toPolygon2d(PolyTree64)`: each node, then its children, depth-first.
pub fn from_tree(tree: &PolyTree64) -> Polygon2d {
    let inv = 1.0 / scale();
    let mut out = Polygon2d { outlines: Vec::new(), sanitized: true };
    fn walk(tree: &PolyTree64, idx: usize, inv: f64, out: &mut Polygon2d) {
        let node = &tree.nodes[idx];
        let path = node.polygon();
        // "When using offset, clipper can get the hole status wrong", so
        // the winding decides.
        let positive = is_positive(path);
        let cleaned = simplify_path(path, SIMPLIFY_EPSILON, true);
        if cleaned.len() >= 3 {
            out.outlines.push(Outline {
                vertices: cleaned.iter().map(|p| [inv * p.x as f64, inv * p.y as f64]).collect(),
                positive,
            });
        }
        for &c in node.children() {
            walk(tree, c, inv, out);
        }
    }
    for &c in tree.root().children() {
        walk(tree, c, inv, &mut out);
    }
    out
}

fn clipper() -> Clipper64 {
    let mut c = Clipper64::new();
    c.set_preserve_collinear(false);
    c
}

/// `sanitize(Paths64)`: an even-odd union.
fn sanitize_paths(paths: &Paths64) -> PolyTree64 {
    let mut c = clipper();
    c.add_subject(paths);
    let mut tree = PolyTree64::new();
    c.execute_tree(ClipType::Union, FillRule::EvenOdd, &mut tree, &mut Paths64::new());
    tree
}

/// `ClipperUtils::sanitize(Polygon2d)`.
pub fn sanitize(poly: &Polygon2d) -> Polygon2d {
    from_tree(&sanitize_paths(&to_paths(poly)))
}

/// `ClipperUtils::apply(polygons, clipType)`: `None` entries are empty
/// children, kept so an empty first child of a difference stays first.
pub fn apply(polys: &[Option<&Polygon2d>], op: Op2) -> Polygon2d {
    let paths: Vec<Paths64> = polys
        .iter()
        .map(|p| match p {
            Some(p) => {
                let paths = to_paths(p);
                if p.sanitized { paths } else { poly_tree_to_paths64(&sanitize_paths(&paths)) }
            }
            None => Paths64::new(),
        })
        .collect();
    apply_paths(&paths, op)
}

fn apply_paths(paths: &[Paths64], op: Op2) -> Polygon2d {
    let mut c = clipper();
    let mut tree = PolyTree64::new();
    if op == Op2::Intersection && paths.len() >= 2 {
        // "intersection operations must be split into a sequence of binary
        // operations"
        let mut source = paths[0].clone();
        for (i, clip) in paths.iter().enumerate().skip(1) {
            c.add_subject(&source);
            c.add_clip(clip);
            c.execute_tree(op.clip_type(), FillRule::NonZero, &mut tree, &mut Paths64::new());
            if i != paths.len() - 1 {
                source = poly_tree_to_paths64(&tree);
                c.clear();
            }
        }
        return from_tree(&tree);
    }
    for (i, p) in paths.iter().enumerate() {
        if i == 0 {
            c.add_subject(p);
        } else {
            c.add_clip(p);
        }
    }
    c.execute_tree(op.clip_type(), FillRule::NonZero, &mut tree, &mut Paths64::new());
    from_tree(&tree)
}

/// `ClipperUtils::applyProjection`: the union of meshes projected face by
/// face (`PolySetUtils::project`), each first unioned with the non-zero
/// rule so faces sharing edges leave no holes. `None` when nothing is left.
pub fn project_union(polys: &[Polygon2d]) -> Option<Polygon2d> {
    let mut sum = clipper();
    for p in polys {
        let mut c = clipper();
        c.add_subject(&to_paths(p));
        let mut result = Paths64::new();
        c.execute(ClipType::Union, FillRule::NonZero, &mut result, None);
        sum.add_subject(&result);
    }
    let mut tree = PolyTree64::new();
    sum.execute_tree(ClipType::Union, FillRule::NonZero, &mut tree, &mut Paths64::new());
    if tree.root().children().is_empty() { None } else { Some(from_tree(&tree)) }
}

/// Offset join types, as `OffsetNode` selects them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Join {
    Round,
    Miter,
    Square,
}

/// `ClipperUtils::applyOffset`. `miter_limit` only applies to Miter (and
/// Clipper's default of 2 otherwise); `arc_tolerance` only to Round.
pub fn offset(poly: &Polygon2d, delta: f64, join: Join, miter_limit: f64, arc_tolerance: f64) -> Polygon2d {
    let s = scale();
    let (jt, ml, at) = match join {
        Join::Round => (JoinType::Round, 2.0, arc_tolerance * s),
        Join::Miter => (JoinType::Miter, miter_limit, 1.0),
        Join::Square => (JoinType::Square, 2.0, 1.0),
    };
    let mut co = ClipperOffset::new(ml, at, false, false);
    co.add_paths(&to_paths(poly), jt, EndType::Polygon);
    let mut tree = PolyTree64::new();
    co.execute_tree(delta * s, &mut tree);
    from_tree(&tree)
}

/// `applyFill2D`: union the children, keep the positive outlines (the outer
/// edges), and union again in case they nest.
pub fn fill(polys: &[Option<&Polygon2d>]) -> Polygon2d {
    let merged = apply(polys, Op2::Union);
    let outer: Vec<Polygon2d> = merged
        .outlines
        .into_iter()
        .filter(|o| o.positive)
        .map(|o| Polygon2d { outlines: vec![o], sanitized: true })
        .collect();
    let refs: Vec<Option<&Polygon2d>> = outer.iter().map(Some).collect();
    apply(&refs, Op2::Union)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn square(x: f64, y: f64, s: f64) -> Polygon2d {
        Polygon2d::from_outline(vec![[x, y], [x + s, y], [x + s, y + s], [x, y + s]])
    }

    fn area(p: &Polygon2d) -> f64 {
        p.outlines
            .iter()
            .map(|o| {
                let v = &o.vertices;
                (0..v.len()).map(|i| v[i][0] * v[(i + 1) % v.len()][1] - v[(i + 1) % v.len()][0] * v[i][1]).sum::<f64>() / 2.0
            })
            .sum()
    }

    #[test]
    fn union_and_difference() {
        let a = square(0.0, 0.0, 2.0);
        let b = square(1.0, 1.0, 2.0);
        let u = apply(&[Some(&a), Some(&b)], Op2::Union);
        assert_eq!(u.outlines.len(), 1);
        assert!((area(&u) - 7.0).abs() < 1e-9);
        let d = apply(&[Some(&a), Some(&b)], Op2::Difference);
        assert!((area(&d) - 3.0).abs() < 1e-9);
        let i = apply(&[Some(&a), Some(&b)], Op2::Intersection);
        assert!((area(&i) - 1.0).abs() < 1e-9);
        // An empty first child of a difference leaves nothing.
        assert!(apply(&[None, Some(&b)], Op2::Difference).is_empty());
    }

    #[test]
    fn hole_is_negative() {
        let a = square(0.0, 0.0, 4.0);
        let b = square(1.0, 1.0, 2.0);
        let d = apply(&[Some(&a), Some(&b)], Op2::Difference);
        assert_eq!(d.outlines.len(), 2);
        assert!(d.outlines[0].positive && !d.outlines[1].positive);
        assert!((area(&d) - 12.0).abs() < 1e-9);
        // fill() drops the hole.
        assert!((area(&fill(&[Some(&d)])) - 16.0).abs() < 1e-9);
    }

    #[test]
    fn even_odd_sanitize() {
        // Two nested paths in the same direction: the inner one is a hole.
        let mut p = square(0.0, 0.0, 4.0);
        p.outlines.push(square(1.0, 1.0, 2.0).outlines.remove(0));
        p.sanitized = false;
        let s = sanitize(&p);
        assert!((area(&s) - 12.0).abs() < 1e-9);
    }

    #[test]
    fn round_offset_vertex_count() {
        // offset(r=1, $fn=8) on a square: the nightly exports 4 + 4 * 2 = 12
        // vertices (a quarter circle at $fn=8 is two steps).
        let n = 8.0f64;
        let tol = 1.0 * (1.0 - eval::trig::cos_degrees(180.0 / n));
        let o = offset(&square(0.0, 0.0, 10.0), 1.0, Join::Round, 2.0, tol);
        assert_eq!(o.outlines.len(), 1);
        assert_eq!(o.outlines[0].vertices.len(), 12);
    }
}
