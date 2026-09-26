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
        MeshRef { vertices: &self.vertices, faces: &self.faces, colors: &self.colors, color_indices: &self.color_indices }
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
        if ci < 0 { None } else { self.colors.get(ci as usize) }
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
        if self.current.is_empty() || (Some(&i) != self.current.last() && Some(&i) != self.current.first()) {
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
        Mesh { vertices: self.vertices, faces: self.faces, colors: self.colors, color_indices: self.color_indices }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
