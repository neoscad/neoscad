//! Polygon meshes as OpenSCAD's `PolySet` holds them
//! (`src/geometry/PolySet.{h,cc}`, `PolySetUtils.cc`): shared vertices,
//! faces as vertex index lists of any length, and optional per-face colours.

use manifold_rust::polygon::triangulate_idx;
use manifold_rust::types::PolyVert;
use manifold_rust::linalg::Vec2;

use crate::color::Color;
use crate::Matrix;

/// A 3D polygon mesh.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PolySet {
    pub vertices: Vec<[f64; 3]>,
    /// Faces, counter-clockwise seen from outside.
    pub faces: Vec<Vec<u32>>,
    /// The palette `color_indices` point into.
    pub colors: Vec<Color>,
    /// One entry per face (-1: no colour), or empty when no face is coloured.
    pub color_indices: Vec<i32>,
    /// `convex_`: `Some(true)` for primitives OpenSCAD knows are convex,
    /// `None` when it has to be computed.
    pub convex: Option<bool>,
    /// Every face is a triangle.
    pub triangular: bool,
}

/// Warnings a mesh operation produced, in order.
pub type Warnings = Vec<String>;

impl PolySet {
    pub fn is_empty(&self) -> bool {
        self.faces.is_empty()
    }

    /// `PolySet::setColor`: one colour for every face.
    pub fn set_color(&mut self, c: Color) {
        self.colors = vec![c];
        self.color_indices = vec![0; self.faces.len()];
    }

    /// `PolySet::transform`: move the vertices, and reverse every face when
    /// the matrix mirrors, so the mesh does not end up inside out.
    pub fn transform(&mut self, m: &Matrix) {
        for v in &mut self.vertices {
            *v = apply(m, *v);
        }
        if determinant3(m) < 0.0 {
            for f in &mut self.faces {
                f.reverse();
            }
        }
    }

    /// Axis-aligned bounds of the vertices faces use, or `None` if empty.
    pub fn bounds(&self) -> Option<([f64; 3], [f64; 3])> {
        let mut it = self.faces.iter().flatten().map(|&i| self.vertices[i as usize]);
        let first = it.next()?;
        Some(it.fold((first, first), |(lo, hi), v| {
            (std::array::from_fn(|k| lo[k].min(v[k])), std::array::from_fn(|k| hi[k].max(v[k])))
        }))
    }

    /// `PolySet::isConvex`: known for primitives, otherwise tested. OpenSCAD
    /// asks CGAL (`is_approximately_convex`); this checks that no vertex lies
    /// in front of any face's plane, which agrees on closed meshes.
    pub fn is_convex(&self) -> bool {
        if self.is_empty() {
            return true;
        }
        if let Some(c) = self.convex {
            return c;
        }
        let scale = self.bounds().map_or(1.0, |(lo, hi)| (0..3).map(|k| hi[k] - lo[k]).fold(0.0, f64::max));
        let eps = 1e-9 * scale.max(1.0);
        for f in &self.faces {
            let pts: Vec<[f64; 3]> = f.iter().map(|&i| self.vertices[i as usize]).collect();
            let n = newell(&pts);
            let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
            if len == 0.0 {
                continue;
            }
            let p0 = pts[0];
            for v in &self.vertices {
                let d = (0..3).map(|k| n[k] / len * (v[k] - p0[k])).sum::<f64>();
                if d > eps {
                    return false;
                }
            }
        }
        true
    }

    /// `PolySetUtils::tessellate_faces`: split every face into triangles.
    ///
    /// Like OpenSCAD it drops faces with fewer than three vertices (with the
    /// warning "PolySet has degenerate polygons"), removes consecutive
    /// vertices that coincide in `float` precision, drops vertices no face
    /// uses, and keeps each face's colour on its triangles. OpenSCAD
    /// triangulates with libtess2; this projects each face onto the plane
    /// of its Newell normal and ear-clips it with Manifold's triangulator,
    /// which gives the same surface for planar faces but may choose other
    /// diagonals (and so other shading) for non-planar ones.
    pub fn tessellate(&self, warnings: &mut Warnings) -> PolySet {
        let mut out = PolySet { convex: self.convex, triangular: true, ..Default::default() };
        if self.triangular {
            let mut c = self.clone();
            c.triangular = true;
            return c;
        }
        let has_colors = !self.color_indices.is_empty();
        let f32v = |i: u32| self.vertices[i as usize].map(|c| c as f32);
        let mut degenerate = 0;
        let mut used = vec![false; self.vertices.len()];
        let mut polygons: Vec<(Vec<u32>, i32)> = Vec::with_capacity(self.faces.len());
        for (i, face) in self.faces.iter().enumerate() {
            if face.len() < 3 {
                degenerate += 1;
                continue;
            }
            let mut cur: Vec<u32> = Vec::with_capacity(face.len());
            for &ind in face {
                if cur.last().is_none_or(|&b| f32v(ind) != f32v(b)) {
                    cur.push(ind);
                }
            }
            let head = f32v(cur[0]);
            while cur.last().is_some_and(|&b| f32v(b) == head) {
                cur.pop();
            }
            if cur.len() < 3 {
                continue;
            }
            for &ind in &cur {
                used[ind as usize] = true;
            }
            polygons.push((cur, if has_colors { self.color_indices[i] } else { -1 }));
        }
        let mut map = vec![u32::MAX; self.vertices.len()];
        for (i, v) in self.vertices.iter().enumerate() {
            if used[i] {
                map[i] = out.vertices.len() as u32;
                out.vertices.push(*v);
            }
        }
        if has_colors {
            out.colors = self.colors.clone();
        }
        for (face, color) in polygons {
            let face: Vec<u32> = face.iter().map(|&i| map[i as usize]).collect();
            let tris = if face.len() == 3 { vec![[face[0], face[1], face[2]]] } else { triangulate_face(&out.vertices, &face) };
            for t in tris {
                out.faces.push(t.to_vec());
                if has_colors {
                    out.color_indices.push(color);
                }
            }
        }
        if degenerate > 0 {
            warnings.push("PolySet has degenerate polygons".into());
        }
        out
    }
}

/// Apply a 4x4 row-major affine matrix to a point.
pub fn apply(m: &Matrix, v: [f64; 3]) -> [f64; 3] {
    std::array::from_fn(|r| m[r][0] * v[0] + m[r][1] * v[1] + m[r][2] * v[2] + m[r][3])
}

/// Determinant of the full 4x4 matrix, as Eigen's `matrix().determinant()`
/// computes it for `Transform3d`. For the affine matrices transforms build
/// (last row 0 0 0 1) this is the determinant of the linear part.
pub fn determinant3(m: &Matrix) -> f64 {
    let a = m;
    let minor = |r: [usize; 3], c: [usize; 3]| -> f64 {
        a[r[0]][c[0]] * (a[r[1]][c[1]] * a[r[2]][c[2]] - a[r[1]][c[2]] * a[r[2]][c[1]])
            - a[r[0]][c[1]] * (a[r[1]][c[0]] * a[r[2]][c[2]] - a[r[1]][c[2]] * a[r[2]][c[0]])
            + a[r[0]][c[2]] * (a[r[1]][c[0]] * a[r[2]][c[1]] - a[r[1]][c[1]] * a[r[2]][c[0]])
    };
    // Laplace expansion along the last row.
    let rows = [0, 1, 2];
    let cols = |skip: usize| -> [usize; 3] {
        let mut c = [0; 3];
        let mut k = 0;
        for j in 0..4 {
            if j != skip {
                c[k] = j;
                k += 1;
            }
        }
        c
    };
    (0..4)
        .map(|j| {
            let sign = if (3 + j) % 2 == 0 { 1.0 } else { -1.0 };
            let e = a[3][j];
            if e == 0.0 { 0.0 } else { sign * e * minor(rows, cols(j)) }
        })
        .sum()
}

/// Newell's method: a normal for a possibly non-planar polygon, with length
/// twice its projected area.
pub fn newell(pts: &[[f64; 3]]) -> [f64; 3] {
    let mut n = [0.0; 3];
    for i in 0..pts.len() {
        let a = pts[i];
        let b = pts[(i + 1) % pts.len()];
        n[0] += (a[1] - b[1]) * (a[2] + b[2]);
        n[1] += (a[2] - b[2]) * (a[0] + b[0]);
        n[2] += (a[0] - b[0]) * (a[1] + b[1]);
    }
    n
}

/// Triangulate one face (vertex indices into `verts`), keeping its winding.
fn triangulate_face(verts: &[[f64; 3]], face: &[u32]) -> Vec<[u32; 3]> {
    let pts: Vec<[f64; 3]> = face.iter().map(|&i| verts[i as usize]).collect();
    let n = newell(&pts);
    // Drop the axis the face is most perpendicular to; flip one kept axis if
    // the normal points down it, so the projection stays counter-clockwise.
    let axis = (0..3).max_by(|&a, &b| n[a].abs().total_cmp(&n[b].abs())).unwrap_or(2);
    let (u, v) = match axis {
        0 => (1, 2),
        1 => (2, 0),
        _ => (0, 1),
    };
    let flip = n[axis] < 0.0;
    let poly: Vec<PolyVert> = pts
        .iter()
        .enumerate()
        .map(|(k, p)| {
            let x = if flip { -p[u] } else { p[u] };
            PolyVert { pos: Vec2::new(x, p[v]), idx: k as i32 }
        })
        .collect();
    let tris = triangulate_idx(&vec![poly], -1.0, true);
    let mut out: Vec<[u32; 3]> = tris
        .iter()
        .map(|t| [face[t.x as usize], face[t.y as usize], face[t.z as usize]])
        .collect();
    if out.is_empty() {
        // A face with no area (all points collinear): fan it so the mesh
        // keeps its edges, as libtess2's zero-area output would be dropped
        // but its neighbours still reference the vertices.
        out = (1..face.len() - 1).map(|k| [face[0], face[k], face[k + 1]]).collect();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn concave_face_tessellates_to_its_area() {
        // An L-shaped hexagon in the z = 0 plane, counter-clockwise.
        let verts = vec![[0.0, 0.0, 0.0], [2.0, 0.0, 0.0], [2.0, 1.0, 0.0], [1.0, 1.0, 0.0], [1.0, 2.0, 0.0], [0.0, 2.0, 0.0]];
        let ps = PolySet { vertices: verts.clone(), faces: vec![(0..6).collect()], ..Default::default() };
        let t = ps.tessellate(&mut Vec::new());
        assert_eq!(t.faces.len(), 4);
        let area: f64 = t
            .faces
            .iter()
            .map(|f| {
                let p: Vec<[f64; 3]> = f.iter().map(|&i| t.vertices[i as usize]).collect();
                newell(&p)[2] / 2.0
            })
            .sum();
        assert!((area - 3.0).abs() < 1e-12, "{area}");
    }

    #[test]
    fn degenerate_faces_warn() {
        let ps = PolySet { vertices: vec![[0.0; 3]; 3], faces: vec![vec![0, 1]], ..Default::default() };
        let mut w = Vec::new();
        let t = ps.tessellate(&mut w);
        assert!(t.faces.is_empty());
        assert_eq!(w, ["PolySet has degenerate polygons"]);
    }

    #[test]
    fn mirror_flips_faces() {
        let mut ps = PolySet { vertices: vec![[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]], faces: vec![vec![0, 1, 2]], ..Default::default() };
        let mut m = crate::IDENTITY;
        m[0][0] = -1.0;
        ps.transform(&m);
        assert_eq!(ps.faces[0], vec![2, 1, 0]);
        assert_eq!(ps.vertices[0], [-1.0, 0.0, 0.0]);
    }
}
