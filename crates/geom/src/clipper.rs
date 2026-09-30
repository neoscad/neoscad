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
    ClipType, Clipper64, ClipperOffset, EndType, FillRule, JoinType, Path64, Paths64, Point64,
    PolyTree64, is_positive, poly_tree_to_paths64, simplify_path,
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
            let mut p: Path64 = o
                .vertices
                .iter()
                .map(|v| Point64::new((v[0] * s).round() as i64, (v[1] * s).round() as i64))
                .collect();
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
    let mut out = Polygon2d {
        outlines: Vec::new(),
        sanitized: true,
    };
    fn walk(tree: &PolyTree64, idx: usize, inv: f64, out: &mut Polygon2d) {
        let node = &tree.nodes[idx];
        let path = node.polygon();
        // "When using offset, clipper can get the hole status wrong", so
        // the winding decides.
        let positive = is_positive(path);
        let cleaned = simplify_path(path, SIMPLIFY_EPSILON, true);
        if cleaned.len() >= 3 {
            out.outlines.push(Outline {
                vertices: cleaned
                    .iter()
                    .map(|p| [inv * p.x as f64, inv * p.y as f64])
                    .collect(),
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
    c.execute_tree(
        ClipType::Union,
        FillRule::EvenOdd,
        &mut tree,
        &mut Paths64::new(),
    );
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
                if p.sanitized {
                    paths
                } else {
                    poly_tree_to_paths64(&sanitize_paths(&paths))
                }
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
            c.execute_tree(
                op.clip_type(),
                FillRule::NonZero,
                &mut tree,
                &mut Paths64::new(),
            );
            if i != paths.len() - 1 {
                source = poly_tree_to_paths64(&tree);
                c.clear();
            }
        }
        return from_tree(&tree);
    }
    if op == Op2::Union
        && let Some(out) = union_by_bands(paths)
    {
        return out;
    }
    let indices: Vec<usize> = (0..paths.len()).collect();
    run(paths, &indices, op, &mut tree);
    from_tree(&tree)
}

/// One Clipper run over the children `indices` of `paths`: the first child
/// of the operation is the subject and every other child a clip, as
/// `ClipperUtils::apply` adds them. Returns Clipper's success flag and how
/// many output records it split off after the sweep (see `union_by_bands`).
fn run(paths: &[Paths64], indices: &[usize], op: Op2, tree: &mut PolyTree64) -> (bool, usize) {
    let mut c = clipper();
    for &i in indices {
        if i == 0 {
            c.add_subject(&paths[i]);
        } else {
            c.add_clip(&paths[i]);
        }
    }
    let ok = c.execute_tree(op.clip_type(), FillRule::NonZero, tree, &mut Paths64::new());
    (ok, c.base.late_outrecs)
}

/// A union of children that fall into two or more separate horizontal
/// bands, as one union per band, or `None` to run the one full union.
///
/// Clipper sweeps a scanline from the largest y to the smallest, and a
/// union's output order is the order in which the sweep creates its output
/// records. When no child's y-range overlaps or touches another band's,
/// the scanline never holds edges of two bands at once, so the full
/// union's sweep is each band's sweep in turn, identical step for step,
/// and its output is the bands' outputs concatenated from the top band
/// down. So this is byte-identical to the full union, which is what makes
/// it usable at all: SVG and DXF exports are compared byte for byte with
/// OpenSCAD's, which runs the full union (`GeometryEvaluator.cc:694`).
/// Children that are separate in x but share y are not split off: their
/// output records interleave in the full sweep, and no per-part result
/// says how.
///
/// One exception breaks the concatenation: after the sweep, Clipper splits
/// some records in two (touching or self-intersecting outlines), and the
/// new record goes to the end of the whole list, after every band. So a
/// band other than the lowest that split anything off (the vendored
/// clipper2-rust's `late_outrecs`) makes this give up and return `None`.
///
/// The gain is that bands are independent: in a render of many lines of
/// `text()` the top-level union of 200 lines was half the run, one thread
/// sweeping every line in turn, and here each line is its own band.
fn union_by_bands(paths: &[Paths64]) -> Option<Polygon2d> {
    // (min y, max y, child) for every child with points, in integer units
    // so that "touching" means exactly what it means to the sweep.
    let mut spans: Vec<(i64, i64, usize)> = paths
        .iter()
        .enumerate()
        .filter_map(|(i, p)| {
            let mut ys = p.iter().flatten().map(|pt| pt.y);
            let first = ys.next()?;
            let (lo, hi) = ys.fold((first, first), |(lo, hi), y| (lo.min(y), hi.max(y)));
            Some((lo, hi, i))
        })
        .collect();
    if spans.len() < 2 {
        return None;
    }
    // Bands from the top down: sort by max y, largest first, and start a
    // new band when a child's top is below the current band's bottom.
    // A child whose top is at or above the current band's bottom shares a
    // scanline with it (touching counts: the sweep handles both at that y).
    spans.sort_by(|a, b| b.1.cmp(&a.1).then(a.2.cmp(&b.2)));
    let mut bands: Vec<(Vec<usize>, i64)> = Vec::new();
    for &(lo, hi, i) in &spans {
        match bands.last_mut() {
            Some((band, bottom)) if hi >= *bottom => {
                band.push(i);
                *bottom = (*bottom).min(lo);
            }
            _ => bands.push((vec![i], lo)),
        }
    }
    if bands.len() < 2 {
        return None;
    }
    for (band, _) in &mut bands {
        // Children keep their order within a band: it decides which is the
        // subject, and the local minima's tie order.
        band.sort_unstable();
    }
    let solve = |(band, _): &(Vec<usize>, i64)| {
        let mut tree = PolyTree64::new();
        let (ok, late) = run(paths, band, Op2::Union, &mut tree);
        (ok, late, from_tree(&tree))
    };
    #[cfg(all(feature = "parallel", not(target_arch = "wasm32")))]
    let results: Vec<(bool, usize, Polygon2d)> = {
        use rayon::prelude::*;
        bands.par_iter().map(solve).collect()
    };
    #[cfg(not(all(feature = "parallel", not(target_arch = "wasm32"))))]
    let results: Vec<(bool, usize, Polygon2d)> = bands.iter().map(solve).collect();
    // The lowest band's late records come last in the full union too.
    let last = results.len() - 1;
    if results
        .iter()
        .enumerate()
        .any(|(i, (ok, late, _))| !ok || (i != last && *late != 0))
    {
        return None;
    }
    let mut out = Polygon2d {
        outlines: Vec::new(),
        sanitized: true,
    };
    for (_, _, p) in results {
        out.outlines.extend(p.outlines);
    }
    Some(out)
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
    sum.execute_tree(
        ClipType::Union,
        FillRule::NonZero,
        &mut tree,
        &mut Paths64::new(),
    );
    if tree.root().children().is_empty() {
        None
    } else {
        Some(from_tree(&tree))
    }
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
pub fn offset(
    poly: &Polygon2d,
    delta: f64,
    join: Join,
    miter_limit: f64,
    arc_tolerance: f64,
) -> Polygon2d {
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
        .map(|o| Polygon2d {
            outlines: vec![o],
            sanitized: true,
        })
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
                (0..v.len())
                    .map(|i| v[i][0] * v[(i + 1) % v.len()][1] - v[(i + 1) % v.len()][0] * v[i][1])
                    .sum::<f64>()
                    / 2.0
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

    /// The full union `apply_paths` runs when `union_by_bands` declines.
    fn full_union(paths: &[Paths64]) -> Polygon2d {
        let indices: Vec<usize> = (0..paths.len()).collect();
        let mut tree = PolyTree64::new();
        run(paths, &indices, Op2::Union, &mut tree);
        from_tree(&tree)
    }

    /// Bit-for-bit equality (`PartialEq` on f64 would let -0.0 == 0.0 by).
    fn same_bits(a: &Polygon2d, b: &Polygon2d) -> bool {
        a.outlines.len() == b.outlines.len()
            && a.outlines.iter().zip(&b.outlines).all(|(x, y)| {
                x.positive == y.positive
                    && x.vertices.len() == y.vertices.len()
                    && x.vertices.iter().zip(&y.vertices).all(|(p, q)| {
                        p[0].to_bits() == q[0].to_bits() && p[1].to_bits() == q[1].to_bits()
                    })
            })
    }

    /// A small deterministic generator (no test dependency needed).
    struct Lcg(u64);
    impl Lcg {
        fn next(&mut self) -> u64 {
            self.0 = self
                .0
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            self.0 >> 33
        }
        fn below(&mut self, n: i64) -> i64 {
            (self.next() % n as u64) as i64
        }
    }

    fn rect(x0: i64, y0: i64, x1: i64, y1: i64, ccw: bool) -> Path64 {
        let mut p = vec![
            Point64::new(x0, y0),
            Point64::new(x1, y0),
            Point64::new(x1, y1),
            Point64::new(x0, y1),
        ];
        if !ccw {
            p.reverse();
        }
        p
    }

    /// One child: rectangles on a coarse grid (shared and touching edges,
    /// which is what makes Clipper join and split records after its sweep),
    /// a ring with a hole, a random star, or a self-intersecting polygon,
    /// inside the box whose corner is (x, y) and whose height is `h`.
    fn child(rng: &mut Lcg, x: i64, y: i64, h: i64) -> Paths64 {
        let g = h / 4;
        match rng.below(5) {
            0 | 1 => (0..1 + rng.below(4))
                .map(|_| {
                    let (a, b) = (rng.below(4), rng.below(4));
                    let (c, d) = (rng.below(4), rng.below(4));
                    let (x0, x1) = (a.min(c), a.max(c) + 1);
                    let (y0, y1) = (b.min(d), b.max(d) + 1);
                    rect(x + x0 * g, y + y0 * g, x + x1 * g, y + y1 * g, true)
                })
                .collect(),
            2 => vec![
                rect(x, y, x + h, y + h, true),
                rect(x + g, y + g, x + h - g, y + h - g, false),
            ],
            3 => {
                let n = 3 + rng.below(9);
                let (cx, cy, r) = (x + h / 2, y + h / 2, h / 2);
                vec![
                    (0..n)
                        .map(|k| {
                            let t = k as f64 * std::f64::consts::TAU / n as f64;
                            let rr = (r / 4 + rng.below(r - r / 4 + 1)) as f64;
                            Point64::new(cx + (rr * t.cos()) as i64, cy + (rr * t.sin()) as i64)
                        })
                        .collect(),
                ]
            }
            _ => vec![
                (0..4 + rng.below(6))
                    .map(|_| Point64::new(x + rng.below(h + 1), y + rng.below(h + 1)))
                    .collect(),
            ],
        }
    }

    /// Whenever `union_by_bands` takes a union, its result is bit-identical
    /// to the one full union's, outline order and start points included.
    /// The layouts are adversarial: rows that touch exactly, children that
    /// overlap in x but not y and the reverse, nested holes, many tiny
    /// shapes, self-intersecting children, and grid rectangles whose joins
    /// split records after the sweep.
    #[test]
    fn bands_match_full_union() {
        let mut rng = Lcg(0x5eed);
        let (mut composed, mut declined) = (0, 0);
        for case in 0..4000 {
            let h: i64 = [8, 40, 4000, 1 << 27][case % 4];
            // Rows exactly `pitch` apart touch when pitch == h.
            let pitch = h + [0, 1, h / 2, -h / 4][rng.below(4) as usize];
            let rows = 1 + rng.below(6);
            let n = 2 + rng.below(if case % 10 == 0 { 60 } else { 12 }) as usize;
            let paths: Vec<Paths64> = (0..n)
                .map(|_| {
                    let row = rng.below(rows);
                    let x = rng.below(6) * h / 2;
                    let jitter = if rng.below(4) == 0 {
                        rng.below(3) - 1
                    } else {
                        0
                    };
                    if rng.below(20) == 0 {
                        Paths64::new()
                    } else {
                        child(&mut rng, x, row * pitch + jitter, h)
                    }
                })
                .collect();
            let full = full_union(&paths);
            match union_by_bands(&paths) {
                Some(bands) => {
                    composed += 1;
                    assert!(same_bits(&bands, &full), "case {case}: {paths:?}");
                }
                None => declined += 1,
            }
        }
        // The test only means something if both paths were taken often.
        assert!(composed > 1000 && declined > 200, "{composed} {declined}");
    }

    /// The bands run on rayon's pool; the concatenation is in band order,
    /// so the result is the full union's on any number of threads.
    #[cfg(feature = "parallel")]
    #[test]
    fn bands_are_the_same_on_any_thread_count() {
        let mut rng = Lcg(7);
        let paths: Vec<Paths64> = (0..300)
            .map(|i| child(&mut rng, (i % 7) * 900, (i / 7) * 1100, 1000))
            .collect();
        let full = full_union(&paths);
        for threads in [1, 2, 8] {
            let pool = rayon::ThreadPoolBuilder::new()
                .num_threads(threads)
                .build()
                .unwrap();
            let bands = pool.install(|| union_by_bands(&paths)).expect("bands");
            assert!(same_bits(&bands, &full), "differs on {threads} threads");
        }
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
