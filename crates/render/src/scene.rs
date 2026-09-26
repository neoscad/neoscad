//! What the renderer draws, built from `geom`'s result the way OpenSCAD's
//! render-mode `PolySetRenderer` builds its vertex buffers
//! (`src/glview/PolySetRenderer.cc`, `VBOBuilder.cc`), with no GPU
//! involved: the app, the web build and the offscreen exporter all draw a
//! [`Scene`].
//!
//! - A 3D result becomes one triangulated mesh. A Manifold solid is turned
//!   into a mesh by `geom` exactly as for export (`toPolySet`, with the
//!   scheme's front colour and its back colour on faces cut by a
//!   `difference()`); a mesh that is already triangulated is shared with
//!   `geom`, not copied.
//! - Each triangle gets its own three vertices carrying the face normal,
//!   so shading is flat, as in OpenSCAD, and its face colour.
//! - A 2D result is its triangulation, unlit in the scheme's 2D face
//!   colour, plus its outlines, drawn over it as 2-pixel lines in the 2D
//!   edge colour.
//!
//! The vertex bytes are produced by iterators, so a GPU target can write
//! them straight into a mapped buffer: geometry reaches the GPU in one pass
//! from `geom`'s own `f64` mesh, with no intermediate vertex array.

use std::sync::Arc;

use geom::Geometry;
use geom::color::Color;
use geom::polygon2d::Polygon2d;
use geom::polyset::PolySet;

use crate::camera::BoundingBox;
use crate::scheme::ColorScheme;

/// Bytes per face vertex: position (3 x f32), normal (3 x f32) and colour
/// (4 x f32), little-endian, in that order. A zero normal marks a vertex
/// that is drawn unlit (2D shapes).
pub const FACE_VERTEX_SIZE: usize = 40;

/// Bytes per outline segment: its two end points (2 x 3 x f32).
pub const EDGE_SEGMENT_SIZE: usize = 24;

/// A 3D mesh and the colour of faces that have none.
#[derive(Debug)]
struct Solid {
    mesh: Arc<PolySet>,
    default_color: Color,
}

/// A 2D shape: its outlines and their triangulation.
#[derive(Debug)]
struct Flat {
    polygon: Arc<Polygon2d>,
    fill: PolySet,
}

/// Everything one image shows, in model coordinates.
#[derive(Debug)]
pub struct Scene {
    solids: Vec<Solid>,
    flats: Vec<Flat>,
    face_2d: Color,
    edge_2d: Color,
    bbox: BoundingBox,
}

impl Scene {
    /// The scene for a render result (`None`: nothing to draw, as for an
    /// empty top level). `scheme` supplies the colours `PolySetRenderer`
    /// takes from it: the default face colour, the Manifold face colours
    /// and the 2D colours.
    pub fn new(geometry: Option<&Geometry>, scheme: &ColorScheme) -> Scene {
        let mut scene = Scene {
            solids: Vec::new(),
            flats: Vec::new(),
            face_2d: scheme.cgal_face_2d,
            edge_2d: scheme.cgal_edge_2d,
            bbox: None,
        };
        match geometry {
            None => {}
            Some(Geometry::PolySet(ps)) => {
                // `PolySetUtils::tessellate_faces`: concave faces must be
                // split before drawing (`polyhedron-concave-test.scad`).
                let mesh = if ps.triangular {
                    ps.clone()
                } else {
                    Arc::new(ps.tessellate(&mut Vec::new()))
                };
                scene.add_solid(mesh, scheme);
            }
            Some(Geometry::Manifold(m)) => {
                scene.add_solid(Arc::new(m.to_polyset(&scheme.geometry_scheme())), scheme);
            }
            Some(Geometry::Polygon2d(p)) => {
                let fill = p.tessellate();
                if let Some((lo, hi)) = p.bounds() {
                    scene.bbox = Some(([lo[0], lo[1], 0.0], [hi[0], hi[1], 0.0]));
                }
                scene.flats.push(Flat {
                    polygon: p.clone(),
                    fill,
                });
            }
        }
        scene
    }

    fn add_solid(&mut self, mesh: Arc<PolySet>, scheme: &ColorScheme) {
        // `createPolySetStates`: the first colour of the mesh, with
        // whichever of its RGB and alpha are unset taken from the scheme's
        // `MATERIAL` colour (`Renderer::getShaderColor`).
        let mut color = mesh.colors.first().copied().unwrap_or(Color([-1.0; 4]));
        if !color.is_valid() {
            let base = scheme.opencsg_face_front.0;
            if color.0[..3].iter().any(|&c| c < 0.0) {
                color.0[..3].copy_from_slice(&base[..3]);
            }
            if color.0[3] < 0.0 {
                color.0[3] = base[3];
            }
        }
        // `PolySet::getBoundingBox` spans every vertex.
        let mut it = mesh.vertices.iter();
        if let Some(&first) = it.next() {
            let (lo, hi) = it.fold((first, first), |(lo, hi), v| {
                (
                    std::array::from_fn(|k| lo[k].min(v[k])),
                    std::array::from_fn(|k| hi[k].max(v[k])),
                )
            });
            self.bbox = Some(match self.bbox {
                None => (lo, hi),
                Some((l, h)) => (
                    std::array::from_fn(|k| l[k].min(lo[k])),
                    std::array::from_fn(|k| h[k].max(hi[k])),
                ),
            });
        }
        self.solids.push(Solid {
            mesh,
            default_color: color,
        });
    }

    /// `PolySetRenderer::getBoundingBox`, which `--viewall` fits.
    pub fn bounding_box(&self) -> BoundingBox {
        self.bbox
    }

    /// Vertices [`Scene::face_vertices`] yields: three per triangle.
    pub fn face_vertex_count(&self) -> usize {
        let solids: usize = self
            .solids
            .iter()
            .map(|s| s.mesh.faces.iter().filter(|f| f.len() >= 3).count())
            .sum();
        let flats: usize = self.flats.iter().map(|f| f.fill.faces.len()).sum();
        3 * (solids + flats)
    }

    /// Every triangle's three vertices ([`FACE_VERTEX_SIZE`] bytes each):
    /// the meshes first, then the 2D fills.
    pub fn face_vertices(&self) -> impl Iterator<Item = [u8; FACE_VERTEX_SIZE]> + '_ {
        let solids = self.solids.iter().flat_map(|s| {
            let ps = &*s.mesh;
            let has_colors = !ps.color_indices.is_empty();
            ps.faces
                .iter()
                .enumerate()
                .filter(|(_, f)| f.len() >= 3)
                .flat_map(move |(i, f)| {
                    // `VBOBuilder::create_surface`: a face's own colour
                    // when it has a valid one, else the mesh default.
                    let color = has_colors
                        .then(|| ps.color_indices.get(i).copied())
                        .flatten()
                        .and_then(|ci| usize::try_from(ci).ok())
                        .and_then(|ci| ps.colors.get(ci))
                        .filter(|c| c.is_valid())
                        .copied()
                        .unwrap_or(s.default_color);
                    let p = [f[0], f[1], f[2]].map(|v| ps.vertices[v as usize]);
                    triangle(p, Some(face_normal(p)), color)
                })
        });
        let flats = self.flats.iter().flat_map(move |fl| {
            fl.fill.faces.iter().flat_map(move |f| {
                let p = [f[0], f[1], f[2]].map(|v| fl.fill.vertices[v as usize]);
                triangle(p, None, self.face_2d)
            })
        });
        solids.chain(flats)
    }

    /// Outline segments [`Scene::edge_segments`] yields.
    pub fn edge_segment_count(&self) -> usize {
        self.flats
            .iter()
            .flat_map(|f| f.polygon.outlines.iter())
            .map(|o| o.vertices.len())
            .sum()
    }

    /// Each 2D outline as a closed loop of segments
    /// ([`EDGE_SEGMENT_SIZE`] bytes each), at z = 0.
    pub fn edge_segments(&self) -> impl Iterator<Item = [u8; EDGE_SEGMENT_SIZE]> + '_ {
        self.flats
            .iter()
            .flat_map(|f| f.polygon.outlines.iter())
            .flat_map(|o| {
                let v = &o.vertices;
                (0..v.len()).map(move |i| {
                    let (a, b) = (v[i], v[(i + 1) % v.len()]);
                    let mut out = [0u8; EDGE_SEGMENT_SIZE];
                    for (k, x) in [a[0], a[1], 0.0, b[0], b[1], 0.0].into_iter().enumerate() {
                        out[4 * k..4 * k + 4].copy_from_slice(&(x as f32).to_le_bytes());
                    }
                    out
                })
            })
    }

    /// The colour 2D outlines are drawn in.
    pub fn edge_color(&self) -> Color {
        self.edge_2d
    }
}

/// `VBOBuilder::create_triangle`'s normal: `(p1 - p0) x (p1 - p2)`,
/// normalised, in `f64`. It points into the solid for counter-clockwise
/// faces; OpenSCAD's two lights are opposite each other, so the sign does
/// not change the shading.
fn face_normal(p: [[f64; 3]; 3]) -> [f64; 3] {
    let (ax, bx) = (p[1][0] - p[0][0], p[1][0] - p[2][0]);
    let (ay, by) = (p[1][1] - p[0][1], p[1][1] - p[2][1]);
    let (az, bz) = (p[1][2] - p[0][2], p[1][2] - p[2][2]);
    let nx = ay * bz - az * by;
    let ny = az * bx - ax * bz;
    let nz = ax * by - ay * bx;
    let nl = (nx * nx + ny * ny + nz * nz).sqrt();
    [nx / nl, ny / nl, nz / nl]
}

/// The three vertices of one triangle; `normal: None` draws it unlit.
fn triangle(
    p: [[f64; 3]; 3],
    normal: Option<[f64; 3]>,
    color: Color,
) -> [[u8; FACE_VERTEX_SIZE]; 3] {
    let n = normal.unwrap_or([0.0; 3]);
    p.map(|v| {
        let mut out = [0u8; FACE_VERTEX_SIZE];
        let floats = [
            v[0] as f32,
            v[1] as f32,
            v[2] as f32,
            n[0] as f32,
            n[1] as f32,
            n[2] as f32,
            color.0[0],
            color.0[1],
            color.0[2],
            color.0[3],
        ];
        for (k, x) in floats.into_iter().enumerate() {
            out[4 * k..4 * k + 4].copy_from_slice(&x.to_le_bytes());
        }
        out
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn floats(v: &[u8]) -> Vec<f32> {
        v.chunks(4)
            .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect()
    }

    #[test]
    fn uncoloured_mesh_uses_the_preview_face_colour() {
        let ps = PolySet {
            vertices: vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
            faces: vec![vec![0, 1, 2]],
            triangular: true,
            ..Default::default()
        };
        let mut scheme = ColorScheme::cornfield();
        scheme.opencsg_face_front = Color::from_u8(1, 2, 3);
        let scene = Scene::new(Some(&Geometry::PolySet(Arc::new(ps))), &scheme);
        assert_eq!(scene.face_vertex_count(), 3);
        let v: Vec<_> = scene.face_vertices().collect();
        assert_eq!(v.len(), 3);
        let f = floats(&v[0]);
        assert_eq!(&f[3..6], &[0.0, 0.0, -1.0], "(p1-p0) x (p1-p2)");
        assert_eq!(&f[6..10], &Color::from_u8(1, 2, 3).0);
        assert_eq!(scene.bounding_box(), Some(([0.0; 3], [1.0, 1.0, 0.0])));
    }

    #[test]
    fn square_has_fill_and_a_closed_outline() {
        let p = Polygon2d::from_outline(vec![[0.0, 0.0], [2.0, 0.0], [2.0, 1.0], [0.0, 1.0]]);
        let scene = Scene::new(
            Some(&Geometry::Polygon2d(Arc::new(p))),
            &ColorScheme::cornfield(),
        );
        assert_eq!(scene.face_vertex_count(), 6);
        assert_eq!(scene.face_vertices().count(), 6);
        let f = floats(&scene.face_vertices().next().unwrap());
        assert_eq!(&f[3..6], &[0.0; 3], "2D is unlit");
        assert_eq!(scene.edge_segment_count(), 4);
        let last = floats(&scene.edge_segments().last().unwrap());
        assert_eq!(last, [0.0, 1.0, 0.0, 0.0, 0.0, 0.0]);
        assert_eq!(scene.bounding_box(), Some(([0.0; 3], [2.0, 1.0, 0.0])));
    }

    #[test]
    fn nothing_to_draw() {
        let scene = Scene::new(None, &ColorScheme::cornfield());
        assert_eq!(scene.face_vertex_count(), 0);
        assert_eq!(scene.bounding_box(), None);
    }
}
