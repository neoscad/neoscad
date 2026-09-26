//! 2D geometry as OpenSCAD's `Polygon2d` holds it
//! (`src/geometry/Polygon2d.{h,cc}`): a list of closed outlines that may
//! overlap and nest, plus a flag saying whether they have been "sanitized"
//! by Clipper (no self-intersections, outer outlines counter-clockwise and
//! holes clockwise). Leaves come out of `primitives` unsanitized unless
//! OpenSCAD marks them otherwise, and the evaluator sanitizes them with
//! [`crate::clipper`] before anything else sees them.

use crate::Matrix;
use crate::polyset::PolySet;

/// One closed outline (`Outline2d`); shared with the `io` crate's readers
/// and writers.
pub use io::Outline;

/// Outlines of a 2D shape (`Polygon2d`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Polygon2d {
    pub outlines: Vec<Outline>,
    /// `isSanitized()`: the outlines are Clipper output (or known to be as
    /// good as, like a square or a circle).
    pub sanitized: bool,
}

impl Polygon2d {
    /// `Polygon2d(Outline2d)`: a single outline is taken as sanitized, the
    /// way `square()` and `circle()` build theirs.
    pub fn from_outline(vertices: Vec<[f64; 2]>) -> Polygon2d {
        Polygon2d { outlines: vec![Outline::new(vertices)], sanitized: true }
    }

    pub fn is_empty(&self) -> bool {
        self.outlines.is_empty()
    }

    /// Axis-aligned bounds of every vertex, or `None` without vertices.
    pub fn bounds(&self) -> Option<([f64; 2], [f64; 2])> {
        let mut it = self.outlines.iter().flat_map(|o| o.vertices.iter());
        let first = *it.next()?;
        Some(it.fold((first, first), |(lo, hi), v| ([lo[0].min(v[0]), lo[1].min(v[1])], [hi[0].max(v[0]), hi[1].max(v[1])])))
    }

    /// The 2D part of a 3D transform (`GeometryEvaluator.cc:754-758`: rows
    /// and columns 0, 1 and 3).
    pub fn matrix_2d(m: &Matrix) -> [[f64; 3]; 3] {
        [[m[0][0], m[0][1], m[0][3]], [m[1][0], m[1][1], m[1][3]], [m[3][0], m[3][1], m[3][3]]]
    }

    /// `Polygon2d::transform(Transform2d)`. A singular matrix empties the
    /// shape with OpenSCAD's warning, which is returned; a zero-area shape
    /// would otherwise reach Clipper and the extruders as degenerate
    /// outlines.
    pub fn transform(&mut self, m: &[[f64; 3]; 3]) -> Option<&'static str> {
        if det3(m) == 0.0 {
            self.outlines.clear();
            return Some("Scaling a 2D object with 0 - removing object");
        }
        for o in &mut self.outlines {
            for p in o.vertices.iter_mut() {
                let (x, y) = (p[0], p[1]);
                // Eigen multiplies the 3x3 matrix by (x, y, 1), summing
                // left to right with each later term fused (see
                // `extrude::apply`); the last term is `m02 * 1`, an exact add.
                *p = [m[0][1].mul_add(y, m[0][0] * x) + m[0][2], m[1][1].mul_add(y, m[1][0] * x) + m[1][2]];
            }
        }
        None
    }

    /// `Polygon2d::is_convex`: one outline with no right turn.
    pub fn is_convex(&self) -> bool {
        if self.outlines.len() > 1 {
            return false;
        }
        let Some(o) = self.outlines.first() else { return true };
        let pts = &o.vertices;
        let n = pts.len();
        for i in 0..n {
            let a = pts[i];
            let b = pts[(i + 1) % n];
            let c = pts[(i + 2) % n];
            let d1 = [b[0] - a[0], b[1] - a[1]];
            let d2 = [c[0] - b[0], c[1] - b[1]];
            if d1[0] * d2[1] - d1[1] * d2[0] < 0.0 {
                return false;
            }
        }
        true
    }

    /// `Polygon2d::tessellate` for the Manifold backend
    /// (`ManifoldUtils::createTriangulatedPolySetFromPolygon2d`): the
    /// outlines' vertices in order at z = 0, triangulated by Manifold's
    /// `Triangulate`. OpenSCAD builds with `USE_MANIFOLD_TRIANGULATOR` on
    /// (`CMakeLists.txt:42`), so libtess2 is not involved. The extruders
    /// rely on the vertices being kept in order, one per outline vertex.
    pub fn tessellate(&self) -> PolySet {
        use manifold_rust::linalg::Vec2;
        let mut ps = PolySet { triangular: true, ..Default::default() };
        let mut polys: Vec<Vec<Vec2>> = Vec::with_capacity(self.outlines.len());
        for o in &self.outlines {
            ps.vertices.extend(o.vertices.iter().map(|v| [v[0], v[1], 0.0]));
            polys.push(o.vertices.iter().map(|v| Vec2::new(v[0], v[1])).collect());
        }
        let tris = manifold_rust::polygon::triangulate(&polys, -1.0, true);
        ps.faces = tris.iter().map(|t| vec![t.x as u32, t.y as u32, t.z as u32]).collect();
        ps
    }
}

/// Determinant of a 3x3 matrix as Eigen computes it
/// (`bruteforce_det3_helper`: cofactors of the first row), so the `== 0`
/// and `<= 0` tests agree with OpenSCAD's on near-singular matrices.
pub fn det3(m: &[[f64; 3]; 3]) -> f64 {
    let h = |a: usize, b: usize, c: usize| m[0][a] * (m[1][b] * m[2][c] - m[1][c] * m[2][b]);
    h(0, 1, 2) - h(1, 0, 2) + h(2, 0, 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn convexity() {
        let sq = Polygon2d::from_outline(vec![[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]]);
        assert!(sq.is_convex());
        let l = Polygon2d::from_outline(vec![[0.0, 0.0], [2.0, 0.0], [2.0, 1.0], [1.0, 1.0], [1.0, 2.0], [0.0, 2.0]]);
        assert!(!l.is_convex());
    }

    #[test]
    fn zero_scale_empties() {
        let mut sq = Polygon2d::from_outline(vec![[0.0, 0.0], [1.0, 0.0], [1.0, 1.0]]);
        let w = sq.transform(&[[0.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]]);
        assert_eq!(w, Some("Scaling a 2D object with 0 - removing object"));
        assert!(sq.is_empty());
    }

    #[test]
    fn tessellation_keeps_vertices() {
        let l = Polygon2d::from_outline(vec![[0.0, 0.0], [2.0, 0.0], [2.0, 1.0], [1.0, 1.0], [1.0, 2.0], [0.0, 2.0]]);
        let ps = l.tessellate();
        assert_eq!(ps.vertices.len(), 6);
        assert_eq!(ps.faces.len(), 4);
    }
}
