//! Drawing a [`Scene`] with wgpu into any colour target.
//!
//! Nothing here knows where the pixels go. [`Renderer`] holds the pipelines
//! for one colour format; [`SceneBuffers`] holds a scene's geometry on the
//! GPU; [`Renderer::draw`] records one frame into a command encoder, for a
//! colour view and a depth view the caller owns. The offscreen exporter
//! ([`crate::offscreen`]) is one caller, with a texture it reads back; the
//! macOS app's `CAMetalLayer` surface and the web app's WebGPU canvas are
//! others, handing in their surface texture's view each frame. A camera
//! change only rewrites the per-frame uniforms, so an interactive view does
//! not touch the geometry.
//!
//! Antialiasing: none. OpenSCAD's offscreen images are drawn into a
//! single-sample framebuffer object (`glRenderbufferStorage(GL_RENDERBUFFER,
//! GL_RGBA8, ...)` in `src/glview/fbo.cc`; the 4 samples its CGL context
//! asks for belong to the window framebuffer, which export does not use),
//! so its expected images have hard edges (a cube is exactly four colours,
//! `tests/regression/render-monotone/cube10-expected.png`). Multisampling
//! would add edge colours those images do not have. An interactive view may
//! want MSAA; [`Renderer::new`] takes the sample count for that reason.

use wgpu::util::DeviceExt;

use crate::camera::Camera;
use crate::scene::{EDGE_SEGMENT_SIZE, FACE_VERTEX_SIZE, Scene};
use crate::scheme::ColorScheme;

/// The depth buffer's format: OpenSCAD's framebuffer has 24-bit depth
/// (`GL_DEPTH24_STENCIL8`).
pub const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth24Plus;

/// `glLineWidth(2)` for 2D outlines (`PolySetRenderer::createPolygonEdgeStates`).
const OUTLINE_WIDTH: f32 = 2.0;

/// Bytes of the `Frame` uniform block in `shader.wgsl`.
const FRAME_SIZE: usize = 2 * 64 + 5 * 16;

/// A scene's geometry on the GPU.
#[derive(Debug)]
pub struct SceneBuffers {
    faces: Option<wgpu::Buffer>,
    face_vertices: u32,
    edges: Option<wgpu::Buffer>,
    edge_segments: u32,
    edge_color: [f32; 4],
}

impl SceneBuffers {
    /// Upload a scene. Each buffer is created mapped and filled straight
    /// from the scene's vertex iterators, so the vertices are written once,
    /// into memory the GPU reads, with no vertex array built beforehand.
    ///
    /// A scene too big for one buffer on this device (its
    /// `max_buffer_size`, or more than `u32::MAX` vertices) is refused
    /// with its size in bytes, rather than failing wgpu's validation.
    pub fn upload(device: &wgpu::Device, scene: &Scene) -> Result<SceneBuffers, TooLarge> {
        let max = device.limits().max_buffer_size;
        let face_vertices = scene.face_vertex_count();
        let edge_segments = scene.edge_segment_count();
        let bytes = (face_vertices as u64 * FACE_VERTEX_SIZE as u64)
            .max(edge_segments as u64 * EDGE_SEGMENT_SIZE as u64);
        let too_many = u32::try_from(face_vertices.max(edge_segments)).is_err();
        if bytes > max || too_many {
            return Err(TooLarge { bytes, max });
        }
        let faces = mapped_buffer(
            device,
            "neoscad faces",
            face_vertices,
            scene.face_vertices(),
        );
        let edges = mapped_buffer(
            device,
            "neoscad outlines",
            edge_segments,
            scene.edge_segments(),
        );
        Ok(SceneBuffers {
            faces,
            face_vertices: face_vertices as u32,
            edges,
            edge_segments: edge_segments as u32,
            edge_color: scene.edge_color().0,
        })
    }
}

/// A scene whose vertex buffer would be `bytes` long, over the device's
/// `max` buffer size.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TooLarge {
    pub bytes: u64,
    pub max: u64,
}

/// A vertex buffer of `count` items of `N` bytes, written from `items`
/// while mapped. `None` for no items (a zero-sized buffer cannot be bound).
fn mapped_buffer<const N: usize>(
    device: &wgpu::Device,
    label: &str,
    count: usize,
    items: impl Iterator<Item = [u8; N]>,
) -> Option<wgpu::Buffer> {
    if count == 0 {
        return None;
    }
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some(label),
        size: (count * N) as wgpu::BufferAddress,
        usage: wgpu::BufferUsages::VERTEX,
        mapped_at_creation: true,
    });
    {
        let mut view = buffer
            .slice(..)
            .get_mapped_range_mut()
            .expect("a buffer mapped at creation is writable");
        let (chunks, rest) = view.slice(..).into_chunks::<N>();
        debug_assert_eq!(rest.len(), 0);
        chunks.write_iter(items.take(count));
    }
    buffer.unmap();
    Some(buffer)
}

/// The per-frame values the shaders read: camera, lighting, image size
/// and the scheme's background.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FrameParams {
    clip_from_model: [[f64; 4]; 4],
    normal_matrix: [[f64; 3]; 3],
    width: u32,
    height: u32,
    background_top: [f32; 4],
    background_bottom: [f32; 4],
}

impl FrameParams {
    /// The view of `camera` (after any `--viewall`) at its pixel size, in
    /// `scheme`'s background.
    pub fn new(camera: &Camera, scheme: &ColorScheme) -> FrameParams {
        let gl = camera.gl_matrices();
        FrameParams {
            clip_from_model: gl.clip_from_model_zero_to_one(),
            normal_matrix: gl.normal_matrix(),
            width: camera.pixel_width,
            height: camera.pixel_height,
            background_top: scheme.background.0,
            background_bottom: scheme.background_stop.0,
        }
    }

    /// The `Frame` uniform block, column-major as WGSL matrices are.
    fn bytes(&self, edge_color: [f32; 4]) -> [u8; FRAME_SIZE] {
        let mut f: Vec<f32> = Vec::with_capacity(FRAME_SIZE / 4);
        for c in 0..4 {
            for r in 0..4 {
                f.push(self.clip_from_model[r][c] as f32);
            }
        }
        for c in 0..4 {
            for r in 0..4 {
                f.push(if r < 3 && c < 3 {
                    self.normal_matrix[r][c] as f32
                } else {
                    0.0
                });
            }
        }
        // GL_LIGHT0 at (-1, 1, 1, 0) in eye space: a direction, which
        // fixed-function lighting normalises.
        let l = 1.0 / 3.0f64.sqrt();
        f.extend([-l as f32, l as f32, l as f32, 0.0]);
        f.extend([self.width as f32, self.height as f32, OUTLINE_WIDTH, 0.0]);
        f.extend(edge_color);
        f.extend(self.background_top);
        f.extend(self.background_bottom);
        let mut out = [0u8; FRAME_SIZE];
        for (i, x) in f.into_iter().enumerate() {
            out[4 * i..4 * i + 4].copy_from_slice(&x.to_le_bytes());
        }
        out
    }
}

/// The pipelines for one colour format and sample count.
#[derive(Debug)]
pub struct Renderer {
    background: wgpu::RenderPipeline,
    faces: wgpu::RenderPipeline,
    edges: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
}

impl Renderer {
    /// Pipelines drawing into targets of `format` with `sample_count`
    /// samples, and a [`DEPTH_FORMAT`] depth buffer. The format should be a
    /// linear (non-sRGB) one: OpenSCAD writes its colours to the
    /// framebuffer unconverted.
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat, sample_count: u32) -> Renderer {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("neoscad render shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shader.wgsl").into()),
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("neoscad frame"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("neoscad render"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        // glBlendFunc(GL_SRC_ALPHA, GL_ONE_MINUS_SRC_ALPHA), which GL
        // applies to the alpha channel as well.
        let blend = wgpu::BlendState {
            color: wgpu::BlendComponent {
                src_factor: wgpu::BlendFactor::SrcAlpha,
                dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                operation: wgpu::BlendOperation::Add,
            },
            alpha: wgpu::BlendComponent {
                src_factor: wgpu::BlendFactor::SrcAlpha,
                dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                operation: wgpu::BlendOperation::Add,
            },
        };
        let face_attributes =
            wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x3, 2 => Float32x4];
        let edge_attributes = wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x3];
        let pipeline = |label: &str,
                        vs: &str,
                        fs: &str,
                        buffers: &[Option<wgpu::VertexBufferLayout<'_>>],
                        depth_test: bool,
                        blend: Option<wgpu::BlendState>| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(&pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some(vs),
                    compilation_options: Default::default(),
                    buffers,
                },
                // No culling: GL_CULL_FACE is disabled in paintGL.
                primitive: wgpu::PrimitiveState {
                    topology: wgpu::PrimitiveTopology::TriangleList,
                    cull_mode: None,
                    ..Default::default()
                },
                // glDepthFunc(GL_LESS) with depth writes, or, for the
                // background and 2D outlines, no depth test at all.
                depth_stencil: Some(wgpu::DepthStencilState {
                    format: DEPTH_FORMAT,
                    depth_write_enabled: Some(depth_test),
                    depth_compare: Some(if depth_test {
                        wgpu::CompareFunction::Less
                    } else {
                        wgpu::CompareFunction::Always
                    }),
                    stencil: Default::default(),
                    bias: Default::default(),
                }),
                multisample: wgpu::MultisampleState {
                    count: sample_count,
                    ..Default::default()
                },
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some(fs),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format,
                        blend,
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                multiview_mask: None,
                cache: None,
            })
        };
        let background = pipeline(
            "neoscad background",
            "background_vs",
            "background_fs",
            &[],
            false,
            None,
        );
        let faces = pipeline(
            "neoscad faces",
            "face_vs",
            "face_fs",
            &[Some(wgpu::VertexBufferLayout {
                array_stride: FACE_VERTEX_SIZE as wgpu::BufferAddress,
                step_mode: wgpu::VertexStepMode::Vertex,
                attributes: &face_attributes,
            })],
            true,
            Some(blend),
        );
        let edges = pipeline(
            "neoscad outlines",
            "edge_vs",
            "edge_fs",
            &[Some(wgpu::VertexBufferLayout {
                array_stride: EDGE_SEGMENT_SIZE as wgpu::BufferAddress,
                step_mode: wgpu::VertexStepMode::Instance,
                attributes: &edge_attributes,
            })],
            false,
            Some(blend),
        );
        Renderer {
            background,
            faces,
            edges,
            layout,
        }
    }

    /// Record one frame: clear `color` and `depth`, draw the background
    /// gradient if the scheme has one, the faces, then the 2D outlines
    /// (`GLView::paintGL`'s order). With multisampling, `resolve` receives
    /// the resolved image.
    #[allow(clippy::too_many_arguments)]
    pub fn draw(
        &self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        color: &wgpu::TextureView,
        resolve: Option<&wgpu::TextureView>,
        depth: &wgpu::TextureView,
        scene: &SceneBuffers,
        frame: &FrameParams,
    ) {
        let uniforms = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("neoscad frame"),
            contents: &frame.bytes(scene.edge_color),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("neoscad frame"),
            layout: &self.layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: uniforms.as_entire_binding(),
            }],
        });
        let [r, g, b, _] = frame.background_top.map(f64::from);
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("neoscad frame"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: color,
                depth_slice: None,
                resolve_target: resolve,
                ops: wgpu::Operations {
                    // glClearColor(background, 1).
                    load: wgpu::LoadOp::Clear(wgpu::Color { r, g, b, a: 1.0 }),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: depth,
                depth_ops: Some(wgpu::Operations {
                    load: wgpu::LoadOp::Clear(1.0),
                    store: wgpu::StoreOp::Discard,
                }),
                stencil_ops: None,
            }),
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_bind_group(0, &bind_group, &[]);
        if frame.background_top != frame.background_bottom {
            pass.set_pipeline(&self.background);
            pass.draw(0..6, 0..1);
        }
        if let Some(faces) = &scene.faces {
            pass.set_pipeline(&self.faces);
            pass.set_vertex_buffer(0, faces.slice(..));
            pass.draw(0..scene.face_vertices, 0..1);
        }
        if let Some(edges) = &scene.edges {
            pass.set_pipeline(&self.edges);
            pass.set_vertex_buffer(0, edges.slice(..));
            pass.draw(0..6, 0..scene.edge_segments);
        }
    }
}
