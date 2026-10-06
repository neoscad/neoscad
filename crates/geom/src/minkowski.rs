//! `minkowski()`, as OpenSCAD's Manifold build computes it.
//!
//! - 2D: `ClipperUtils::applyMinkowski` (`ClipperUtils.cc:289-335`). Each
//!   outline of the left operand is convolved with each outline of the
//!   right one into small quads, a copy of each operand is placed at a
//!   vertex of every positive outline of the other to fill the inside, and
//!   everything is unioned with the non-zero rule. [`minkowski_2d`] is a
//!   straight port, including its quirks with empty operands.
//! - 3D: `ManifoldUtils::applyMinkowski`
//!   (`manifold-applyops-minkowski.cc`), the default: Manifold's own
//!   `MinkowskiSum` is only used with `USE_MANIFOLD_MINKOWSKI`, which is
//!   off (`CMakeLists.txt:43`); the nightly's output confirms it (a unit
//!   cube summed with a cube 5 units away gives one 8-vertex cube, where
//!   Manifold's would also keep the untranslated first operand). OpenSCAD
//!   splits each non-convex operand into convex parts with CGAL's Nef
//!   polyhedra, takes the hull of the pairwise vertex sums of every pair of
//!   parts, and unions the hulls. [`minkowski_3d`] computes the same set
//!   without Nef polyhedra; see there. Messages follow OpenSCAD's: none
//!   when the sum succeeds, and its fallback's when it does not.

use std::collections::HashMap;
use std::sync::Arc;

use clipper2_rust::{ClipType, FillRule, Path64, Paths64, Point64, PolyTree64, is_positive};
use manifold_rust::cancel::CancelToken;
use manifold_rust::impl_mesh::ManifoldImpl;
use manifold_rust::linalg::{Vec3, dot};
use manifold_rust::manifold::Manifold;
use manifold_rust::types::OpType;

use crate::Geometry;
use crate::clipper;
use crate::manifold_geom::{IdSource, ManifoldGeometry};
use crate::polygon2d::Polygon2d;
use crate::polyset::Warnings;

/// Clipper2's `MinkowskiInternal` without its final union
/// (`minkowski_outline`, `ClipperUtils.cc:37-75`): one quad per pair of
/// consecutive vertices, each turned counter-clockwise. OpenSCAD leaves the
/// union to the caller because unioning here moved intersection points and
/// opened cracks.
fn minkowski_outline(poly: &Path64, path: &Path64, quads: &mut Paths64) {
    let (poly_cnt, path_cnt) = (poly.len(), path.len());
    if poly_cnt == 0 || path_cnt == 0 {
        return;
    }
    let pp: Vec<Path64> = path
        .iter()
        .map(|q| {
            poly.iter()
                .map(|p| Point64::new(q.x + p.x, q.y + p.y))
                .collect()
        })
        .collect();
    for i in 0..path_cnt {
        for j in 0..poly_cnt {
            let mut quad = vec![
                pp[i % path_cnt][j % poly_cnt],
                pp[(i + 1) % path_cnt][j % poly_cnt],
                pp[(i + 1) % path_cnt][(j + 1) % poly_cnt],
                pp[i % path_cnt][(j + 1) % poly_cnt],
            ];
            if !is_positive(&quad) {
                quad.reverse();
            }
            quads.push(quad);
        }
    }
}

/// `fill_minkowski_insides`: `a` translated to the first vertex of each
/// positive outline of `b`. The convolution only covers the boundary; any
/// point of `b` works, since `a` moved there stays inside the sum.
fn fill_insides(a: &Paths64, b: &Paths64, target: &mut Paths64) {
    for b_path in b {
        if !b_path.is_empty() && is_positive(b_path) {
            let d = b_path[0];
            for path in a {
                target.push(
                    path.iter()
                        .map(|p| Point64::new(p.x + d.x, p.y + d.y))
                        .collect(),
                );
            }
        }
    }
}

/// `ClipperUtils::applyMinkowski`. `None` entries are empty children.
/// Returns `None` where OpenSCAD returns a null shape (one empty child, or
/// nothing but empty children).
pub fn minkowski_2d(polys: &[Option<&Polygon2d>]) -> Option<Polygon2d> {
    if polys.len() == 1 {
        return polys[0].cloned();
    }
    if polys.iter().all(Option::is_none) {
        return None;
    }
    // The first operand is the first child even if it is empty (then the
    // sum is empty too); later empty children are skipped.
    let mut lhs = polys[0].map(clipper::to_paths).unwrap_or_default();
    let mut c = clipper2_rust::Clipper64::new();
    c.set_preserve_collinear(false);
    for (i, p) in polys.iter().enumerate().skip(1) {
        let Some(p) = p else { continue };
        let rhs = clipper::to_paths(p);
        let mut terms = Paths64::new();
        for rhs_path in &rhs {
            for lhs_path in &lhs {
                minkowski_outline(lhs_path, rhs_path, &mut terms);
            }
        }
        fill_insides(&lhs, &rhs, &mut terms);
        fill_insides(&rhs, &lhs, &mut terms);
        // "This union operation must be performed at each iteration since
        // the minkowski_terms now contain lots of small quads."
        c.clear();
        c.add_subject(&terms);
        if i != polys.len() - 1 {
            lhs = Paths64::new();
            c.execute(ClipType::Union, FillRule::NonZero, &mut lhs, None);
        }
    }
    let mut tree = PolyTree64::new();
    c.execute_tree(
        ClipType::Union,
        FillRule::NonZero,
        &mut tree,
        &mut Paths64::new(),
    );
    Some(clipper::from_tree(&tree))
}

/// One 3D operand: convex pieces (as their vertices) whose union it is, or,
/// when cutting it up would take too many pieces, the solid itself, whose
/// boundary is then split into convex planar patches (see [`pair_terms`]).
enum Operand {
    Pieces(Vec<Vec<Vec3>>),
    Boundary(Box<ManifoldGeometry>),
}

/// What stops a 3D sum: OpenSCAD's `throw 0`, after which it logs
/// "[manifold] Minkowski hard-crashed, falling back to Nef operation." and
/// retries (see `fallback`).
struct Failed;

fn vec3(v: [f64; 3]) -> Vec3 {
    Vec3::new(v[0], v[1], v[2])
}

/// `surfaceMeshFromGeometry` plus the convexity test: a mesh is convex by
/// `PolySet::isConvex`, a solid by its edges (`is_weakly_convex`). A
/// non-convex mesh becomes a solid through [`PolySet::triangulate_faces`],
/// as CGAL would read it; if that needs any repair, the whole sum fails as
/// OpenSCAD's Nef conversion does. Conversion messages are dropped:
/// OpenSCAD's CGAL reading prints none of Manifold's, and the fallback in
/// [`minkowski_3d`] repeats the conversion that does.
///
/// `token` stops the cutting into convex pieces ([`convex_pieces`]), whose
/// splits are booleans of their own; a stopped cut is `Err(Failed)`.
fn operand(
    g: &Geometry,
    ids: &dyn IdSource,
    token: Option<&CancelToken>,
) -> Result<Operand, Failed> {
    let solid = match g {
        Geometry::PolySet(ps) => {
            if ps.is_empty() {
                return Err(Failed);
            }
            if ps.is_convex() {
                // The vertices faces use, each once.
                let mut used = vec![false; ps.vertices.len()];
                let mut points = Vec::new();
                for &i in ps.faces.iter().flatten() {
                    if !std::mem::replace(&mut used[i as usize], true) {
                        points.push(vec3(ps.vertices[i as usize]));
                    }
                }
                return Ok(Operand::Pieces(vec![points]));
            }
            // A mesh Manifold has to repair (faces wound inconsistently, as
            // in `issue2090.scad`) is one CGAL cannot make a valid Nef
            // polyhedron of either.
            let mut warnings = Vec::new();
            let m = ManifoldGeometry::from_polyset(
                &ps.triangulate_faces(),
                ids,
                &mut warnings,
                &mut Vec::new(),
            );
            if m.is_empty() || !warnings.is_empty() {
                return Err(Failed);
            }
            m
        }
        Geometry::Manifold(m) => {
            if m.is_empty() {
                return Err(Failed);
            }
            let imp = m.manifold.as_impl();
            if !imp.is_soup && imp.is_convex() {
                return Ok(Operand::Pieces(vec![imp.vert_pos.clone()]));
            }
            (**m).clone()
        }
        Geometry::Polygon2d(_) => return Err(Failed),
    };
    let pieces = convex_pieces(&solid.manifold, token);
    if manifold_rust::cancel::is_cancelled(token) {
        return Err(Failed);
    }
    Ok(match pieces {
        Some(pieces) => Operand::Pieces(pieces),
        None => Operand::Boundary(Box::new(solid)),
    })
}

/// The cut for each reflex edge (the solid bends inwards across it by more
/// than a rounding error): the plane through the edge that halves the
/// angle the solid fills there, `(normal, offset)`. Each side of it then
/// holds less than half a turn of solid at the edge, so the edge is convex
/// in both pieces.
///
/// The cut used to run along the plane of one of the edge's two faces,
/// which also resolves the edge but makes every split a boolean with a
/// face lying in the cutting plane. Manifold's halfspace is a rotated cube,
/// so that face and the cut were only nearly coplanar: the side that should
/// have lost the face kept it as a flap of no volume, the edge stayed
/// reflex, and every further cut along the same plane split off another
/// empty sliver and added triangles. A 24-sided hole in a cube grew from
/// 112 triangles to millions within 30 cuts (127 s and 18.8 GB in a
/// minkowski with a sphere). The bisecting plane meets the edge's faces
/// only along the edge itself.
fn reflex_cuts(imp: &ManifoldImpl) -> Vec<(Vec3, f64)> {
    use manifold_rust::linalg::cross;
    let mut out = Vec::new();
    for (i, h) in imp.halfedge.iter().enumerate() {
        if !h.is_forward() || h.paired_halfedge < 0 {
            continue;
        }
        let (t0, t1) = (i / 3, h.paired_halfedge as usize / 3);
        let (n0, n1) = (imp.face_normal[t0], imp.face_normal[t1]);
        let e = imp.vert_pos[h.end_vert as usize] - imp.vert_pos[h.start_vert as usize];
        let len = dot(e, e).sqrt();
        if len == 0.0 || dot(e, cross(n0, n1)) / len >= -1e-9 {
            continue;
        }
        // The bisector contains the edge and the direction n0 + n1, which
        // points out of the solid between the two faces (into the notch);
        // its normal is perpendicular to both. Faces folded flat onto each
        // other (n0 = -n1) have no bisector worth cutting along.
        let n = cross(e / len, n0 + n1);
        let l = dot(n, n).sqrt();
        if l < 1e-9 {
            continue;
        }
        let n = n / l;
        out.push((n, dot(n, imp.vert_pos[h.start_vert as usize])));
    }
    out
}

/// Beyond these, a solid is covered through its boundary instead: every cut
/// is two booleans, and a curved concave surface (a gear's notches, a
/// round hole) has a reflex edge per facet.
const MAX_REFLEX: usize = 48;
const MAX_PIECES: usize = 64;

/// The solid cut into convex pieces, the way CGAL's
/// `convex_decomposition_3` does it in spirit: while a piece has a reflex
/// edge, cut it along that edge's bisecting plane ([`reflex_cuts`]), and
/// split pieces that fall apart into their components. `None` when that
/// would take too long, or when `token` is cancelled: the token is looked
/// at before every cut, since each cut is two booleans (manifold-rust's
/// `split_by_plane`, which takes no token) and a solid can need up to
/// `2 * MAX_PIECES` of them.
///
/// Rounding can still leave a cut degenerate (a plane through an edge or a
/// vertex elsewhere on the piece). So a cut only counts when both sides
/// keep some volume, pieces with none are dropped (they lie on a cut face
/// of a neighbour, whose sum covers theirs), and the triangles of the
/// pieces in hand are capped: a decomposition that grows instead of
/// converging gives up and the solid is covered through its boundary,
/// rather than running until memory runs out.
fn convex_pieces(m: &Manifold, token: Option<&CancelToken>) -> Option<Vec<Vec<Vec3>>> {
    if reflex_cuts(m.as_impl()).len() > MAX_REFLEX {
        return None;
    }
    // Volume below this is rounding, relative to the whole solid's.
    let tiny = m.volume().abs() * 1e-9;
    let max_tris = 8 * m.num_tri() + 64 * MAX_PIECES;
    let mut live = m.num_tri();
    let mut stack = vec![m.clone()];
    let mut out = Vec::new();
    let mut cuts = 0;
    while let Some(s) = stack.pop() {
        if manifold_rust::cancel::is_cancelled(token) {
            return None;
        }
        let imp = s.as_impl();
        if s.is_empty() || s.volume() <= tiny {
            live -= imp.num_tri();
            continue;
        }
        if component_vertices(imp).len() > 1 {
            stack.extend(s.decompose().into_iter().rev());
            continue;
        }
        let planes = reflex_cuts(imp);
        if planes.is_empty() {
            out.push(imp.vert_pos.clone());
            if out.len() > MAX_PIECES {
                return None;
            }
            continue;
        }
        let mut halves = None;
        for (n, d) in planes {
            cuts += 1;
            if cuts > 2 * MAX_PIECES || manifold_rust::cancel::is_cancelled(token) {
                return None;
            }
            let (a, b) = s.split_by_plane(n, d);
            if a.volume() > tiny && b.volume() > tiny {
                halves = Some((a, b));
                break;
            }
        }
        // Reflex edges but no cut that divides the piece: rounding has the
        // last word here, so leave it to the boundary cover.
        let (a, b) = halves?;
        live = live + a.num_tri() + b.num_tri() - imp.num_tri();
        if live > max_tris {
            return None;
        }
        stack.push(b);
        stack.push(a);
    }
    Some(out)
}

/// The boundary of a solid as convex planar patches: triangles grown into
/// larger convex polygons across edges while they stay coplanar and their
/// union stays convex. Each patch is summed with the other operand as one
/// hull, so a flat face split into many triangles costs one hull, not many.
fn convex_patches(imp: &ManifoldImpl) -> Vec<Vec<Vec3>> {
    let nt = imp.num_tri();
    let tri = |t: usize| [0, 1, 2].map(|k| imp.halfedge[3 * t + k].start_vert as usize);
    let scale = {
        let (lo, hi) = (imp.bbox.min, imp.bbox.max);
        [hi.x - lo.x, hi.y - lo.y, hi.z - lo.z]
            .into_iter()
            .fold(1e-300, f64::max)
    };
    let eps = imp.epsilon.max(1e-12 * scale);
    let mut done = vec![false; nt];
    let mut patches = Vec::new();
    for t in 0..nt {
        if done[t] {
            continue;
        }
        done[t] = true;
        let n = imp.face_normal[t];
        // Project onto the plane of the two axes the normal is least along.
        let axis = if n.x.abs() >= n.y.abs() && n.x.abs() >= n.z.abs() {
            0
        } else if n.y.abs() >= n.z.abs() {
            1
        } else {
            2
        };
        let flat = |v: Vec3| -> [f64; 2] {
            match axis {
                0 => [v.y, v.z],
                1 => [v.z, v.x],
                _ => [v.x, v.y],
            }
        };
        let area = |t: usize| {
            let [a, b, c] = tri(t).map(|i| flat(imp.vert_pos[i]));
            ((b[0] - a[0]) * (c[1] - a[1]) - (c[0] - a[0]) * (b[1] - a[1])).abs() / 2.0
        };
        let mut verts: Vec<usize> = tri(t).to_vec();
        let mut sum = area(t);
        let mut members = vec![t];
        let mut k = 0;
        // Patches are kept small: the convexity test re-hulls the patch for
        // every candidate.
        while k < members.len() && verts.len() < 64 {
            let m = members[k];
            k += 1;
            for e in 0..3 {
                let pair = imp.halfedge[3 * m + e].paired_halfedge;
                if pair < 0 {
                    continue;
                }
                let u = pair as usize / 3;
                if done[u] || dot(imp.face_normal[u], n) < 1.0 - 1e-12 {
                    continue;
                }
                let p0 = imp.vert_pos[verts[0]];
                if tri(u)
                    .iter()
                    .any(|&i| dot(imp.vert_pos[i] - p0, n).abs() > eps)
                {
                    continue;
                }
                let mut cand = verts.clone();
                for i in tri(u) {
                    if !cand.contains(&i) {
                        cand.push(i);
                    }
                }
                let pts: Vec<[f64; 2]> = cand.iter().map(|&i| flat(imp.vert_pos[i])).collect();
                let hull_area = polygon_area(&crate::hull::convex_hull_2(&pts));
                let new_sum = sum + area(u);
                if (hull_area - new_sum).abs() <= 1e-9 * hull_area {
                    done[u] = true;
                    members.push(u);
                    verts = cand;
                    sum = new_sum;
                }
            }
        }
        patches.push(verts.into_iter().map(|i| imp.vert_pos[i]).collect());
    }
    patches
}

fn polygon_area(p: &[[f64; 2]]) -> f64 {
    let n = p.len();
    (0..n)
        .map(|i| p[i][0] * p[(i + 1) % n][1] - p[(i + 1) % n][0] * p[i][1])
        .sum::<f64>()
        .abs()
        / 2.0
}

/// One vertex of each connected piece of a solid's surface.
fn component_vertices(imp: &ManifoldImpl) -> Vec<Vec3> {
    let n = imp.vert_pos.len();
    let mut parent: Vec<usize> = (0..n).collect();
    fn find(p: &mut [usize], mut i: usize) -> usize {
        while p[i] != i {
            p[i] = p[p[i]];
            i = p[i];
        }
        i
    }
    for h in &imp.halfedge {
        if h.start_vert < 0 || h.end_vert < 0 {
            continue;
        }
        let (a, b) = (
            find(&mut parent, h.start_vert as usize),
            find(&mut parent, h.end_vert as usize),
        );
        if a != b {
            parent[a.max(b)] = a.min(b);
        }
    }
    let mut seen = HashMap::new();
    let mut out = Vec::new();
    for h in imp.halfedge.iter().step_by(3) {
        if h.start_vert < 0 {
            continue;
        }
        let r = find(&mut parent, h.start_vert as usize);
        seen.entry(r)
            .or_insert_with(|| out.push(imp.vert_pos[h.start_vert as usize]));
    }
    out
}

/// The sums a pair of operands needs: point sets whose hulls are unioned,
/// plus translated copies of the operands' solids.
fn pair_terms(a: &Operand, b: &Operand) -> (Vec<Vec<Vec3>>, Vec<(ManifoldGeometry, Vec3)>) {
    let sums = |p: &[Vec3], q: &[Vec3]| -> Vec<Vec3> {
        let mut out = Vec::with_capacity(p.len() * q.len());
        for &x in p {
            out.extend(q.iter().map(|&y| x + y));
        }
        out
    };
    match (a, b) {
        // What OpenSCAD does: every pair of convex pieces.
        (Operand::Pieces(pa), Operand::Pieces(pb)) => (
            pa.iter()
                .flat_map(|p| pb.iter().map(move |q| (p, q)))
                .map(|(p, q)| sums(p, q))
                .collect(),
            Vec::new(),
        ),
        // For a convex piece Q, S + Q is the sum of S's boundary with Q (each
        // convex patch P of it giving the convex P + Q), plus S moved by any
        // point of Q, which covers what lies deeper inside: a point s + q
        // not in S + q0 is reached from s + q0 along q0..q, and on that way
        // p - q(t) leaves S through its boundary. Manifold's own
        // `MinkowskiSum` takes the same route but adds S unmoved, which is
        // only right when Q contains the origin.
        (Operand::Boundary(s), Operand::Pieces(pieces))
        | (Operand::Pieces(pieces), Operand::Boundary(s)) => {
            let patches = convex_patches(s.manifold.as_impl());
            let mut parts = Vec::with_capacity(patches.len() * pieces.len());
            let mut copies = Vec::with_capacity(pieces.len());
            for q in pieces {
                parts.extend(patches.iter().map(|p| sums(p, q)));
                copies.push(((**s).clone(), q[0]));
            }
            (parts, copies)
        }
        // Both covered by their boundaries: the same argument twice. A + B
        // is the sum of the two boundaries, plus A moved to a point of each
        // surface piece of B, plus B moved to a point of each surface piece
        // of A.
        (Operand::Boundary(sa), Operand::Boundary(sb)) => {
            let (ia, ib) = (sa.manifold.as_impl(), sb.manifold.as_impl());
            let (pa, pb) = (convex_patches(ia), convex_patches(ib));
            let normal = |p: &[Vec3]| {
                let n = manifold_rust::linalg::cross(p[1] - p[0], p[2] - p[0]);
                let l = dot(n, n).sqrt();
                if l > 0.0 { n / l } else { n }
            };
            let nb: Vec<Vec3> = pb.iter().map(|p| normal(p)).collect();
            let mut parts = Vec::new();
            for p in &pa {
                let n = normal(p);
                for (q, m) in pb.iter().zip(&nb) {
                    // Parallel patches sum to something flat, with no volume.
                    if dot(n, *m).abs() > 1.0 - 1e-12 {
                        continue;
                    }
                    parts.push(sums(p, q));
                }
            }
            let mut copies: Vec<(ManifoldGeometry, Vec3)> = component_vertices(ib)
                .into_iter()
                .map(|v| ((**sa).clone(), v))
                .collect();
            copies.extend(
                component_vertices(ia)
                    .into_iter()
                    .map(|v| ((**sb).clone(), v)),
            );
            (parts, copies)
        }
    }
}

/// All points on one plane (or line, or point), decided exactly as CGAL's
/// `coplanar` predicate would.
fn coplanar(pts: &[Vec3]) -> bool {
    use manifold_rust::linalg::cross;
    use manifold_rust::robust::exact::{Sign, filtered::orient3d};
    let a = pts[0];
    let Some(&b) = pts.iter().find(|&&p| p != a) else {
        return true;
    };
    // The point making the largest triangle with a and b spans the plane;
    // if rounding hides an exactly collinear triple, every orientation
    // below is zero and the set is reported flat, which it then is.
    let mut c = b;
    let mut best = 0.0;
    for &p in pts {
        let n = cross(b - a, p - a);
        let d = dot(n, n);
        if d > best {
            best = d;
            c = p;
        }
    }
    if best == 0.0 {
        return true;
    }
    pts.iter().all(|&d| orient3d(a, b, c, d) == Sign::Zero)
}

/// Hulls of the point sets, in order; on rayon's pool with the `parallel`
/// feature. Each hull's result depends only on its points, so the order of
/// the output (and everything after) is the same at any thread count.
///
/// Once `token` is cancelled the remaining hulls are skipped (left empty):
/// a sum of two dense non-convex operands builds thousands of them, and
/// without the check a cancel or a time limit would wait for every one
/// before the union could notice it. The caller discards the result.
fn hulls(sets: &[Vec<Vec3>], token: Option<&CancelToken>) -> Vec<ManifoldImpl> {
    // "if (minkowski_points.size() <= 3) return empty"; and a flat set
    // gives nothing either: OpenSCAD keeps only hull vertices whose faces
    // are not all coplanar, which leaves none of a planar hull, where
    // QuickHull returns a zero-volume mesh.
    let one = |s: &Vec<Vec3>| {
        if s.len() <= 3 || coplanar(s) || manifold_rust::cancel::is_cancelled(token) {
            ManifoldImpl::new()
        } else {
            crate::hull::hull_3d(s)
        }
    };
    #[cfg(all(feature = "parallel", not(target_arch = "wasm32")))]
    {
        use rayon::prelude::*;
        sets.par_iter().map(one).collect()
    }
    #[cfg(not(all(feature = "parallel", not(target_arch = "wasm32"))))]
    sets.iter().map(one).collect()
}

/// `ManifoldUtils::applyMinkowski`: fold the children left to right, each
/// step the union of convex sums, made one original. `children` are the
/// non-empty 3D results, at least two. Mesh operands are converted with
/// IDs from `conv(i)` (child `i`); the result's own ID comes from `own`.
///
/// OpenSCAD splits a non-convex operand into convex solids with Nef
/// polyhedra (`convex_decomposition_3`, exact arithmetic) and sums every
/// pair of parts. Here the solid is cut into convex pieces with Manifold
/// booleans instead ([`convex_pieces`]), and a solid with too many reflex
/// edges for that is covered through its boundary ([`pair_terms`]). Either
/// way the union is the same solid; the pieces, and so the triangulation,
/// differ from CGAL's. The pieces matter for speed: every hull overlaps its
/// neighbours along curved, nearly coincident surfaces, and the union of
/// those overlaps is most of the cost, so fewer and larger pieces win (an
/// extruded L summed with a 32-segment sphere took 53 ms through boundary
/// patches, 5 ms as 2 pieces).
///
/// `token` is the request's kernel token
/// ([`crate::manifold_geom::kernel_token`]): the hulls and the union of
/// each step stop on it, so a cancel or a limit ends a long sum instead of
/// waiting for it. A cancelled sum is `None` (or an empty solid), without
/// the fallback's warning; the caller sees the token and reports an
/// interruption rather than this result.
pub fn minkowski_3d<'a>(
    children: &[Geometry],
    conv: &dyn Fn(usize) -> Box<dyn IdSource + 'a>,
    own: &dyn IdSource,
    token: Option<&CancelToken>,
    warnings: &mut Warnings,
    errors: &mut Warnings,
) -> Option<ManifoldGeometry> {
    match fold(children, conv, own, token) {
        Ok(m) => return Some(m),
        Err(Failed) if manifold_rust::cancel::is_cancelled(token) => return None,
        Err(Failed) => {}
    }
    warnings.push("[manifold] Minkowski hard-crashed, falling back to Nef operation.".into());
    fallback(children, conv, own, token, warnings, errors)
}

/// The fallback, `applyOperator3DManifold(children, MINKOWSKI)`: each child
/// converted to a solid the usual way (with its messages), empty ones
/// skipped, and the rest summed pairwise by `ManifoldGeometry::minkowski`,
/// which goes through Nef polyhedra. A flat solid makes an empty Nef
/// polyhedron and so an empty sum (`issue1671.scad`); a child that cannot
/// be converted drops out, leaving the others (`issue1137.scad`).
fn fallback<'a>(
    children: &[Geometry],
    conv: &dyn Fn(usize) -> Box<dyn IdSource + 'a>,
    own: &dyn IdSource,
    token: Option<&CancelToken>,
    warnings: &mut Warnings,
    errors: &mut Warnings,
) -> Option<ManifoldGeometry> {
    let mut geom: Option<ManifoldGeometry> = None;
    for (i, g) in children.iter().enumerate() {
        let m = match g {
            Geometry::PolySet(ps) => {
                ManifoldGeometry::from_polyset(ps, &*conv(i), warnings, errors)
            }
            Geometry::Manifold(m) => (**m).clone(),
            Geometry::Polygon2d(_) => continue,
        };
        if m.is_empty() {
            continue;
        }
        geom = Some(match geom {
            None => m,
            Some(acc) => {
                if acc.is_empty() || acc.manifold.volume() == 0.0 || m.manifold.volume() == 0.0 {
                    ManifoldGeometry::default()
                } else {
                    let pair = [
                        Geometry::Manifold(Arc::new(acc)),
                        Geometry::Manifold(Arc::new(m)),
                    ];
                    fold(&pair, conv, own, token).unwrap_or_default()
                }
            }
        });
    }
    geom
}

/// `Err(Failed)` both where OpenSCAD's sum throws and when `token` stops
/// it; [`minkowski_3d`] tells the two apart by the token.
fn fold<'a>(
    children: &[Geometry],
    conv: &dyn Fn(usize) -> Box<dyn IdSource + 'a>,
    own: &dyn IdSource,
    token: Option<&CancelToken>,
) -> Result<ManifoldGeometry, Failed> {
    let mut lhs = operand(&children[0], &*conv(0), token)?;
    let mut result: Option<ManifoldGeometry> = None;
    for (i, g) in children.iter().enumerate().skip(1) {
        if let Some(n) = result.take() {
            lhs = operand(&Geometry::Manifold(Arc::new(n)), own, token)?;
        }
        let rhs = operand(g, &*conv(i), token)?;
        let (sets, copies) = pair_terms(&lhs, &rhs);
        let built = hulls(&sets, token);
        if manifold_rust::cancel::is_cancelled(token) {
            return Err(Failed);
        }
        // IDs for the hulls, consecutive and in order, so the union orders
        // their triangles the same way whichever thread built them.
        let first = Manifold::reserve_ids(built.len() as u32);
        let mut parts: Vec<ManifoldGeometry> = Vec::with_capacity(built.len() + copies.len());
        for (k, imp) in built.into_iter().enumerate() {
            if imp.num_tri() > 0 {
                parts.push(ManifoldGeometry::from_built(imp, first + k as u32));
            }
        }
        if parts.is_empty() {
            // "if (!N) throw 0": nothing but flat hulls.
            return Err(Failed);
        }
        for (mut s, v) in copies {
            s.transform(&translation(v));
            parts.push(s);
        }
        let mut n = ManifoldGeometry::batch_until(OpType::Add, parts, token)
            .filter(|m| !m.is_empty() && !m.is_cancelled())
            .ok_or(Failed)?;
        // Not `to_original`: when the sum is a single hull, the batch
        // returns that hull, which already counts as one original under
        // the ID it was built with. That ID comes from Manifold's global
        // counter, drawn while sibling subtrees run on other threads, so
        // keeping it made the order of this solid's triangles in a parent
        // union depend on scheduling. The hull IDs themselves may stay
        // global: they are drawn in one call, so their relative order is
        // fixed, they are above every ID reserved before the render, and
        // they never outlive this step.
        n.to_fresh_original(own);
        result = Some(n);
    }
    result.ok_or(Failed)
}

fn translation(v: Vec3) -> crate::Matrix {
    [
        [1.0, 0.0, 0.0, v.x],
        [0.0, 1.0, 0.0, v.y],
        [0.0, 0.0, 1.0, v.z],
        [0.0, 0.0, 0.0, 1.0],
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifold_geom::GlobalIds;
    use crate::primitives;

    fn cube_at(x: f64, size: [f64; 3]) -> Geometry {
        let mut ps = primitives::cube(size, false);
        ps.transform(&translation(Vec3::new(x, 0.0, 0.0)));
        Geometry::PolySet(Arc::new(ps))
    }

    fn sum(children: &[Geometry]) -> ManifoldGeometry {
        let mut w = Vec::new();
        let mut e = Vec::new();
        let conv = |_: usize| -> Box<dyn IdSource> { Box::new(GlobalIds) };
        minkowski_3d(children, &conv, &GlobalIds, None, &mut w, &mut e).expect("a solid")
    }

    #[test]
    fn convex_pair_is_one_hull() {
        // Unit cubes at the origin and 5 units away: a 2-unit cube at
        // x = 5, with nothing left at the origin.
        let m = sum(&[cube_at(0.0, [1.0; 3]), cube_at(5.0, [1.0; 3])]);
        assert!((m.manifold.volume() - 8.0).abs() < 1e-9);
        assert_eq!(m.manifold.num_vert(), 8);
        let b = m.bounds().expect("bounds");
        assert_eq!(b, ([5.0, 0.0, 0.0], [7.0, 2.0, 2.0]));
    }

    #[test]
    fn non_convex_with_convex() {
        // An L of two boxes plus a unit cube away from the origin.
        let l = {
            let a = primitives::cube([2.0, 1.0, 1.0], false);
            let b = primitives::cube([1.0, 2.0, 1.0], false);
            let mut w = Vec::new();
            let mut e = Vec::new();
            let pa = ManifoldGeometry::from_polyset(&a, &GlobalIds, &mut w, &mut e);
            let pb = ManifoldGeometry::from_polyset(&b, &GlobalIds, &mut w, &mut e);
            Geometry::Manifold(Arc::new(pa.boolean(&pb, OpType::Add)))
        };
        let m = sum(&[l.clone(), cube_at(3.0, [1.0; 3])]);
        // The L grown by one in every direction: a 3x3x2 box less a 1x1x2
        // corner, moved by (3, 0, 0).
        assert!(
            (m.manifold.volume() - 16.0).abs() < 1e-9,
            "{}",
            m.manifold.volume()
        );
        assert_eq!(
            m.bounds().expect("bounds"),
            ([3.0, 0.0, 0.0], [6.0, 3.0, 2.0])
        );
        // Order does not matter.
        let m2 = sum(&[cube_at(3.0, [1.0; 3]), l.clone()]);
        assert!((m2.manifold.volume() - 16.0).abs() < 1e-9);
        // Non-convex with non-convex: L + L is a staircase of 4x2, 3x3 and
        // 2x4 rectangles (area 13), 2 high.
        let m3 = sum(&[l.clone(), l]);
        assert!(
            (m3.manifold.volume() - 26.0).abs() < 1e-9,
            "{}",
            m3.manifold.volume()
        );
    }

    #[test]
    fn flat_operands_fail_like_openscad() {
        // issue1671: sums of unit cubes scaled flat leave nothing but
        // OpenSCAD's warning.
        let flat = |s: [f64; 3]| {
            let mut ps = primitives::cube([1.0; 3], false);
            ps.transform(&[
                [s[0], 0.0, 0.0, 0.0],
                [0.0, s[1], 0.0, 0.0],
                [0.0, 0.0, s[2], 0.0],
                [0.0, 0.0, 0.0, 1.0],
            ]);
            Geometry::PolySet(Arc::new(ps))
        };
        let mut w = Vec::new();
        let mut e = Vec::new();
        let conv = |_: usize| -> Box<dyn IdSource> { Box::new(GlobalIds) };
        let r = minkowski_3d(
            &[
                flat([0.0, 0.0, 1.0]),
                flat([0.0, 1.0, 0.0]),
                flat([1.0, 0.0, 0.0]),
            ],
            &conv,
            &GlobalIds,
            None,
            &mut w,
            &mut e,
        );
        assert!(r.is_none_or(|m| m.is_empty()));
        assert_eq!(
            w,
            vec!["[manifold] Minkowski hard-crashed, falling back to Nef operation.".to_string()]
        );
    }

    fn solid(ps: &crate::polyset::PolySet) -> ManifoldGeometry {
        ManifoldGeometry::from_polyset(ps, &GlobalIds, &mut Vec::new(), &mut Vec::new())
    }

    fn disc(n: f64) -> eval::node::Discretizer {
        eval::node::Discretizer {
            fn_: n,
            fa: 12.0,
            fs: 2.0,
        }
    }

    /// A block with a faceted spherical dent: 48 or more reflex edges, so
    /// it is covered through its boundary rather than cut into pieces.
    fn dented_block() -> Geometry {
        let mut dent = primitives::sphere(3.0, &disc(16.0));
        dent.transform(&translation(Vec3::new(5.0, 5.0, 10.0)));
        let block = solid(&primitives::cube([10.0, 10.0, 10.0], false));
        Geometry::Manifold(Arc::new(block.boolean(&solid(&dent), OpType::Subtract)))
    }

    #[test]
    fn dented_block_is_covered_by_its_boundary() {
        let Geometry::Manifold(m) = dented_block() else {
            unreachable!()
        };
        assert!(reflex_cuts(m.manifold.as_impl()).len() > MAX_REFLEX);
        assert!(matches!(
            operand(&Geometry::Manifold(m), &GlobalIds, None),
            Ok(Operand::Boundary(_))
        ));
        // The sum with a small cube: the dent shrinks by the cube, the block
        // grows by it. Its volume lies between the two blocks' volumes.
        let m = sum(&[dented_block(), cube_at(0.0, [1.0; 3])]);
        assert_eq!(
            m.bounds().expect("bounds"),
            ([0.0, 0.0, 0.0], [11.0, 11.0, 11.0])
        );
        let v = m.manifold.volume();
        assert!(
            v > 1331.0 - 4.0 / 3.0 * std::f64::consts::PI * 27.0 / 2.0 && v < 1331.0,
            "{v}"
        );
    }

    /// A 10-unit block with a hole of `sides` sides through it: one reflex
    /// edge per side, all along the hole.
    fn holed_block(sides: f64) -> ManifoldGeometry {
        let hole = primitives::cylinder(12.0, 3.0, 3.0, true, &disc(sides));
        let block = solid(&primitives::cube([10.0; 3], true));
        block.boolean(&solid(&hole), OpType::Subtract)
    }

    /// The followup's repro: cutting the holed block along its faces' own
    /// planes left an empty flap on every cut, so the same edge was cut
    /// again and again, and the pieces doubled in triangles each time (127
    /// s and 18.8 GB summed with a sphere). Cut along bisectors, a hole of
    /// n sides is n pieces in n - 1 cuts.
    ///
    /// The 12-sided hole goes first, under a fuse: a token that fires after
    /// 40 checks (one per piece taken off the stack and one per cut; this
    /// needs 34). Face-plane cuts give it 13 pieces without running away,
    /// so a return to them fails here, quickly, before the 24-sided case,
    /// which they took past 3 GB even with a token firing at 80. That case
    /// is bounded by `convex_pieces`' own caps on cuts and triangles.
    #[test]
    fn round_hole_is_one_piece_per_facet() {
        let m = holed_block(12.0);
        let seen = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let fuse = counting_token(seen, 40);
        let pieces = convex_pieces(&m.manifold, Some(&fuse)).map(|p| p.len());
        assert_eq!(pieces, Some(12));

        let m = holed_block(24.0);
        assert_eq!(reflex_cuts(m.manifold.as_impl()).len(), 24);
        let pieces = convex_pieces(&m.manifold, None).map(|p| p.len());
        assert_eq!(pieces, Some(24));

        // And the sum is the nightly's: its STL export of this sum has a
        // volume of 2533.0873 and an area of 1099.9202 (float precision).
        let ball = Geometry::PolySet(Arc::new(primitives::sphere(2.0, &disc(24.0))));
        let s = sum(&[Geometry::Manifold(Arc::new(m)), ball]);
        let v = s.manifold.volume();
        assert!((v - 2533.0873).abs() < 1e-3, "{v}");
        let a = s.manifold.surface_area();
        assert!((a - 1099.9202).abs() < 1e-3, "{a}");
    }

    /// A token counting its checks, firing at the `fire_at`-th (never, with
    /// `usize::MAX`). A count, not a timer: timed, a busy machine can let
    /// the sum finish before the cancel, which says nothing about the
    /// checks (as in `tests/kernel_cancel.rs`).
    fn counting_token(seen: Arc<std::sync::atomic::AtomicUsize>, fire_at: usize) -> CancelToken {
        use std::sync::atomic::Ordering;
        CancelToken::new().with_check(Arc::new(move || {
            seen.fetch_add(1, Ordering::Relaxed) + 1 >= fire_at
        }))
    }

    /// A cancel that lands inside a sum stops it there: the hulls, the cuts
    /// into convex pieces and the union of each step look at the request's
    /// token. Before they did, nothing in `minkowski_3d` looked, so a
    /// cancel or a time limit waited for the whole sum (a cube with a
    /// round hole summed with a sphere ran for minutes).
    #[test]
    fn a_cancel_stops_the_sum() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let ball = Geometry::PolySet(Arc::new(primitives::sphere(2.0, &disc(24.0))));
        let children = [dented_block(), ball];
        let conv = |_: usize| -> Box<dyn IdSource> { Box::new(GlobalIds) };
        let run = |token: Option<&CancelToken>| {
            let (mut w, mut e) = (Vec::new(), Vec::new());
            let r = minkowski_3d(&children, &conv, &GlobalIds, token, &mut w, &mut e);
            (r, w, e)
        };

        // A token that never fires: every check is counted, and the sum is
        // the same, triangle for triangle, as without a token.
        let plain = run(None).0.expect("a solid");
        let total = Arc::new(AtomicUsize::new(0));
        let (r, w, _) = run(Some(&counting_token(total.clone(), usize::MAX)));
        let r = r.expect("a solid");
        assert!(w.is_empty() && !r.is_cancelled());
        let mesh = |m: &ManifoldGeometry| m.to_polyset(&crate::color::CORNFIELD);
        assert!(mesh(&r) == mesh(&plain), "an unfired token changed the sum");
        let total = total.load(Ordering::Relaxed);
        assert!(total >= 8, "only {total} checks in a whole sum");

        // Fired half way: the sum stops, with no fallback (and so no
        // "hard-crashed" warning), after few more checks.
        let seen = Arc::new(AtomicUsize::new(0));
        let (r, w, e) = run(Some(&counting_token(seen.clone(), total / 2)));
        assert!(r.is_none_or(|m| m.is_empty()));
        assert!(w.is_empty() && e.is_empty(), "{w:?} {e:?}");
        let seen = seen.load(Ordering::Relaxed);
        assert!(seen < total, "{seen} checks before stopping, of {total}");

        // Cancelled before it starts: nothing is built.
        let seen = Arc::new(AtomicUsize::new(0));
        let (r, w, _) = run(Some(&counting_token(seen.clone(), 1)));
        assert!(r.is_none() && w.is_empty());
        assert!(seen.load(Ordering::Relaxed) <= 1);
    }

    /// The hulls are built on rayon's pool; the result must not depend on
    /// how many threads there are or which finishes first.
    #[cfg(all(feature = "parallel", not(target_arch = "wasm32")))]
    #[test]
    fn same_output_at_any_thread_count() {
        let l = {
            let a = solid(&primitives::cube([20.0, 5.0, 5.0], false));
            let b = solid(&primitives::cube([5.0, 20.0, 5.0], false));
            Geometry::Manifold(Arc::new(a.boolean(&b, OpType::Add)))
        };
        let ball = Geometry::PolySet(Arc::new(primitives::sphere(2.0, &disc(24.0))));
        for children in [vec![l, ball.clone()], vec![dented_block(), ball]] {
            let run = |threads: usize| {
                let pool = rayon::ThreadPoolBuilder::new()
                    .num_threads(threads)
                    .build()
                    .expect("pool");
                pool.install(|| sum(&children).to_polyset(&crate::color::CORNFIELD))
            };
            let one = run(1);
            assert!(!one.faces.is_empty());
            for threads in [2, 8] {
                assert!(run(threads) == one, "output differs at {threads} threads");
            }
        }
    }

    #[test]
    fn two_d_squares() {
        let a = Polygon2d::from_outline(vec![[0.0, 0.0], [2.0, 0.0], [2.0, 2.0], [0.0, 2.0]]);
        let b = Polygon2d::from_outline(vec![[5.0, 0.0], [6.0, 0.0], [6.0, 1.0], [5.0, 1.0]]);
        let s = minkowski_2d(&[Some(&a), Some(&b)]).expect("a shape");
        assert_eq!(s.outlines.len(), 1);
        let mut v = s.outlines[0].vertices.clone();
        v.sort_by(|p, q| p.partial_cmp(q).expect("finite"));
        assert_eq!(v, vec![[5.0, 0.0], [5.0, 3.0], [8.0, 0.0], [8.0, 3.0]]);
        // An empty first child leaves nothing; an empty later one is skipped.
        assert!(minkowski_2d(&[None, Some(&b)]).expect("a shape").is_empty());
        assert_eq!(minkowski_2d(&[Some(&a), Some(&b), None]), Some(s));
        assert_eq!(minkowski_2d(&[None, None]), None);
    }
}
