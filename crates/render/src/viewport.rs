//! An interactive view: a model on the GPU, a camera the user moves, the
//! view options, and a target to draw into. The macOS app draws into a
//! `CAMetalLayer` surface (made from the layer in `crates/ffi`, the only
//! place that touches the raw layer), the web app draws into a canvas
//! surface (`crates/web-view`, with [`Gpu::with_surface`] and models from
//! [`Gpu::upload_packed`]), and tests draw into a texture and read it back. Everything
//! here is target-agnostic: a [`Viewport`] takes a finished
//! `wgpu::Surface` or makes its own texture.
//!
//! # Where the work happens
//!
//! Building a scene and uploading it ([`Gpu::upload`]) is the expensive
//! part, so it is a free-standing call on a shared [`Gpu`] that any thread
//! can make; the result, a [`Model`], is handed to the viewport with
//! [`Viewport::set_model`], which only swaps a pointer. Drawing
//! ([`Viewport::draw`]) re-encodes one frame from buffers already on the
//! GPU, plus the view-option lines for the current camera, and is cheap
//! enough for every display refresh. It only draws when something changed
//! (the camera, the model, the size or the options): [`Viewport::needs_draw`]
//! says whether it would.
//!
//! # Antialiasing
//!
//! A surface or texture attached here is drawn with [`MSAA_SAMPLES`]
//! samples per pixel when the format allows, and resolved into the target.
//! The exporter ([`crate::offscreen`]) stays single-sample, as OpenSCAD's
//! images are (see [`crate::gpu`]).

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use crate::Lighting;
use crate::camera::{BoundingBox, Camera, Projection};
use crate::gpu::{DEPTH_FORMAT, FrameParams, PackedUploadError, Renderer, SceneBuffers};
use crate::offscreen::{Error, Gate, Readback};
use crate::overlay::{self, ViewOptions};
use crate::packed::PackedScene;
use crate::scene::Scene;
use crate::scheme::ColorScheme;
use crate::{Image, snapshot};

/// Samples per pixel in an interactive view: 4x MSAA, which every Metal
/// and WebGPU device supports for 8-bit colour and 24-bit depth.
pub const MSAA_SAMPLES: u32 = 4;

/// The usage of the multisampled colour and the depth buffers: attachments
/// that live only inside the frame's one render pass (cleared on load,
/// discarded on store, the colour resolved into the target). On Apple GPUs
/// `TRANSIENT_ATTACHMENT` makes them memoryless, kept in tile memory only
/// (`wgpu-hal-30.0.1/src/metal/device.rs:561`): together a window's two 4x
/// buffers took 61 MB of GPU memory at 1280x1520 pixels (phase 8f,
/// `footprint`), about half of an idle app's footprint with one window
/// open. Elsewhere wgpu gives an ordinary texture. The web build keeps
/// plain render attachments: whether browsers' WebGPU accepts the flag
/// was not checked.
#[cfg(not(target_arch = "wasm32"))]
const TRANSIENT: wgpu::TextureUsages =
    wgpu::TextureUsages::RENDER_ATTACHMENT.union(wgpu::TextureUsages::TRANSIENT_ATTACHMENT);
#[cfg(target_arch = "wasm32")]
const TRANSIENT: wgpu::TextureUsages = wgpu::TextureUsages::RENDER_ATTACHMENT;

/// A GPU device shared by every viewport (and by the uploads for them),
/// with the instance its surfaces must be made from and the pipelines for
/// each target format.
#[derive(Debug)]
pub struct Gpu {
    instance: wgpu::Instance,
    adapter: wgpu::Adapter,
    device: wgpu::Device,
    queue: wgpu::Queue,
    /// Every submission and surface configure on this device goes through
    /// it (see [`Gate`]); offscreen renderers on the device share it.
    gate: Gate,
    renderers: Mutex<HashMap<(wgpu::TextureFormat, u32), Arc<Renderer>>>,
}

impl Gpu {
    /// Open the default GPU on `backends`.
    pub async fn new(backends: wgpu::Backends) -> Result<Gpu, Error> {
        let (instance, adapter, device, queue) =
            crate::offscreen::open_device(backends, "neoscad viewport").await?;
        Ok(Gpu {
            instance,
            adapter,
            device,
            queue,
            gate: Gate::default(),
            renderers: Mutex::new(HashMap::new()),
        })
    }

    /// Open a GPU that can present to `surface`, made from `instance`
    /// (which chose the backends). A browser needs this rather than
    /// [`Gpu::new`]: WebGL has no adapter apart from the canvas's own GL
    /// context, so the adapter must be asked for with the surface, and on
    /// WebGPU asking with it costs nothing. Pass the surface on to
    /// [`Viewport::attach_surface`].
    pub async fn with_surface(
        instance: wgpu::Instance,
        surface: &wgpu::Surface<'static>,
    ) -> Result<Gpu, Error> {
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                compatible_surface: Some(surface),
                ..Default::default()
            })
            .await
            .map_err(|e| Error::NoAdapter(e.to_string()))?;
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("neoscad viewport"),
                required_limits: adapter.limits(),
                ..Default::default()
            })
            .await
            .map_err(|e| Error::Device(e.to_string()))?;
        Ok(Gpu {
            instance,
            adapter,
            device,
            queue,
            gate: Gate::default(),
            renderers: Mutex::new(HashMap::new()),
        })
    }

    /// [`Gpu::new`], blocking until the device is ready.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn new_blocking(backends: wgpu::Backends) -> Result<Gpu, Error> {
        pollster::block_on(Gpu::new(backends))
    }

    /// The instance a surface for [`Viewport::attach_surface`] must be
    /// created from.
    pub fn instance(&self) -> &wgpu::Instance {
        &self.instance
    }

    pub fn device(&self) -> &wgpu::Device {
        &self.device
    }

    pub fn queue(&self) -> &wgpu::Queue {
        &self.queue
    }

    pub fn adapter_info(&self) -> wgpu::AdapterInfo {
        self.adapter.get_info()
    }

    pub(crate) fn gate(&self) -> &Gate {
        &self.gate
    }

    /// The pipelines for `format` and `samples`, made on first use and
    /// shared by every viewport with that target (a second window opens
    /// without compiling them again).
    fn renderer(&self, format: wgpu::TextureFormat, samples: u32) -> Arc<Renderer> {
        let mut map = self
            .renderers
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        map.entry((format, samples))
            .or_insert_with(|| Arc::new(Renderer::new(&self.device, format, samples)))
            .clone()
    }

    /// The sample count to draw `format` with: [`MSAA_SAMPLES`] when both
    /// it and the depth format allow that many, else one.
    fn samples_for(&self, format: wgpu::TextureFormat) -> u32 {
        let ok = |f: wgpu::TextureFormat| {
            self.adapter
                .get_texture_format_features(f)
                .flags
                .sample_count_supported(MSAA_SAMPLES)
        };
        if ok(format) && ok(DEPTH_FORMAT) {
            MSAA_SAMPLES
        } else {
            1
        }
    }

    /// Put `scene` on the GPU. This is the slow part of showing a new
    /// model (a vertex per triangle corner, written into mapped memory),
    /// so it runs on whatever thread built the scene, never inside a
    /// frame.
    pub fn upload(&self, scene: &Scene) -> Result<Model, Error> {
        let buffers =
            SceneBuffers::upload(&self.device, scene).map_err(|t| Error::SceneTooLarge {
                bytes: t.bytes,
                max: t.max,
            })?;
        self.release_staging();
        Ok(Model {
            buffers,
            bbox: scene.bounding_box(),
        })
    }

    /// [`Gpu::upload`] for a scene packed elsewhere (a web worker built
    /// it; see [`crate::packed`]): the same buffers, from its bytes. A
    /// packed scene that does not hold together is refused
    /// ([`Error::InvalidScene`]) before anything is uploaded.
    pub fn upload_packed(&self, packed: &PackedScene) -> Result<Model, Error> {
        let buffers = SceneBuffers::upload_packed(&self.device, packed).map_err(|e| match e {
            PackedUploadError::Invalid(e) => Error::InvalidScene(e.to_string()),
            PackedUploadError::TooLarge(t) => Error::SceneTooLarge {
                bytes: t.bytes,
                max: t.max,
            },
        })?;
        self.release_staging();
        Ok(Model {
            buffers,
            bbox: packed.meta.bbox,
        })
    }

    /// Free the staging copies an upload left (see [`Gpu::upload`]).
    fn release_staging(&self) {
        // The buffers were written through staging copies of the same
        // size, which wgpu frees only once the copy into them has run and
        // the device is polled again. An idle view submits nothing more,
        // so they stayed: a model's whole vertex data twice over (57 MB for
        // 125 spheres, phase 8f). Submitting the copies now and waiting
        // for them (a few milliseconds, on the uploading thread, never the
        // main one) frees the staging at once. The wait is on this
        // submission only and bounded: freeing the staging early is an
        // economy, and a device that does not finish must not hang the
        // upload (the model is usable either way). A browser's main thread
        // cannot block on the GPU, so the web build does not wait.
        #[cfg(not(target_arch = "wasm32"))]
        {
            let submitted = self.gate.submit(&self.queue, std::iter::empty());
            let _ = self.device.poll(wgpu::PollType::Wait {
                submission_index: Some(submitted),
                timeout: Some(crate::offscreen::READBACK_WAIT),
            });
        }
    }
}

/// A scene on the GPU, ready for [`Viewport::set_model`].
#[derive(Debug)]
pub struct Model {
    buffers: SceneBuffers,
    bbox: BoundingBox,
}

impl Model {
    /// What View All fits.
    pub fn bounding_box(&self) -> BoundingBox {
        self.bbox
    }
}

/// What the app's View menu toggles besides the camera.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ViewSettings {
    /// The axes, and the small axes in the corner.
    pub axes: bool,
    /// Scale markers on the axes (shown only with the axes, as in
    /// OpenSCAD).
    pub scales: bool,
    /// The ground grid ([`overlay::grid`]; NeoSCAD's, not OpenSCAD's).
    pub grid: bool,
    pub edges: bool,
    pub crosshairs: bool,
    pub lighting: Lighting,
}

impl Default for ViewSettings {
    /// OpenSCAD's GUI defaults (`MainWindow::loadViewSettings`: axes and
    /// scale markers on, edges and crosshairs off) plus the grid, lit the
    /// OpenSCAD way.
    fn default() -> Self {
        ViewSettings {
            axes: true,
            scales: true,
            grid: true,
            edges: false,
            crosshairs: false,
            lighting: Lighting::OpenScad,
        }
    }
}

/// Marks the app draws over the model: the check panel's findings and the
/// measure panel's section outline and picked points. They are view state,
/// not part of the model, so a new model from the next run keeps them
/// until the app replaces them, and exports never include them.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Annotations {
    pub lines: Vec<AnnotationLine>,
    pub markers: Vec<AnnotationMarker>,
}

/// A polyline in model coordinates, drawn over everything: a section
/// outline is inside the model by definition, so hiding it behind the
/// faces would hide exactly what it shows.
#[derive(Debug, Clone, PartialEq)]
pub struct AnnotationLine {
    pub points: Vec<[f64; 3]>,
    /// Join the last point back to the first.
    pub closed: bool,
    pub color: [f32; 4],
}

/// A ring of fixed size on the screen around a model point, with a label
/// beside it (a finding's number, as `snapshot --issues` numbers them).
#[derive(Debug, Clone, PartialEq)]
pub struct AnnotationMarker {
    pub point: [f64; 3],
    pub label: String,
    pub color: [f32; 4],
}

/// The marker ring's radius in points.
const MARKER_RADIUS: f64 = 9.0;

/// The annotations' line segments for one frame of `camera` at `scale`
/// pixels per point, into `out` (drawn over everything).
fn annotation_lines(
    out: &mut Vec<overlay::LineVertex>,
    a: &Annotations,
    camera: &Camera,
    scale: f64,
) {
    for l in &a.lines {
        let mut pen = overlay::Pen {
            out,
            space: overlay::Space::Model,
            color: l.color,
            stipple: false,
        };
        for w in l.points.windows(2) {
            pen.line(w[0], w[1]);
        }
        if l.closed
            && l.points.len() > 2
            && let (Some(first), Some(last)) = (l.points.first(), l.points.last())
        {
            pen.line(*last, *first);
        }
    }
    let (w, h) = (
        f64::from(camera.pixel_width),
        f64::from(camera.pixel_height),
    );
    if w <= 0.0 || h <= 0.0 {
        return;
    }
    for m in &a.markers {
        let Some(p) = camera.project(m.point) else {
            continue;
        };
        // Pixels from the lower left, as `overlay::pixel_text` takes them.
        let (px, py) = ((p[0] + 1.0) / 2.0 * w, (p[1] + 1.0) / 2.0 * h);
        let clip = |x: f64, y: f64| [2.0 * x / w - 1.0, 2.0 * y / h - 1.0, 0.0];
        let mut pen = overlay::Pen {
            out,
            space: overlay::Space::Clip,
            color: m.color,
            stipple: false,
        };
        // Two rings a pixel apart, so the mark stays visible at one-pixel
        // line width on a 2x display; and a cross at the point itself.
        const SIDES: usize = 24;
        for r in [MARKER_RADIUS * scale, MARKER_RADIUS * scale - 1.0] {
            for i in 0..SIDES {
                let a0 = i as f64 / SIDES as f64 * std::f64::consts::TAU;
                let a1 = (i + 1) as f64 / SIDES as f64 * std::f64::consts::TAU;
                pen.line(
                    clip(px + r * a0.cos(), py + r * a0.sin()),
                    clip(px + r * a1.cos(), py + r * a1.sin()),
                );
            }
        }
        let c = 3.0 * scale;
        pen.line(clip(px - c, py), clip(px + c, py));
        pen.line(clip(px, py - c), clip(px, py + c));
        if !m.label.is_empty() {
            overlay::pixel_text(
                out,
                &m.label,
                px + (MARKER_RADIUS + 3.0) * scale,
                py - 5.0 * scale,
                crate::hershey::Align::Left,
                12.0 * scale,
                m.color,
                (camera.pixel_width, camera.pixel_height),
            );
        }
    }
}

/// What [`Viewport::draw`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Drawn {
    /// A frame was drawn and presented.
    Frame,
    /// Nothing had changed, or there is nowhere to draw: no frame.
    Idle,
    /// The target could not take a frame now (the window is hidden, or no
    /// drawable came back in time); the change is kept for the next try.
    Deferred,
}

/// Where a viewport draws.
#[derive(Debug)]
enum Target {
    /// A window surface, configured to the viewport's size.
    Surface {
        surface: wgpu::Surface<'static>,
        config: wgpu::SurfaceConfiguration,
    },
    /// A texture of the viewport's size (tests and headless use); `None`
    /// until the viewport has a size.
    Texture(Option<wgpu::Texture>),
}

/// A target with the pipelines and the buffers drawn into before the
/// resolve.
#[derive(Debug)]
struct Attached {
    target: Target,
    format: wgpu::TextureFormat,
    samples: u32,
    renderer: Arc<Renderer>,
    /// The multisampled colour buffer, resolved into the target (`None`
    /// when drawing single-sample straight into it).
    msaa: Option<wgpu::TextureView>,
    /// `None` until the viewport has a size.
    depth: Option<wgpu::TextureView>,
    /// Whether `msaa` and `depth` keep their contents between render
    /// passes (see [`Viewport::make_buffers`]).
    stored: bool,
}

/// One interactive view (see the module documentation).
#[derive(Debug)]
pub struct Viewport {
    gpu: Arc<Gpu>,
    attached: Option<Attached>,
    /// The drawable size in pixels, and pixels per point.
    width: u32,
    height: u32,
    scale: f64,
    camera: Camera,
    settings: ViewSettings,
    scheme: ColorScheme,
    model: Option<Arc<Model>>,
    annotations: Annotations,
    /// What an empty view draws (no faces, no outlines).
    empty: SceneBuffers,
    /// The generation of the model shown: an older one arriving late is
    /// ignored.
    generation: u64,
    /// Whether View All has run for a model yet: the first model is fitted,
    /// later ones keep the camera the user chose.
    fitted: bool,
    /// The box View All last fitted, while the camera is still that fit
    /// (nothing has moved it since). A resize fits it again, so the fit
    /// follows the view's shape: a browser pane laid out after the first
    /// model arrived, or a window made narrower, would otherwise keep a
    /// fit made for the old aspect and cut a wide model off at the sides.
    /// Any other camera change ends it.
    auto_fit: Option<([f64; 3], [f64; 3])>,
    dirty: bool,
}

impl Viewport {
    /// A viewport with no target yet, OpenSCAD's default camera, the
    /// default [`ViewSettings`] and `scheme`'s colours.
    pub fn new(gpu: Arc<Gpu>, scheme: ColorScheme) -> Result<Viewport, Error> {
        let empty =
            SceneBuffers::upload(&gpu.device, &Scene::empty(&scheme, None)).map_err(|t| {
                Error::SceneTooLarge {
                    bytes: t.bytes,
                    max: t.max,
                }
            })?;
        Ok(Viewport {
            gpu,
            attached: None,
            width: 0,
            height: 0,
            scale: 1.0,
            camera: Camera::default(),
            settings: ViewSettings::default(),
            scheme,
            model: None,
            annotations: Annotations::default(),
            empty,
            generation: 0,
            fitted: false,
            auto_fit: None,
            dirty: true,
        })
    }

    pub fn gpu(&self) -> &Arc<Gpu> {
        &self.gpu
    }

    /// Draw into `surface` (made from [`Gpu::instance`]) from now on, at
    /// `width` by `height` pixels and `scale` pixels per point. With
    /// `readable`, frames can also be read back ([`Viewport::read_pixels`]);
    /// that keeps the drawable out of framebuffer-only memory, so only
    /// tests ask for it. A previous target is dropped first.
    pub fn attach_surface(
        &mut self,
        surface: wgpu::Surface<'static>,
        width: u32,
        height: u32,
        scale: f64,
        readable: bool,
    ) -> Result<(), Error> {
        self.attached = None;
        let caps = surface.get_capabilities(&self.gpu.adapter);
        // Linear 8-bit colour: OpenSCAD writes its colours to the
        // framebuffer unconverted, and an sRGB format would brighten every
        // shade (see `Renderer::new`).
        let format = caps
            .formats
            .iter()
            .copied()
            .find(|f| *f == wgpu::TextureFormat::Bgra8Unorm)
            .or_else(|| caps.formats.iter().copied().find(|f| !f.is_srgb()))
            .ok_or_else(|| Error::Device("the surface offers no linear colour format".into()))?;
        let mut usage = wgpu::TextureUsages::RENDER_ATTACHMENT;
        if readable {
            if !caps.usages.contains(wgpu::TextureUsages::COPY_SRC) {
                return Err(Error::Readback(
                    "this surface's frames cannot be copied".into(),
                ));
            }
            usage |= wgpu::TextureUsages::COPY_SRC;
        }
        let config = wgpu::SurfaceConfiguration {
            usage,
            format,
            color_space: wgpu::SurfaceColorSpace::Auto,
            width: width.max(1),
            height: height.max(1),
            // The caller paces frames (the app draws from its display
            // link, once a refresh at most), so the surface need not:
            // `Immediate` (Metal's `displaySyncEnabled = false`) presents
            // without waiting for a vsync of its own, and the compositor
            // still shows a windowed layer at the display's refresh, so a
            // window does not tear. Measured in the app (phase 8c, a 60 Hz
            // display): with `Fifo` the drawable request blocked the main
            // thread for up to a whole refresh (p50 3.0 to 16.4 ms a
            // frame, depending on the link's phase); with `Immediate`,
            // p50 2.6 ms and p95 3.4 ms, at the same 60 frames a second.
            // `Fifo` is the fallback for a surface without it.
            present_mode: if caps.present_modes.contains(&wgpu::PresentMode::Immediate) {
                wgpu::PresentMode::Immediate
            } else {
                wgpu::PresentMode::Fifo
            },
            desired_maximum_frame_latency: 2,
            alpha_mode: wgpu::CompositeAlphaMode::Opaque,
            view_formats: vec![],
        };
        let samples = self.gpu.samples_for(format);
        self.attached = Some(Attached {
            target: Target::Surface { surface, config },
            format,
            samples,
            renderer: self.gpu.renderer(format, samples),
            msaa: None,
            depth: None,
            stored: false,
        });
        self.resize(width, height, scale);
        Ok(())
    }

    /// Draw into a texture of the viewport's size instead of a window
    /// (tests, and hosts without a window). Frames are read with
    /// [`Viewport::read_pixels`].
    pub fn attach_texture(&mut self, width: u32, height: u32, scale: f64) {
        self.attached = None;
        let format = wgpu::TextureFormat::Rgba8Unorm;
        let samples = self.gpu.samples_for(format);
        self.attached = Some(Attached {
            target: Target::Texture(None),
            format,
            samples,
            renderer: self.gpu.renderer(format, samples),
            msaa: None,
            depth: None,
            stored: false,
        });
        self.resize(width, height, scale);
    }

    /// Stop drawing into the target, releasing it (for a surface, the
    /// layer it holds). Whether there was one.
    pub fn detach(&mut self) -> bool {
        self.attached.take().is_some()
    }

    pub fn is_attached(&self) -> bool {
        self.attached.is_some()
    }

    /// The sample count frames are drawn with (0 when detached).
    pub fn samples(&self) -> u32 {
        self.attached.as_ref().map_or(0, |a| a.samples)
    }

    /// The drawable size in pixels.
    pub fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// Pixels per point.
    pub fn scale(&self) -> f64 {
        self.scale
    }

    /// A new drawable size (`width` by `height` pixels, `scale` pixels per
    /// point): the target and the buffers behind it are remade at that size.
    /// A zero size (a collapsed split view) keeps the old buffers and draws
    /// nothing until it grows again.
    pub fn resize(&mut self, width: u32, height: u32, scale: f64) {
        self.width = width;
        self.height = height;
        if scale.is_finite() && scale > 0.0 {
            self.scale = scale;
        }
        self.dirty = true;
        if width == 0 || height == 0 {
            return;
        }
        if let Some(bbox) = self.auto_fit {
            self.fit(Some(bbox));
        }
        let device = &self.gpu.device;
        let Some(a) = &mut self.attached else {
            return;
        };
        let max = device.limits().max_texture_dimension_2d;
        let (w, h) = (width.min(max), height.min(max));
        match &mut a.target {
            Target::Surface { surface, config } => {
                config.width = w;
                config.height = h;
                self.gpu.gate.configure(surface, device, config);
            }
            Target::Texture(t) => {
                *t = Some(texture(
                    device,
                    "neoscad viewport colour",
                    (w, h),
                    a.format,
                    1,
                    wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
                ));
            }
        }
        self.make_buffers();
    }

    /// Make the multisampled colour buffer and the depth buffer at the
    /// drawable size. They are memoryless ([`TRANSIENT`]) unless the model
    /// has an image-space CSG product: its frame is several render passes
    /// (`gpu::Renderer::draw`), and a memoryless buffer loses its contents
    /// when a pass ends, which wgpu refuses to allow. Only such models pay
    /// for buffers in memory.
    fn make_buffers(&mut self) {
        let stored = self
            .model
            .as_ref()
            .is_some_and(|m| m.buffers.has_image_csg());
        let device = &self.gpu.device;
        let Some(a) = &mut self.attached else {
            return;
        };
        let max = device.limits().max_texture_dimension_2d;
        let (w, h) = (self.width.min(max), self.height.min(max));
        if w == 0 || h == 0 {
            return;
        }
        let usage = if stored {
            wgpu::TextureUsages::RENDER_ATTACHMENT
        } else {
            TRANSIENT
        };
        a.stored = stored;
        a.msaa = (a.samples > 1).then(|| {
            texture(
                device,
                "neoscad viewport multisample",
                (w, h),
                a.format,
                a.samples,
                usage,
            )
            .create_view(&Default::default())
        });
        a.depth = Some(
            texture(
                device,
                "neoscad viewport depth",
                (w, h),
                DEPTH_FORMAT,
                a.samples,
                usage,
            )
            .create_view(&Default::default()),
        );
    }

    /// Remake the buffers if the model now shown needs them kept in
    /// memory and they are not, or the other way round.
    fn match_buffers_to_model(&mut self) {
        let stored = self
            .model
            .as_ref()
            .is_some_and(|m| m.buffers.has_image_csg());
        if self
            .attached
            .as_ref()
            .is_some_and(|a| a.depth.is_some() && a.stored != stored)
        {
            self.make_buffers();
        }
    }

    // --- The model ----------------------------------------------------------

    /// Show `model`, made by request number `generation`, unless a newer
    /// request's model is already shown: renders finish out of order, and
    /// the last one asked for must win. The first model is fitted to the
    /// view (View All); later ones keep the camera. Whether it was shown.
    pub fn set_model(&mut self, model: Arc<Model>, generation: u64) -> bool {
        if generation < self.generation {
            return false;
        }
        self.generation = generation;
        if !self.fitted && model.bbox.is_some() {
            self.fit(model.bbox);
            self.fitted = true;
        }
        self.model = Some(model);
        self.match_buffers_to_model();
        self.dirty = true;
        true
    }

    /// The program's own view: each of `$vpt`, `$vpr`, `$vpd` and `$vpf`
    /// it assigned (`None` for one it did not), as OpenSCAD's GUI moves its
    /// camera after an evaluation (`Camera::updateView`). A view set this
    /// way is not replaced by View All when the first model arrives.
    pub fn set_file_view(
        &mut self,
        vpt: Option<[f64; 3]>,
        vpr: Option<[f64; 3]>,
        vpd: Option<f64>,
        vpf: Option<f64>,
    ) {
        self.with_camera(|c| {
            if let Some([x, y, z]) = vpt {
                c.set_vpt(x, y, z);
            }
            if let Some([x, y, z]) = vpr {
                c.set_vpr(x, y, z);
            }
            if let Some(d) = vpd {
                c.set_vpd(d);
            }
            if let Some(f) = vpf {
                c.set_vpf(f);
            }
        });
        self.fitted = true;
    }

    /// Draw `annotations` over the model from now on (replacing the ones
    /// before).
    pub fn set_annotations(&mut self, annotations: Annotations) {
        if annotations != self.annotations {
            self.annotations = annotations;
            self.dirty = true;
        }
    }

    pub fn annotations(&self) -> &Annotations {
        &self.annotations
    }

    /// Show nothing (keeping the camera).
    pub fn clear_model(&mut self) {
        self.model = None;
        self.match_buffers_to_model();
        self.dirty = true;
    }

    pub fn model(&self) -> Option<&Arc<Model>> {
        self.model.as_ref()
    }

    // --- Settings -----------------------------------------------------------

    pub fn settings(&self) -> ViewSettings {
        self.settings
    }

    pub fn set_settings(&mut self, settings: ViewSettings) {
        if settings != self.settings {
            self.settings = settings;
            self.dirty = true;
        }
    }

    pub fn scheme(&self) -> &ColorScheme {
        &self.scheme
    }

    /// Draw in `scheme`'s background and lines from now on. A model's face
    /// colours were fixed when its scene was built, so the caller rebuilds
    /// the model in the new scheme too.
    pub fn set_scheme(&mut self, scheme: ColorScheme) {
        if scheme != self.scheme {
            self.scheme = scheme;
            self.dirty = true;
        }
    }

    // --- The camera ---------------------------------------------------------

    pub fn camera(&self) -> &Camera {
        &self.camera
    }

    /// Change the camera; the next frame shows it.
    pub fn with_camera(&mut self, f: impl FnOnce(&mut Camera)) {
        f(&mut self.camera);
        self.auto_fit = None;
        self.dirty = true;
    }

    /// Draw the next frame even though nothing changed, leaving the
    /// camera alone (so a View All fit still follows resizes).
    pub fn redraw(&mut self) {
        self.dirty = true;
    }

    /// View All for `bbox` at the view's current shape
    /// ([`Camera::view_all_to_fit`], not OpenSCAD's vertical-only
    /// [`Camera::view_all`]: this is an interactive view, and a portrait
    /// one would cut a wide model off). The camera takes the drawable size
    /// first, as the stored camera's own size is whatever it was made
    /// with; an unsized view fits as a square, and the first resize fits
    /// again.
    fn fit(&mut self, bbox: BoundingBox) {
        self.camera.pixel_width = self.width;
        self.camera.pixel_height = self.height;
        self.camera.view_all_to_fit(bbox);
        self.auto_fit = bbox;
        self.dirty = true;
    }

    /// A left drag of `dx`, `dy` points: [`Camera::orbit`].
    pub fn orbit(&mut self, dx: f64, dy: f64) {
        self.with_camera(|c| c.orbit(dx, dy));
    }

    /// A right drag of `dx`, `dy` points: [`Camera::pan`] across the view's
    /// size in points.
    pub fn pan(&mut self, dx: f64, dy: f64) {
        let (w, h) = (
            f64::from(self.width) / self.scale,
            f64::from(self.height) / self.scale,
        );
        self.with_camera(|c| c.pan(dx, dy, w, h));
    }

    /// View > View All: fit the model shown (or the default distance for
    /// none), in both directions of the view.
    pub fn view_all(&mut self) {
        let bbox = self.model.as_ref().and_then(|m| m.bbox);
        self.fit(bbox);
    }

    /// A standard view: OpenSCAD's rotation for it, keeping the centre and
    /// distance, as its View menu does.
    pub fn set_view(&mut self, view: snapshot::View) {
        self.with_camera(|c| c.object_rot = view.object_rot());
    }

    /// The camera as a frame draws it: the viewport's pixel size filled in
    /// (the stored camera keeps whatever size it was made with).
    fn frame_camera(&self) -> Camera {
        let mut c = self.camera;
        c.pixel_width = self.width;
        c.pixel_height = self.height;
        c
    }

    /// The ray under a point of the view, `x` and `y` in points from the
    /// top left (as AppKit reports a click in a flipped view): origin and
    /// unit direction in model coordinates. `None` before the view has a
    /// size.
    pub fn ray_at(&self, x: f64, y: f64) -> Option<([f64; 3], [f64; 3])> {
        if self.width == 0 || self.height == 0 {
            return None;
        }
        let (w, h) = (f64::from(self.width), f64::from(self.height));
        let nx = 2.0 * x * self.scale / w - 1.0;
        let ny = 1.0 - 2.0 * y * self.scale / h;
        self.frame_camera().ray(nx, ny)
    }

    /// The view as it is now (model, camera, view options and scheme),
    /// drawn into a texture of its own at `width` by `height` pixels and
    /// read back: File > Export's image of the current view. The grid and
    /// the annotations are left out, as they are the app's own marks and
    /// not the model's (OpenSCAD's image export has neither). The window's
    /// own surface is never read: it is not readable, and reading it would
    /// hold the main thread for the GPU.
    ///
    /// This makes the copy; [`Viewport::read_pixels_blocking`] on it draws
    /// and reads it, which the caller can do without holding whatever
    /// guards this viewport.
    pub fn copy_for_image(&self, width: u32, height: u32) -> Result<Viewport, Error> {
        let mut v = Viewport::new(self.gpu.clone(), self.scheme.clone())?;
        v.attach_texture(width, height, self.scale);
        v.camera = self.camera;
        v.settings = ViewSettings {
            grid: false,
            ..self.settings
        };
        v.model = self.model.clone();
        v.fitted = true;
        Ok(v)
    }

    /// Look at `point`, keeping the rotation and the distance: how the
    /// check panel brings a finding to the middle of the view.
    pub fn look_at(&mut self, point: [f64; 3]) {
        self.with_camera(|c| c.set_vpt(point[0], point[1], point[2]));
    }

    /// View > Reset View (`QGLView::resetView`).
    pub fn reset_view(&mut self) {
        self.with_camera(Camera::reset_view);
    }

    pub fn set_projection(&mut self, projection: Projection) {
        let auto_fit = self.auto_fit;
        self.with_camera(|c| c.projection = projection);
        // A portrait fit depends on the projection, so an untouched View
        // All is made again for the new one rather than dropped.
        if auto_fit.is_some() {
            self.fit(auto_fit);
        }
    }

    // --- Frames -------------------------------------------------------------

    /// Whether [`Viewport::draw`] would draw: something changed since the
    /// last frame, and there is a target with a size.
    pub fn needs_draw(&self) -> bool {
        self.dirty
            && self.width > 0
            && self.height > 0
            && self.attached.as_ref().is_some_and(|a| a.depth.is_some())
    }

    /// Draw a frame if anything changed since the last one.
    ///
    /// On a surface the frame is presented without waiting for a vsync
    /// (see [`Viewport::attach_surface`]), so the caller paces frames: draw
    /// from the display's refresh callback, not a loop. The model is
    /// already on the GPU; a frame records one render pass and a few
    /// kilobytes of lines.
    pub fn draw(&mut self) -> Result<Drawn, Error> {
        if !self.needs_draw() {
            return Ok(Drawn::Idle);
        }
        let Some(a) = &self.attached else {
            return Ok(Drawn::Idle);
        };
        match &a.target {
            Target::Surface { surface, config } => {
                let texture = match surface.get_current_texture() {
                    wgpu::CurrentSurfaceTexture::Success(t) => t,
                    wgpu::CurrentSurfaceTexture::Suboptimal(t) => {
                        // Still drawable; reconfigure for the next frame.
                        self.gpu.gate.configure(surface, &self.gpu.device, config);
                        t
                    }
                    wgpu::CurrentSurfaceTexture::Timeout
                    | wgpu::CurrentSurfaceTexture::Occluded => return Ok(Drawn::Deferred),
                    wgpu::CurrentSurfaceTexture::Outdated => {
                        self.gpu.gate.configure(surface, &self.gpu.device, config);
                        return Ok(Drawn::Deferred);
                    }
                    wgpu::CurrentSurfaceTexture::Lost => {
                        return Err(Error::Device("the window surface was lost".into()));
                    }
                    wgpu::CurrentSurfaceTexture::Validation => {
                        return Err(Error::Device("the window surface failed validation".into()));
                    }
                };
                let view = texture.texture.create_view(&Default::default());
                let encoder = self.encode(a, &view);
                self.gpu
                    .gate
                    .submit_and_present(&self.gpu.queue, [encoder.finish()], texture);
            }
            Target::Texture(t) => {
                let Some(t) = t else {
                    return Ok(Drawn::Idle);
                };
                let view = t.create_view(&Default::default());
                let encoder = self.encode(a, &view);
                self.gpu.gate.submit(&self.gpu.queue, [encoder.finish()]);
            }
        }
        self.dirty = false;
        Ok(Drawn::Frame)
    }

    /// Draw a frame now (changed or not) and read it back, RGBA with the
    /// top row first. For a surface this needs `readable` at attach time.
    pub async fn read_pixels(&mut self) -> Result<Image, Error> {
        let Some(a) = &self.attached else {
            return Err(Error::Readback("the viewport has no target".into()));
        };
        let (width, height) = (self.width, self.height);
        let max = self.gpu.device.limits().max_texture_dimension_2d;
        if width == 0 || height == 0 || width > max || height > max || a.depth.is_none() {
            return Err(Error::Size { width, height, max });
        }
        let readback = Readback::new(&self.gpu.device, width, height).ok_or(Error::Size {
            width,
            height,
            max,
        })?;
        let bgra = matches!(
            a.format,
            wgpu::TextureFormat::Bgra8Unorm | wgpu::TextureFormat::Bgra8UnormSrgb
        );
        let submitted = match &a.target {
            Target::Surface { surface, config } => {
                if !config.usage.contains(wgpu::TextureUsages::COPY_SRC) {
                    return Err(Error::Readback(
                        "the surface was attached without `readable`".into(),
                    ));
                }
                let texture = match surface.get_current_texture() {
                    wgpu::CurrentSurfaceTexture::Success(t)
                    | wgpu::CurrentSurfaceTexture::Suboptimal(t) => t,
                    other => {
                        return Err(Error::Readback(format!(
                            "no drawable to read ({})",
                            surface_status(&other)
                        )));
                    }
                };
                let view = texture.texture.create_view(&Default::default());
                let mut encoder = self.encode(a, &view);
                readback.copy(&mut encoder, &texture.texture);
                self.gpu
                    .gate
                    .submit_and_present(&self.gpu.queue, [encoder.finish()], texture)
            }
            Target::Texture(t) => {
                let Some(t) = t else {
                    return Err(Error::Readback("the viewport has no size yet".into()));
                };
                let view = t.create_view(&Default::default());
                let mut encoder = self.encode(a, &view);
                readback.copy(&mut encoder, t);
                self.gpu.gate.submit(&self.gpu.queue, [encoder.finish()])
            }
        };
        self.dirty = false;
        let mut rgba = readback.read(&self.gpu.device, submitted).await?;
        if bgra {
            for p in rgba.as_chunks_mut::<4>().0 {
                p.swap(0, 2);
            }
        }
        Ok(Image {
            width,
            height,
            rgba,
        })
    }

    /// [`Viewport::read_pixels`], blocking until the image is back.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn read_pixels_blocking(&mut self) -> Result<Image, Error> {
        pollster::block_on(self.read_pixels())
    }

    /// Record one frame into `target` (resolving into it with MSAA).
    fn encode(&self, a: &Attached, target: &wgpu::TextureView) -> wgpu::CommandEncoder {
        let device = &self.gpu.device;
        let mut camera = self.camera;
        camera.pixel_width = self.width;
        camera.pixel_height = self.height;
        let s = self.settings;
        let frame = FrameParams::new(&camera, &self.scheme, s.edges).with_lighting(s.lighting);
        let view = ViewOptions {
            axes: s.axes,
            scales: s.scales,
            edges: s.edges,
            crosshairs: s.crosshairs,
        };
        // `preview: false`: the GUI shows crosshairs in both modes; only
        // OpenSCAD's preview *export* leaves them out.
        let mut lines = overlay::overlay(&camera, &self.scheme, &view, false);
        if s.grid {
            overlay::grid(&mut lines.behind, &camera, self.scheme.axes.0);
        }
        annotation_lines(&mut lines.after, &self.annotations, &camera, self.scale);
        let buffers = self.model.as_ref().map_or(&self.empty, |m| &m.buffers);
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("neoscad viewport"),
        });
        let (color, resolve) = match &a.msaa {
            Some(msaa) => (msaa, Some(target)),
            None => (target, None),
        };
        let Some(depth) = &a.depth else {
            // Callers check for a size first; without one there is nothing
            // to draw into.
            return encoder;
        };
        a.renderer.draw(
            device,
            &mut encoder,
            color,
            resolve,
            depth,
            buffers,
            &frame,
            &lines,
        );
        encoder
    }
}

/// A 2D texture of one mip level.
fn texture(
    device: &wgpu::Device,
    label: &str,
    (width, height): (u32, u32),
    format: wgpu::TextureFormat,
    samples: u32,
    usage: wgpu::TextureUsages,
) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: samples,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage,
        view_formats: &[],
    })
}

fn surface_status(s: &wgpu::CurrentSurfaceTexture) -> &'static str {
    match s {
        wgpu::CurrentSurfaceTexture::Success(_) => "success",
        wgpu::CurrentSurfaceTexture::Suboptimal(_) => "suboptimal",
        wgpu::CurrentSurfaceTexture::Timeout => "timeout",
        wgpu::CurrentSurfaceTexture::Occluded => "occluded",
        wgpu::CurrentSurfaceTexture::Outdated => "outdated",
        wgpu::CurrentSurfaceTexture::Lost => "lost",
        wgpu::CurrentSurfaceTexture::Validation => "validation error",
    }
}
