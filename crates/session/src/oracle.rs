//! The geometry oracle (`eval::oracle`): `child_bounds()` and
//! `child_measure()` (`--enable query`) render the child they ask about
//! through the request's own `geom::Renderer`, during evaluation.
//!
//! **One cache.** The oracle renders into the renderer the final render
//! uses, so the subtree a query built is a cache hit when the model is
//! rendered: a query costs one render of its child, not two. A cache
//! entry's original IDs are rebased onto each render's own blocks
//! (`geom::evaluate`'s `IdRef`), so what a query left in the cache cannot
//! change the bytes of the export (`tests/query.rs` compares exports with
//! and without queries, warm and cold).
//!
//! **Messages.** A query render's messages are dropped: the child's
//! warnings are printed once, by the final render, where they would be
//! without the query. For that the final render must replay the messages
//! of the cached nodes it finds first (`geom::RenderOptions::replay`),
//! which the session always does; the command line does it when a query
//! has rendered (`Oracle::asked`).
//!
//! **The fast path.** `child_bounds()` asks only for a box, which for
//! primitives under transforms, unions and hulls is found without the
//! kernels (`geom::fastbounds`); the oracle tries that first and renders
//! when it declines. It is used only where `tests/fastbounds.rs` shows it
//! equal to the rendered box, bit for bit.
//!
//! **Distances.** `child_distance()` renders both children and measures
//! as `measure --between` does (`crate::measure::between`): 0 when they
//! overlap, else the exact smallest distance between their surfaces (in
//! 2D, between their outlines' triangles in the plane).
//!
//! **Answers.** The box is the result's own (minima and maxima, which no
//! order of evaluation changes). Areas and volumes are serial sums in mesh
//! order: over a 2D result's outlines, over a mesh's faces, or over a
//! solid's triangles (`crate::mesh::Mesh`), so they are the same at any
//! thread count and on every platform.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use eval::oracle::{Bounds, Distance, Facts, GeometryOracle, OracleError};
use geom::Geometry;
use geom::polyset::PolySet;

/// A [`GeometryOracle`] over a renderer and the render settings of the
/// request it belongs to.
pub struct Oracle {
    renderer: Arc<geom::Renderer>,
    /// The request's render settings; the interrupt flag and limits come
    /// with each query (the evaluation's own).
    opts: geom::RenderOptions,
    /// Answers already given in this request, by the subtree's result
    /// key: the same child asked about twice (a loop, a repeated call)
    /// is measured once. Kept per request, so it never outlives the
    /// sources or limits it was computed under.
    answers: Mutex<HashMap<u128, Facts>>,
    /// Distances already measured, by the two subtrees' result keys.
    distances: Mutex<HashMap<(u128, u128), Distance>>,
    asked: AtomicBool,
}

impl std::fmt::Debug for Oracle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Oracle")
            .field("opts", &self.opts)
            .field("asked", &self.asked)
            .finish()
    }
}

impl Oracle {
    /// An oracle rendering through `renderer` with `opts`, which should be
    /// the final render's settings (its scheme, files, fonts and replay
    /// epoch). `force` is ignored: a query measures what a render gives,
    /// and the box, area and volume do not depend on the conversion.
    pub fn new(renderer: Arc<geom::Renderer>, mut opts: geom::RenderOptions) -> Oracle {
        opts.force = false;
        opts.interrupt = None;
        opts.guard = None;
        Oracle {
            renderer,
            opts,
            answers: Mutex::new(HashMap::new()),
            distances: Mutex::new(HashMap::new()),
            asked: AtomicBool::new(false),
        }
    }

    /// Whether any query has asked this oracle, so rendered into the
    /// cache (a host whose renders do not replay messages must then
    /// replay them, or the child's warnings would be lost).
    pub fn asked(&self) -> bool {
        self.asked.load(Ordering::Relaxed)
    }
}

impl Oracle {
    /// `subtree` rendered through the shared renderer under the
    /// evaluation's interrupt flag and limits, with its result key.
    fn render(
        &self,
        subtree: &eval::Node,
        keys: &eval::dump::Keys,
        interrupt: Option<&Arc<AtomicBool>>,
        guard: Option<&Arc<eval::limits::Guard>>,
    ) -> Result<Option<Geometry>, OracleError> {
        self.asked.store(true, Ordering::Relaxed);
        let mut opts = self.opts.clone();
        opts.interrupt = interrupt.cloned();
        opts.guard = guard.cloned();
        let r =
            self.renderer
                .render(subtree, keys, opts)
                .map_err(|u| match u.is_interrupted() {
                    true => OracleError::Interrupted,
                    false => {
                        OracleError::Unsupported(format!("{}() is not implemented yet", u.what))
                    }
                })?;
        Ok(r.geometry)
    }
}

impl GeometryOracle for Oracle {
    fn bounds(
        &self,
        subtree: &eval::Node,
        interrupt: Option<&Arc<AtomicBool>>,
        guard: Option<&Arc<eval::limits::Guard>>,
    ) -> Result<Bounds, OracleError> {
        if let Some(b) = geom::fastbounds::bounds(subtree, guard.map(|g| &**g)) {
            return Ok(b);
        }
        self.measure(subtree, interrupt, guard).map(|f| f.bounds())
    }

    fn distance(
        &self,
        a: &eval::Node,
        b: &eval::Node,
        interrupt: Option<&Arc<AtomicBool>>,
        guard: Option<&Arc<eval::limits::Guard>>,
    ) -> Result<Distance, OracleError> {
        let (keys_a, keys_b) = (
            eval::dump::Keys::new(a, &*self.opts.fs),
            eval::dump::Keys::new(b, &*self.opts.fs),
        );
        let key = (geom::result_key(a, &keys_a), geom::result_key(b, &keys_b));
        if let Some(d) = self
            .distances
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&key)
        {
            return Ok(*d);
        }
        let ga = self.render(a, &keys_a, interrupt, guard)?;
        let gb = self.render(b, &keys_b, interrupt, guard)?;
        let d = distance(ga.as_ref(), gb.as_ref());
        self.distances
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(key, d);
        Ok(d)
    }

    fn measure(
        &self,
        subtree: &eval::Node,
        interrupt: Option<&Arc<AtomicBool>>,
        guard: Option<&Arc<eval::limits::Guard>>,
    ) -> Result<Facts, OracleError> {
        let keys = eval::dump::Keys::new(subtree, &*self.opts.fs);
        let key = geom::result_key(subtree, &keys);
        if let Some(f) = self
            .answers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&key)
        {
            self.asked.store(true, Ordering::Relaxed);
            return Ok(*f);
        }
        let g = self.render(subtree, &keys, interrupt, guard)?;
        let f = facts(g.as_ref());
        self.answers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(key, f);
        Ok(f)
    }
}

/// The facts of a render's result.
pub fn facts(g: Option<&Geometry>) -> Facts {
    let Some(g) = g.filter(|g| !g.is_empty()) else {
        return Facts::Empty;
    };
    match g {
        Geometry::Polygon2d(p) => {
            let Some((min, max)) = p.bounds() else {
                return Facts::Empty;
            };
            // Outlines are sanitised (outer ones counter-clockwise, holes
            // clockwise), so the signed areas add up to the shape's, as
            // `stats::geometry` adds them.
            let area: f64 = p.outlines.iter().map(|o| shoelace(&o.vertices)).sum();
            Facts::Flat {
                min,
                max,
                area: area.abs(),
            }
        }
        Geometry::PolySet(ps) => solid_of_mesh(ps),
        Geometry::Manifold(m) => {
            let Some((min, max)) = m.bounds() else {
                return Facts::Empty;
            };
            let (volume, surface_area, _) = crate::mesh::Mesh::of_solid(m).mass();
            Facts::Solid {
                min,
                max,
                volume,
                surface_area,
            }
        }
    }
}

/// The distance between two rendered results, as `measure --between`
/// finds it between parts: 0 when they overlap by more than a sliver (one
/// inside the other included, which their surfaces alone would not
/// show), otherwise the smallest distance between their surfaces. 2D
/// shapes are compared in the plane: overlap by a Clipper intersection,
/// distance between their triangulations at z = 0. Everything is serial,
/// so the answer is the same at any thread count.
pub fn distance(a: Option<&Geometry>, b: Option<&Geometry>) -> Distance {
    use crate::mesh::{Bvh, Mesh};
    let (Some(a), Some(b)) = (a.filter(|g| !g.is_empty()), b.filter(|g| !g.is_empty())) else {
        return Distance::Empty;
    };
    if a.dimension() != b.dimension() {
        return Distance::Mixed;
    }
    let area = |p: &geom::polygon2d::Polygon2d| -> f64 {
        p.outlines
            .iter()
            .map(|o| shoelace(&o.vertices))
            .sum::<f64>()
            .abs()
    };
    let (ma, mb) = match (a, b) {
        (Geometry::Polygon2d(p), Geometry::Polygon2d(q)) => {
            let both =
                geom::clipper::apply(&[Some(&**p), Some(&**q)], geom::clipper::Op2::Intersection);
            if area(&both) > 1e-12_f64.max(1e-9 * area(p).min(area(q))) {
                return Distance::Apart(0.0);
            }
            let flat = |p: &geom::polygon2d::Polygon2d| {
                let ps = p.tessellate();
                Mesh {
                    verts: ps.vertices.clone(),
                    tris: ps
                        .faces
                        .iter()
                        .filter(|f| f.len() == 3)
                        .map(|f| [f[0], f[1], f[2]])
                        .collect(),
                    ..Mesh::default()
                }
            };
            (flat(p), flat(q))
        }
        _ => {
            let (sa, sb) = (crate::stats::solid(a), crate::stats::solid(b));
            let both = sa.boolean(&sb, geom::manifold_geom::OpType::Intersect);
            let (ma, mb) = (Mesh::of_solid(&sa), Mesh::of_solid(&sb));
            // Serial volumes in mesh order, as the other answers are; the
            // floor is `measure --between`'s.
            let (va, vb) = (ma.mass().0, mb.mass().0);
            let overlap = Mesh::of_solid(&both).mass().0;
            if overlap > 1e-9_f64.max(1e-9 * va.min(vb)) {
                return Distance::Apart(0.0);
            }
            (ma, mb)
        }
    };
    match Bvh::new(&ma).closest(&ma, &Bvh::new(&mb), &mb) {
        Some((d, _, _)) => Distance::Apart(d),
        None => Distance::Empty,
    }
}

/// The signed area of a closed outline (positive counter-clockwise).
fn shoelace(v: &[[f64; 2]]) -> f64 {
    let n = v.len();
    (0..n)
        .map(|i| {
            let (a, b) = (v[i], v[(i + 1) % n]);
            a[0] * b[1] - b[0] * a[1]
        })
        .sum::<f64>()
        / 2.0
}

/// A mesh result (a primitive, a polyhedron, an extrusion, or their
/// transforms): its box over the vertices its faces use, and its volume
/// and surface area from a fan of each face, which is exact for the
/// planar faces such results have. Faces point outward, so the signed
/// tetrahedra from the origin add up to the enclosed volume.
fn solid_of_mesh(ps: &PolySet) -> Facts {
    let Some((min, max)) = ps.bounds() else {
        return Facts::Empty;
    };
    let (mut volume, mut surface_area) = (0.0, 0.0);
    for f in &ps.faces {
        let Some(&first) = f.first() else {
            continue;
        };
        let a = ps.vertices[first as usize];
        for w in f[1..].windows(2) {
            let (b, c) = (ps.vertices[w[0] as usize], ps.vertices[w[1] as usize]);
            let n = crate::mesh::cross(crate::mesh::sub(b, a), crate::mesh::sub(c, a));
            surface_area += crate::mesh::norm(n) / 2.0;
            volume += crate::mesh::dot(a, crate::mesh::cross(b, c)) / 6.0;
        }
    }
    // A polyhedron written inside out encloses the same volume.
    Facts::Solid {
        min,
        max,
        volume: volume.abs(),
        surface_area,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_unit_cube_mesh_measures_one() {
        let ps = PolySet {
            vertices: vec![
                [0.0, 0.0, 0.0],
                [1.0, 0.0, 0.0],
                [1.0, 1.0, 0.0],
                [0.0, 1.0, 0.0],
                [0.0, 0.0, 1.0],
                [1.0, 0.0, 1.0],
                [1.0, 1.0, 1.0],
                [0.0, 1.0, 1.0],
            ],
            // Counter-clockwise seen from outside.
            faces: vec![
                vec![0, 3, 2, 1],
                vec![4, 5, 6, 7],
                vec![0, 1, 5, 4],
                vec![1, 2, 6, 5],
                vec![2, 3, 7, 6],
                vec![3, 0, 4, 7],
            ],
            colors: Vec::new(),
            color_indices: Vec::new(),
            convex: Some(true),
            triangular: false,
        };
        let Facts::Solid {
            min,
            max,
            volume,
            surface_area,
        } = solid_of_mesh(&ps)
        else {
            panic!("a solid")
        };
        assert_eq!((min, max), ([0.0; 3], [1.0; 3]));
        // The tetrahedra from the origin round (to 1 - 2^-53 here).
        assert!((volume - 1.0).abs() < 1e-12, "{volume}");
        assert!((surface_area - 6.0).abs() < 1e-12, "{surface_area}");
    }
}
