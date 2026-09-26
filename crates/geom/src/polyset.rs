//! Polygon meshes as OpenSCAD's `PolySet` holds them
//! (`src/geometry/PolySet.{h,cc}`, `PolySetUtils.cc`): shared vertices,
//! faces as vertex index lists of any length, and optional per-face colours.

use manifold_rust::linalg::Vec2;
use manifold_rust::polygon::triangulate_idx;
use manifold_rust::types::PolyVert;

use crate::Matrix;
use crate::color::Color;

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

    /// The mesh fields, as the `io` writers take them.
    pub fn mesh(&self) -> io::MeshRef<'_> {
        io::MeshRef {
            vertices: &self.vertices,
            faces: &self.faces,
            colors: &self.colors,
            color_indices: &self.color_indices,
        }
    }

    /// A mesh a reader built (`PolySetBuilder::build`): convexity unknown,
    /// triangular when every face is a triangle.
    pub fn from_mesh(m: io::Mesh) -> PolySet {
        let triangular = m.faces.iter().all(|f| f.len() <= 3);
        PolySet {
            vertices: m.vertices,
            faces: m.faces,
            colors: m.colors,
            color_indices: m.color_indices,
            convex: None,
            triangular,
        }
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
        let mut it = self
            .faces
            .iter()
            .flatten()
            .map(|&i| self.vertices[i as usize]);
        let first = it.next()?;
        Some(it.fold((first, first), |(lo, hi), v| {
            (
                std::array::from_fn(|k| lo[k].min(v[k])),
                std::array::from_fn(|k| hi[k].max(v[k])),
            )
        }))
    }

    /// `PolySet::isConvex`: known for primitives, otherwise
    /// `CGALUtils::is_approximately_convex` (`cgalutils.cc:178-253`): the
    /// mesh must be closed (every directed edge used once and its reverse
    /// present, edges compared by position), connected, and no two faces
    /// sharing an edge may bend inwards by more than 0.1 degrees. Linear in
    /// the number of faces, so it is cheap for large imported meshes.
    pub fn is_convex(&self) -> bool {
        if self.is_empty() {
            return true;
        }
        if let Some(c) = self.convex {
            return c;
        }
        type Edge = [u64; 6];
        let key = |a: [f64; 3], b: [f64; 3]| -> Edge {
            let k = |c: f64| if c == 0.0 { 0u64 } else { c.to_bits() };
            [k(a[0]), k(a[1]), k(a[2]), k(b[0]), k(b[1]), k(b[2])]
        };
        let v = |i: u32| self.vertices[i as usize];
        let angle_threshold = eval::trig::cos_degrees(0.1);
        let mut edges: std::collections::HashMap<Edge, usize> =
            std::collections::HashMap::with_capacity(self.faces.len() * 3);
        // (normal, d) of each face's plane, `Plane_3(v[0], newell normal)`.
        let mut planes: Vec<([f64; 3], f64)> = Vec::with_capacity(self.faces.len());
        for (i, f) in self.faces.iter().enumerate() {
            let n = f.len();
            let mut plane = ([0.0; 3], 0.0);
            if n >= 3 {
                for j in 0..n {
                    if edges.insert(key(v(f[j]), v(f[(j + 1) % n])), i).is_some() {
                        return false;
                    }
                }
                let pts: Vec<[f64; 3]> = f.iter().map(|&k| v(k)).collect();
                let normal = newell(&pts);
                let p = pts[0];
                plane = (
                    normal,
                    -(normal[0] * p[0] + normal[1] * p[1] + normal[2] * p[2]),
                );
            }
            planes.push(plane);
        }
        let unit = |u: [f64; 3]| {
            let l = (u[0] * u[0] + u[1] * u[1] + u[2] * u[2]).sqrt();
            u.map(|c| c / l)
        };
        for (i, f) in self.faces.iter().enumerate() {
            let n = f.len();
            if n < 3 {
                continue;
            }
            for j in 0..n {
                let Some(&other) = edges.get(&key(v(f[(j + 1) % n]), v(f[j]))) else {
                    return false;
                };
                let p = v(f[(j + 2) % n]);
                let (u, d) = planes[other];
                if u[0] * p[0] + u[1] * p[1] + u[2] * p[2] + d > 0.0 {
                    let (a, b) = (unit(u), unit(planes[i].0));
                    if a[0] * b[0] + a[1] * b[1] + a[2] * b[2] < angle_threshold {
                        return false;
                    }
                }
            }
        }
        // Every face reachable from the first across shared edges.
        let mut seen = vec![false; self.faces.len()];
        seen[0] = true;
        let mut count = 1;
        let mut queue = std::collections::VecDeque::from([0usize]);
        while let Some(f) = queue.pop_front() {
            let face = &self.faces[f];
            for i in 0..face.len() {
                let j = (i + 1) % face.len();
                let Some(&o) = edges.get(&key(v(face[j]), v(face[i]))) else {
                    return false;
                };
                if !seen[o] {
                    seen[o] = true;
                    count += 1;
                    queue.push_back(o);
                }
            }
        }
        count == self.faces.len()
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
        let mut out = PolySet {
            convex: self.convex,
            triangular: true,
            ..Default::default()
        };
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
            polygons.push((
                cur,
                if has_colors {
                    self.color_indices[i]
                } else {
                    -1
                },
            ));
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
            let tris = if face.len() == 3 {
                vec![[face[0], face[1], face[2]]]
            } else {
                triangulate_face(&out.vertices, &face)
            };
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

    /// Every face split into triangles over the same vertices, with none
    /// merged: the mesh `createSurfaceMeshFromPolySet` hands CGAL, which
    /// takes the faces as they are. [`PolySet::tessellate`] merges vertices
    /// that coincide in `float` precision, which tears a mesh whose
    /// vertices are closer than that apart (`issue1138.scad`, 2e-7) into a
    /// non-manifold one; minkowski() reads its operands the CGAL way.
    /// Faces with fewer than three vertices are dropped; colours are not
    /// kept.
    pub fn triangulate_faces(&self) -> PolySet {
        let mut out = PolySet {
            vertices: self.vertices.clone(),
            convex: self.convex,
            triangular: true,
            ..Default::default()
        };
        for f in self.faces.iter().filter(|f| f.len() >= 3) {
            let tris = if f.len() == 3 {
                vec![[f[0], f[1], f[2]]]
            } else {
                triangulate_face(&self.vertices, f)
            };
            out.faces.extend(tris.into_iter().map(|t| t.to_vec()));
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
            if e == 0.0 {
                0.0
            } else {
                sign * e * minor(rows, cols(j))
            }
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
    let axis = (0..3)
        .max_by(|&a, &b| n[a].abs().total_cmp(&n[b].abs()))
        .unwrap_or(2);
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
            PolyVert {
                pos: Vec2::new(x, p[v]),
                idx: k as i32,
            }
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
        out = (1..face.len() - 1)
            .map(|k| [face[0], face[k], face[k + 1]])
            .collect();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn concave_face_tessellates_to_its_area() {
        // An L-shaped hexagon in the z = 0 plane, counter-clockwise.
        let verts = vec![
            [0.0, 0.0, 0.0],
            [2.0, 0.0, 0.0],
            [2.0, 1.0, 0.0],
            [1.0, 1.0, 0.0],
            [1.0, 2.0, 0.0],
            [0.0, 2.0, 0.0],
        ];
        let ps = PolySet {
            vertices: verts.clone(),
            faces: vec![(0..6).collect()],
            ..Default::default()
        };
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
        let ps = PolySet {
            vertices: vec![[0.0; 3]; 3],
            faces: vec![vec![0, 1]],
            ..Default::default()
        };
        let mut w = Vec::new();
        let t = ps.tessellate(&mut w);
        assert!(t.faces.is_empty());
        assert_eq!(w, ["PolySet has degenerate polygons"]);
    }

    #[test]
    fn mirror_flips_faces() {
        let mut ps = PolySet {
            vertices: vec![[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
            faces: vec![vec![0, 1, 2]],
            ..Default::default()
        };
        let mut m = crate::IDENTITY;
        m[0][0] = -1.0;
        ps.transform(&m);
        assert_eq!(ps.faces[0], vec![2, 1, 0]);
        assert_eq!(ps.vertices[0], [-1.0, 0.0, 0.0]);
    }
}
