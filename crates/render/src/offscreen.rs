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
use std::sync::{Arc, Mutex};

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
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::NoAdapter(e) => write!(f, "no GPU adapter for offscreen rendering ({e})"),
            Error::Device(e) => write!(f, "cannot create a GPU device ({e})"),
            Error::Readback(e) => write!(f, "cannot read the rendered image back ({e})"),
            Error::SceneTooLarge { bytes, max } => write!(
                f,
                "the model needs a {bytes}-byte vertex buffer; the GPU allows {max} bytes"
            ),
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
                label: Some("neoscad offscreen"),
                // Everything the adapter offers: a large model needs a large
                // vertex buffer, and a large image a large texture and
                // readback buffer; WebGPU's portable defaults (256 MiB
                // buffers) would refuse models the GPU could draw.
                required_limits: adapter.limits(),
                ..Default::default()
            })
            .await
            .map_err(|e| Error::Device(e.to_string()))?;
        let renderer = Renderer::new(&device, FORMAT, 1);
        Ok(Offscreen {
            device,
            queue,
            renderer,
            adapter: adapter.get_info(),
        })
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
        let buffers =
            SceneBuffers::upload(&self.device, scene).map_err(|t| Error::SceneTooLarge {
                bytes: t.bytes,
                max: t.max,
            })?;
        let mut out = Vec::with_capacity(views.len());
        for (camera, overlay) in views {
            out.push(self.draw(&buffers, camera, scheme, overlay, edges).await?);
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
        let frame = FrameParams::new(camera, scheme, edges);

        // Rows of a texture copy are padded to 256 bytes.
        let row = 4 * width;
        let padded_row =
            row.div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT) * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let readback_size = u64::from(padded_row) * u64::from(height);
        if readback_size > self.device.limits().max_buffer_size {
            return Err(Error::Size { width, height, max });
        }
        let readback = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("neoscad readback"),
            size: readback_size,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });

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
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &color,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &readback,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded_row),
                    rows_per_image: Some(height),
                },
            },
            size,
        );
        self.queue.submit([encoder.finish()]);

        let (tx, rx) = futures_channel();
        readback.slice(..).map_async(wgpu::MapMode::Read, move |r| {
            tx.send(r.map_err(|e| e.to_string()));
        });
        // Native backends need polling to finish the work and run the
        // callback; on the web the browser does it and this is a no-op.
        self.device
            .poll(wgpu::PollType::wait_indefinitely())
            .map_err(|e| Error::Readback(e.to_string()))?;
        rx.recv().await.map_err(Error::Readback)?;

        let mapped = readback
            .slice(..)
            .get_mapped_range()
            .map_err(|e| Error::Readback(e.to_string()))?;
        let mut rgba = Vec::with_capacity((row * height) as usize);
        for r in 0..height as usize {
            let start = r * padded_row as usize;
            rgba.extend_from_slice(&mapped[start..start + row as usize]);
        }
        drop(mapped);
        readback.unmap();
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
    waker: Option<std::task::Waker>,
}

struct Sender(Arc<Mutex<Slot>>);
struct Receiver(Arc<Mutex<Slot>>);

fn lock(m: &Mutex<Slot>) -> std::sync::MutexGuard<'_, Slot> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

impl Sender {
    fn send(self, r: Result<(), String>) {
        let mut slot = lock(&self.0);
        slot.result = Some(r);
        if let Some(w) = slot.waker.take() {
            w.wake();
        }
    }
}

impl Receiver {
    /// The callback's result, once it has run. Native backends run it
    /// inside `poll(wait)`, before this is awaited; a browser runs it from
    /// its event loop, which then wakes this future.
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
