//! What the renderer draws, with no GPU involved: the app, the web build and
//! the offscreen exporter all draw a [`Scene`].
//!
//! A scene is a list of surfaces (a mesh, the matrix placing it and the
//! colour its uncoloured faces get), each drawn with a [`DrawState`]:
//! which faces are culled, how depth is tested, and whether colour is
//! written at all. Render mode needs one state; OpenSCAD's previews change
//! state between objects (a subtracted object shows only its back faces,
//! a transparent one its back faces before its front faces, a CSG product
//! is drawn where its depth is equal to the depth pass's), and a scene
//! records those changes in order, as OpenSCAD's `VertexState` list does.
//!
//! - Render mode ([`Scene::new`], after `PolySetRenderer`): a 3D result is
//!   one triangulated mesh; a Manifold solid is turned into a mesh by
//!   `geom` exactly as for export (with the scheme's front colour and its
//!   back colour on faces cut by a `difference()`). A 2D result is its
//!   triangulation, unlit in the scheme's 2D face colour, plus its
//!   outlines, drawn over it as 2-pixel lines in the 2D edge colour.
//! - Previews: see [`crate::preview`].
//!
//! Faces become triangles as `VBOBuilder::create_surface` makes them:
//! triangles as they are, a quad as two triangles sharing its 1-3 diagonal,
//! larger polygons as a fan around their centroid. Each triangle gets its
//! own three vertices carrying the face normal (so shading is flat, as in
//! OpenSCAD), its colour, and the barycentric flags the edge view draws
//! from (a diagonal a quad or fan introduced is not an edge).
//!
//! The vertex bytes come from iterators, so a GPU target can write them
//! straight into a mapped buffer, from `geom`'s own `f64` meshes.

use std::sync::Arc;

use geom::Geometry;
use geom::color::Color;
use geom::polygon2d::Polygon2d;
use geom::polyset::PolySet;
use serde::{Deserialize, Serialize};

use crate::camera::BoundingBox;
use crate::scheme::ColorScheme;

/// Bytes per face vertex: position (3 x f32), normal (3 x f32), colour
/// (4 x f32), little-endian, then four barycentric bytes (0 or 1; the
/// fourth unused). A zero normal marks a vertex that is drawn unlit (2D
/// shapes in render mode).
pub const FACE_VERTEX_SIZE: usize = 44;

/// Bytes per outline segment: its two end points (2 x 3 x f32).
pub const EDGE_SEGMENT_SIZE: usize = 24;

/// Which faces are not drawn (`glCullFace`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Cull {
    None,
    /// Front faces (counter-clockwise on screen) are culled: only back
    /// faces are drawn.
    Front,
    /// Back faces are culled.
    Back,
}

/// The depth test (`glDepthFunc`); depth is always written when the test
/// passes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Depth {
    Less,
    LessEqual,
    Equal,
    Always,
}

/// Fixed-function state for one run of surfaces.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct DrawState {
    pub cull: Cull,
    pub depth: Depth,
    /// `false` for a depth-only pass.
    pub color_write: bool,
    /// Pull the faces a hair towards the camera. A preview's highlighted
    /// objects are drawn over the model's CSG result, and a `#` subtracted
    /// object lies exactly on the cut it makes: OpenCSG compares depths of
    /// the very same triangles there, which tie, and `GL_LEQUAL` lets the
    /// highlight through. The model's surface here comes from a boolean
    /// that split those triangles, whose depths differ in the last bits, so
    /// without the offset the highlight would z-fight with the cut. (A `%`
    /// object is taken out of the CSG, so it never lies on a cut by
    /// construction, and gets no offset: where it merely touches the
    /// model, OpenSCAD shows the model.)
    pub bias: bool,
}

impl DrawState {
    /// Render mode's state: no culling, `GL_LESS`, colour on.
    pub const DEFAULT: DrawState = DrawState {
        cull: Cull::None,
        depth: Depth::Less,
        color_write: true,
        bias: false,
    };
}

/// A range of [`Scene::face_vertices`] drawn with one state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Draw {
    pub first: u32,
    pub count: u32,
    pub state: DrawState,
}

/// One mesh placed and coloured for drawing (`VBOBuilder::create_surface`).
#[derive(Debug, Clone)]
pub struct Surface {
    pub mesh: Arc<PolySet>,
    /// Model coordinates of the mesh's vertices; `None` for identity.
    pub matrix: Option<geom::Matrix>,
    /// The colour of faces without a valid colour of their own, or of
    /// every face with `force_color`.
    pub color: Color,
    pub force_color: bool,
    /// `false` draws the faces unlit (2D shapes in render mode).
    pub lit: bool,
    pub state: DrawState,
}

/// Which part a primitive plays in an image-space CSG product
/// (`OpenCSG::Operation`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CsgOp {
    Intersection,
    Subtraction,
}

/// One primitive of an image-space CSG product: a mesh placed in the
/// model, drawn only into depth and the primitive ID buffer.
#[derive(Debug, Clone)]
pub struct CsgPrimitive {
    pub mesh: Arc<PolySet>,
    pub matrix: Option<geom::Matrix>,
    pub op: CsgOp,
}

/// A product whose depth OpenCSG's SCS algorithm finds per pixel
/// ([`Scene::push_image_csg`]), in place of a boolean.
#[derive(Debug)]
struct ImageProduct {
    /// Surfaces pushed before it.
    at: usize,
    primitives: Vec<CsgPrimitive>,
}

/// An image-space product's vertex ranges, for the GPU: its primitives
/// in the order given, each with its ID (from 1; 0 marks no primitive).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImageCsgDraws {
    /// Draws of [`Scene::draws`] that come before the product.
    pub at_draw: usize,
    pub primitives: Vec<ImageCsgPrimitive>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImageCsgPrimitive {
    pub first: u32,
    pub count: u32,
    pub op: CsgOp,
    pub id: u32,
}

/// A 2D outline set, drawn over everything as 2-pixel lines.
#[derive(Debug)]
struct Outlines {
    polygon: Arc<Polygon2d>,
}

/// Everything one image shows, in model coordinates.
#[derive(Debug)]
pub struct Scene {
    surfaces: Vec<Surface>,
    products: Vec<ImageProduct>,
    outlines: Vec<Outlines>,
    edge_2d: Color,
    bbox: BoundingBox,
}

impl Scene {
    /// A scene with nothing in it yet; `bbox` is what `--viewall` fits.
    pub fn empty(scheme: &ColorScheme, bbox: BoundingBox) -> Scene {
        Scene {
            surfaces: Vec::new(),
            products: Vec::new(),
            outlines: Vec::new(),
            edge_2d: scheme.cgal_edge_2d,
            bbox,
        }
    }

    /// The render-mode scene for a render result (`None`: nothing to draw,
    /// as for an empty top level). `scheme` supplies the colours
    /// `PolySetRenderer` takes from it: the default face colour, the
    /// Manifold face colours and the 2D colours.
    pub fn new(geometry: Option<&Geometry>, scheme: &ColorScheme) -> Scene {
        let mut scene = Scene::empty(scheme, None);
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
                scene.add_render_solid(mesh, scheme);
            }
            Some(Geometry::Manifold(m)) => {
                scene.add_render_solid(Arc::new(m.to_polyset(&scheme.geometry_scheme())), scheme);
            }
            Some(Geometry::Polygon2d(p)) => {
                let fill = p.tessellate();
                if let Some((lo, hi)) = p.bounds() {
                    scene.bbox = Some(([lo[0], lo[1], 0.0], [hi[0], hi[1], 0.0]));
                }
                scene.surfaces.push(Surface {
                    mesh: Arc::new(fill),
                    matrix: None,
                    color: scheme.cgal_face_2d,
                    force_color: true,
                    lit: false,
                    state: DrawState::DEFAULT,
                });
                scene.outlines.push(Outlines { polygon: p.clone() });
            }
        }
        scene
    }

    fn add_render_solid(&mut self, mesh: Arc<PolySet>, scheme: &ColorScheme) {
        // `createPolySetStates`: the first colour of the mesh, with
        // whichever of its RGB and alpha are unset taken from the scheme's
        // `MATERIAL` colour (`Renderer::getShaderColor`).
        let first = mesh.colors.first().copied().unwrap_or(Color([-1.0; 4]));
        let color =
            crate::preview::shader_color(crate::preview::ColorMode::Material, first, scheme);
        // `PolySet::getBoundingBox` spans every vertex.
        self.bbox = merge(self.bbox, vertex_box(&mesh.vertices));
        self.surfaces.push(Surface {
            mesh,
            matrix: None,
            color,
            force_color: false,
            lit: true,
            state: DrawState::DEFAULT,
        });
    }

    /// Append a surface; surfaces are drawn in the order they are added.
    pub fn push(&mut self, surface: Surface) {
        self.surfaces.push(surface);
    }

    /// Append a product drawn the way OpenCSG's SCS algorithm draws it:
    /// its visible depth is found per pixel from the primitives' faces
    /// (which way each points on screen deciding inside and outside) and
    /// merged into the depth buffer, with the surfaces pushed next drawn
    /// where their depth is equal to it. See [`crate::gpu`].
    pub fn push_image_csg(&mut self, primitives: Vec<CsgPrimitive>) {
        self.products.push(ImageProduct {
            at: self.surfaces.len(),
            primitives,
        });
    }

    /// The box `--viewall` fits: `PolySetRenderer::getBoundingBox` in
    /// render mode, the products' box in a preview.
    pub fn bounding_box(&self) -> BoundingBox {
        self.bbox
    }

    /// The surfaces, in drawing order.
    pub fn surfaces(&self) -> &[Surface] {
        &self.surfaces
    }

    /// Where each surface's vertices start in [`Scene::face_vertices`],
    /// and whether they are its own; then where the image-space
    /// primitives' vertices start.
    ///
    /// A surface with the same vertices as the one before it draws that
    /// one's again instead of a copy: an OpenCSG product's depth pass and
    /// colour pass are one mesh twice, and so are a transparent leaf's two
    /// culled passes. Copies doubled a preview's vertex bytes, which the
    /// web core packs and moves to the page and the page uploads (46 MB
    /// for the threaded-ring example, now 23 MB). Not across an
    /// image-space product, so a product's surfaces are always its own.
    fn layout(&self) -> (Vec<(u32, bool)>, u32) {
        let mut out: Vec<(u32, bool)> = Vec::with_capacity(self.surfaces.len());
        let mut next = 0u32;
        let mut breaks = self.products.iter().map(|p| p.at).peekable();
        for (i, s) in self.surfaces.iter().enumerate() {
            let mut split = false;
            while breaks.next_if(|&at| at <= i).is_some() {
                split = true;
            }
            match i.checked_sub(1) {
                Some(j) if !split && same_vertices(&self.surfaces[j], s) => {
                    out.push((out[j].0, false));
                }
                _ => {
                    out.push((next, true));
                    next += surface_vertex_count(s) as u32;
                }
            }
        }
        (out, next)
    }

    /// Vertices [`Scene::face_vertices`] yields.
    pub fn face_vertex_count(&self) -> usize {
        self.layout().1 as usize
            + self
                .products
                .iter()
                .flat_map(|p| &p.primitives)
                .map(|c| mesh_vertex_count(&c.mesh))
                .sum::<usize>()
    }

    /// The draw calls: consecutive surfaces with the same state and
    /// consecutive vertices share one, unless an image-space product
    /// comes between them.
    pub fn draws(&self) -> Vec<Draw> {
        self.draws_by_surface()
            .into_iter()
            .map(|(d, _)| d)
            .collect()
    }

    /// [`Scene::draws`], each with the index of its first surface.
    fn draws_by_surface(&self) -> Vec<(Draw, usize)> {
        let (layout, _) = self.layout();
        let mut out: Vec<(Draw, usize)> = Vec::new();
        let mut breaks = self.products.iter().map(|p| p.at).peekable();
        for (i, (s, &(first, _))) in self.surfaces.iter().zip(&layout).enumerate() {
            let mut split = false;
            while breaks.next_if(|&at| at <= i).is_some() {
                split = true;
            }
            let count = surface_vertex_count(s) as u32;
            match out.last_mut() {
                Some((d, _)) if !split && d.state == s.state && d.first + d.count == first => {
                    d.count += count
                }
                _ => out.push((
                    Draw {
                        first,
                        count,
                        state: s.state,
                    },
                    i,
                )),
            }
        }
        out.retain(|(d, _)| d.count > 0);
        out
    }

    /// The image-space products, with their place among [`Scene::draws`]
    /// and their primitives' ranges of [`Scene::face_vertices`] (after
    /// every surface's).
    pub fn image_csg(&self) -> Vec<ImageCsgDraws> {
        let draws = self.draws_by_surface();
        // Where the primitives' vertices start.
        let mut first = self.layout().1;
        self.products
            .iter()
            .map(|p| {
                // `draws` never joins surfaces across a product, so the
                // draws before it are exactly those of the surfaces before
                // it.
                let at_draw = draws.iter().take_while(|(_, i)| *i < p.at).count();
                let primitives = p
                    .primitives
                    .iter()
                    .zip(1u32..)
                    .map(|(c, id)| {
                        let count = mesh_vertex_count(&c.mesh) as u32;
                        let out = ImageCsgPrimitive {
                            first,
                            count,
                            op: c.op,
                            id,
                        };
                        first += count;
                        out
                    })
                    .collect();
                ImageCsgDraws {
                    at_draw,
                    primitives,
                }
            })
            .collect()
    }

    /// Every triangle's three vertices ([`FACE_VERTEX_SIZE`] bytes each),
    /// surface by surface, then each image-space product's primitives with
    /// their ID in the colour's first component.
    pub fn face_vertices(&self) -> impl Iterator<Item = [u8; FACE_VERTEX_SIZE]> + '_ {
        let primitives = self.products.iter().flat_map(|p| {
            p.primitives.iter().zip(1u32..).flat_map(|(c, id)| {
                // Unlit, in a colour that is the ID: nothing shades these.
                let s = Surface {
                    mesh: c.mesh.clone(),
                    matrix: c.matrix,
                    color: Color([id as f32, 0.0, 0.0, 1.0]),
                    force_color: true,
                    lit: false,
                    state: DrawState::DEFAULT,
                };
                surface_vertices(&s).collect::<Vec<_>>()
            })
        });
        let (layout, _) = self.layout();
        self.surfaces
            .iter()
            .zip(layout)
            .filter(|(_, (_, own))| *own)
            .flat_map(|(s, _)| surface_vertices(s))
            .chain(primitives)
    }

    /// Outline segments [`Scene::edge_segments`] yields.
    pub fn edge_segment_count(&self) -> usize {
        self.outlines
            .iter()
            .flat_map(|f| f.polygon.outlines.iter())
            .map(|o| o.vertices.len())
            .sum()
    }

    /// Each 2D outline as a closed loop of segments
    /// ([`EDGE_SEGMENT_SIZE`] bytes each), at z = 0.
    pub fn edge_segments(&self) -> impl Iterator<Item = [u8; EDGE_SEGMENT_SIZE]> + '_ {
        self.outlines
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

/// The box of a list of points.
pub(crate) fn vertex_box(v: &[[f64; 3]]) -> BoundingBox {
    let mut it = v.iter();
    let first = *it.next()?;
    Some(it.fold((first, first), |(lo, hi), v| {
        (
            std::array::from_fn(|k| lo[k].min(v[k])),
            std::array::from_fn(|k| hi[k].max(v[k])),
        )
    }))
}

pub(crate) fn merge(a: BoundingBox, b: BoundingBox) -> BoundingBox {
    match (a, b) {
        (None, b) => b,
        (a, None) => a,
        (Some((al, ah)), Some((bl, bh))) => Some((
            std::array::from_fn(|k| al[k].min(bl[k])),
            std::array::from_fn(|k| ah[k].max(bh[k])),
        )),
    }
}

/// Whether two surfaces give the same vertices: [`surface_vertices`]
/// reads only these fields (the draw state is not in the vertices).
fn same_vertices(a: &Surface, b: &Surface) -> bool {
    Arc::ptr_eq(&a.mesh, &b.mesh)
        && a.matrix == b.matrix
        && a.color == b.color
        && a.force_color == b.force_color
        && a.lit == b.lit
}

fn surface_vertex_count(s: &Surface) -> usize {
    mesh_vertex_count(&s.mesh)
}

fn mesh_vertex_count(mesh: &PolySet) -> usize {
    mesh.faces
        .iter()
        .map(|f| match f.len() {
            0..=2 => 0,
            3 => 3,
            4 => 6,
            n => 3 * n,
        })
        .sum()
}

/// `VBOBuilder::add_barycentric_attribute`: the flags of vertex `active`
/// of triangle `primitive` cut from a face of `shape_size` vertices. A 1
/// in component `k` means the edge opposite vertex `k` is not drawn.
fn barycentric(active: usize, primitive: usize, shape_size: usize) -> [u8; 3] {
    let _ = primitive;
    let mut f = match shape_size {
        3 => [0, 0, 0],
        4 => [1, 0, 0],
        _ => [0, 1, 1],
    };
    f[active] = 1;
    f
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

/// Determinant of the matrix's linear part: negative for a mirror.
fn determinant(m: &geom::Matrix) -> f64 {
    m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1])
        - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
        + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0])
}

/// The triangles of one surface, as `VBOBuilder::create_surface` emits
/// them.
fn surface_vertices(s: &Surface) -> impl Iterator<Item = [u8; FACE_VERTEX_SIZE]> + '_ {
    let ps = &*s.mesh;
    let has_colors = !ps.color_indices.is_empty();
    let mirrored = s.matrix.as_ref().is_some_and(|m| determinant(m) < 0.0);
    let place = move |v: [f64; 3]| match &s.matrix {
        None => v,
        Some(m) => geom::polyset::apply(m, v),
    };
    ps.faces
        .iter()
        .enumerate()
        .filter(|(_, f)| f.len() >= 3)
        .flat_map(move |(i, f)| {
            // A face's own colour when it has a valid one (and the surface
            // does not force its colour), else the surface's.
            let color = (!s.force_color && has_colors)
                .then(|| ps.color_indices.get(i).copied())
                .flatten()
                .and_then(|ci| usize::try_from(ci).ok())
                .and_then(|ci| ps.colors.get(ci))
                .filter(|c| c.is_valid())
                .copied()
                .unwrap_or(s.color);
            let v = |k: usize| place(ps.vertices[f[k] as usize]);
            let n = f.len();
            let tris: Vec<([[f64; 3]; 3], usize)> = match n {
                3 => vec![([v(0), v(1), v(2)], 0)],
                4 => vec![([v(0), v(1), v(3)], 0), ([v(2), v(3), v(1)], 1)],
                _ => {
                    // The centroid of the untransformed vertices, moved.
                    let mut c = [0.0; 3];
                    for &k in f {
                        let p = ps.vertices[k as usize];
                        c = [c[0] + p[0], c[1] + p[1], c[2] + p[2]];
                    }
                    let c = place(c.map(|x| x / n as f64));
                    (1..=n).map(|j| ([c, v(j - 1), v(j % n)], j - 1)).collect()
                }
            };
            tris.into_iter().flat_map(move |(p, prim)| {
                triangle(p, prim, n, s.lit.then(|| face_normal(p)), color, mirrored)
            })
        })
}

/// The three vertices of one triangle; `normal: None` draws it unlit.
/// Mirrored surfaces emit the vertices as 0, 2, 1, so the winding on
/// screen is the model's again.
fn triangle(
    p: [[f64; 3]; 3],
    primitive: usize,
    shape_size: usize,
    normal: Option<[f64; 3]>,
    color: Color,
    mirrored: bool,
) -> [[u8; FACE_VERTEX_SIZE]; 3] {
    let n = normal.unwrap_or([0.0; 3]);
    let order = if mirrored { [0, 2, 1] } else { [0, 1, 2] };
    order.map(|k| {
        let v = p[k];
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
        for (i, x) in floats.into_iter().enumerate() {
            out[4 * i..4 * i + 4].copy_from_slice(&x.to_le_bytes());
        }
        let b = barycentric(k, primitive, shape_size);
        out[40..43].copy_from_slice(&b);
        out
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn floats(v: &[u8]) -> Vec<f32> {
        v[..40]
            .chunks(4)
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
        assert_eq!(&v[0][40..43], &[1, 0, 0]);
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
        let last = scene.edge_segments().last().unwrap();
        let last: Vec<f32> = last
            .chunks(4)
            .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect();
        assert_eq!(last, [0.0, 1.0, 0.0, 0.0, 0.0, 0.0]);
        assert_eq!(scene.bounding_box(), Some(([0.0; 3], [2.0, 1.0, 0.0])));
    }

    #[test]
    fn quads_split_on_their_diagonal_and_fans_hide_inner_edges() {
        let ps = PolySet {
            vertices: vec![
                [0.0, 0.0, 0.0],
                [1.0, 0.0, 0.0],
                [1.0, 1.0, 0.0],
                [0.0, 1.0, 0.0],
                [-1.0, 0.5, 0.0],
            ],
            faces: vec![vec![0, 1, 2, 3], vec![0, 1, 2, 3, 4]],
            ..Default::default()
        };
        let mut scene = Scene::empty(&ColorScheme::cornfield(), None);
        scene.push(Surface {
            mesh: Arc::new(ps),
            matrix: None,
            color: Color::from_u8(1, 2, 3),
            force_color: false,
            lit: true,
            state: DrawState::DEFAULT,
        });
        assert_eq!(scene.face_vertex_count(), 6 + 15);
        let v: Vec<_> = scene.face_vertices().collect();
        // The quad's first triangle is 0, 1, 3: its 1-3 edge (opposite
        // vertex 0) is the hidden diagonal.
        assert_eq!(
            [&v[0][40..43], &v[1][40..43], &v[2][40..43]],
            [[1, 0, 0], [1, 1, 0], [1, 0, 1]]
        );
        // A fan triangle (centroid, a, b) keeps only the edge a-b.
        assert_eq!(&v[6][40..43], &[1, 1, 1]);
        assert_eq!(&v[7][40..43], &[0, 1, 1]);
        assert_eq!(
            scene.draws(),
            vec![Draw {
                first: 0,
                count: 21,
                state: DrawState::DEFAULT
            }]
        );
    }

    #[test]
    fn nothing_to_draw() {
        let scene = Scene::new(None, &ColorScheme::cornfield());
        assert_eq!(scene.face_vertex_count(), 0);
        assert_eq!(scene.bounding_box(), None);
        assert!(scene.draws().is_empty());
    }

    /// Surfaces with one state share a draw, except across an image-space
    /// product, whose primitives follow every surface's vertices with
    /// their IDs (from 1) in the colour.
    #[test]
    fn image_csg_products_split_draws_and_follow_the_surfaces() {
        let tri = Arc::new(PolySet {
            vertices: vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
            faces: vec![vec![0, 1, 2]],
            triangular: true,
            ..Default::default()
        });
        let scheme = ColorScheme::cornfield();
        // In different colours, so no surface repeats the one before it.
        let surface = |red: f32| Surface {
            mesh: tri.clone(),
            matrix: None,
            color: Color([red, 0.5, 0.5, 1.0]),
            force_color: true,
            lit: true,
            state: DrawState::DEFAULT,
        };
        let primitive = |op| CsgPrimitive {
            mesh: tri.clone(),
            matrix: None,
            op,
        };
        let mut scene = Scene::empty(&scheme, None);
        scene.push(surface(0.0));
        scene.push_image_csg(vec![
            primitive(CsgOp::Intersection),
            primitive(CsgOp::Subtraction),
        ]);
        scene.push(surface(0.5));
        scene.push(surface(1.0));
        let draws = scene.draws();
        assert_eq!(
            draws.iter().map(|d| (d.first, d.count)).collect::<Vec<_>>(),
            vec![(0, 3), (3, 6)]
        );
        let csg = scene.image_csg();
        assert_eq!(csg.len(), 1);
        assert_eq!(csg[0].at_draw, 1);
        assert_eq!(
            csg[0].primitives,
            vec![
                ImageCsgPrimitive {
                    first: 9,
                    count: 3,
                    op: CsgOp::Intersection,
                    id: 1
                },
                ImageCsgPrimitive {
                    first: 12,
                    count: 3,
                    op: CsgOp::Subtraction,
                    id: 2
                },
            ]
        );
        assert_eq!(scene.face_vertex_count(), 15);
        let v: Vec<_> = scene.face_vertices().collect();
        assert_eq!(v.len(), 15);
        assert_eq!(floats(&v[9])[6], 1.0);
        assert_eq!(floats(&v[14])[6], 2.0);
    }

    /// An OpenCSG product's depth pass and colour pass are one mesh drawn
    /// twice: the colour pass draws the depth pass's vertices again, and
    /// the vertices are packed once. The same mesh after an image-space
    /// product, or in another colour, gets its own.
    #[test]
    fn a_repeated_surface_draws_the_same_vertices() {
        let tri = Arc::new(PolySet {
            vertices: vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
            faces: vec![vec![0, 1, 2]],
            triangular: true,
            ..Default::default()
        });
        let scheme = ColorScheme::cornfield();
        let surface = |depth, color_write, color| Surface {
            mesh: tri.clone(),
            matrix: None,
            color,
            force_color: false,
            lit: true,
            state: DrawState {
                cull: Cull::None,
                depth,
                color_write,
                bias: false,
            },
        };
        let front = scheme.opencsg_face_front;
        let mut scene = Scene::empty(&scheme, None);
        scene.push(surface(Depth::Less, false, front));
        scene.push(surface(Depth::Equal, true, front));
        scene.push_image_csg(vec![CsgPrimitive {
            mesh: tri.clone(),
            matrix: None,
            op: CsgOp::Intersection,
        }]);
        scene.push(surface(Depth::Equal, true, front));
        scene.push(surface(Depth::Equal, true, Color([1.0, 0.0, 0.0, 1.0])));
        let draws = scene.draws();
        assert_eq!(
            draws.iter().map(|d| (d.first, d.count)).collect::<Vec<_>>(),
            vec![(0, 3), (0, 3), (3, 6)]
        );
        assert!(!draws[0].state.color_write && draws[1].state.color_write);
        let csg = scene.image_csg();
        assert_eq!(csg[0].at_draw, 2);
        assert_eq!(csg[0].primitives[0].first, 9);
        assert_eq!(scene.face_vertex_count(), 12);
        assert_eq!(scene.face_vertices().count(), 12);
    }
}
