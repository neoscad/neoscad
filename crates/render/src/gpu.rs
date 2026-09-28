//! Drawing a [`Scene`] with wgpu into any colour target.
//!
//! Nothing here knows where the pixels go. [`Renderer`] holds the pipelines
//! for one colour format; [`SceneBuffers`] holds a scene's geometry on the
//! GPU; [`Renderer::draw`] records one frame into a command encoder, for a
//! colour view and a depth view the caller owns. The offscreen exporter
//! ([`crate::offscreen`]) is one caller, with a texture it reads back; the
//! macOS app's `CAMetalLayer` surface and the web app's WebGPU canvas are
//! others, handing in their surface texture's view each frame. A camera
//! change only rewrites the per-frame uniforms and the view-option lines,
//! so an interactive view does not touch the geometry.
//!
//! A scene's draws each carry fixed-function state (culling, depth test,
//! colour writes; see [`DrawState`]). Each combination is a separate
//! pipeline, made the first time a scene needs it and kept.
//!
//! Antialiasing: none. OpenSCAD's offscreen images are drawn into a
//! single-sample framebuffer object (`glRenderbufferStorage(GL_RENDERBUFFER,
//! GL_RGBA8, ...)` in `src/glview/fbo.cc`; the 4 samples its CGL context
//! asks for belong to the window framebuffer, which export does not use),
//! so its expected images have hard edges (a cube is exactly four colours,
//! `tests/regression/render-monotone/cube10-expected.png`). Multisampling
//! would add edge colours those images do not have. An interactive view may
//! want MSAA; [`Renderer::new`] takes the sample count for that reason.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use wgpu::util::DeviceExt;

use crate::camera::{self, Camera};
use crate::overlay::{LINE_VERTEX_SIZE, LineVertex, Overlay};
use crate::scene::{
    CsgOp, Cull, Depth, Draw, DrawState, EDGE_SEGMENT_SIZE, FACE_VERTEX_SIZE, ImageCsgDraws, Scene,
};
use crate::scheme::ColorScheme;

/// The depth buffer's format: OpenSCAD's framebuffer has 24-bit depth
/// (`GL_DEPTH24_STENCIL8`).
pub const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth24Plus;

/// `glLineWidth(2)` for 2D outlines (`PolySetRenderer::createPolygonEdgeStates`).
const OUTLINE_WIDTH: f32 = 2.0;

/// Bytes of the `Frame` uniform block in `shader.wgsl`.
const FRAME_SIZE: usize = 4 * 64 + 6 * 16;

/// A scene's geometry on the GPU.
#[derive(Debug)]
pub struct SceneBuffers {
    faces: Option<wgpu::Buffer>,
    draws: Vec<Draw>,
    image_csg: Vec<ImageCsgDraws>,
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
            draws: scene.draws(),
            image_csg: scene.image_csg(),
            edges,
            edge_segments: edge_segments as u32,
            edge_color: scene.edge_color().0,
        })
    }
}

impl SceneBuffers {
    /// Whether the scene has an image-space CSG product, whose frame is
    /// several render passes: the frame's colour and depth buffers must
    /// then keep their contents between passes.
    pub fn has_image_csg(&self) -> bool {
        !self.image_csg.is_empty()
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

/// The per-frame values the shaders read: camera, lighting, image size,
/// the scheme's background and whether edges are shown.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FrameParams {
    clip_from_model: [[f64; 4]; 4],
    normal_matrix: [[f64; 3]; 3],
    clip_from_view: [[f64; 4]; 4],
    clip_from_small_axes: [[f64; 4]; 4],
    width: u32,
    height: u32,
    background_top: [f32; 4],
    background_bottom: [f32; 4],
    edges: bool,
    lighting: crate::Lighting,
}

impl FrameParams {
    /// The view of `camera` (after any `--viewall`) at its pixel size, in
    /// `scheme`'s background; `edges` draws faces with their edges
    /// (`--view edges`).
    pub fn new(camera: &Camera, scheme: &ColorScheme, edges: bool) -> FrameParams {
        let gl = camera.gl_matrices();
        // `setupCamera` leaves the modelview without the translation for
        // the crosshairs.
        let untranslated = camera::mul(
            &gl.modelview,
            &camera::translation(camera.object_trans.map(|t| -t)),
        );
        FrameParams {
            clip_from_model: gl.clip_from_model_zero_to_one(),
            normal_matrix: gl.normal_matrix(),
            clip_from_view: camera::zero_to_one(&camera::mul(&gl.projection, &untranslated)),
            clip_from_small_axes: crate::overlay::small_axes_clip(camera),
            width: camera.pixel_width,
            height: camera.pixel_height,
            background_top: scheme.background.0,
            background_bottom: scheme.background_stop.0,
            edges,
            lighting: crate::Lighting::OpenScad,
        }
    }

    /// The same frame lit another way (OpenSCAD's lighting by default).
    pub fn with_lighting(self, lighting: crate::Lighting) -> FrameParams {
        FrameParams { lighting, ..self }
    }

    /// The `Frame` uniform block, column-major as WGSL matrices are.
    fn bytes(&self, edge_color: [f32; 4]) -> [u8; FRAME_SIZE] {
        let mut f: Vec<f32> = Vec::with_capacity(FRAME_SIZE / 4);
        let mat4 = |f: &mut Vec<f32>, m: &[[f64; 4]; 4]| {
            for c in 0..4 {
                f.extend(m.iter().map(|row| row[c] as f32));
            }
        };
        mat4(&mut f, &self.clip_from_model);
        for c in 0..4 {
            for r in 0..4 {
                f.push(if r < 3 && c < 3 {
                    self.normal_matrix[r][c] as f32
                } else {
                    0.0
                });
            }
        }
        mat4(&mut f, &self.clip_from_view);
        mat4(&mut f, &self.clip_from_small_axes);
        // OpenSCAD's GL_LIGHT0 is at (-1, 1, 1, 0) in eye space: a
        // direction, which fixed-function lighting normalises.
        let (l, ambient, diffuse) = self.lighting.params();
        f.extend([l[0] as f32, l[1] as f32, l[2] as f32, 0.0]);
        f.extend([
            self.width as f32,
            self.height as f32,
            OUTLINE_WIDTH,
            if self.edges { 1.0 } else { 0.0 },
        ]);
        f.extend(edge_color);
        f.extend(self.background_top);
        f.extend(self.background_bottom);
        f.extend([ambient as f32, diffuse as f32, 0.0, 0.0]);
        let mut out = [0u8; FRAME_SIZE];
        for (i, x) in f.into_iter().enumerate() {
            out[4 * i..4 * i + 4].copy_from_slice(&x.to_le_bytes());
        }
        out
    }
}

/// What every pipeline is built from.
#[derive(Debug)]
struct Base {
    device: wgpu::Device,
    format: wgpu::TextureFormat,
    sample_count: u32,
    shader: wgpu::ShaderModule,
    pipeline_layout: wgpu::PipelineLayout,
}

/// The pipelines for one colour format and sample count.
#[derive(Debug)]
pub struct Renderer {
    base: Base,
    background: wgpu::RenderPipeline,
    edges: wgpu::RenderPipeline,
    /// View-option lines: depth-tested before the model, over it after.
    lines_tested: wgpu::RenderPipeline,
    lines_over: wgpu::RenderPipeline,
    /// Lines hidden by the model but not hiding it (the app's grid): built
    /// on first use, so the command line, which never draws them, does not
    /// pay for the pipeline at start-up.
    lines_behind: OnceLock<wgpu::RenderPipeline>,
    faces: Mutex<HashMap<DrawState, Arc<wgpu::RenderPipeline>>>,
    layout: wgpu::BindGroupLayout,
    /// Image-space CSG's pipelines, built the first time a scene has such
    /// a product, and its buffers at the last size drawn.
    csg: OnceLock<CsgPipelines>,
    csg_targets: Mutex<Option<Arc<CsgTargets>>>,
}

/// glBlendFunc(GL_SRC_ALPHA, GL_ONE_MINUS_SRC_ALPHA), which GL applies to
/// the alpha channel as well; GL_BLEND is on throughout.
const BLEND: wgpu::BlendState = wgpu::BlendState {
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

/// How one pipeline differs from the others.
struct PipelineSpec<'a> {
    label: &'a str,
    vs: &'a str,
    fs: &'a str,
    buffers: &'a [Option<wgpu::VertexBufferLayout<'a>>],
    topology: wgpu::PrimitiveTopology,
    cull: Option<wgpu::Face>,
    depth: wgpu::CompareFunction,
    depth_write: bool,
    bias: wgpu::DepthBiasState,
    color_write: bool,
}

impl Base {
    fn pipeline(&self, s: &PipelineSpec<'_>) -> wgpu::RenderPipeline {
        self.device
            .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(s.label),
                layout: Some(&self.pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &self.shader,
                    entry_point: Some(s.vs),
                    compilation_options: Default::default(),
                    buffers: s.buffers,
                },
                // Counter-clockwise is front, as in OpenGL.
                primitive: wgpu::PrimitiveState {
                    topology: s.topology,
                    front_face: wgpu::FrontFace::Ccw,
                    cull_mode: s.cull,
                    ..Default::default()
                },
                depth_stencil: Some(wgpu::DepthStencilState {
                    format: DEPTH_FORMAT,
                    depth_write_enabled: Some(s.depth_write),
                    depth_compare: Some(s.depth),
                    stencil: Default::default(),
                    bias: s.bias,
                }),
                multisample: wgpu::MultisampleState {
                    count: self.sample_count,
                    ..Default::default()
                },
                fragment: Some(wgpu::FragmentState {
                    module: &self.shader,
                    entry_point: Some(s.fs),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: self.format,
                        blend: Some(BLEND),
                        write_mask: if s.color_write {
                            wgpu::ColorWrites::ALL
                        } else {
                            wgpu::ColorWrites::empty()
                        },
                    })],
                }),
                multiview_mask: None,
                cache: None,
            })
    }
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
        let base = Base {
            device: device.clone(),
            format,
            sample_count,
            shader,
            pipeline_layout,
        };
        let edge_attributes = wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x3];
        let line_attributes = wgpu::vertex_attr_array![
            0 => Float32x4, 1 => Float32x4, 2 => Float32x4, 3 => Uint32, 4 => Uint32
        ];
        let no_bias = wgpu::DepthBiasState::default();
        let background = base.pipeline(&PipelineSpec {
            label: "neoscad background",
            vs: "background_vs",
            fs: "background_fs",
            buffers: &[],
            topology: wgpu::PrimitiveTopology::TriangleList,
            cull: None,
            depth: wgpu::CompareFunction::Always,
            depth_write: false,
            bias: no_bias,
            color_write: true,
        });
        let edges = base.pipeline(&PipelineSpec {
            label: "neoscad outlines",
            vs: "edge_vs",
            fs: "edge_fs",
            buffers: &[Some(wgpu::VertexBufferLayout {
                array_stride: EDGE_SEGMENT_SIZE as wgpu::BufferAddress,
                step_mode: wgpu::VertexStepMode::Instance,
                attributes: &edge_attributes,
            })],
            topology: wgpu::PrimitiveTopology::TriangleList,
            cull: None,
            depth: wgpu::CompareFunction::Always,
            depth_write: false,
            bias: no_bias,
            color_write: true,
        });
        let line_buffers = [Some(wgpu::VertexBufferLayout {
            array_stride: LINE_VERTEX_SIZE as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &line_attributes,
        })];
        let lines_tested = base.pipeline(&PipelineSpec {
            label: "neoscad lines",
            vs: "line_vs",
            fs: "line_fs",
            buffers: &line_buffers,
            topology: wgpu::PrimitiveTopology::LineList,
            cull: None,
            depth: wgpu::CompareFunction::Less,
            depth_write: true,
            bias: no_bias,
            color_write: true,
        });
        let lines_over = base.pipeline(&PipelineSpec {
            label: "neoscad lines over",
            vs: "line_vs",
            fs: "line_fs",
            buffers: &line_buffers,
            topology: wgpu::PrimitiveTopology::LineList,
            cull: None,
            depth: wgpu::CompareFunction::Always,
            depth_write: false,
            bias: no_bias,
            color_write: true,
        });
        Renderer {
            base,
            background,
            edges,
            lines_tested,
            lines_over,
            lines_behind: OnceLock::new(),
            faces: Mutex::new(HashMap::new()),
            layout,
            csg: OnceLock::new(),
            csg_targets: Mutex::new(None),
        }
    }

    /// The pipeline for [`Overlay::behind`], built on first use.
    fn lines_behind(&self) -> &wgpu::RenderPipeline {
        self.lines_behind.get_or_init(|| {
            let attributes = wgpu::vertex_attr_array![
                0 => Float32x4, 1 => Float32x4, 2 => Float32x4, 3 => Uint32, 4 => Uint32
            ];
            self.base.pipeline(&PipelineSpec {
                label: "neoscad lines behind",
                vs: "line_vs",
                fs: "line_fs",
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: LINE_VERTEX_SIZE as wgpu::BufferAddress,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &attributes,
                })],
                topology: wgpu::PrimitiveTopology::LineList,
                cull: None,
                depth: wgpu::CompareFunction::Less,
                depth_write: false,
                bias: wgpu::DepthBiasState::default(),
                color_write: true,
            })
        })
    }

    /// The face pipeline for `state`, built on first use.
    fn faces(&self, state: DrawState) -> Arc<wgpu::RenderPipeline> {
        let mut map = self.faces.lock().unwrap_or_else(|p| p.into_inner());
        map.entry(state)
            .or_insert_with(|| {
                let attributes = wgpu::vertex_attr_array![
                    0 => Float32x3, 1 => Float32x3, 2 => Float32x4, 3 => Unorm8x4
                ];
                Arc::new(self.base.pipeline(&PipelineSpec {
                    label: "neoscad faces",
                    vs: "face_vs",
                    fs: "face_fs",
                    buffers: &[Some(wgpu::VertexBufferLayout {
                        array_stride: FACE_VERTEX_SIZE as wgpu::BufferAddress,
                        step_mode: wgpu::VertexStepMode::Vertex,
                        attributes: &attributes,
                    })],
                    topology: wgpu::PrimitiveTopology::TriangleList,
                    cull: match state.cull {
                        Cull::None => None,
                        Cull::Front => Some(wgpu::Face::Front),
                        Cull::Back => Some(wgpu::Face::Back),
                    },
                    depth: match state.depth {
                        Depth::Less => wgpu::CompareFunction::Less,
                        Depth::LessEqual => wgpu::CompareFunction::LessEqual,
                        Depth::Equal => wgpu::CompareFunction::Equal,
                        Depth::Always => wgpu::CompareFunction::Always,
                    },
                    depth_write: true,
                    bias: if state.bias {
                        wgpu::DepthBiasState {
                            constant: -2,
                            slope_scale: -0.5,
                            clamp: 0.0,
                        }
                    } else {
                        wgpu::DepthBiasState::default()
                    },
                    color_write: state.color_write,
                }))
            })
            .clone()
    }

    /// Record one frame: clear `color` and `depth`, draw the background
    /// gradient if the scheme has one, the view-option lines that go under
    /// the model, the model's draws, its 2D outlines, the lines the model
    /// hides without being hidden by them (the app's grid, which OpenSCAD
    /// does not have), and the lines that go over everything
    /// (`GLView::paintGL`'s order). With multisampling,
    /// `resolve` receives the resolved image.
    ///
    /// A scene with image-space CSG products ([`SceneBuffers::has_image_csg`])
    /// is drawn in several render passes, so `color` and `depth` must then
    /// keep their contents between passes (not be memoryless).
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
        overlay: &Overlay,
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
        let lines = |v: &[LineVertex]| {
            mapped_buffer(
                device,
                "neoscad lines",
                v.len(),
                v.iter().map(LineVertex::bytes),
            )
        };
        let (before, behind, after) = (
            lines(&overlay.before),
            lines(&overlay.behind),
            lines(&overlay.after),
        );
        let behind_pipeline = behind.as_ref().map(|_| self.lines_behind());
        let pipelines: Vec<(Draw, Arc<wgpu::RenderPipeline>)> = scene
            .draws
            .iter()
            .map(|d| (*d, self.faces(d.state)))
            .collect();
        let [r, g, b, _] = frame.background_top.map(f64::from);
        let csg = (!scene.image_csg.is_empty()).then(|| {
            let size = color.texture().size();
            (
                self.csg_pipelines(),
                self.csg_targets(size.width, size.height),
            )
        });
        // The frame is one render pass, split before each image-space
        // product: its SCS pass draws into buffers of its own, and the
        // frame's pass resumes with the product's merge.
        let segments = scene.image_csg.len() + 1;
        let mut next_draw = 0;
        for segment in 0..segments {
            let first = segment == 0;
            let last = segment + 1 == segments;
            let end_draw = match scene.image_csg.get(segment) {
                Some(p) => p.at_draw,
                None => pipelines.len(),
            };
            if let (Some((csg, targets)), false) = (&csg, first) {
                csg.render(
                    encoder,
                    targets,
                    &bind_group,
                    scene,
                    &scene.image_csg[segment - 1],
                );
            }
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("neoscad frame"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: color,
                    depth_slice: None,
                    resolve_target: if last { resolve } else { None },
                    ops: wgpu::Operations {
                        // glClearColor(background, 1).
                        load: if first {
                            wgpu::LoadOp::Clear(wgpu::Color { r, g, b, a: 1.0 })
                        } else {
                            wgpu::LoadOp::Load
                        },
                        // With a resolve target only the resolved image is
                        // wanted; keeping the multisampled one would write
                        // four samples a pixel back to memory every frame.
                        store: if last && resolve.is_some() {
                            wgpu::StoreOp::Discard
                        } else {
                            wgpu::StoreOp::Store
                        },
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: depth,
                    depth_ops: Some(wgpu::Operations {
                        load: if first {
                            wgpu::LoadOp::Clear(1.0)
                        } else {
                            wgpu::LoadOp::Load
                        },
                        store: if last {
                            wgpu::StoreOp::Discard
                        } else {
                            wgpu::StoreOp::Store
                        },
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_bind_group(0, &bind_group, &[]);
            if first {
                if frame.background_top != frame.background_bottom {
                    pass.set_pipeline(&self.background);
                    pass.draw(0..6, 0..1);
                }
                if let Some(b) = &before {
                    pass.set_pipeline(&self.lines_tested);
                    pass.set_vertex_buffer(0, b.slice(..));
                    pass.draw(0..overlay.before.len() as u32, 0..1);
                }
            }
            if let Some(faces) = &scene.faces {
                pass.set_vertex_buffer(0, faces.slice(..));
                if let (Some((csg, targets)), false) = (&csg, first) {
                    csg.merge(&mut pass, targets, &scene.image_csg[segment - 1]);
                }
                for (d, p) in &pipelines[next_draw..end_draw] {
                    pass.set_pipeline(p);
                    pass.draw(d.first..d.first + d.count, 0..1);
                }
            }
            next_draw = end_draw;
            if !last {
                continue;
            }
            if let Some(edges) = &scene.edges {
                pass.set_pipeline(&self.edges);
                pass.set_vertex_buffer(0, edges.slice(..));
                pass.draw(0..6, 0..scene.edge_segments);
            }
            if let (Some(b), Some(p)) = (&behind, behind_pipeline) {
                pass.set_pipeline(p);
                pass.set_vertex_buffer(0, b.slice(..));
                pass.draw(0..overlay.behind.len() as u32, 0..1);
            }
            if let Some(a) = &after {
                pass.set_pipeline(&self.lines_over);
                pass.set_vertex_buffer(0, a.slice(..));
                pass.draw(0..overlay.after.len() as u32, 0..1);
            }
        }
    }

    /// Image-space CSG's pipelines, built on first use.
    fn csg_pipelines(&self) -> &CsgPipelines {
        self.csg
            .get_or_init(|| CsgPipelines::new(&self.base, &self.layout))
    }

    /// Image-space CSG's ID and depth-stencil buffers for a frame of
    /// `width` by `height`, kept while frames stay that size.
    fn csg_targets(&self, width: u32, height: u32) -> Arc<CsgTargets> {
        let mut t = self.csg_targets.lock().unwrap_or_else(|p| p.into_inner());
        match &*t {
            Some(t) if t.size == (width, height) => t.clone(),
            _ => {
                let new = Arc::new(CsgTargets::new(
                    &self.base.device,
                    &self.csg_pipelines().ids_layout,
                    width,
                    height,
                ));
                *t = Some(new.clone());
                new
            }
        }
    }
}

// --- Image-space CSG ---------------------------------------------------------------
//
// OpenSCAD draws every CSG product with OpenCSG (`OpenCSGRenderer::draw`);
// here most products are drawn from a boolean instead (see `preview.rs`),
// which shows the same thing whenever each leaf bounds a solid. A product
// with a leaf that does not (inside out, a face flipped, not closed) is
// drawn as OpenCSG draws it, with the Sequenced Convex Subtraction
// algorithm (`renderSCS.cpp`), which OpenCSG picks when no primitive has a
// convexity of 2 or more (`chooseAlgorithm`):
//
// 1. Into an ID buffer and a depth-stencil buffer of its own, depth cleared
//    to 0: the furthest front face of the intersected primitives, with its
//    primitive's ID (`renderIntersectedFront`). With several intersected
//    primitives, the back faces behind it are counted in the stencil, and
//    where there are fewer than primitives nothing is inside all of them:
//    depth 0 and no ID.
// 2. For each subtracted primitive in the Schoenfield order: mark where its
//    front faces lie in front of the depth, and there move the depth back
//    to its back faces behind it, with its ID (`subtractPrimitives`).
// 3. Where an intersected primitive's back face lies in front of the depth,
//    the pixel is outside that primitive: no ID (`renderIntersectedBack`).
// 4. In the frame: each primitive's faces (front for intersected, back for
//    subtracted) set the depth where the ID buffer holds its ID
//    (`SCSChannelManagerGLSLProgram::merge`), with `GL_LESS`.
//
// Front and back are winding on screen, never geometry: that is what makes
// an inside-out primitive disappear (its "front" faces are its far side, and
// its "back" faces, in front of them, mask it in step 3).
//
// OpenSCAD gives OpenCSG no bounding boxes, so each primitive fills the
// screen as far as OpenCSG knows: every subtracted primitive is a batch of
// its own and the scissor is the whole frame. With more than 20 primitives
// OpenCSG repeats the subtractions until occlusion queries report no change
// where the GPU has them; the Schoenfield order is used here throughout.
// The camera-outside optimisation is on, OpenCSG's default.

/// The ID buffer's format: one primitive ID a pixel, 0 for none.
const CSG_ID_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::R32Uint;
const CSG_DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth24PlusStencil8;

/// OpenCSG's stencil reference values run up to `stencilMax - 1` (8
/// stencil bits) before it clears the stencil and starts again at 1.
const STENCIL_MAX: u32 = 256;

#[derive(Debug)]
struct CsgPipelines {
    /// Furthest front faces, with their ID.
    front_id: wgpu::RenderPipeline,
    /// Count the back faces behind the depth in the stencil.
    count_back: wgpu::RenderPipeline,
    /// Where the count is not the reference: depth 0, no ID.
    reset: wgpu::RenderPipeline,
    /// Mark where front faces lie in front of the depth.
    mark_front: wgpu::RenderPipeline,
    /// Where marked, back faces behind the depth, with their ID.
    subtract_back: wgpu::RenderPipeline,
    /// Back faces in front of the depth: no ID.
    mask_back: wgpu::RenderPipeline,
    /// Stencil back to 0 (`glClear(GL_STENCIL_BUFFER_BIT)`).
    clear_stencil: wgpu::RenderPipeline,
    /// The frame's merge, for intersected and for subtracted primitives.
    merge_intersected: wgpu::RenderPipeline,
    merge_subtracted: wgpu::RenderPipeline,
    ids_layout: wgpu::BindGroupLayout,
}

/// How one SCS pipeline differs from the others.
struct CsgSpec {
    label: &'static str,
    /// A screen-sized quad instead of the primitives' faces.
    quad: bool,
    /// Write the primitive's ID (else 0) where colour is written.
    id: bool,
    cull: Option<wgpu::Face>,
    depth: wgpu::CompareFunction,
    depth_write: bool,
    color_write: bool,
    stencil: Option<(
        wgpu::CompareFunction,
        wgpu::StencilOperation,
        wgpu::StencilOperation,
    )>,
}

fn face_buffers() -> [Option<wgpu::VertexBufferLayout<'static>>; 1] {
    const ATTRIBUTES: [wgpu::VertexAttribute; 4] = wgpu::vertex_attr_array![
        0 => Float32x3, 1 => Float32x3, 2 => Float32x4, 3 => Unorm8x4
    ];
    [Some(wgpu::VertexBufferLayout {
        array_stride: FACE_VERTEX_SIZE as wgpu::BufferAddress,
        step_mode: wgpu::VertexStepMode::Vertex,
        attributes: &ATTRIBUTES,
    })]
}

impl CsgPipelines {
    fn new(base: &Base, frame_layout: &wgpu::BindGroupLayout) -> CsgPipelines {
        use wgpu::CompareFunction as C;
        use wgpu::StencilOperation as S;
        let device = &base.device;
        let scs = |spec: CsgSpec| {
            // `(compare, fail and depth-fail op, pass op)`; OpenCSG's
            // `glStencilOp` calls set the fail and depth-fail ops alike.
            let stencil = match spec.stencil {
                None => wgpu::StencilState::default(),
                Some((compare, fail, pass)) => {
                    let face = wgpu::StencilFaceState {
                        compare,
                        fail_op: fail,
                        depth_fail_op: fail,
                        pass_op: pass,
                    };
                    wgpu::StencilState {
                        front: face,
                        back: face,
                        read_mask: 0xff,
                        write_mask: 0xff,
                    }
                }
            };
            let buffers = face_buffers();
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(spec.label),
                layout: Some(&base.pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &base.shader,
                    entry_point: Some(if spec.quad { "csg_quad_vs" } else { "face_vs" }),
                    compilation_options: Default::default(),
                    buffers: if spec.quad { &[] } else { &buffers },
                },
                primitive: wgpu::PrimitiveState {
                    topology: wgpu::PrimitiveTopology::TriangleList,
                    front_face: wgpu::FrontFace::Ccw,
                    cull_mode: spec.cull,
                    ..Default::default()
                },
                depth_stencil: Some(wgpu::DepthStencilState {
                    format: CSG_DEPTH_FORMAT,
                    depth_write_enabled: Some(spec.depth_write),
                    depth_compare: Some(spec.depth),
                    stencil,
                    bias: wgpu::DepthBiasState::default(),
                }),
                multisample: wgpu::MultisampleState::default(),
                fragment: Some(wgpu::FragmentState {
                    module: &base.shader,
                    entry_point: Some(if spec.id { "csg_id_fs" } else { "csg_zero_fs" }),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: CSG_ID_FORMAT,
                        blend: None,
                        write_mask: if spec.color_write {
                            wgpu::ColorWrites::ALL
                        } else {
                            wgpu::ColorWrites::empty()
                        },
                    })],
                }),
                multiview_mask: None,
                cache: None,
            })
        };
        let front = Some(wgpu::Face::Front);
        let back = Some(wgpu::Face::Back);
        let front_id = scs(CsgSpec {
            label: "neoscad csg front",
            quad: false,
            id: true,
            cull: back,
            depth: C::Greater,
            depth_write: true,
            color_write: true,
            stencil: None,
        });
        let count_back = scs(CsgSpec {
            label: "neoscad csg count",
            quad: false,
            id: false,
            cull: front,
            depth: C::Greater,
            depth_write: false,
            color_write: false,
            stencil: Some((C::Always, S::Keep, S::IncrementClamp)),
        });
        let reset = scs(CsgSpec {
            label: "neoscad csg reset",
            quad: true,
            id: false,
            cull: None,
            depth: C::Always,
            depth_write: true,
            color_write: true,
            stencil: Some((C::NotEqual, S::Zero, S::Zero)),
        });
        let mark_front = scs(CsgSpec {
            label: "neoscad csg mark",
            quad: false,
            id: false,
            cull: back,
            depth: C::Less,
            depth_write: false,
            color_write: false,
            stencil: Some((C::Always, S::Keep, S::Replace)),
        });
        let subtract_back = scs(CsgSpec {
            label: "neoscad csg subtract",
            quad: false,
            id: true,
            cull: front,
            depth: C::Greater,
            depth_write: true,
            color_write: true,
            stencil: Some((C::Equal, S::Zero, S::Zero)),
        });
        let mask_back = scs(CsgSpec {
            label: "neoscad csg mask",
            quad: false,
            id: false,
            cull: front,
            depth: C::Less,
            depth_write: false,
            color_write: true,
            stencil: None,
        });
        let clear_stencil = scs(CsgSpec {
            label: "neoscad csg clear stencil",
            quad: true,
            id: false,
            cull: None,
            depth: C::Always,
            depth_write: false,
            color_write: false,
            stencil: Some((C::Always, S::Replace, S::Replace)),
        });
        let ids_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("neoscad csg ids"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Uint,
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            }],
        });
        let merge_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("neoscad csg merge"),
            bind_group_layouts: &[Some(frame_layout), Some(&ids_layout)],
            immediate_size: 0,
        });
        let merge = |label, cull| {
            let buffers = face_buffers();
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(&merge_layout),
                vertex: wgpu::VertexState {
                    module: &base.shader,
                    entry_point: Some("face_vs"),
                    compilation_options: Default::default(),
                    buffers: &buffers,
                },
                primitive: wgpu::PrimitiveState {
                    topology: wgpu::PrimitiveTopology::TriangleList,
                    front_face: wgpu::FrontFace::Ccw,
                    cull_mode: cull,
                    ..Default::default()
                },
                depth_stencil: Some(wgpu::DepthStencilState {
                    format: DEPTH_FORMAT,
                    depth_write_enabled: Some(true),
                    depth_compare: Some(C::Less),
                    stencil: Default::default(),
                    bias: wgpu::DepthBiasState::default(),
                }),
                multisample: wgpu::MultisampleState {
                    count: base.sample_count,
                    ..Default::default()
                },
                fragment: Some(wgpu::FragmentState {
                    module: &base.shader,
                    entry_point: Some("csg_merge_fs"),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: base.format,
                        blend: Some(BLEND),
                        write_mask: wgpu::ColorWrites::empty(),
                    })],
                }),
                multiview_mask: None,
                cache: None,
            })
        };
        CsgPipelines {
            front_id,
            count_back,
            reset,
            mark_front,
            subtract_back,
            mask_back,
            clear_stencil,
            merge_intersected: merge("neoscad csg merge intersected", back),
            merge_subtracted: merge("neoscad csg merge subtracted", front),
            ids_layout,
        }
    }

    /// OpenCSG's `renderSCS` for one product, into `targets`.
    fn render(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        targets: &CsgTargets,
        frame: &wgpu::BindGroup,
        scene: &SceneBuffers,
        product: &ImageCsgDraws,
    ) {
        let Some(faces) = &scene.faces else { return };
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("neoscad csg"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &targets.ids,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                    store: wgpu::StoreOp::Store,
                },
            })],
            // "glClearDepth(0.0): near clipping plane! essential for
            // algorithm!"
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: &targets.depth,
                depth_ops: Some(wgpu::Operations {
                    load: wgpu::LoadOp::Clear(0.0),
                    store: wgpu::StoreOp::Discard,
                }),
                stencil_ops: Some(wgpu::Operations {
                    load: wgpu::LoadOp::Clear(0),
                    store: wgpu::StoreOp::Discard,
                }),
            }),
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_bind_group(0, frame, &[]);
        pass.set_vertex_buffer(0, faces.slice(..));
        let intersected: Vec<_> = product
            .primitives
            .iter()
            .filter(|p| p.op == CsgOp::Intersection)
            .collect();
        let subtracted: Vec<_> = product
            .primitives
            .iter()
            .filter(|p| p.op == CsgOp::Subtraction)
            .collect();
        let draw = |pass: &mut wgpu::RenderPass<'_>, p: &crate::scene::ImageCsgPrimitive| {
            if p.count > 0 {
                pass.draw(p.first..p.first + p.count, 0..1);
            }
        };
        // renderIntersectedFront.
        pass.set_pipeline(&self.front_id);
        for p in &intersected {
            draw(&mut pass, p);
        }
        if intersected.len() != 1 {
            pass.set_pipeline(&self.count_back);
            for p in &intersected {
                draw(&mut pass, p);
            }
            pass.set_pipeline(&self.reset);
            pass.set_stencil_reference(intersected.len() as u32);
            pass.draw(0..6, 0..1);
        }
        // subtractPrimitives, the Schoenfield sequence of batches.
        let n = subtracted.len();
        let mut stencil_ref = 0;
        for i in 0..schoenfield_len(n) {
            let p = subtracted[schoenfield_index(n, i)];
            stencil_ref += 1;
            if stencil_ref == STENCIL_MAX {
                pass.set_pipeline(&self.clear_stencil);
                pass.set_stencil_reference(0);
                pass.draw(0..6, 0..1);
                stencil_ref = 1;
            }
            pass.set_stencil_reference(stencil_ref);
            pass.set_pipeline(&self.mark_front);
            draw(&mut pass, p);
            pass.set_pipeline(&self.subtract_back);
            draw(&mut pass, p);
        }
        // renderIntersectedBack.
        pass.set_pipeline(&self.mask_back);
        for p in &intersected {
            draw(&mut pass, p);
        }
    }

    /// OpenCSG's merge of one product into the frame's depth.
    fn merge(
        &self,
        pass: &mut wgpu::RenderPass<'_>,
        targets: &CsgTargets,
        product: &ImageCsgDraws,
    ) {
        pass.set_bind_group(1, &targets.bind_group, &[]);
        for p in product.primitives.iter().filter(|p| p.count > 0) {
            pass.set_pipeline(match p.op {
                CsgOp::Intersection => &self.merge_intersected,
                CsgOp::Subtraction => &self.merge_subtracted,
            });
            pass.draw(p.first..p.first + p.count, 0..1);
        }
    }
}

/// `SchoenfieldSequencer::size`: how many subtractions `n` batches take.
fn schoenfield_len(n: usize) -> usize {
    match n {
        0 => 0,
        1 => 1,
        2 => 3,
        n => n * n - 2 * n + 4,
    }
}

/// `SchoenfieldSequencer::index`: the batch subtracted at `position`.
fn schoenfield_index(n: usize, position: usize) -> usize {
    if n == 1 {
        0
    } else if n == 2 {
        position & 1
    } else if position < n {
        position
    } else if (position - 1).is_multiple_of(n - 1) {
        0
    } else {
        (position * (n - 2) / (n - 1)) % (n - 1) + 1
    }
}

/// The ID buffer (with its bind group for the merge) and depth-stencil
/// buffer image-space CSG draws into.
#[derive(Debug)]
struct CsgTargets {
    size: (u32, u32),
    ids: wgpu::TextureView,
    depth: wgpu::TextureView,
    bind_group: wgpu::BindGroup,
}

impl CsgTargets {
    fn new(
        device: &wgpu::Device,
        layout: &wgpu::BindGroupLayout,
        width: u32,
        height: u32,
    ) -> CsgTargets {
        let texture = |label, format, usage| {
            device
                .create_texture(&wgpu::TextureDescriptor {
                    label: Some(label),
                    size: wgpu::Extent3d {
                        width,
                        height,
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format,
                    usage,
                    view_formats: &[],
                })
                .create_view(&Default::default())
        };
        let ids = texture(
            "neoscad csg ids",
            CSG_ID_FORMAT,
            wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
        );
        let depth = texture(
            "neoscad csg depth",
            CSG_DEPTH_FORMAT,
            wgpu::TextureUsages::RENDER_ATTACHMENT,
        );
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("neoscad csg ids"),
            layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&ids),
            }],
        });
        CsgTargets {
            size: (width, height),
            ids,
            depth,
            bind_group,
        }
    }
}
