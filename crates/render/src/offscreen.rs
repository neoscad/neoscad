//! Offscreen rendering: a [`Scene`] drawn into a texture and read back as
//! RGBA, which is how `-o x.png` and agent snapshots get their pixels
//! (OpenSCAD's `OffscreenView`, `src/glview/OffscreenView.cc`).
//!
//! The async API works everywhere wgpu does; [`Offscreen::new_blocking`]
//! and [`Offscreen::render_blocking`] wrap it for native callers. In a
//! browser a render cannot block, so the web app awaits the futures (or,
//! more likely, draws into its canvas with [`crate::gpu::Renderer`]
//! directly).

use std::fmt;
use std::sync::{Arc, Mutex, PoisonError, RwLock};

pub use wgpu::Backends;

use crate::camera::Camera;
use crate::gpu::{DEPTH_FORMAT, FrameParams, Renderer, SceneBuffers};
use crate::overlay::Overlay;
use crate::scene::Scene;
use crate::scheme::ColorScheme;

/// The colour format of the offscreen target: 8-bit RGBA without sRGB
/// conversion, as OpenSCAD's `GL_RGBA8` renderbuffer.
const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

/// Why an offscreen render failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// No GPU adapter (no Metal, Vulkan, Direct3D 12 or WebGPU device).
    NoAdapter(String),
    /// The adapter would not create a device.
    Device(String),
    /// The image could not be read back.
    Readback(String),
    /// The requested size is zero or larger than the device allows.
    Size { width: u32, height: u32, max: u32 },
    /// The model's vertices would need a bigger buffer than the device
    /// allows.
    SceneTooLarge { bytes: u64, max: u64 },
    /// A packed scene ([`crate::packed`]) that does not hold together.
    InvalidScene(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::NoAdapter(e) => write!(f, "no GPU adapter ({e})"),
            Error::Device(e) => write!(f, "cannot create a GPU device ({e})"),
            Error::Readback(e) => write!(f, "cannot read the rendered image back ({e})"),
            Error::SceneTooLarge { bytes, max } => write!(
                f,
                "the model needs a {bytes}-byte vertex buffer; the GPU allows {max} bytes"
            ),
            Error::InvalidScene(e) => write!(f, "{e}"),
            Error::Size { width, height, max } => {
                write!(
                    f,
                    "cannot render a {width}x{height} image (the GPU allows 1..={max} pixels a side)"
                )
            }
        }
    }
}

impl std::error::Error for Error {}

pub use crate::Image;

/// A GPU device with the render pipelines, ready to draw offscreen.
#[derive(Debug)]
pub struct Offscreen {
    device: wgpu::Device,
    queue: wgpu::Queue,
    /// Shared with the [`crate::viewport::Gpu`] this was made on, if any.
    gate: Gate,
    renderer: Renderer,
    adapter: wgpu::AdapterInfo,
}

/// What `--info` says about the GPU (OpenSCAD prints its GL context's
/// renderer, vendor and version in the same place).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GpuInfo {
    /// The graphics API wgpu drives: Metal, Vulkan, Dx12, Gl or WebGPU.
    pub backend: String,
    pub name: String,
    pub device_type: String,
    pub driver: String,
    pub driver_info: String,
    /// The largest image side the device allows, in pixels.
    pub max_texture_size: u32,
}

impl Offscreen {
    /// Open the default GPU on `backends` (for example
    /// [`wgpu::Backends::PRIMARY`]).
    pub async fn new(backends: wgpu::Backends) -> Result<Offscreen, Error> {
        let (_, adapter, device, queue) = open_device(backends, "neoscad offscreen").await?;
        let renderer = Renderer::new(&device, FORMAT, 1);
        Ok(Offscreen {
            device,
            queue,
            gate: Gate::default(),
            renderer,
            adapter: adapter.get_info(),
        })
    }

    /// An offscreen renderer on `gpu`'s device, not a device of its own. A
    /// host that already draws viewports (the macOS app) snapshots on the
    /// same device: a second Metal device costs its own command queues,
    /// pipeline caches and driver allocations for nothing.
    pub fn on_gpu(gpu: &crate::viewport::Gpu) -> Offscreen {
        let device = gpu.device().clone();
        Offscreen {
            renderer: Renderer::new(&device, FORMAT, 1),
            queue: gpu.queue().clone(),
            gate: gpu.gate().clone(),
            device,
            adapter: gpu.adapter_info(),
        }
    }

    /// The GPU this draws on.
    pub fn info(&self) -> GpuInfo {
        let a = &self.adapter;
        GpuInfo {
            backend: format!("{:?}", a.backend),
            name: a.name.clone(),
            device_type: format!("{:?}", a.device_type),
            driver: a.driver.clone(),
            driver_info: a.driver_info.clone(),
            max_texture_size: self.device.limits().max_texture_dimension_2d,
        }
    }

    /// Draw `scene` as `camera` sees it, at the camera's pixel size, in
    /// `scheme`'s colours. The camera must already be final (`--viewall`
    /// applied; see [`crate::fit_camera`]).
    pub async fn render(
        &self,
        scene: &Scene,
        camera: &Camera,
        scheme: &ColorScheme,
    ) -> Result<Image, Error> {
        self.render_view(scene, camera, scheme, &Overlay::default(), false)
            .await
    }

    /// [`Offscreen::render`] with view-option lines (see
    /// [`crate::overlay`]) and, with `edges`, the faces' edges.
    pub async fn render_view(
        &self,
        scene: &Scene,
        camera: &Camera,
        scheme: &ColorScheme,
        overlay: &Overlay,
        edges: bool,
    ) -> Result<Image, Error> {
        let mut images = self
            .render_views(scene, &[(*camera, overlay.clone())], scheme, edges)
            .await?;
        Ok(images.pop().expect("one view gives one image"))
    }

    /// The scene from several cameras, each with its own lines: the scene
    /// is uploaded once (snapshots draw one model from four sides).
    pub async fn render_views(
        &self,
        scene: &Scene,
        views: &[(Camera, Overlay)],
        scheme: &ColorScheme,
        edges: bool,
    ) -> Result<Vec<Image>, Error> {
        self.render_views_lit(scene, views, scheme, edges, crate::Lighting::OpenScad)
            .await
    }

    /// [`Offscreen::render_views`] with another [`crate::Lighting`].
    pub async fn render_views_lit(
        &self,
        scene: &Scene,
        views: &[(Camera, Overlay)],
        scheme: &ColorScheme,
        edges: bool,
        lighting: crate::Lighting,
    ) -> Result<Vec<Image>, Error> {
        let buffers =
            SceneBuffers::upload(&self.device, scene).map_err(|t| Error::SceneTooLarge {
                bytes: t.bytes,
                max: t.max,
            })?;
        let mut out = Vec::with_capacity(views.len());
        for (camera, overlay) in views {
            out.push(
                self.draw(&buffers, camera, scheme, overlay, edges, lighting)
                    .await?,
            );
        }
        Ok(out)
    }

    async fn draw(
        &self,
        buffers: &SceneBuffers,
        camera: &Camera,
        scheme: &ColorScheme,
        overlay: &Overlay,
        edges: bool,
        lighting: crate::Lighting,
    ) -> Result<Image, Error> {
        let (width, height) = (camera.pixel_width, camera.pixel_height);
        let max = self.device.limits().max_texture_dimension_2d;
        if width == 0 || height == 0 || width > max || height > max {
            return Err(Error::Size { width, height, max });
        }
        let size = wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        };
        let target = |label: &str, format: wgpu::TextureFormat, usage: wgpu::TextureUsages| {
            self.device.create_texture(&wgpu::TextureDescriptor {
                label: Some(label),
                size,
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage,
                view_formats: &[],
            })
        };
        let color = target(
            "neoscad offscreen colour",
            FORMAT,
            wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        );
        let depth = target(
            "neoscad offscreen depth",
            DEPTH_FORMAT,
            wgpu::TextureUsages::RENDER_ATTACHMENT,
        );
        let frame = FrameParams::new(camera, scheme, edges).with_lighting(lighting);

        let readback =
            Readback::new(&self.device, width, height).ok_or(Error::Size { width, height, max })?;

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("neoscad offscreen"),
            });
        self.renderer.draw(
            &self.device,
            &mut encoder,
            &color.create_view(&Default::default()),
            None,
            &depth.create_view(&Default::default()),
            buffers,
            &frame,
            overlay,
        );
        readback.copy(&mut encoder, &color);
        let submitted = self.gate.submit(&self.queue, [encoder.finish()]);
        let rgba = readback.read(&self.device, submitted).await?;
        Ok(Image {
            width,
            height,
            rgba,
        })
    }

    /// [`Offscreen::new`], blocking until the device is ready.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn new_blocking(backends: wgpu::Backends) -> Result<Offscreen, Error> {
        pollster::block_on(Offscreen::new(backends))
    }

    /// [`Offscreen::render`], blocking until the image is read back.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn render_blocking(
        &self,
        scene: &Scene,
        camera: &Camera,
        scheme: &ColorScheme,
    ) -> Result<Image, Error> {
        pollster::block_on(self.render(scene, camera, scheme))
    }

    /// [`Offscreen::render_views`], blocking until the images are read back.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn render_views_blocking(
        &self,
        scene: &Scene,
        views: &[(Camera, Overlay)],
        scheme: &ColorScheme,
        edges: bool,
    ) -> Result<Vec<Image>, Error> {
        pollster::block_on(self.render_views(scene, views, scheme, edges))
    }

    /// [`Offscreen::render_view`], blocking until the image is read back.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn render_view_blocking(
        &self,
        scene: &Scene,
        camera: &Camera,
        scheme: &ColorScheme,
        overlay: &Overlay,
        edges: bool,
    ) -> Result<Image, Error> {
        pollster::block_on(self.render_view(scene, camera, scheme, overlay, edges))
    }
}

/// The default GPU on `backends`, with its instance (which a window
/// surface must be made from) and a device with every limit the adapter
/// offers: a large model needs a large vertex buffer, and a large image a
/// large texture and readback buffer; WebGPU's portable defaults (256 MiB
/// buffers) would refuse models the GPU could draw.
pub(crate) async fn open_device(
    backends: wgpu::Backends,
    label: &str,
) -> Result<(wgpu::Instance, wgpu::Adapter, wgpu::Device, wgpu::Queue), Error> {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends,
        ..wgpu::InstanceDescriptor::new_without_display_handle()
    });
    let adapter = instance
        .request_adapter(&wgpu::RequestAdapterOptions::default())
        .await
        .map_err(|e| Error::NoAdapter(e.to_string()))?;
    let (device, queue) = adapter
        .request_device(&wgpu::DeviceDescriptor {
            label: Some(label),
            required_limits: adapter.limits(),
            ..Default::default()
        })
        .await
        .map_err(|e| Error::Device(e.to_string()))?;
    Ok((instance, adapter, device, queue))
}

/// A colour texture on its way back to the CPU: a mappable buffer the
/// texture is copied into, rows padded to the 256 bytes a copy needs.
/// The offscreen exporter and the viewport's pixel read share it.
#[derive(Debug)]
pub(crate) struct Readback {
    buffer: wgpu::Buffer,
    width: u32,
    height: u32,
    padded_row: u32,
}

impl Readback {
    /// A buffer for a `width` by `height` image of 4-byte pixels; `None`
    /// when it would be larger than the device allows.
    pub(crate) fn new(device: &wgpu::Device, width: u32, height: u32) -> Option<Readback> {
        let row = 4 * width;
        let padded_row =
            row.div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT) * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let size = u64::from(padded_row) * u64::from(height);
        if size > device.limits().max_buffer_size {
            return None;
        }
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("neoscad readback"),
            size,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        Some(Readback {
            buffer,
            width,
            height,
            padded_row,
        })
    }

    /// Record the copy of `texture` (the image's size, 4 bytes a pixel).
    pub(crate) fn copy(&self, encoder: &mut wgpu::CommandEncoder, texture: &wgpu::Texture) {
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &self.buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(self.padded_row),
                    rows_per_image: Some(self.height),
                },
            },
            wgpu::Extent3d {
                width: self.width,
                height: self.height,
                depth_or_array_layers: 1,
            },
        );
    }

    /// The pixels, top row first and unpadded, once `submitted` (the
    /// submission that recorded [`Readback::copy`]) has finished.
    ///
    /// Natively this drives the device itself until this buffer is mapped,
    /// and gives up with [`Error::Readback`] after [`READBACK_WAIT`]
    /// rather than block for ever. It does not trust the map callback
    /// alone: on a device shared between threads, whichever thread polls
    /// first collects every finished mapping, and wgpu-core 30.0.1 drops
    /// the callbacks it collected, unrun, when `Surface::configure`'s wait
    /// fails (`Device::configure_surface`, `device/resource.rs:5343` and
    /// `:5351`: the closures are fired only on success). The buffer is
    /// mapped all the same, so a lost callback is recovered by asking the
    /// buffer; before this, `Core::picture` waited in `pollster` for a
    /// wake that never came (the `neoscad-ffi` tests hung for hours).
    pub(crate) async fn read(
        self,
        device: &wgpu::Device,
        submitted: wgpu::SubmissionIndex,
    ) -> Result<Vec<u8>, Error> {
        let (tx, rx) = futures_channel();
        self.buffer
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |r| {
                tx.send(r.map_err(|e| e.to_string()));
            });
        #[cfg(not(target_arch = "wasm32"))]
        self.wait_mapped(device, &submitted, &rx)?;
        // On the web the browser drives the device and runs the callback
        // from its event loop, which wakes this future; nothing here can
        // poll or block.
        #[cfg(target_arch = "wasm32")]
        {
            let _ = (device, submitted);
            rx.recv().await.map_err(Error::Readback)?;
        }

        let row = 4 * self.width as usize;
        let mapped = self
            .buffer
            .slice(..)
            .get_mapped_range()
            .map_err(|e| Error::Readback(e.to_string()))?;
        let mut rgba = Vec::with_capacity(row * self.height as usize);
        for r in 0..self.height as usize {
            let start = r * self.padded_row as usize;
            rgba.extend_from_slice(&mapped[start..start + row]);
        }
        drop(mapped);
        self.buffer.unmap();
        Ok(rgba)
    }

    /// Poll the device, waiting on `submitted` a slice at a time, until the
    /// buffer is mapped, the mapping fails, or [`READBACK_WAIT`] has been
    /// spent waiting.
    ///
    /// Each round is `poll(Wait)` on this submission, then two checks: the
    /// callback's result, and whether the buffer is mapped. A successful
    /// wait triages the submission and maps the buffer under the queue's
    /// lifetime lock, so once the GPU is done the second check passes even
    /// if another thread took (or lost) the callback. The budget is counted
    /// in poll slices rather than read from a clock (library crates never
    /// read the clock); a slice that returns early means the submission is
    /// done, and then the next check succeeds.
    #[cfg(not(target_arch = "wasm32"))]
    fn wait_mapped(
        &self,
        device: &wgpu::Device,
        submitted: &wgpu::SubmissionIndex,
        rx: &Receiver,
    ) -> Result<(), Error> {
        let slices = READBACK_WAIT.as_millis() / READBACK_SLICE.as_millis();
        for _ in 0..slices {
            match device.poll(wgpu::PollType::Wait {
                submission_index: Some(submitted.clone()),
                timeout: Some(READBACK_SLICE),
            }) {
                Ok(_) | Err(wgpu::PollError::Timeout) => {}
                Err(e) => return Err(Error::Readback(e.to_string())),
            }
            match rx.try_recv() {
                Some(Ok(())) => return Ok(()),
                // A callback that reports failure (or was dropped) may
                // still have left the buffer mapped; the check below says.
                Some(Err(e)) if !self.is_mapped() => return Err(Error::Readback(e)),
                _ => {}
            }
            if self.is_mapped() {
                return Ok(());
            }
        }
        Err(Error::Readback(format!(
            "the GPU did not finish within {} s",
            READBACK_WAIT.as_secs()
        )))
    }

    /// Whether the buffer's mapping has completed. Asking for the range of
    /// a buffer still waiting to be mapped is an error, not a panic, and
    /// changes nothing.
    #[cfg(not(target_arch = "wasm32"))]
    fn is_mapped(&self) -> bool {
        self.buffer.slice(..).get_mapped_range().is_ok()
    }
}

/// How long a readback waits for the GPU before it reports an error: far
/// longer than any image takes (an 8192-pixel export draws in well under
/// a second), short enough that a wedged device fails a request instead of
/// hanging the app or a test run.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) const READBACK_WAIT: std::time::Duration = std::time::Duration::from_secs(10);

/// One `poll(Wait)` of a readback: short, so a mapping that completed on
/// another thread is noticed promptly.
#[cfg(not(target_arch = "wasm32"))]
const READBACK_SLICE: std::time::Duration = std::time::Duration::from_millis(50);

/// Keeps `Surface::configure` from running while another thread submits
/// on the same device. A configure waits for the queue to drain and fails
/// (`ConfigureSurfaceError::GpuWaitTimeout`, "Failed to wait for GPU to
/// come idle") when a submission lands during that wait (wgpu-core 30.0.1,
/// `device/resource.rs:5351`); the failure goes to wgpu's uncaptured-error
/// handler, which panics, and the finished mappings that wait collected
/// are dropped with it. Submissions and presents take the gate shared,
/// so they never wait for each other; a configure takes it alone. One
/// gate belongs to one device: the viewports' [`crate::viewport::Gpu`]
/// and every [`Offscreen`] made on it share a clone.
#[derive(Debug, Clone, Default)]
pub(crate) struct Gate(Arc<RwLock<()>>);

impl Gate {
    /// `queue.submit(buffers)`, never during a configure.
    pub(crate) fn submit<I: IntoIterator<Item = wgpu::CommandBuffer>>(
        &self,
        queue: &wgpu::Queue,
        buffers: I,
    ) -> wgpu::SubmissionIndex {
        let _shared = self.0.read().unwrap_or_else(PoisonError::into_inner);
        queue.submit(buffers)
    }

    /// [`Gate::submit`], then present `frame`. A present is a submission
    /// too: wgpu-core submits the barrier that moves the drawable to its
    /// present state (`Queue::prepare_surface_texture_for_present`,
    /// `device/queue.rs:125`), so it takes the gate like any other.
    pub(crate) fn submit_and_present<I: IntoIterator<Item = wgpu::CommandBuffer>>(
        &self,
        queue: &wgpu::Queue,
        buffers: I,
        frame: wgpu::SurfaceTexture,
    ) -> wgpu::SubmissionIndex {
        let _shared = self.0.read().unwrap_or_else(PoisonError::into_inner);
        let submitted = queue.submit(buffers);
        queue.present(frame);
        submitted
    }

    /// `surface.configure(device, config)`, with no submission in flight
    /// from another thread.
    pub(crate) fn configure(
        &self,
        surface: &wgpu::Surface<'_>,
        device: &wgpu::Device,
        config: &wgpu::SurfaceConfiguration,
    ) {
        let _alone = self.0.write().unwrap_or_else(PoisonError::into_inner);
        surface.configure(device, config);
    }
}

/// A one-shot channel for the map callback, which wgpu requires to be
/// `Send` on native targets. Built on `std` so the crate needs no async
/// runtime.
fn futures_channel() -> (Sender, Receiver) {
    let slot = Arc::new(Mutex::new(Slot::default()));
    (Sender(slot.clone()), Receiver(slot))
}

#[derive(Default)]
struct Slot {
    result: Option<Result<(), String>>,
    /// Whether the sender has put a result in (which the receiver may
    /// already have taken).
    delivered: bool,
    waker: Option<std::task::Waker>,
}

struct Sender(Arc<Mutex<Slot>>);
struct Receiver(Arc<Mutex<Slot>>);

fn lock(m: &Mutex<Slot>) -> std::sync::MutexGuard<'_, Slot> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

impl Sender {
    fn send(self, r: Result<(), String>) {
        deliver(&self.0, r);
    }
}

/// wgpu can drop a map callback without running it (see
/// [`Readback::read`]); the receiver then hears so instead of waiting for
/// ever.
impl Drop for Sender {
    fn drop(&mut self) {
        deliver(&self.0, Err("the map callback was dropped unrun".into()));
    }
}

/// Put the first result in the slot and wake the receiver; later ones
/// (the drop after a send) are ignored.
fn deliver(slot: &Mutex<Slot>, r: Result<(), String>) {
    let mut slot = lock(slot);
    if slot.delivered {
        return;
    }
    slot.delivered = true;
    slot.result = Some(r);
    if let Some(w) = slot.waker.take() {
        w.wake();
    }
}

impl Receiver {
    /// The callback's result if it has run (or been dropped).
    #[cfg(not(target_arch = "wasm32"))]
    fn try_recv(&self) -> Option<Result<(), String>> {
        lock(&self.0).result.take()
    }

    /// The callback's result, once it has run: a browser runs it from its
    /// event loop, which then wakes this future.
    #[cfg(target_arch = "wasm32")]
    async fn recv(self) -> Result<(), String> {
        std::future::poll_fn(|cx| {
            let mut slot = lock(&self.0);
            match slot.result.take() {
                Some(r) => std::task::Poll::Ready(r),
                None => {
                    slot.waker = Some(cx.waker().clone());
                    std::task::Poll::Pending
                }
            }
        })
        .await
    }
}
