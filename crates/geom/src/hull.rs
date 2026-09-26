//! `hull()`, as OpenSCAD's Manifold build computes it.
//!
//! - 2D: `GeometryEvaluator::applyHull2D` (`GeometryEvaluator.cc:232-269`)
//!   collects every outline vertex of every child into a `std::list` and
//!   calls CGAL's `convex_hull_2`. A list iterator is bidirectional, so CGAL
//!   dispatches to Akl-Toussaint (`convex_hull_2.h`), which [`hull_2d`]
//!   ports step for step. The output order (counter-clockwise from the
//!   lexicographically smallest point) is what exported SVG and DXF files
//!   show, and the predicates are the inexact `Simple_cartesian<double>`
//!   ones (`cgal.h:63`), so near-collinear points are kept or dropped as
//!   OpenSCAD's are.
//! - 3D: `applyOperator3DManifold(children, HULL)`
//!   (`manifold-applyops.cc:29-64`) gathers points (a mesh's face corners,
//!   repeats included; a solid's vertices) and calls `Manifold::Hull`,
//!   manifold-rust's port of Manifold's QuickHull. [`hull_points`] gathers
//!   them in the same order, since QuickHull's triangulation of a
//!   face with more than three vertices depends on it.

use manifold_rust::linalg::Vec3;

use crate::Geometry;
use crate::polygon2d::{Outline, Polygon2d};

/// `Left_turn_2` of `Simple_cartesian<double>`: `orientationC2` is the sign
/// of `determinant2x2(qx-px, qy-py, rx-px, ry-py)`, computed as
/// `a00*a11 - a10*a01`. Apple clang contracts that into one fused
/// multiply-subtract with the second product rounded first, so the same
/// form is used here; a plain evaluation can flip the sign of a
/// near-collinear triple.
fn left_turn(p: [f64; 2], q: [f64; 2], r: [f64; 2]) -> bool {
    orientation(p, q, r) > 0.0
}

fn orientation(p: [f64; 2], q: [f64; 2], r: [f64; 2]) -> f64 {
    let (a00, a01, a10, a11) = (q[0] - p[0], q[1] - p[1], r[0] - p[0], r[1] - p[1]);
    a00.mul_add(a11, -(a10 * a01))
}

fn less_xy(a: [f64; 2], b: [f64; 2]) -> bool {
    a[0] < b[0] || (a[0] == b[0] && a[1] < b[1])
}

fn less_yx(a: [f64; 2], b: [f64; 2]) -> bool {
    a[1] < b[1] || (a[1] == b[1] && a[0] < b[0])
}

/// `ch__ref_graham_andrew_scan` over `pts` (first and last are extreme
/// points, the rest sorted along the chain): appends the chain's hull
/// vertices after the first, excluding the last.
fn graham_andrew_scan(pts: &[[f64; 2]], out: &mut Vec<[f64; 2]>) {
    let last = pts.len() - 1;
    // Indices into `pts`; `S` in CGAL, starting with (last, first).
    let mut s: Vec<usize> = vec![last, 0];
    let mut iter = 0;
    loop {
        iter += 1;
        if iter == last || left_turn(pts[last], pts[0], pts[iter]) {
            break;
        }
    }
    if iter != last {
        s.push(iter);
        let mut alpha = iter;
        let mut beta = s[s.len() - 2];
        iter += 1;
        while iter != last {
            if left_turn(pts[alpha], pts[iter], pts[last]) {
                while !left_turn(pts[beta], pts[alpha], pts[iter]) {
                    s.pop();
                    alpha = beta;
                    beta = s[s.len() - 2];
                }
                s.push(iter);
                beta = alpha;
                alpha = iter;
            }
            iter += 1;
        }
    }
    out.extend(s[1..].iter().map(|&i| pts[i]));
}

/// CGAL's `ch_akl_toussaint` (`ch_akl_toussaint_impl.h`) on a forward
/// range: the extreme points north, south, west and east, the rest sorted
/// into the four regions between them, and a Graham-Andrew scan per region.
pub fn convex_hull_2(points: &[[f64; 2]]) -> Vec<[f64; 2]> {
    let mut out = Vec::new();
    if points.is_empty() {
        return out;
    }
    // `ch_nswe_point_with_order` for forward iterators: strict comparisons,
    // so the first of equal candidates wins.
    let (mut n, mut s, mut w, mut e) = (0, 0, 0, 0);
    for (i, &p) in points.iter().enumerate() {
        if less_xy(p, points[w]) {
            w = i;
        }
        if less_xy(points[e], p) {
            e = i;
        }
        if less_yx(points[n], p) {
            n = i;
        }
        if less_yx(p, points[s]) {
            s = i;
        }
    }
    let (pn, ps, pw, pe) = (points[n], points[s], points[w], points[e]);
    if pn == ps {
        out.push(pw);
        return out;
    }
    // The four extreme positions in range order; the points at them are
    // not assigned to regions (they head them).
    let mut ranges = [w, e, n, s];
    ranges.sort_unstable();
    let duplicated = usize::from(ranges[0] == ranges[1])
        + usize::from(ranges[1] == ranges[2])
        + usize::from(ranges[2] == ranges[3]);

    let mut regions: [Vec<[f64; 2]>; 4] = [vec![pw], vec![ps], vec![pe], vec![pn]];
    // `r1`/`r3` of the degenerate assignment: an extreme point that is two
    // of the four at once collapses a region onto its neighbour.
    let r1 = if s == w { 1 } else { 0 };
    let r3 = if n == e { 3 } else { 2 };
    let assign = |p: [f64; 2], regions: &mut [Vec<[f64; 2]>; 4]| {
        if duplicated == 0 {
            if left_turn(pe, pw, p) {
                if left_turn(ps, pw, p) {
                    regions[0].push(p);
                } else if left_turn(pe, ps, p) {
                    regions[1].push(p);
                }
            } else if left_turn(pn, pe, p) {
                regions[2].push(p);
            } else if left_turn(pw, pn, p) {
                regions[3].push(p);
            }
        } else if duplicated == 2 {
            let o = orientation(pe, pw, p);
            if o > 0.0 {
                regions[r1].push(p);
            } else if o < 0.0 {
                regions[r3].push(p);
            }
        } else if s == w || s == e {
            if left_turn(pe, pw, p) {
                regions[r1].push(p);
            } else if left_turn(pn, pe, p) {
                regions[2].push(p);
            } else if left_turn(pw, pn, p) {
                regions[3].push(p);
            }
        } else if left_turn(pe, pw, p) {
            if s != w && left_turn(ps, pw, p) {
                regions[0].push(p);
            } else if e != s && left_turn(pe, ps, p) {
                regions[1].push(p);
            }
        } else {
            regions[r3].push(p);
        }
    };
    let skip = |i: usize| ranges.contains(&i);
    for (i, &p) in points.iter().enumerate() {
        if !skip(i) {
            assign(p, &mut regions);
        }
    }
    // Regions 1 and 2 run west to east, 3 and 4 east to west. The sorts are
    // not stable in CGAL either, but they only reorder equal points, which
    // the scan treats alike.
    let asc = |a: &[f64; 2], b: &[f64; 2]| {
        if less_xy(*a, *b) {
            std::cmp::Ordering::Less
        } else if less_xy(*b, *a) {
            std::cmp::Ordering::Greater
        } else {
            std::cmp::Ordering::Equal
        }
    };
    regions[0][1..].sort_by(asc);
    regions[1][1..].sort_by(asc);
    regions[2][1..].sort_by(|a, b| asc(b, a));
    regions[3][1..].sort_by(|a, b| asc(b, a));
    let ends = [ps, pe, pn, pw];
    let starts = [pw, ps, pe, pn];
    for k in 0..4 {
        if starts[k] != ends[k] {
            let mut r = std::mem::take(&mut regions[k]);
            r.push(ends[k]);
            graham_andrew_scan(&r, &mut out);
        }
    }
    out
}

/// `applyHull2D`: the hull of every child outline's vertices as one
/// sanitized outline, or an empty shape when there are no points.
pub fn hull_2d(children: &[Option<&Polygon2d>]) -> Polygon2d {
    let points: Vec<[f64; 2]> = children
        .iter()
        .flatten()
        .flat_map(|p| p.outlines.iter())
        .flat_map(|o| o.vertices.iter().copied())
        .collect();
    if points.is_empty() {
        return Polygon2d::default();
    }
    Polygon2d {
        outlines: vec![Outline::new(convex_hull_2(&points))],
        sanitized: true,
    }
}

/// The points `applyOperator3DManifold(HULL)` hands to `Manifold::Hull`: for
/// a mesh, the vertex of every face corner (so shared vertices repeat); for
/// a solid, its vertices in order.
pub fn hull_points(children: &[Geometry], out: &mut Vec<Vec3>) {
    for g in children {
        match g {
            Geometry::PolySet(ps) => {
                out.reserve(ps.faces.iter().map(Vec::len).sum());
                for f in &ps.faces {
                    out.extend(f.iter().map(|&i| {
                        let v = ps.vertices[i as usize];
                        Vec3::new(v[0], v[1], v[2])
                    }));
                }
            }
            Geometry::Manifold(m) => {
                if !m.is_empty() {
                    out.extend_from_slice(&m.manifold.as_impl().vert_pos);
                }
            }
            Geometry::Polygon2d(_) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn square_with_inner_points() {
        let pts = [
            [1.0, 1.0],
            [0.0, 0.0],
            [2.0, 0.0],
            [0.5, 0.5],
            [2.0, 2.0],
            [0.0, 2.0],
            [1.0, 0.0],
        ];
        // Counter-clockwise from the lexicographically smallest point; the
        // collinear (1, 0) is dropped.
        assert_eq!(
            convex_hull_2(&pts),
            vec![[0.0, 0.0], [2.0, 0.0], [2.0, 2.0], [0.0, 2.0]]
        );
    }

    #[test]
    fn degenerate_inputs() {
        assert!(convex_hull_2(&[]).is_empty());
        assert_eq!(convex_hull_2(&[[1.0, 1.0], [1.0, 1.0]]), vec![[1.0, 1.0]]);
        // Collinear points: the two ends.
        assert_eq!(
            convex_hull_2(&[[0.0, 0.0], [1.0, 1.0], [2.0, 2.0]]),
            vec![[0.0, 0.0], [2.0, 2.0]]
        );
        // A triangle whose south point is also its west point.
        assert_eq!(
            convex_hull_2(&[[0.0, 0.0], [2.0, 1.0], [1.0, 2.0]]),
            vec![[0.0, 0.0], [2.0, 1.0], [1.0, 2.0]]
        );
    }

    #[test]
    fn circle_order_matches_rotation() {
        // A regular octagon starting at angle 0: the hull starts at the
        // west-most point (the lower of the two with smallest x).
        let pts: Vec<[f64; 2]> = (0..8)
            .map(|i| {
                let a = f64::from(i) * std::f64::consts::PI / 4.0 + 0.1;
                [a.cos(), a.sin()]
            })
            .collect();
        let h = convex_hull_2(&pts);
        assert_eq!(h.len(), 8);
        let w = pts
            .iter()
            .copied()
            .fold(pts[0], |a, b| if less_xy(b, a) { b } else { a });
        assert_eq!(h[0], w);
        for i in 0..8 {
            assert!(orientation(h[i], h[(i + 1) % 8], h[(i + 2) % 8]) > 0.0);
        }
    }
}
