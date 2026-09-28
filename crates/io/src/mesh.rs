//! Meshes as readers produce them and writers take them, and OpenSCAD's
//! `PolySetBuilder` (`src/geometry/PolySetBuilder.cc`), which most readers
//! build through.

use std::collections::HashMap;

use crate::Color;

/// A polygon mesh with the fields of OpenSCAD's `PolySet`: shared
/// vertices, faces as vertex index lists (counter-clockwise from outside),
/// and optional per-face colours.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Mesh {
    pub vertices: Vec<[f64; 3]>,
    pub faces: Vec<Vec<u32>>,
    /// The palette `color_indices` point into.
    pub colors: Vec<Color>,
    /// One entry per face (-1: no colour), or empty when no face is coloured.
    pub color_indices: Vec<i32>,
}

impl Mesh {
    pub fn as_ref(&self) -> MeshRef<'_> {
        MeshRef {
            vertices: &self.vertices,
            faces: &self.faces,
            colors: &self.colors,
            color_indices: &self.color_indices,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.faces.is_empty()
    }
}

/// A borrowed [`Mesh`], so writers can take any mesh type's fields
/// without a copy.
#[derive(Debug, Clone, Copy)]
pub struct MeshRef<'a> {
    pub vertices: &'a [[f64; 3]],
    pub faces: &'a [Vec<u32>],
    pub colors: &'a [Color],
    pub color_indices: &'a [i32],
}

impl MeshRef<'_> {
    /// The colour of face `i`, if it has one.
    pub fn face_color(&self, i: usize) -> Option<&Color> {
        let ci = *self.color_indices.get(i)?;
        if ci < 0 {
            None
        } else {
            self.colors.get(ci as usize)
        }
    }
}

/// `createSortedPolySet` (`src/io/export.cc:317-372`), what the writers
/// apply under `--enable=predictible-output`: the same surface with its
/// vertices in lexicographic (x, y, z) order and its faces sorted, so the
/// file no longer depends on the order a kernel happened to emit.
///
/// As upstream: `-0` becomes `0`; vertices of equal position merge (and
/// vertices no face uses are dropped); each face is rotated to start at
/// its lowest index, keeping its winding; faces are then sorted as index
/// lists, each keeping its colour. Face count and colours are unchanged.
///
/// Upstream sorts coloured faces with `std::sort`, which leaves equal
/// faces of different colours in an unspecified order; a stable sort here
/// makes that tie deterministic, and only a mesh with duplicate faces can
/// tell the difference.
pub fn sorted(mesh: MeshRef<'_>) -> Mesh {
    // `remove_negative_zero`: `x == -0 ? 0 : x`, which maps both zeros to
    // `+0`, so the two merge and `-0` never reaches the file.
    let clean = |v: [f64; 3]| v.map(|c| if c == 0.0 { 0.0 } else { c });
    // First pass: each distinct position gets an id in first-use order
    // (the `std::map::emplace(v, map.size())` ids).
    let mut ids: HashMap<[u64; 3], u32> = HashMap::with_capacity(mesh.vertices.len());
    let mut unique: Vec<[f64; 3]> = Vec::with_capacity(mesh.vertices.len());
    let mut faces: Vec<Vec<u32>> = mesh
        .faces
        .iter()
        .map(|f| {
            f.iter()
                .map(|&i| {
                    let v = clean(mesh.vertices[i as usize]);
                    *ids.entry(v.map(f64::to_bits)).or_insert_with(|| {
                        unique.push(v);
                        unique.len() as u32 - 1
                    })
                })
                .collect()
        })
        .collect();
    // Second pass: number the positions in lexicographic order. The map's
    // `std::less<double>` and `total_cmp` agree on every finite value once
    // `-0` is gone; `total_cmp` also gives NaN a fixed place, where upstream's
    // order is undefined.
    let mut order: Vec<u32> = (0..unique.len() as u32).collect();
    order.sort_unstable_by(|&a, &b| {
        let (a, b) = (unique[a as usize], unique[b as usize]);
        a[0].total_cmp(&b[0])
            .then(a[1].total_cmp(&b[1]))
            .then(a[2].total_cmp(&b[2]))
    });
    let mut translate = vec![0u32; unique.len()];
    let vertices: Vec<[f64; 3]> = order
        .iter()
        .enumerate()
        .map(|(new, &old)| {
            translate[old as usize] = new as u32;
            unique[old as usize]
        })
        .collect();
    for f in &mut faces {
        for i in f.iter_mut() {
            *i = translate[*i as usize];
        }
        // `std::rotate` to the first occurrence of the minimum.
        if let Some(start) = (0..f.len()).min_by_key(|&k| (f[k], k)) {
            f.rotate_left(start);
        }
    }
    let color_indices = if mesh.color_indices.is_empty() {
        faces.sort_unstable();
        Vec::new()
    } else {
        let mut pairs: Vec<(Vec<u32>, i32)> = faces
            .drain(..)
            // A short colour list (not one `Mesh` promises) leaves the
            // rest uncoloured rather than dropping faces.
            .zip(
                mesh.color_indices
                    .iter()
                    .copied()
                    .chain(std::iter::repeat(-1)),
            )
            .collect();
        pairs.sort_by(|a, b| a.0.cmp(&b.0));
        let (f, c): (Vec<_>, Vec<_>) = pairs.into_iter().unzip();
        faces = f;
        c
    };
    Mesh {
        vertices,
        faces,
        colors: mesh.colors.to_vec(),
        color_indices,
    }
}

/// `PolySetBuilder`: vertices are shared by exact value (the first copy of
/// a position keeps its index), a face drops a vertex equal to its
/// previous or first one, and faces left with fewer than three vertices
/// are dropped silently.
#[derive(Debug, Default)]
pub struct MeshBuilder {
    vertices: Vec<[f64; 3]>,
    index: HashMap<[u64; 3], u32>,
    faces: Vec<Vec<u32>>,
    colors: Vec<Color>,
    color_indices: Vec<i32>,
    current: Vec<u32>,
}

impl MeshBuilder {
    pub fn new() -> MeshBuilder {
        MeshBuilder::default()
    }

    /// `vertexIndex`: the index of a position, adding it if new. `-0` and
    /// `0` are one position, as `==` has them.
    pub fn vertex_index(&mut self, v: [f64; 3]) -> u32 {
        let key = v.map(|c| if c == 0.0 { 0u64 } else { c.to_bits() });
        if let Some(&i) = self.index.get(&key) {
            return i;
        }
        let i = self.vertices.len() as u32;
        self.vertices.push(v);
        self.index.insert(key, i);
        i
    }

    /// `beginPolygon`: finish any open face, start a new one.
    pub fn begin_polygon(&mut self) {
        self.end_polygon(None);
    }

    /// `addVertex(int)`: consecutive duplicates (and a repeat of the first
    /// vertex) are skipped.
    pub fn add_index(&mut self, i: u32) {
        if self.current.is_empty()
            || (Some(&i) != self.current.last() && Some(&i) != self.current.first())
        {
            self.current.push(i);
        }
    }

    pub fn add_vertex(&mut self, v: [f64; 3]) {
        let i = self.vertex_index(v);
        self.add_index(i);
    }

    /// `endPolygon(color)`: keep the face if it has three vertices or more,
    /// with the colour when it is valid.
    pub fn end_polygon(&mut self, color: Option<Color>) {
        if self.current.len() >= 3 {
            self.faces.push(std::mem::take(&mut self.current));
            if let Some(c) = color.filter(Color::is_valid) {
                if self.color_indices.is_empty() && self.faces.len() > 1 {
                    self.color_indices.resize(self.faces.len() - 1, -1);
                }
                let ci = match self.colors.iter().position(|x| *x == c) {
                    Some(i) => i,
                    None => {
                        self.colors.push(c);
                        self.colors.len() - 1
                    }
                };
                self.color_indices.push(ci as i32);
            }
        }
        self.current.clear();
    }

    /// `appendPolygon(std::vector<Vector3d>)`.
    pub fn append_polygon(&mut self, pts: &[[f64; 3]]) {
        self.begin_polygon();
        for &p in pts {
            self.add_vertex(p);
        }
        self.end_polygon(None);
    }

    pub fn build(mut self) -> Mesh {
        self.end_polygon(None);
        Mesh {
            vertices: self.vertices,
            faces: self.faces,
            colors: self.colors,
            color_indices: self.color_indices,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `cube(1)` as OpenSCAD builds it, sorted: the vertices and faces of
    /// `openscad --enable=predictible-output -o c.off` (2026.09.23 nightly).
    #[test]
    fn sorted_cube_matches_the_nightly() {
        let mut vertices = Vec::new();
        for z in [0.0, 1.0] {
            for y in [0.0, 1.0] {
                for x in [0.0, 1.0] {
                    vertices.push([x, y, z]);
                }
            }
        }
        let faces = vec![
            vec![4, 5, 7, 6],
            vec![2, 3, 1, 0],
            vec![0, 1, 5, 4],
            vec![1, 3, 7, 5],
            vec![3, 2, 6, 7],
            vec![2, 0, 4, 6],
        ];
        let m = Mesh {
            vertices,
            faces,
            ..Default::default()
        };
        let s = sorted(m.as_ref());
        let v: Vec<[f64; 3]> = vec![
            [0.0, 0.0, 0.0],
            [0.0, 0.0, 1.0],
            [0.0, 1.0, 0.0],
            [0.0, 1.0, 1.0],
            [1.0, 0.0, 0.0],
            [1.0, 0.0, 1.0],
            [1.0, 1.0, 0.0],
            [1.0, 1.0, 1.0],
        ];
        assert_eq!(s.vertices, v);
        assert_eq!(
            s.faces,
            vec![
                vec![0, 1, 3, 2],
                vec![0, 2, 6, 4],
                vec![0, 4, 5, 1],
                vec![1, 5, 7, 3],
                vec![2, 3, 7, 6],
                vec![4, 6, 7, 5],
            ]
        );
        assert!(s.color_indices.is_empty());
    }

    /// `-0` merges with `0` and is written as `0`; positions no face uses
    /// are dropped; each face keeps its colour through the sort.
    #[test]
    fn sorting_merges_zeros_and_carries_colours() {
        let red = Color([1.0, 0.0, 0.0, 1.0]);
        let blue = Color([0.0, 0.0, 1.0, 1.0]);
        let m = Mesh {
            vertices: vec![
                [5.0, 5.0, 5.0], // unused
                [1.0, 0.0, 0.0],
                [-0.0, 1.0, 0.0],
                [0.0, 1.0, 0.0],
                [0.0, 0.0, -0.0],
            ],
            faces: vec![vec![1, 2, 4], vec![4, 1, 3]],
            colors: vec![red, blue],
            color_indices: vec![0, 1],
        };
        let s = sorted(m.as_ref());
        assert_eq!(
            s.vertices,
            vec![[0.0, 0.0, 0.0], [0.0, 1.0, 0.0], [1.0, 0.0, 0.0]]
        );
        assert!(s.vertices.iter().flatten().all(|c| c.is_sign_positive()));
        // Both faces become (0, 2, 1) after merging, rotation keeps the
        // winding, and each keeps its own colour in its original order.
        assert_eq!(s.faces, vec![vec![0, 2, 1], vec![0, 2, 1]]);
        assert_eq!(s.color_indices, vec![0, 1]);
        assert_eq!(s.colors, vec![red, blue]);
    }

    #[test]
    fn shares_vertices_and_drops_degenerate_faces() {
        let mut b = MeshBuilder::new();
        b.append_polygon(&[[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]]);
        b.append_polygon(&[[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [1.0, 0.0, 0.0]]);
        b.append_polygon(&[[1.0, 0.0, 0.0], [-0.0, 1.0, 0.0], [1.0, 1.0, 0.0]]);
        let m = b.build();
        assert_eq!(m.vertices.len(), 4);
        assert_eq!(m.faces, vec![vec![0, 1, 2], vec![1, 2, 3]]);
    }
}
