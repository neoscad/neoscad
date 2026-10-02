//! A [`Scene`] flattened into plain bytes and a small description, so it
//! can be built on one thread (a web worker) and drawn on another (the
//! page's WebGPU canvas) without the meshes crossing over.
//!
//! # The format
//!
//! ```text
//! PackedScene {
//!     faces: Vec<u8>,      // Scene::face_vertices, FACE_VERTEX_SIZE (44) bytes each
//!     edges: Vec<u8>,      // Scene::edge_segments, EDGE_SEGMENT_SIZE (24) bytes each
//!     meta: PackedMeta,
//! }
//! PackedMeta {             // serde; JSON on the wire (PackedMeta::to_json)
//!     draws: Vec<Draw>,              // Scene::draws
//!     image_csg: Vec<ImageCsgDraws>, // Scene::image_csg
//!     bbox: Option<([f64; 3], [f64; 3])>, // Scene::bounding_box (View All)
//!     edge_color: [f32; 4],          // Scene::edge_color
//! }
//! ```
//!
//! A face vertex is position (3 x f32), normal (3 x f32), colour (4 x f32),
//! little-endian, then four barycentric bytes; an edge segment is its two
//! end points (2 x 3 x f32). Both are exactly the bytes the GPU buffers
//! hold, so a receiver copies them into a vertex buffer and nothing else.
//!
//! Draws may share a range: a surface drawn twice in a row in different
//! states (an OpenCSG product's depth pass, then its colour pass) is
//! packed once and both draws name it.
//!
//! On the web the two byte arrays travel as transferable `ArrayBuffer`s
//! (moved, not copied, between the worker and the page) and `meta` as a
//! JSON string of a few hundred bytes; [`PackedScene::from_parts`] puts
//! them back together and checks them.
//!
//! In JSON, `Draw` is `{"first": u32, "count": u32, "state": {"cull":
//! "None" | "Front" | "Back", "depth": "Less" | "LessEqual" | "Equal" |
//! "Always", "color_write": bool, "bias": bool}}`, an image-space product
//! is `{"at_draw": usize, "primitives": [{"first", "count", "op":
//! "Intersection" | "Subtraction", "id"}]}`, and `bbox` is `null` or
//! `[[x, y, z], [x, y, z]]`.
//!
//! # Why the check
//!
//! The bytes come from another thread or another program. A draw range
//! past the end of the buffer is a GPU validation error, which native
//! wgpu turns into a panic and a browser into a lost frame; a length that
//! is not a whole number of vertices would misalign every attribute after
//! it. [`PackedScene::validate`] refuses both before anything reaches the
//! GPU, and `Gpu::upload_packed` (feature `gpu`) calls it.

use serde::{Deserialize, Serialize};

use crate::camera::BoundingBox;
use crate::scene::{Draw, EDGE_SEGMENT_SIZE, FACE_VERTEX_SIZE, ImageCsgDraws, Scene};

/// A scene as bytes plus its description (see the module documentation).
#[derive(Debug, Clone, PartialEq)]
pub struct PackedScene {
    /// Every face vertex, [`FACE_VERTEX_SIZE`] bytes each, in
    /// [`Scene::face_vertices`] order.
    pub faces: Vec<u8>,
    /// Every 2D outline segment, [`EDGE_SEGMENT_SIZE`] bytes each, in
    /// [`Scene::edge_segments`] order.
    pub edges: Vec<u8>,
    pub meta: PackedMeta,
}

/// Everything about a packed scene except its vertex bytes: small, and
/// serialisable (JSON on the web).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PackedMeta {
    /// [`Scene::draws`].
    pub draws: Vec<Draw>,
    /// [`Scene::image_csg`].
    pub image_csg: Vec<ImageCsgDraws>,
    /// [`Scene::bounding_box`]: what View All fits.
    pub bbox: BoundingBox,
    /// [`Scene::edge_color`], RGBA from 0 to 1.
    pub edge_color: [f32; 4],
}

/// Why a packed scene was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidPacked(pub String);

impl std::fmt::Display for InvalidPacked {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "invalid packed scene: {}", self.0)
    }
}

impl std::error::Error for InvalidPacked {}

impl Scene {
    /// This scene as [`PackedScene`]: the same bytes and draws
    /// [`crate::gpu::SceneBuffers::upload`] would put on the GPU.
    pub fn pack(&self) -> PackedScene {
        let mut faces = Vec::with_capacity(self.face_vertex_count() * FACE_VERTEX_SIZE);
        for v in self.face_vertices() {
            faces.extend_from_slice(&v);
        }
        let mut edges = Vec::with_capacity(self.edge_segment_count() * EDGE_SEGMENT_SIZE);
        for s in self.edge_segments() {
            edges.extend_from_slice(&s);
        }
        PackedScene {
            faces,
            edges,
            meta: PackedMeta {
                draws: self.draws(),
                image_csg: self.image_csg(),
                bbox: self.bounding_box(),
                edge_color: self.edge_color().0,
            },
        }
    }
}

impl PackedMeta {
    pub fn to_json(&self) -> String {
        // Only numbers, booleans and unit enum variants: this cannot fail
        // except on a non-finite bounding box, which serde_json writes as
        // `null` and `from_json` then refuses.
        serde_json::to_string(self).unwrap_or_default()
    }

    pub fn from_json(json: &str) -> Result<PackedMeta, InvalidPacked> {
        serde_json::from_str(json).map_err(|e| InvalidPacked(format!("metadata: {e}")))
    }
}

impl PackedScene {
    /// Reassemble a scene sent as its two byte arrays and its metadata
    /// JSON, checking it ([`PackedScene::validate`]).
    pub fn from_parts(
        faces: Vec<u8>,
        edges: Vec<u8>,
        meta_json: &str,
    ) -> Result<PackedScene, InvalidPacked> {
        let p = PackedScene {
            faces,
            edges,
            meta: PackedMeta::from_json(meta_json)?,
        };
        p.validate()?;
        Ok(p)
    }

    /// Face vertices in [`PackedScene::faces`].
    pub fn face_vertex_count(&self) -> usize {
        self.faces.len() / FACE_VERTEX_SIZE
    }

    /// Segments in [`PackedScene::edges`].
    pub fn edge_segment_count(&self) -> usize {
        self.edges.len() / EDGE_SEGMENT_SIZE
    }

    /// Check that the bytes are whole vertices and segments and that every
    /// draw and image-space primitive lies inside them (see the module
    /// documentation for why).
    pub fn validate(&self) -> Result<(), InvalidPacked> {
        let bad = |m: String| Err(InvalidPacked(m));
        if !self.faces.len().is_multiple_of(FACE_VERTEX_SIZE) {
            return bad(format!(
                "{} face bytes is not a whole number of {FACE_VERTEX_SIZE}-byte vertices",
                self.faces.len()
            ));
        }
        if !self.edges.len().is_multiple_of(EDGE_SEGMENT_SIZE) {
            return bad(format!(
                "{} edge bytes is not a whole number of {EDGE_SEGMENT_SIZE}-byte segments",
                self.edges.len()
            ));
        }
        let vertices = self.face_vertex_count() as u64;
        let in_range = |first: u32, count: u32| u64::from(first) + u64::from(count) <= vertices;
        for d in &self.meta.draws {
            if !in_range(d.first, d.count) {
                return bad(format!(
                    "draw {}..+{} is past the {vertices} face vertices",
                    d.first, d.count
                ));
            }
        }
        for p in &self.meta.image_csg {
            if p.at_draw > self.meta.draws.len() {
                return bad(format!(
                    "an image-space product after draw {} of {}",
                    p.at_draw,
                    self.meta.draws.len()
                ));
            }
            for c in &p.primitives {
                if !in_range(c.first, c.count) {
                    return bad(format!(
                        "primitive {}..+{} is past the {vertices} face vertices",
                        c.first, c.count
                    ));
                }
            }
        }
        if let Some((lo, hi)) = self.meta.bbox
            && lo.iter().chain(&hi).any(|x| !x.is_finite())
        {
            return bad("a bounding box that is not finite".into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use geom::Geometry;
    use geom::polyset::PolySet;

    use super::*;
    use crate::ColorScheme;

    fn cube() -> Geometry {
        let v = |x: f64, y: f64, z: f64| [x, y, z];
        Geometry::PolySet(Arc::new(PolySet {
            vertices: vec![
                v(0.0, 0.0, 0.0),
                v(1.0, 0.0, 0.0),
                v(1.0, 1.0, 0.0),
                v(0.0, 1.0, 0.0),
                v(0.0, 0.0, 1.0),
                v(1.0, 0.0, 1.0),
                v(1.0, 1.0, 1.0),
                v(0.0, 1.0, 1.0),
            ],
            faces: vec![
                vec![4, 5, 6, 7],
                vec![3, 2, 1, 0],
                vec![0, 1, 5, 4],
                vec![1, 2, 6, 5],
                vec![2, 3, 7, 6],
                vec![3, 0, 4, 7],
            ],
            convex: Some(true),
            ..Default::default()
        }))
    }

    #[test]
    fn pack_matches_the_scene_and_round_trips_through_json() {
        let scheme = ColorScheme::cornfield();
        let scene = Scene::new(Some(&cube()), &scheme);
        let p = scene.pack();
        assert_eq!(p.face_vertex_count(), scene.face_vertex_count());
        assert_eq!(p.face_vertex_count(), 36);
        assert_eq!(p.faces.len(), 36 * FACE_VERTEX_SIZE);
        assert_eq!(p.meta.draws, scene.draws());
        assert_eq!(p.meta.bbox, scene.bounding_box());
        let json = p.meta.to_json();
        let back = PackedScene::from_parts(p.faces.clone(), p.edges.clone(), &json).unwrap();
        assert_eq!(back, p);
    }

    #[test]
    fn bad_packs_are_refused() {
        let scheme = ColorScheme::cornfield();
        let p = Scene::new(Some(&cube()), &scheme).pack();
        let json = p.meta.to_json();
        let short = p.faces[..p.faces.len() - 1].to_vec();
        assert!(PackedScene::from_parts(short, vec![], &json).is_err());
        let fewer = p.faces[..p.faces.len() - FACE_VERTEX_SIZE].to_vec();
        let e = PackedScene::from_parts(fewer, vec![], &json).unwrap_err();
        assert!(e.0.contains("past the 35"), "{e}");
        assert!(PackedScene::from_parts(p.faces.clone(), vec![0; 5], &json).is_err());
        assert!(PackedScene::from_parts(p.faces, vec![], "{").is_err());
    }

    /// Scenes covering every part of the format: a lit 3D mesh, a 2D shape
    /// with outlines, and a preview-style scene with states that differ
    /// and an image-space CSG product between its surfaces.
    fn scenes() -> Vec<(&'static str, Scene)> {
        use crate::scene::{CsgOp, CsgPrimitive, Cull, Depth, DrawState, Surface};
        use geom::polygon2d::Polygon2d;
        let scheme = ColorScheme::cornfield();
        let square = Polygon2d::from_outline(vec![[0.0, 0.0], [2.0, 0.0], [2.0, 1.0], [0.0, 1.0]]);
        let Geometry::PolySet(mesh) = cube() else {
            unreachable!()
        };
        let mesh = Arc::new(mesh.tessellate(&mut Vec::new()));
        let mut preview = Scene::empty(&scheme, Some(([0.0; 3], [1.0; 3])));
        let surface = |state| Surface {
            mesh: mesh.clone(),
            matrix: None,
            color: scheme.opencsg_face_front,
            force_color: false,
            lit: true,
            state,
        };
        preview.push(surface(DrawState::DEFAULT));
        preview.push_image_csg(vec![
            CsgPrimitive {
                mesh: mesh.clone(),
                matrix: None,
                op: CsgOp::Intersection,
            },
            CsgPrimitive {
                mesh: mesh.clone(),
                matrix: None,
                op: CsgOp::Subtraction,
            },
        ]);
        preview.push(surface(DrawState {
            cull: Cull::Front,
            depth: Depth::Equal,
            color_write: true,
            bias: true,
        }));
        vec![
            ("3D", Scene::new(Some(&cube()), &scheme)),
            (
                "2D",
                Scene::new(Some(&Geometry::Polygon2d(Arc::new(square))), &scheme),
            ),
            ("preview", preview),
            ("empty", Scene::new(None, &scheme)),
        ]
    }

    /// `Gpu::upload_packed(scene.pack())` puts exactly what
    /// `Gpu::upload(scene)` does on the GPU: the same bytes in both vertex
    /// buffers, read back from the device, and the same draws. This is
    /// what lets the web build pack a scene in its worker and upload it in
    /// the page without drawing anything different from the app.
    #[cfg(feature = "gpu")]
    #[test]
    fn upload_packed_equals_upload() {
        let gpu = match crate::viewport::Gpu::new_blocking(wgpu::Backends::PRIMARY) {
            Ok(g) => g,
            Err(e) => {
                eprintln!("skipped: {e}");
                return;
            }
        };
        for (name, scene) in scenes() {
            let direct = crate::gpu::SceneBuffers::upload(gpu.device(), &scene).unwrap();
            let packed = scene.pack();
            // Through the wire format, as the web build sends it.
            let packed =
                PackedScene::from_parts(packed.faces, packed.edges, &packed.meta.to_json())
                    .unwrap();
            let from_packed =
                crate::gpu::SceneBuffers::upload_packed(gpu.device(), &packed).unwrap();
            assert_eq!(direct.shape(), from_packed.shape(), "{name}: draws");
            let (df, de) = direct.vertex_buffers();
            let (pf, pe) = from_packed.vertex_buffers();
            assert_eq!(read(&gpu, df), read(&gpu, pf), "{name}: face bytes");
            assert_eq!(read(&gpu, de), read(&gpu, pe), "{name}: edge bytes");
            assert_eq!(read(&gpu, pf), packed.faces, "{name}: faces as packed");
            match name {
                "2D" => assert!(!packed.edges.is_empty()),
                "preview" => assert_eq!(packed.meta.image_csg.len(), 1),
                "empty" => assert!(df.is_none() && pf.is_none()),
                _ => {}
            }
            // And the model a viewport shows keeps the scene's box.
            let model = gpu.upload_packed(&packed).unwrap();
            assert_eq!(model.bounding_box(), scene.bounding_box());
        }
        let mut bad = scenes().remove(0).1.pack();
        bad.faces.truncate(bad.faces.len() - FACE_VERTEX_SIZE);
        assert!(matches!(
            gpu.upload_packed(&bad),
            Err(crate::offscreen::Error::InvalidScene(_))
        ));
    }

    /// A vertex buffer's bytes (empty for none), copied into a mappable
    /// buffer and read back.
    #[cfg(feature = "gpu")]
    fn read(gpu: &crate::viewport::Gpu, buffer: Option<&wgpu::Buffer>) -> Vec<u8> {
        let Some(buffer) = buffer else {
            return Vec::new();
        };
        let device = gpu.device();
        let staging = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("test readback"),
            size: buffer.size(),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = device.create_command_encoder(&Default::default());
        encoder.copy_buffer_to_buffer(buffer, 0, &staging, 0, buffer.size());
        gpu.queue().submit([encoder.finish()]);
        staging
            .slice(..)
            .map_async(wgpu::MapMode::Read, |r| r.unwrap());
        device
            .poll(wgpu::PollType::wait_indefinitely())
            .expect("the copy finishes");
        let out = staging
            .slice(..)
            .get_mapped_range()
            .expect("mapped for reading")
            .to_vec();
        staging.unmap();
        out
    }
}
