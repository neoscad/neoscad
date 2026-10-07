//! The export render's 2D shapes and extrusions (stage 2 of the
//! exact-geometry audit's path 1).
//!
//! A 2D subtree under `linear_extrude` or `rotate_extrude` is built again
//! with every edge attributed to the exact curve it lies on
//! ([`super::super::profile`]): circles whose fragments come from
//! `$fa`/`$fs` are tessellated as the 3D primitives are (a multiple of 4
//! segments, vertices on the axes) and become exact circles; `offset(r)`
//! arcs become circles; `polygon()` and `square()` edges are lines; sketch
//! arcs are circles. Shapes only the normal render builds (text, imports,
//! hulls, projections) keep their edges as facets.
//!
//! The extrusion then tags each side triangle with the surface its edge's
//! curve sweeps: a line gives a plane, a circle a cylinder (or a cone
//! under a uniform `scale` toward its centre) in `linear_extrude`, and a
//! plane, cylinder or cone (lines by their slope) or a sphere or torus
//! (arcs) in `rotate_extrude`. What has no exact surface here (a twist, a
//! non-uniform scale, an arc scaled off its centre) is reported and built
//! by the normal render as facets.

use std::collections::BTreeMap;

use eval::node::{CsgOp, Discretizer, LinearExtrude, Node, NodeKind, OffsetJoin, SketchValues};
use eval::trig::{cos_degrees, sin_degrees};
use meshbrep::{Surface, TaggedMesh};

use super::{Res, SubstitutionKind, Walk, fn_is_explicit, is_background, similarity_scale};
use crate::clipper::{self, Join, Op2};
use crate::evaluate::Unsupported;
use crate::exact::profile::{
    self, Curve2, FACETED, Joins, Tagged2, attribute_boolean, attribute_offset, line_through,
};
use crate::polygon2d::Polygon2d;
use crate::polyset::PolySet;
use crate::{Geometry, Matrix, extrude, fragments};

/// How deep a 2D subtree is followed before the whole subtree is left to
/// the normal render. The 3D walk keeps its stack on the heap because
/// recursive modules nest groups 100,000 deep; 2D profiles that deep are
/// rare, and the normal render (which has its own heap stack) builds them
/// as facets instead of overflowing this one.
const MAX_2D_DEPTH: usize = 256;

/// Positions merged where they are bit-for-bit equal (signed zeros
/// together), and the triangles that collapse dropped. `rotate_extrude`
/// puts one vertex per section on the axis for a profile vertex at x = 0,
/// and a `scale` of 0 puts a whole ring on one point; Manifold needs them
/// shared to see a closed mesh.
fn weld(vertices: &[[f64; 3]], tris: &[[u32; 3]]) -> (Vec<[f64; 3]>, Vec<[u32; 3]>, Vec<usize>) {
    let key = |p: [f64; 3]| p.map(|x| if x == 0.0 { 0u64 } else { x.to_bits() });
    let mut first: BTreeMap<[u64; 3], u32> = BTreeMap::new();
    let mut map = Vec::with_capacity(vertices.len());
    let mut out = Vec::new();
    for &p in vertices {
        let id = *first.entry(key(p)).or_insert_with(|| {
            out.push(p);
            (out.len() - 1) as u32
        });
        map.push(id);
    }
    let mut kept = Vec::with_capacity(tris.len());
    let mut which = Vec::with_capacity(tris.len());
    for (i, t) in tris.iter().enumerate() {
        let w = t.map(|k| map[k as usize]);
        if w[0] != w[1] && w[1] != w[2] && w[0] != w[2] {
            kept.push(w);
            which.push(i);
        }
    }
    (out, kept, which)
}

/// The largest column norm of a 3D matrix's linear part: how much it can
/// stretch a length, to scale the volume allowances.
fn stretch(m: &Matrix) -> f64 {
    (0..3)
        .map(|j| (m[0][j] * m[0][j] + m[1][j] * m[1][j] + m[2][j] * m[2][j]).sqrt())
        .fold(0.0, f64::max)
}

/// A triangular PolySet's faces as triangles.
fn triangles(ps: &PolySet) -> Vec<[u32; 3]> {
    let mut out = Vec::with_capacity(ps.faces.len());
    for f in &ps.faces {
        for i in 1..f.len().saturating_sub(1) {
            out.push([f[0], f[i], f[i + 1]]);
        }
    }
    out
}

/// For each ring position (a vertex index modulo the ring size) of an
/// extrusion, its outline and vertex.
fn ring_map(poly: &Polygon2d) -> Vec<(usize, usize)> {
    poly.outlines
        .iter()
        .enumerate()
        .flat_map(|(o, ol)| (0..ol.vertices.len()).map(move |i| (o, i)))
        .collect()
}

/// The profile edge a side triangle of an extrusion sweeps, from its ring
/// positions: two neighbours in one outline. `None` for a cap triangle
/// (all three in one ring) or anything else.
fn side_edge(
    t: [u32; 3],
    stride: u32,
    rmap: &[(usize, usize)],
    poly: &Polygon2d,
) -> Option<(usize, usize)> {
    let rings = t.map(|k| k / stride);
    if rings[0] == rings[1] && rings[1] == rings[2] {
        return None;
    }
    let ks = t.map(|k| rmap[(k % stride) as usize]);
    let mut pos: Vec<(usize, usize)> = ks.to_vec();
    pos.sort_unstable();
    pos.dedup();
    if pos.len() != 2 || pos[0].0 != pos[1].0 {
        return None;
    }
    let (o, a) = pos[0];
    let b = pos[1].1;
    let n = poly.outlines[o].vertices.len();
    if b == a + 1 {
        Some((o, a))
    } else if a == 0 && b == n - 1 {
        Some((o, n - 1))
    } else {
        None
    }
}

fn edge_len(poly: &Polygon2d, o: usize, i: usize) -> f64 {
    let v = &poly.outlines[o].vertices;
    let (a, b) = (v[i], v[(i + 1) % v.len()]);
    ((b[0] - a[0]).powi(2) + (b[1] - a[1]).powi(2)).sqrt()
}

impl Walk<'_> {
    /// The 2D shape of `n`'s children, unioned and sanitized as the
    /// extruders take it, with its edges' curves. `None` when there is no
    /// 2D geometry.
    fn children_2d(&mut self, n: &Node, depth: usize) -> Result<Option<Tagged2>, Unsupported> {
        let mut kids = Vec::with_capacity(n.children.len());
        for c in n.children.iter().filter(|c| !is_background(c)) {
            kids.push(self.shape2(c, depth + 1)?);
        }
        Ok(self.boolean2(kids, Op2::Union))
    }

    /// A Clipper boolean of 2D shapes, its edges attributed to the inputs'
    /// curves. Empty children are kept in place (an empty first child of a
    /// difference leaves nothing), as `clipper::apply` takes them.
    fn boolean2(&mut self, kids: Vec<Option<Tagged2>>, op: Op2) -> Option<Tagged2> {
        if kids.iter().all(Option::is_none) {
            return None;
        }
        let refs: Vec<Option<&Polygon2d>> =
            kids.iter().map(|k| k.as_ref().map(|t| &t.poly)).collect();
        let out = clipper::apply(&refs, op);
        let sources: Vec<&Tagged2> = kids.iter().flatten().collect();
        let tags = attribute_boolean(&out, &sources, &self.curves2);
        Some(Tagged2 { poly: out, tags })
    }

    /// The export render of a 2D node.
    fn shape2(&mut self, n: &Node, depth: usize) -> Result<Option<Tagged2>, Unsupported> {
        if self.stopped() {
            return Err(Unsupported::interrupted());
        }
        if depth > MAX_2D_DEPTH {
            return self.delegate2(
                n,
                "is exported as planar facets: its 2D subtree is too deep to attribute",
            );
        }
        match &n.kind {
            NodeKind::Square { size, center } => {
                let poly = crate::primitives::square(*size, *center);
                if poly.is_empty() {
                    return Ok(None);
                }
                Ok(Some(self.lines(poly)))
            }
            NodeKind::Circle { r, disc } => self.circle2(n, *r, disc),
            NodeKind::Polygon { .. } | NodeKind::Sketch(_) => {
                let (points, paths, _) = n.kind.polygon().expect("a polygon");
                let poly = crate::primitives::polygon(points, paths);
                if poly.is_empty() {
                    return Ok(None);
                }
                let mut t = self.lines(poly);
                if let NodeKind::Sketch(s) = &n.kind {
                    self.sketch_arcs(n, &mut t, &s.report.entities);
                }
                Ok(Some(t))
            }
            NodeKind::Root
            | NodeKind::Group { .. }
            | NodeKind::Render { .. }
            | NodeKind::Color { .. }
            | NodeKind::Part { .. }
            | NodeKind::Csg(CsgOp::Union) => self.children_2d(n, depth),
            NodeKind::Csg(CsgOp::Difference) | NodeKind::Csg(CsgOp::Intersection) | NodeKind::IntersectionFor => {
                let op = if matches!(n.kind, NodeKind::Csg(CsgOp::Difference)) {
                    Op2::Difference
                } else {
                    Op2::Intersection
                };
                let mut kids = Vec::with_capacity(n.children.len());
                for c in n.children.iter().filter(|c| !is_background(c)) {
                    kids.push(self.shape2(c, depth + 1)?);
                }
                // The normal render leaves out children with no geometry
                // except the first of a difference (`applyToChildren2D`).
                let first = kids.first().cloned().flatten();
                if first.is_none() {
                    return Ok(None);
                }
                let kids: Vec<Option<Tagged2>> = kids
                    .into_iter()
                    .enumerate()
                    .filter(|(i, k)| *i == 0 || k.is_some())
                    .map(|(_, k)| k)
                    .collect();
                Ok(self.boolean2(kids, op))
            }
            NodeKind::Transform { matrix, .. } => {
                if matrix.iter().flatten().any(|v| !v.is_finite()) {
                    return Ok(None);
                }
                let Some(t) = self.children_2d_raw(n, depth)? else {
                    return Ok(None);
                };
                Ok(self.transform2(n, t, matrix))
            }
            NodeKind::Offset {
                delta, join, disc, ..
            } => {
                let Some(t) = self.children_2d(n, depth)? else {
                    return Ok(None);
                };
                Ok(Some(self.offset2(n, t, *delta, *join, disc)))
            }
            NodeKind::Cube { .. }
            | NodeKind::Sphere { .. }
            | NodeKind::Cylinder { .. }
            | NodeKind::Polyhedron { .. }
            | NodeKind::Surface { .. }
            | NodeKind::LinearExtrude(_)
            | NodeKind::RotateExtrude { .. } => Ok(None),
            NodeKind::Text(_) => self.delegate2(
                n,
                "is exported as planar facets: glyph outlines (Bézier curves) have no exact surfaces in STEP export yet",
            ),
            _ => {
                let module = super::module_name(&n.kind);
                self.delegate2(
                    n,
                    &format!("is exported as planar facets: {module}() has no exact curves in STEP export yet"),
                )
            }
        }
    }

    /// The children of a transform: a single child as it is (a transform
    /// keeps its edges' order), several unioned.
    fn children_2d_raw(&mut self, n: &Node, depth: usize) -> Result<Option<Tagged2>, Unsupported> {
        let live: Vec<&Node> = n.children.iter().filter(|c| !is_background(c)).collect();
        if live.len() == 1 {
            return self.shape2(live[0], depth + 1);
        }
        self.children_2d(n, depth)
    }

    /// A 2D node the normal render builds, every edge a facet.
    fn delegate2(&mut self, n: &Node, why: &str) -> Result<Option<Tagged2>, Unsupported> {
        let r = self.renderer.render(n, self.keys, self.opts.clone())?;
        match r.geometry {
            Some(Geometry::Polygon2d(p)) if !p.is_empty() => {
                self.note(SubstitutionKind::Faceted, n, why.to_string());
                let poly = std::sync::Arc::unwrap_or_clone(p);
                Ok(Some(Tagged2::uniform(poly, FACETED)))
            }
            _ => Ok(None),
        }
    }

    fn push_curve(&mut self, c: Curve2) -> u32 {
        self.curves2.push(c);
        (self.curves2.len() - 1) as u32
    }

    /// A shape whose every edge is a line of its own.
    fn lines(&mut self, poly: Polygon2d) -> Tagged2 {
        let mut tags = Vec::with_capacity(poly.outlines.len());
        for o in &poly.outlines {
            let n = o.vertices.len();
            let mut t = Vec::with_capacity(n);
            for i in 0..n {
                let tag = match line_through(o.vertices[i], o.vertices[(i + 1) % n]) {
                    Some(c) => self.push_curve(c),
                    None => FACETED,
                };
                t.push(tag);
            }
            tags.push(t);
        }
        Tagged2 { poly, tags }
    }

    fn circle2(
        &mut self,
        n: &Node,
        r: f64,
        disc: &Discretizer,
    ) -> Result<Option<Tagged2>, Unsupported> {
        let fragments = fragments::circular_segments(disc, r);
        if r <= 0.0 || !r.is_finite() || fragments.is_none() {
            let poly = crate::primitives::circle2d(r, disc);
            return Ok((!poly.is_empty()).then(|| self.lines(poly)));
        }
        let f = fragments.unwrap_or(3).max(3) as u32;
        if fn_is_explicit(disc) {
            self.note(
                SubstitutionKind::Polygon,
                n,
                format!("keeps its {f}-sided polygon because $fn is set; leave $fn unset (use $fa and $fs) to export a true circle"),
            );
            return Ok(Some(self.lines(crate::primitives::circle2d(r, disc))));
        }
        let segs = meshbrep::primitives::aligned_segments(f.saturating_mul(self.mult));
        let pts: Vec<[f64; 2]> = (0..segs)
            .map(|i| {
                let a = 360.0 * f64::from(i) / f64::from(segs);
                [r * cos_degrees(a), r * sin_degrees(a)]
            })
            .collect();
        self.note(
            SubstitutionKind::Exact,
            n,
            format!("is exported as an exact circle, not the {f}-sided polygon of the mesh ($fn is not set)"),
        );
        let id = self.push_curve(Curve2::Circle {
            c: [0.0, 0.0],
            r,
            step: std::f64::consts::TAU / f64::from(segs),
            chord_r: r,
            sag_normal: r * (1.0 - (std::f64::consts::PI / f64::from(f)).cos()),
        });
        Ok(Some(Tagged2::uniform(Polygon2d::from_outline(pts), id)))
    }

    /// Re-tags a sketch's polygon edges that lie on its solved arcs and
    /// circles (the polygon holds their chords) with exact circles; its
    /// lines stay lines.
    fn sketch_arcs(&mut self, n: &Node, t: &mut Tagged2, entities: &[eval::node::SketchEntity]) {
        let mut lines: Vec<([f64; 2], [f64; 2])> = Vec::new();
        let mut circles: Vec<([f64; 2], f64)> = Vec::new();
        for e in entities.iter().filter(|e| !e.construction) {
            match &e.solved {
                Some(SketchValues::Line { start, end, .. }) => lines.push((*start, *end)),
                Some(SketchValues::Arc { center, radius, .. })
                | Some(SketchValues::Circle { center, radius }) => circles.push((*center, *radius)),
                _ => {}
            }
        }
        if circles.is_empty() {
            return;
        }
        // The polygon's points are the solved entities' own points and
        // their arcs' chord ends, computed from the same solved values;
        // only rounding separates them.
        let tol = profile::tolerance(&t.poly);
        let near_line = |a: [f64; 2], b: [f64; 2]| {
            lines.iter().any(|&(s, e)| {
                let d = [e[0] - s[0], e[1] - s[1]];
                let l = (d[0] * d[0] + d[1] * d[1]).sqrt();
                l > 0.0
                    && [a, b].iter().all(|p| {
                        let w = [p[0] - s[0], p[1] - s[1]];
                        let along = (w[0] * d[0] + w[1] * d[1]) / l;
                        (w[0] * d[1] - w[1] * d[0]).abs() / l <= tol
                            && along >= -tol
                            && along <= l + tol
                    })
            })
        };
        // Per circle: its record, once an edge is found on it.
        let mut ids: Vec<Option<u32>> = vec![None; circles.len()];
        let mut found = 0usize;
        for (o, tags) in t.poly.outlines.iter().zip(t.tags.iter_mut()) {
            let m = o.vertices.len();
            for (i, tag) in tags.iter_mut().enumerate() {
                let (a, b) = (o.vertices[i], o.vertices[(i + 1) % m]);
                if near_line(a, b) {
                    continue;
                }
                let l = ((b[0] - a[0]).powi(2) + (b[1] - a[1]).powi(2)).sqrt();
                let on = |c: [f64; 2], r: f64, p: [f64; 2]| {
                    (((p[0] - c[0]).powi(2) + (p[1] - c[1]).powi(2)).sqrt() - r).abs()
                        <= tol * (1.0 + r)
                };
                let Some(k) = circles
                    .iter()
                    .position(|&(c, r)| l > 0.0 && l < r && on(c, r, a) && on(c, r, b))
                else {
                    continue;
                };
                let (c, r) = circles[k];
                let step = 2.0 * (0.5 * l / r).min(1.0).asin();
                let id = match ids[k] {
                    Some(id) => id,
                    None => {
                        let id = self.push_curve(Curve2::Circle {
                            c,
                            r,
                            step,
                            chord_r: r,
                            sag_normal: 0.0,
                        });
                        ids[k] = Some(id);
                        id
                    }
                };
                if let Curve2::Circle {
                    step: s,
                    sag_normal,
                    ..
                } = &mut self.curves2[id as usize]
                {
                    *s = s.max(step);
                    // The sketch's own polygon is what the mesh shows.
                    *sag_normal = sag_normal.max(r * (1.0 - (0.5 * step).cos()));
                }
                *tag = id;
                found += 1;
            }
        }
        if found > 0 {
            self.note(
                SubstitutionKind::Exact,
                n,
                "arcs are exported as exact circles, not the polygon of the mesh".into(),
            );
        }
    }

    /// A 2D transform of a tagged shape, its curves mapped with it.
    fn transform2(&mut self, n: &Node, mut t: Tagged2, matrix: &Matrix) -> Option<Tagged2> {
        let m2 = Polygon2d::matrix_2d(matrix);
        if t.poly.transform(&m2).is_some() {
            return None;
        }
        let lin = |v: [f64; 2]| {
            [
                m2[0][0] * v[0] + m2[0][1] * v[1],
                m2[1][0] * v[0] + m2[1][1] * v[1],
            ]
        };
        let aff = |v: [f64; 2]| {
            let l = lin(v);
            [l[0] + m2[0][2], l[1] + m2[1][2]]
        };
        let (c0, c1) = (lin([1.0, 0.0]), lin([0.0, 1.0]));
        let (n0, n1) = (c0[0] * c0[0] + c0[1] * c0[1], c1[0] * c1[0] + c1[1] * c1[1]);
        let similar =
            (n0 - n1).abs() <= 1e-10 * n0 && (c0[0] * c1[0] + c0[1] * c1[1]).abs() <= 1e-10 * n0;
        let s = n0.sqrt();
        let mut map: BTreeMap<u32, u32> = BTreeMap::new();
        let mut ellipse = false;
        for tags in &mut t.tags {
            for tag in tags.iter_mut() {
                if *tag == FACETED {
                    continue;
                }
                if let Some(&k) = map.get(tag) {
                    *tag = k;
                    continue;
                }
                let new = match self.curves2[*tag as usize] {
                    Curve2::Line { p, d } => {
                        let q = lin(d);
                        let l = (q[0] * q[0] + q[1] * q[1]).sqrt();
                        Curve2::Line {
                            p: aff(p),
                            d: [q[0] / l, q[1] / l],
                        }
                    }
                    Curve2::Circle {
                        c,
                        r,
                        step,
                        chord_r,
                        sag_normal,
                    } if similar => Curve2::Circle {
                        c: aff(c),
                        r: r * s,
                        step,
                        chord_r: chord_r * s,
                        sag_normal: sag_normal * s,
                    },
                    Curve2::Circle { .. } => {
                        ellipse = true;
                        Curve2::Faceted
                    }
                    Curve2::Faceted => Curve2::Faceted,
                };
                let k = if new == Curve2::Faceted {
                    FACETED
                } else {
                    self.push_curve(new)
                };
                map.insert(*tag, k);
                *tag = k;
            }
        }
        if ellipse {
            self.note(
                SubstitutionKind::Faceted,
                n,
                "is exported as planar facets: a non-uniform scale or shear makes its circles ellipses, which STEP export cannot extrude exactly yet".into(),
            );
        }
        // A mirror reverses every outline; Clipper puts them right, as
        // the normal render does.
        if t.poly.sanitized && crate::polygon2d::det3(&m2) <= 0.0 {
            let out = clipper::sanitize(&t.poly);
            let tags = attribute_boolean(&out, &[&t], &self.curves2);
            return Some(Tagged2 { poly: out, tags });
        }
        Some(t)
    }

    /// `offset()`: `chamfer` is already in `join` (Square), as the
    /// evaluator builds the node.
    fn offset2(
        &mut self,
        n: &Node,
        t: Tagged2,
        delta: f64,
        join: OffsetJoin,
        disc: &Discretizer,
    ) -> Tagged2 {
        let (cj, joins, tolerance) = match join {
            OffsetJoin::Round => {
                let f = fragments::circular_segments(disc, delta.abs())
                    .unwrap_or(3)
                    .max(3) as u32;
                if fn_is_explicit(disc) || delta == 0.0 {
                    let tol = delta.abs() * (1.0 - cos_degrees(180.0 / f64::from(f)));
                    if delta != 0.0 {
                        self.note(
                            SubstitutionKind::Polygon,
                            n,
                            format!("keeps its {f}-sided rounding because $fn is set; leave $fn unset (use $fa and $fs) to export true arcs"),
                        );
                    }
                    (Join::Round, Joins::Polygon, tol)
                } else {
                    let segs = meshbrep::primitives::aligned_segments(f.saturating_mul(self.mult));
                    let step = std::f64::consts::TAU / f64::from(segs);
                    let tol = delta.abs() * (1.0 - (0.5 * step).cos());
                    self.note(
                        SubstitutionKind::Exact,
                        n,
                        format!("rounds with exact arcs, not the {f}-sided polygon of the mesh ($fn is not set)"),
                    );
                    (
                        Join::Round,
                        Joins::Arcs {
                            step,
                            sag_normal: delta.abs()
                                * (1.0 - (std::f64::consts::PI / f64::from(f)).cos()),
                        },
                        tol,
                    )
                }
            }
            OffsetJoin::Miter => (Join::Miter, Joins::Straight, 1.0),
            OffsetJoin::Square => (Join::Square, Joins::Straight, 1.0),
        };
        let out = clipper::offset(&t.poly, delta, cj, 1_000_000.0, tolerance);
        let tags = attribute_offset(&out, &t, delta, joins, &mut self.curves2);
        Tagged2 { poly: out, tags }
    }

    /// `linear_extrude`, side faces on the exact surfaces their edges
    /// sweep.
    pub(super) fn linear_extrude(
        &mut self,
        n: &Node,
        e: &LinearExtrude,
        m: &Matrix,
    ) -> Result<Res, Unsupported> {
        let (sx, sy) = (e.scale[0], e.scale[1]);
        let why = if e.twist != 0.0 {
            Some(
                "is exported as planar facets: a twisted extrusion's side faces are helical, which STEP export cannot write exactly yet",
            )
        } else if sx != sy {
            Some(
                "is exported as planar facets: a non-uniform scale bends its side faces, which STEP export cannot write exactly yet",
            )
        } else if e.has_segments && e.segments > 0 {
            Some("is exported as planar facets: its edges are split into segments")
        } else {
            None
        };
        if let Some(why) = why {
            return self.delegate(n, m, Some(why.into()));
        }
        if e.height[2] <= 0.0
            || !e.height.iter().all(|h| h.is_finite())
            || !sx.is_finite()
            || sx < 0.0
        {
            return self.delegate(n, m, None);
        }
        let Some(prof) = self.children_2d(n, 0)? else {
            return Ok(Res::Nothing);
        };
        if prof.poly.is_empty() {
            return Ok(Res::Solid(manifold_rust::manifold::Manifold::empty()));
        }
        let ps = extrude::linear_extrude(e, &prof.poly);
        let stride: u32 = prof
            .poly
            .outlines
            .iter()
            .map(|o| o.vertices.len() as u32)
            .sum();
        let slices = (ps.vertices.len() as u32 / stride.max(1)).saturating_sub(1);
        let rmap = ring_map(&prof.poly);
        let (h1, h2) = if e.center {
            (e.height.map(|h| -h / 2.0), e.height.map(|h| h / 2.0))
        } else {
            ([0.0; 3], e.height)
        };
        let hv = [h2[0] - h1[0], h2[1] - h1[1], h2[2] - h1[2]];
        let vertical = hv[0] == 0.0 && hv[1] == 0.0;
        let s = sx;
        let mut surfaces = vec![
            Surface::Plane {
                origin: [0.0, 0.0, h1[2]],
                normal: [0.0, 0.0, -1.0],
            },
            Surface::Plane {
                origin: [0.0, 0.0, h2[2]],
                normal: [0.0, 0.0, 1.0],
            },
            Surface::Faceted,
        ];
        let mut of_curve: BTreeMap<u32, u32> = BTreeMap::new();
        let (mut cylinders, mut cones) = (false, false);
        let mut oblique_arcs = false;
        let mut bound = 0.0f64;
        let mut sag = 0.0f64;
        let tol = profile::tolerance(&prof.poly);
        let height = hv[2];
        for tags in &prof.tags {
            for &tag in tags {
                if of_curve.contains_key(&tag) {
                    continue;
                }
                let surf = match self.curves2[tag as usize] {
                    Curve2::Line { p, d } => {
                        let b0 = [p[0] + h1[0], p[1] + h1[1], h1[2]];
                        let t0 = [s * p[0] + h2[0], s * p[1] + h2[1], h2[2]];
                        let w = [t0[0] - b0[0], t0[1] - b0[1], t0[2] - b0[2]];
                        let nrm = [d[1] * w[2], -d[0] * w[2], d[0] * w[1] - d[1] * w[0]];
                        let l = (nrm[0] * nrm[0] + nrm[1] * nrm[1] + nrm[2] * nrm[2]).sqrt();
                        if l > 0.0 {
                            Some(Surface::Plane {
                                origin: b0,
                                normal: nrm.map(|x| x / l),
                            })
                        } else {
                            None
                        }
                    }
                    Curve2::Circle { c, r, .. } if vertical && s == 1.0 => {
                        Some(Surface::Cylinder {
                            origin: [c[0], c[1], 0.0],
                            axis: [0.0, 0.0, 1.0],
                            radius: r,
                        })
                    }
                    Curve2::Circle { c, r, .. }
                        if vertical && s != 1.0 && c[0].abs() <= tol && c[1].abs() <= tol =>
                    {
                        // The radius goes from r to s r: the cone's apex
                        // is where it would reach 0.
                        if s < 1.0 {
                            Some(Surface::Cone {
                                apex: [0.0, 0.0, h1[2] + height / (1.0 - s)],
                                axis: [0.0, 0.0, -1.0],
                                slope: r * (1.0 - s) / height,
                            })
                        } else {
                            Some(Surface::Cone {
                                apex: [0.0, 0.0, h1[2] - height / (s - 1.0)],
                                axis: [0.0, 0.0, 1.0],
                                slope: r * (s - 1.0) / height,
                            })
                        }
                    }
                    Curve2::Circle { .. } => {
                        oblique_arcs = true;
                        None
                    }
                    Curve2::Faceted => None,
                };
                let id = match surf {
                    Some(sf) => {
                        cylinders |= matches!(sf, Surface::Cylinder { .. });
                        cones |= matches!(sf, Surface::Cone { .. });
                        surfaces.push(sf);
                        (surfaces.len() - 1) as u32
                    }
                    None => 2,
                };
                of_curve.insert(tag, id);
            }
        }
        for (o, tags) in prof.tags.iter().enumerate() {
            for (i, &tag) in tags.iter().enumerate() {
                if let Curve2::Circle { sag_normal, .. } = self.curves2[tag as usize] {
                    let k = s.max(1.0);
                    bound += edge_len(&prof.poly, o, i) * k * height.abs() * sag_normal * k;
                    sag = sag.max(sag_normal * k);
                }
            }
        }
        let tris = triangles(&ps);
        let mut tri_surface = Vec::with_capacity(tris.len());
        for t in &tris {
            let sid = match side_edge(*t, stride, &rmap, &prof.poly) {
                Some((o, i)) => of_curve[&prof.tags[o][i]],
                None => {
                    if t[0] / stride == 0 {
                        0
                    } else if t[0] / stride == slices {
                        1
                    } else {
                        2
                    }
                }
            };
            tri_surface.push(sid);
        }
        if oblique_arcs {
            self.note(
                SubstitutionKind::Faceted,
                n,
                "is exported with planar facets on its arcs: an arc extruded obliquely, or scaled off its centre, sweeps an oblique cone or an elliptic cylinder, which STEP export cannot write exactly yet".into(),
            );
        }
        let scale = similarity_scale(m);
        if cylinders || cones {
            if scale.is_some() {
                self.note(
                    SubstitutionKind::Exact,
                    n,
                    format!(
                        "is exported with exact {} on its arcs",
                        if cones { "cones" } else { "cylinders" }
                    ),
                );
            } else {
                self.note(
                    SubstitutionKind::Faceted,
                    n,
                    "is exported with planar facets on its arcs: a non-uniform scale or shear makes them elliptic, which STEP export cannot write exactly yet".into(),
                );
            }
        }
        let k = stretch(m);
        self.normal_sagitta = self.normal_sagitta.max(sag * k);
        self.normal_volume_bound += bound * k * k * k;
        self.place_mesh(&ps.vertices, &tris, &tri_surface, surfaces, m, scale)
    }

    /// `rotate_extrude`, side faces on the surfaces of revolution their
    /// edges sweep.
    pub(super) fn rotate_extrude(
        &mut self,
        n: &Node,
        angle: f64,
        start: f64,
        disc: &Discretizer,
        m: &Matrix,
    ) -> Result<Res, Unsupported> {
        if angle == 0.0 || !angle.is_finite() || !start.is_finite() {
            return self.delegate(n, m, None);
        }
        let Some(prof) = self.children_2d(n, 0)? else {
            return Ok(Res::Nothing);
        };
        if prof.poly.is_empty() {
            return Ok(Res::Nothing);
        }
        let (mut min_x, mut max_x) = (0.0f64, 0.0f64);
        for v in prof.poly.outlines.iter().flat_map(|o| o.vertices.iter()) {
            min_x = min_x.min(v[0]);
            max_x = max_x.max(v[0]);
        }
        if min_x < 0.0 {
            // Across the axis is an error, and a profile left of it is
            // mirrored: both are the normal render's to report or build.
            return self.delegate(
                n,
                m,
                Some("is exported as planar facets: its profile lies left of the axis".into()),
            );
        }
        let normal_sections = extrude::rotate_sections(angle, disc, &prof.poly).max(1);
        let turn = angle.abs().min(360.0);
        if fn_is_explicit(disc) {
            self.note(
                SubstitutionKind::Polygon,
                n,
                format!("keeps its {normal_sections} sections because $fn is set; leave $fn unset (use $fa and $fs) to export exact surfaces of revolution"),
            );
            let Some(Ok(Some(ps))) = extrude::rotate_extrude_with(
                angle,
                start,
                disc,
                &prof.poly,
                &crate::primitives::never,
            ) else {
                return self.delegate(n, m, None);
            };
            let tris = triangles(&ps);
            let (pos, wt, _) = weld(&ps.vertices, &tris);
            let mut surfaces = Vec::with_capacity(wt.len());
            let mut tri_surface = Vec::with_capacity(wt.len());
            for t in &wt {
                let [a, b, c] = t.map(|k| pos[k as usize]);
                let u = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
                let w = [c[0] - a[0], c[1] - a[1], c[2] - a[2]];
                let nrm = [
                    u[1] * w[2] - u[2] * w[1],
                    u[2] * w[0] - u[0] * w[2],
                    u[0] * w[1] - u[1] * w[0],
                ];
                let l = (nrm[0] * nrm[0] + nrm[1] * nrm[1] + nrm[2] * nrm[2]).sqrt();
                surfaces.push(if l > 0.0 {
                    Surface::Plane {
                        origin: a,
                        normal: nrm.map(|x| x / l),
                    }
                } else {
                    Surface::Faceted
                });
                tri_surface.push((surfaces.len() - 1) as u32);
            }
            return self.place_mesh(&pos, &wt, &tri_surface, surfaces, m, similarity_scale(m));
        }
        let full = fragments::circular_segments(disc, max_x)
            .unwrap_or(3)
            .max(3) as u32;
        let n_full = meshbrep::primitives::aligned_segments(full.saturating_mul(self.mult));
        let fine = Discretizer {
            fn_: f64::from(n_full),
            fa: disc.fa,
            fs: disc.fs,
        };
        let Some(Ok(Some(ps))) = extrude::rotate_extrude_with(
            angle,
            start,
            &fine,
            &prof.poly,
            &crate::primitives::never,
        ) else {
            return self.delegate(n, m, None);
        };
        let sections = extrude::rotate_sections(angle, &fine, &prof.poly).max(1) as u32;
        let closed = angle == 360.0;
        let stride: u32 = prof
            .poly
            .outlines
            .iter()
            .map(|o| o.vertices.len() as u32)
            .sum();
        let rmap = ring_map(&prof.poly);
        let tol = profile::tolerance(&prof.poly);
        let z = [0.0, 0.0, 1.0];
        let plane_at = |a: f64| Surface::Plane {
            origin: [0.0; 3],
            normal: [-sin_degrees(a), cos_degrees(a), 0.0],
        };
        let mut surfaces = vec![plane_at(start), plane_at(start + angle), Surface::Faceted];
        let mut of_curve: BTreeMap<u32, u32> = BTreeMap::new();
        let mut spindle = false;
        for tags in &prof.tags {
            for &tag in tags {
                if of_curve.contains_key(&tag) {
                    continue;
                }
                let surf = match self.curves2[tag as usize] {
                    Curve2::Line { p, d } => {
                        if d[0].abs() < 1e-12 {
                            (p[0] > tol).then_some(Surface::Cylinder {
                                origin: [0.0; 3],
                                axis: z,
                                radius: p[0],
                            })
                        } else if d[1].abs() < 1e-12 {
                            Some(Surface::Plane {
                                origin: [0.0, 0.0, p[1]],
                                normal: z,
                            })
                        } else {
                            // Where the line meets the axis, and which way
                            // along it the radius grows.
                            let za = p[1] - p[0] * d[1] / d[0];
                            let dxdz = d[0] / d[1];
                            Some(Surface::Cone {
                                apex: [0.0, 0.0, za],
                                axis: if dxdz > 0.0 { z } else { [0.0, 0.0, -1.0] },
                                slope: dxdz.abs(),
                            })
                        }
                    }
                    Curve2::Circle { c, r, .. } => {
                        if c[0].abs() <= tol {
                            Some(Surface::Sphere {
                                center: [0.0, 0.0, c[1]],
                                radius: r,
                            })
                        } else if c[0] > r + tol {
                            Some(Surface::Torus {
                                center: [0.0, 0.0, c[1]],
                                axis: z,
                                major_radius: c[0],
                                minor_radius: r,
                            })
                        } else {
                            spindle = true;
                            None
                        }
                    }
                    Curve2::Faceted => None,
                };
                let id = match surf {
                    Some(sf) => {
                        surfaces.push(sf);
                        (surfaces.len() - 1) as u32
                    }
                    None => 2,
                };
                of_curve.insert(tag, id);
            }
        }
        // The normal render's sections stand inside the exact surfaces by
        // the sagitta of their angle, and its arcs inside their circles.
        let fac =
            1.0 - (std::f64::consts::PI * turn / 360.0 / f64::from(normal_sections as u32)).cos();
        let mut bound = 0.0f64;
        let mut arc_sag = 0.0f64;
        for (o, tags) in prof.tags.iter().enumerate() {
            let v = &prof.poly.outlines[o].vertices;
            for (i, &tag) in tags.iter().enumerate() {
                let sag_arc = match self.curves2[tag as usize] {
                    Curve2::Circle { sag_normal, .. } => sag_normal,
                    _ => 0.0,
                };
                arc_sag = arc_sag.max(sag_arc);
                let xm = 0.5 * (v[i][0] + v[(i + 1) % v.len()][0]) + sag_arc;
                bound += edge_len(&prof.poly, o, i)
                    * std::f64::consts::TAU
                    * xm
                    * (turn / 360.0)
                    * (sag_arc + max_x * fac);
            }
        }
        let tris = triangles(&ps);
        let mut tri_surface = Vec::with_capacity(tris.len());
        let nv = ps.vertices.len() as u32;
        for t in &tris {
            let sid = match side_edge(*t, stride, &rmap, &prof.poly) {
                Some((o, i)) => of_curve[&prof.tags[o][i]],
                None => {
                    let ring = t[0] / stride;
                    if closed {
                        2
                    } else if ring == 0 {
                        0
                    } else if ring == sections || t[0] >= nv - stride {
                        1
                    } else {
                        2
                    }
                }
            };
            tri_surface.push(sid);
        }
        if spindle {
            self.note(
                SubstitutionKind::Faceted,
                n,
                "is exported with planar facets on an arc that reaches past the axis (a self-intersecting torus), which STEP export cannot write exactly yet".into(),
            );
        }
        let scale = similarity_scale(m);
        self.note(
            if scale.is_some() {
                SubstitutionKind::Exact
            } else {
                SubstitutionKind::Faceted
            },
            n,
            if scale.is_some() {
                format!("is exported with exact surfaces of revolution, not the {normal_sections} sections of the mesh ($fn is not set)")
            } else {
                "is exported with planar facets on its curved faces: a non-uniform scale or shear makes them elliptic, which STEP export cannot write exactly yet".into()
            },
        );
        let k = stretch(m);
        self.normal_sagitta = self.normal_sagitta.max((arc_sag + max_x * fac) * k);
        self.normal_volume_bound += bound * k * k * k;
        self.place_mesh(&ps.vertices, &tris, &tri_surface, surfaces, m, scale)
    }

    /// A built mesh (welded here) with its surface per triangle, placed
    /// under `m` as one new original.
    fn place_mesh(
        &mut self,
        vertices: &[[f64; 3]],
        tris: &[[u32; 3]],
        tri_surface: &[u32],
        surfaces: Vec<Surface>,
        m: &Matrix,
        scale: Option<f64>,
    ) -> Result<Res, Unsupported> {
        let (pos, wt, which) = weld(vertices, tris);
        let t = TaggedMesh {
            positions: pos,
            triangles: wt,
            triangle_surface: which.iter().map(|&i| tri_surface[i]).collect(),
            surfaces,
        };
        self.place(&t, m, scale)
    }
}
