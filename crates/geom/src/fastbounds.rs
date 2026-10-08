//! The fast path for `child_bounds()` (`--enable query`;
//! `docs/language-extensions.md`, sections 5.4 and 11.6): the box a
//! subtree renders to, found without the kernels where that is provably
//! the same box, bit for bit.
//!
//! The renderer spends its time in booleans: Manifold for 3D unions,
//! Clipper for 2D ones. Everything else a box depends on is cheap and is
//! done here with the renderer's own functions, so it rounds the same:
//! the primitives are generated as `evaluate::compute` generates them,
//! transforms are applied one node at a time with the same `transform`,
//! extrusions are built with the same extruders, and 2D hulls with the
//! same hull. Only one step is replaced by reasoning about the box: **a
//! 3D union of several solids** is the union of their boxes. The extreme
//! vertex in each direction of a union is an extreme vertex of one of its
//! operands (new vertices lie on two surfaces, so inside both boxes), and
//! Manifold keeps input positions as they are.
//!
//! A box is all that is known after that step, so a transform above it
//! (which needs the vertices) makes the fast path decline, as does
//! everything else whose box needs the kernel: a 2D union of more than
//! one child (Clipper rounds to its 2^-27 grid and simplifies), a 3D hull
//! (QuickHull's tolerance drops points a bit outside a face), difference,
//! intersection, `minkowski`, `offset`, `projection`, `resize`, `fill`,
//! imports, text and parts. A union drops an operand with no volume, so a
//! polyhedron (which may be open) and anything flattened by a singular
//! transform are taken only where no union converts them. When the fast
//! path declines, the oracle renders.
//!
//! None of this is assumed: `crates/session/tests/fastbounds.rs` renders
//! generated trees and requires the fast path's box to equal the rendered
//! one wherever the fast path answers.

use std::sync::Arc;

use eval::limits::{Guard, Limit};
use eval::node::{CsgOp, Node, NodeKind};
use eval::oracle::Bounds;

use crate::Geometry;
use crate::evaluate::{is_background, leaf_2d, transform};
use crate::polygon2d::Polygon2d;
use crate::{extrude, fragments, hull, primitives};

/// The most vertices the fast path builds before it leaves a subtree to
/// the renderer: a few tens of megabytes, so a query never holds much
/// more than the render it saves would.
const MAX_VERTICES: usize = 1 << 20;

/// The deepest tree the fast path walks (it recurses); deeper subtrees,
/// such as long recursive modules, are rendered.
const MAX_DEPTH: usize = 256;

/// What a subtree gives, as far as the fast path knows it.
enum Fast {
    /// No geometry (`None` in the renderer).
    Nothing,
    /// Exactly the renderer's geometry. `closed` says a union may convert
    /// it to a solid without losing it (primitives and extrusions; not a
    /// polyhedron, which may be open).
    Geo { g: Geometry, closed: bool },
    /// A 3D result of the kernel whose box alone is known.
    Box { min: [f64; 3], max: [f64; 3] },
}

impl Fast {
    fn dimension(&self) -> Option<u32> {
        match self {
            Fast::Nothing => None,
            Fast::Geo { g, .. } => Some(g.dimension()),
            Fast::Box { .. } => Some(3),
        }
    }

    fn is_empty(&self) -> bool {
        match self {
            Fast::Nothing => true,
            Fast::Geo { g, .. } => g.is_empty(),
            Fast::Box { .. } => false,
        }
    }
}

struct Walk<'a> {
    guard: Option<&'a Guard>,
    vertices: usize,
}

/// The box `subtree` renders to, when it can be found without the
/// kernels; `None` when only a render can tell. `guard` is the
/// evaluation's: a primitive past a fragment or triangle limit is left to
/// the renderer, which reports it.
pub fn bounds(subtree: &Node, guard: Option<&Guard>) -> Option<Bounds> {
    let mut w = Walk { guard, vertices: 0 };
    Some(match w.node(subtree, 0)? {
        Fast::Nothing => Bounds::Empty,
        Fast::Geo { g, .. } => match &g {
            Geometry::PolySet(ps) => match ps.bounds() {
                Some((min, max)) => Bounds::Solid { min, max },
                None => Bounds::Empty,
            },
            Geometry::Polygon2d(p) => match p.bounds() {
                Some((min, max)) => Bounds::Flat { min, max },
                None => Bounds::Empty,
            },
            // The walk never builds a solid.
            Geometry::Manifold(_) => return None,
        },
        Fast::Box { min, max } => Bounds::Solid { min, max },
    })
}

impl Walk<'_> {
    /// Charge `n` vertices; `None` past the budget.
    fn spend(&mut self, n: usize) -> Option<()> {
        self.vertices = self.vertices.checked_add(n)?;
        (self.vertices <= MAX_VERTICES).then_some(())
    }

    /// Whether `asked` of `l` passes the guard's limit: the renderer
    /// would stop there with an error, so the render must answer.
    fn over(&self, l: Limit, asked: f64) -> bool {
        self.guard
            .is_some_and(|g| g.exceeds(l, asked, "").is_some())
    }

    fn leaf(&mut self, g: Geometry, closed: bool) -> Option<Fast> {
        let n = match &g {
            Geometry::PolySet(ps) => ps.vertices.len(),
            Geometry::Polygon2d(p) => p.outlines.iter().map(|o| o.vertices.len()).sum(),
            Geometry::Manifold(_) => return None,
        };
        self.spend(n)?;
        Some(Fast::Geo { g, closed })
    }

    /// A circle's fragment count, if it is within the limits and the
    /// vertex budget (`tris` turns fragments into triangles, `verts` into
    /// vertices).
    fn fragments(
        &mut self,
        disc: &eval::node::Discretizer,
        r: f64,
        tris: impl Fn(f64) -> f64,
        verts: impl Fn(f64) -> f64,
    ) -> Option<()> {
        if !(r > 0.0 && r.is_finite()) {
            return Some(());
        }
        let f = f64::from(fragments::circular_segments(disc, r).unwrap_or(3));
        if self.over(Limit::Fragments, f) || self.over(Limit::Triangles, tris(f)) {
            return None;
        }
        (verts(f) <= MAX_VERTICES as f64).then_some(())
    }

    fn node(&mut self, n: &Node, depth: usize) -> Option<Fast> {
        if depth > MAX_DEPTH {
            return None;
        }
        let stop = primitives::never;
        match &n.kind {
            NodeKind::Cube { size, center } => self.leaf(
                Geometry::PolySet(Arc::new(primitives::cube(*size, *center))),
                true,
            ),
            NodeKind::Sphere { r, disc } => {
                self.fragments(disc, *r, |f| f * (f + 1.0), |f| f * (f + 1.0))?;
                let s = primitives::sphere_with(*r, disc, &stop)?;
                self.leaf(Geometry::PolySet(Arc::new(s)), true)
            }
            NodeKind::Cylinder {
                h,
                r1,
                r2,
                center,
                disc,
            } => {
                self.fragments(disc, r1.max(*r2), |f| 4.0 * f, |f| 2.0 * f)?;
                let c = primitives::cylinder_with(*h, *r1, *r2, *center, disc, &stop)?;
                self.leaf(Geometry::PolySet(Arc::new(c)), true)
            }
            NodeKind::Polyhedron { points, faces, .. } => {
                self.spend(points.len())?;
                let p = primitives::polyhedron(points, faces);
                self.leaf(Geometry::PolySet(Arc::new(p)), false)
            }
            NodeKind::Square { size, center } => {
                self.leaf(leaf_2d(primitives::square(*size, *center)), true)
            }
            NodeKind::Circle { r, disc } => {
                self.fragments(disc, *r, |f| f, |f| f)?;
                let c = primitives::circle2d_with(*r, disc, &stop)?;
                self.leaf(leaf_2d(c), true)
            }
            k @ (NodeKind::Polygon { .. } | NodeKind::Sketch(_)) => {
                let (points, paths, _) = k.polygon()?;
                self.spend(points.len())?;
                self.leaf(leaf_2d(primitives::polygon(points, paths)), true)
            }
            NodeKind::Root
            | NodeKind::Group { .. }
            | NodeKind::Render { .. }
            | NodeKind::Csg(CsgOp::Union) => self.union(n, depth),
            NodeKind::Color { .. } => {
                // A colour changes no position: on a mesh it sets the
                // faces' colour, on a solid it relabels its IDs, and on 2D
                // it is dropped.
                self.union(n, depth)
            }
            NodeKind::Transform { matrix, .. } => {
                if matrix.iter().flatten().any(|v| !v.is_finite()) {
                    // The renderer removes the object, with a warning.
                    return Some(Fast::Nothing);
                }
                match self.union(n, depth)? {
                    Fast::Nothing => Some(Fast::Nothing),
                    Fast::Geo { g, closed } => Some(Fast::Geo {
                        g: transform(g, matrix, &mut Vec::new()),
                        // A singular matrix flattens a solid to no volume,
                        // which a union then drops (found by the
                        // differential test: `scale([1, 1, 0])` beside
                        // another child, and a shear whose determinant
                        // rounds to 6e-17 rather than 0).
                        closed: closed && !singular(matrix),
                    }),
                    // A solid's box does not give its transformed box.
                    Fast::Box { .. } => None,
                }
            }
            NodeKind::Hull => self.hull(n, depth),
            NodeKind::LinearExtrude(e) => {
                let Some(p) = self.profile(n, depth)? else {
                    return Some(Fast::Nothing);
                };
                if e.height[2] > 0.0 {
                    let slices = f64::from(extrude::num_slices(e, &p));
                    let ring: usize = p.outlines.iter().map(|o| o.vertices.len()).sum();
                    if self.over(Limit::Slices, slices)
                        || self.over(Limit::Triangles, 2.0 * slices * ring as f64)
                        || (slices + 1.0) * ring as f64 > MAX_VERTICES as f64
                    {
                        return None;
                    }
                }
                let ps = extrude::linear_extrude_with(e, &p, &stop)?;
                self.leaf(Geometry::PolySet(Arc::new(ps)), true)
            }
            NodeKind::RotateExtrude {
                angle, start, disc, ..
            } => {
                let Some(p) = self.profile(n, depth)? else {
                    return Some(Fast::Nothing);
                };
                if *angle != 0.0 {
                    let f = f64::from(extrude::rotate_sections(*angle, disc, &p));
                    let ring: usize = p.outlines.iter().map(|o| o.vertices.len()).sum();
                    if self.over(Limit::Fragments, f)
                        || self.over(Limit::Triangles, 2.0 * f * ring as f64)
                        || (f + 1.0) * ring as f64 > MAX_VERTICES as f64
                    {
                        return None;
                    }
                }
                match extrude::rotate_extrude_with(*angle, *start, disc, &p, &stop)? {
                    Ok(Some(ps)) => self.leaf(Geometry::PolySet(Arc::new(ps)), true),
                    // No geometry, or OpenSCAD's error for a profile across
                    // the axis: nothing either way.
                    Ok(None) | Err(_) => Some(Fast::Nothing),
                }
            }
            NodeKind::IntersectionFor
            | NodeKind::Csg(CsgOp::Intersection | CsgOp::Difference)
            | NodeKind::Fill
            | NodeKind::Offset { .. }
            | NodeKind::Projection { .. }
            | NodeKind::Minkowski { .. }
            | NodeKind::Resize { .. }
            | NodeKind::Surface { .. }
            | NodeKind::Import(_)
            | NodeKind::Text(_)
            | NodeKind::Part { .. } => None,
        }
    }

    /// The children that count (background ones are left out, as every
    /// operation leaves them out), each as the fast path knows it, and
    /// their dimension; `None` when one is unknown or 2D and 3D mix.
    fn children(&mut self, n: &Node, depth: usize) -> Option<(Vec<Fast>, Option<u32>)> {
        let mut out = Vec::with_capacity(n.children.len());
        let mut dim = None;
        for c in &n.children {
            if is_background(c) {
                continue;
            }
            let f = self.node(c, depth + 1)?;
            match (dim, f.dimension()) {
                (_, None) => {}
                (None, d) => dim = d,
                (Some(a), Some(b)) if a != b => return None,
                _ => {}
            }
            out.push(f);
        }
        Some((out, dim))
    }

    /// `applyToChildren(UNION)`: one child passes through; several 3D ones
    /// give the union of their boxes; several 2D ones need Clipper.
    fn union(&mut self, n: &Node, depth: usize) -> Option<Fast> {
        let (mut kids, dim) = self.children(n, depth)?;
        match dim {
            None => Some(Fast::Nothing),
            Some(2) => match kids.len() {
                1 => kids.pop(),
                // Every child, empty ones too, makes a Clipper union.
                _ => None,
            },
            _ => {
                if kids.len() == 1 {
                    return kids.pop();
                }
                let mut actual: Vec<Fast> = kids.into_iter().filter(|k| !k.is_empty()).collect();
                match actual.len() {
                    0 => Some(Fast::Nothing),
                    1 => actual.pop(),
                    _ => {
                        let mut boxes = Vec::with_capacity(actual.len());
                        for k in &actual {
                            boxes.push(match k {
                                Fast::Geo {
                                    g: Geometry::PolySet(ps),
                                    closed: true,
                                } => ps.bounds()?,
                                Fast::Box { min, max } => (*min, *max),
                                _ => return None,
                            });
                        }
                        union_box(&boxes).map(|(min, max)| Fast::Box { min, max })
                    }
                }
            }
        }
    }

    /// `hull()` in 2D: the renderer's own hull of the children's outlines.
    fn hull(&mut self, n: &Node, depth: usize) -> Option<Fast> {
        let (kids, dim) = self.children(n, depth)?;
        match dim {
            None => Some(Fast::Nothing),
            Some(2) => {
                let polys: Vec<Option<&Polygon2d>> = kids
                    .iter()
                    .map(|k| match k {
                        Fast::Geo {
                            g: Geometry::Polygon2d(p),
                            ..
                        } if !p.is_empty() => Some(&**p),
                        _ => None,
                    })
                    .collect();
                let h = hull::hull_2d(&polys);
                self.leaf(Geometry::Polygon2d(Arc::new(h)), true)
            }
            // Manifold's QuickHull leaves out points within its tolerance
            // of a face, even ones a last bit outside it, so the box of the
            // points is not always the hull's (the differential test found
            // a cube's half-width of 8/3 against a revolved vertex one bit
            // past it): only the render knows.
            _ => None,
        }
    }

    /// The 2D union of an extrusion's children as the renderer takes it
    /// (`children_2d_union`): `Some(None)` for no shape, `None` when it
    /// needs Clipper (several children) or a child is 3D or unknown.
    fn profile(&mut self, n: &Node, depth: usize) -> Option<Option<Polygon2d>> {
        let (mut kids, dim) = self.children(n, depth)?;
        if dim == Some(3) {
            return None;
        }
        match kids.len() {
            0 => Some(None),
            1 => match kids.pop()? {
                Fast::Nothing => Some(None),
                Fast::Geo {
                    g: Geometry::Polygon2d(p),
                    ..
                } => Some((!p.is_empty()).then(|| Arc::unwrap_or_clone(p))),
                _ => None,
            },
            _ => None,
        }
    }
}

/// Whether `m`'s linear part flattens space, up to rounding: its
/// determinant is tiny next to the product of its rows' lengths (the
/// largest it could be).
fn singular(m: &crate::Matrix) -> bool {
    let row = |r: usize| (m[r][0] * m[r][0] + m[r][1] * m[r][1] + m[r][2] * m[r][2]).sqrt();
    let bound = row(0) * row(1) * row(2);
    let d = crate::polyset::determinant3(m).abs();
    d.is_nan() || d <= 1e-9 * bound
}

/// The box of a Manifold union of solids with boxes `boxes`, where that
/// is the union of the boxes for certain; `None` where the kernel's
/// tolerance could decide otherwise. Manifold merges what lies within its
/// tolerance of another surface, so an operand's extreme a hair beyond
/// the next one (a revolved vertex one bit past a cube's face, found by
/// the differential test) may not survive; nor may an operand thinner
/// than the tolerance, which the union drops as having no volume.
fn union_box(boxes: &[([f64; 3], [f64; 3])]) -> Option<([f64; 3], [f64; 3])> {
    let scale = boxes
        .iter()
        .flat_map(|(lo, hi)| lo.iter().chain(hi).copied())
        .fold(1.0f64, |m, v| m.max(v.abs()));
    let tol = UNION_CLEARANCE * scale;
    if boxes
        .iter()
        .any(|(lo, hi)| (0..3).any(|i| (hi[i] - lo[i]).is_nan() || hi[i] - lo[i] <= tol))
    {
        return None;
    }
    let mut min = [f64::INFINITY; 3];
    let mut max = [f64::NEG_INFINITY; 3];
    for i in 0..3 {
        // The extreme and the runner-up from another operand: equal is
        // fine (the same value either way), apart by more than the
        // tolerance is fine, in between is the kernel's call.
        let mut lows: Vec<f64> = boxes.iter().map(|b| b.0[i]).collect();
        let mut highs: Vec<f64> = boxes.iter().map(|b| b.1[i]).collect();
        lows.sort_by(f64::total_cmp);
        highs.sort_by(|a, b| b.total_cmp(a));
        let close = |a: f64, b: f64| a != b && (a - b).abs() <= tol;
        if close(lows[0], lows[1]) || close(highs[0], highs[1]) {
            return None;
        }
        min[i] = lows[0];
        max[i] = highs[0];
    }
    Some((min, max))
}

/// How far apart, relative to the coordinates' size, two operands'
/// extremes must be (or how thick an operand) for a union's box to be
/// theirs for certain: far above Manifold's own tolerance (about 1e-12
/// relative).
const UNION_CLEARANCE: f64 = 1e-6;
