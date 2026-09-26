//! An interactive view: a model on the GPU, a camera the user moves, the
//! view options, and a target to draw into. The macOS app draws into a
//! `CAMetalLayer` surface (made from the layer in `crates/ffi`, the only
//! place that touches the raw layer), the web app will draw into a canvas
//! surface, and tests draw into a texture and read it back. Everything
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
use crate::gpu::{DEPTH_FORMAT, FrameParams, Renderer, SceneBuffers};
use crate::offscreen::{Error, Readback};
use crate::overlay::{self, ViewOptions};
use crate::scene::Scene;
use crate::scheme::ColorScheme;
use crate::{Image, snapshot};

/// Samples per pixel in an interactive view: 4x MSAA, which every Metal
/// and WebGPU device supports for 8-bit colour and 24-bit depth.
pub const MSAA_SAMPLES: u32 = 4;

/// A GPU device shared by every viewport (and by the uploads for them),
/// with the instance its surfaces must be made from and the pipelines for
/// each target format.
#[derive(Debug)]
pub struct Gpu {
    instance: wgpu::Instance,
    adapter: wgpu::Adapter,
    device: wgpu::Device,
    queue: wgpu::Queue,
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
        Ok(Model {
            buffers,
            bbox: scene.bounding_box(),
        })
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
    /// What an empty view draws (no faces, no outlines).
    empty: SceneBuffers,
    /// The generation of the model shown: an older one arriving late is
    /// ignored.
    generation: u64,
    /// Whether View All has run for a model yet: the first model is fitted,
    /// later ones keep the camera the user chose.
    fitted: bool,
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
            empty,
            generation: 0,
            fitted: false,
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
                surface.configure(device, config);
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
        a.msaa = (a.samples > 1).then(|| {
            texture(
                device,
                "neoscad viewport multisample",
                (w, h),
                a.format,
                a.samples,
                wgpu::TextureUsages::RENDER_ATTACHMENT,
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
                wgpu::TextureUsages::RENDER_ATTACHMENT,
            )
            .create_view(&Default::default()),
        );
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
            self.camera.view_all_centered(model.bbox);
            self.fitted = true;
        }
        self.model = Some(model);
        self.dirty = true;
        true
    }

    /// Show nothing (keeping the camera).
    pub fn clear_model(&mut self) {
        self.model = None;
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
    /// none).
    pub fn view_all(&mut self) {
        let bbox = self.model.as_ref().and_then(|m| m.bbox);
        self.with_camera(|c| c.view_all_centered(bbox));
    }

    /// A standard view: OpenSCAD's rotation for it, keeping the centre and
    /// distance, as its View menu does.
    pub fn set_view(&mut self, view: snapshot::View) {
        self.with_camera(|c| c.object_rot = view.object_rot());
    }

    /// View > Reset View (`QGLView::resetView`).
    pub fn reset_view(&mut self) {
        self.with_camera(Camera::reset_view);
    }

    pub fn set_projection(&mut self, projection: Projection) {
        self.with_camera(|c| c.projection = projection);
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
                        surface.configure(&self.gpu.device, config);
                        t
                    }
                    wgpu::CurrentSurfaceTexture::Timeout
                    | wgpu::CurrentSurfaceTexture::Occluded => return Ok(Drawn::Deferred),
                    wgpu::CurrentSurfaceTexture::Outdated => {
                        surface.configure(&self.gpu.device, config);
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
                self.gpu.queue.submit([encoder.finish()]);
                self.gpu.queue.present(texture);
            }
            Target::Texture(t) => {
                let Some(t) = t else {
                    return Ok(Drawn::Idle);
                };
                let view = t.create_view(&Default::default());
                let encoder = self.encode(a, &view);
                self.gpu.queue.submit([encoder.finish()]);
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
        match &a.target {
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
                self.gpu.queue.submit([encoder.finish()]);
                self.gpu.queue.present(texture);
            }
            Target::Texture(t) => {
                let Some(t) = t else {
                    return Err(Error::Readback("the viewport has no size yet".into()));
                };
                let view = t.create_view(&Default::default());
                let mut encoder = self.encode(a, &view);
                readback.copy(&mut encoder, t);
                self.gpu.queue.submit([encoder.finish()]);
            }
        }
        self.dirty = false;
        let mut rgba = readback.read(&self.gpu.device).await?;
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
