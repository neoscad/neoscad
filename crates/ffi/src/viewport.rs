//! The 3D view of a document window (`docs/audits/macos-prep.md`, step
//! 8c): a [`render::viewport::Viewport`] behind a UniFFI object.
//!
//! # Threads
//!
//! - **Main thread:** everything the view does to it: attaching the layer,
//!   resizing, camera moves, settings and [`Viewport::draw`], which the
//!   view calls from its display link. A frame re-encodes buffers already
//!   on the GPU and takes well under a millisecond of CPU; nothing on this
//!   path builds geometry.
//! - **The engine's queue** (any other thread): [`Core::render_into`],
//!   which evaluates, builds the scene and uploads it, then takes the
//!   viewport's lock only to swap the finished model in. The main thread
//!   therefore never waits for geometry, and waits for the lock at most as
//!   long as that swap.
//!
//! The camera lives here, in Rust ([`render::camera`]): OpenSCAD's
//! gimbal, its drag and wheel maths, View All and the standard views are
//! already there for the exporter and the web app will need them too.
//! Swift sends pointer deltas in points and never holds a camera of its
//! own, so the two cannot drift apart.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Instant;

use render::viewport::{Drawn, Gpu};

use crate::{Core, CoreError, RenderMode, RenderResult, guarded, host, layer};

/// A standard view (OpenSCAD's View menu).
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum ViewPreset {
    Top,
    Bottom,
    Left,
    Right,
    Front,
    Back,
    /// OpenSCAD's "Diagonal", the default camera's direction.
    Diagonal,
}

impl From<ViewPreset> for render::snapshot::View {
    fn from(v: ViewPreset) -> Self {
        use render::snapshot::View;
        match v {
            ViewPreset::Top => View::Top,
            ViewPreset::Bottom => View::Bottom,
            ViewPreset::Left => View::Left,
            ViewPreset::Right => View::Right,
            ViewPreset::Front => View::Front,
            ViewPreset::Back => View::Back,
            ViewPreset::Diagonal => View::Iso,
        }
    }
}

/// How faces are lit ([`render::Lighting`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum LightingStyle {
    /// OpenSCAD's two lights: images look as OpenSCAD's do.
    OpenScad,
    /// A light at the eye: every visible face stays legible.
    Headlight,
}

/// What the View menu toggles.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Record)]
pub struct ViewportSettings {
    pub axes: bool,
    /// Scale markers along the axes (drawn only with the axes).
    pub scales: bool,
    /// The ground grid (NeoSCAD's; OpenSCAD has none).
    pub grid: bool,
    pub edges: bool,
    pub crosshairs: bool,
    pub lighting: LightingStyle,
    pub orthographic: bool,
}

/// What one call to [`Viewport::draw`] did.
#[derive(Debug, Clone, Copy, PartialEq, uniffi::Record)]
pub struct FrameReport {
    /// A frame was drawn and presented.
    pub drawn: bool,
    /// Something changed but the layer could not take a frame (the window
    /// is hidden, or no drawable came in time): try again next refresh.
    pub deferred: bool,
    /// CPU time of the call: acquiring the drawable, encoding and
    /// submitting (the GPU's own time is not included).
    pub cpu_ms: f64,
}

/// The camera, as OpenSCAD's `$vp*` variables.
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct CameraState {
    pub vpt: Vec<f64>,
    pub vpr: Vec<f64>,
    pub vpd: f64,
    pub vpf: f64,
}

/// A frame read back: `width * height` RGBA pixels, top row first.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct ViewportImage {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// One document window's 3D view. Made without a layer; the view attaches
/// its layer while it is in a window and detaches it when it leaves, and
/// the model, camera and settings survive in between.
#[derive(uniffi::Object)]
pub struct Viewport {
    pub(crate) gpu: Arc<Gpu>,
    inner: Mutex<render::viewport::Viewport>,
    /// Numbers `render_into` requests, so a slow old render cannot replace
    /// a newer one's model.
    pub(crate) requests: AtomicU64,
    /// The file's `$vp*` the last document run applied, with its
    /// generation (`document.rs`, `apply_file_view`).
    #[allow(clippy::type_complexity)]
    pub(crate) file_view: Mutex<Option<(FileView, u64)>>,
}

/// The `$vpt`, `$vpr`, `$vpd` and `$vpf` a file assigned (`None` for
/// those it did not).
pub(crate) type FileView = (Option<[f64; 3]>, Option<[f64; 3]>, Option<f64>, Option<f64>);

impl std::fmt::Debug for Viewport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Viewport").finish_non_exhaustive()
    }
}

impl Viewport {
    pub(crate) fn lock(&self) -> MutexGuard<'_, render::viewport::Viewport> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// A size in points at `scale` as whole pixels, as wgpu-hal measures a
    /// layer (`bounds * contentsScale`, truncated;
    /// `wgpu-hal-30.0.1/src/metal/surface.rs:196-203`).
    fn pixels(points: f64, scale: f64) -> u32 {
        let px = points * scale;
        if px.is_finite() && px > 0.0 {
            px.min(f64::from(u32::MAX)) as u32
        } else {
            0
        }
    }
}

fn scheme(name: &str) -> Result<render::ColorScheme, CoreError> {
    render::scheme::find(name).ok_or_else(|| CoreError::InvalidArgument {
        message: format!("unknown colour scheme '{name}'"),
    })
}

fn failed(e: impl std::fmt::Display) -> CoreError {
    CoreError::Failed {
        message: e.to_string(),
    }
}

#[uniffi::export]
impl Viewport {
    /// A viewport drawing in the colour scheme called `color_scheme` (one
    /// of OpenSCAD's, e.g. "Cornfield"), on the shared GPU.
    #[uniffi::constructor]
    pub fn new(color_scheme: String) -> Result<Arc<Viewport>, CoreError> {
        guarded(|| {
            let gpu = host::viewport_gpu().map_err(|message| CoreError::Failed { message })?;
            let inner = render::viewport::Viewport::new(gpu.clone(), scheme(&color_scheme)?)
                .map_err(failed)?;
            Ok(Arc::new(Viewport {
                gpu,
                inner: Mutex::new(inner),
                requests: AtomicU64::new(0),
                file_view: Mutex::new(None),
            }))
        })
    }

    // --- The layer ----------------------------------------------------------

    /// Draw into the `CAMetalLayer` at address `layer` from now on (see
    /// `layer.rs` for the contract: a live layer, on the main thread),
    /// `width` by `height` points at `scale` pixels a point. `readable`
    /// lets [`Viewport::read_pixels`] copy frames (tests only).
    pub fn attach_layer(
        &self,
        layer: u64,
        width: f64,
        height: f64,
        scale: f64,
        readable: bool,
    ) -> Result<(), CoreError> {
        guarded(|| {
            let surface = layer::surface_from_layer(self.gpu.instance(), layer)?;
            self.lock()
                .attach_surface(
                    surface,
                    Self::pixels(width, scale),
                    Self::pixels(height, scale),
                    scale,
                    readable,
                )
                .map_err(failed)
        })
    }

    /// Stop drawing and release the layer. Whether one was attached.
    pub fn detach(&self) -> Result<bool, CoreError> {
        guarded(|| Ok(self.lock().detach()))
    }

    /// The view's new size in points, and pixels per point.
    pub fn resize(&self, width: f64, height: f64, scale: f64) -> Result<(), CoreError> {
        guarded(|| {
            self.lock().resize(
                Self::pixels(width, scale),
                Self::pixels(height, scale),
                scale,
            );
            Ok(())
        })
    }

    /// The sample count frames are drawn with (4 for MSAA; 0 detached).
    pub fn samples(&self) -> Result<u32, CoreError> {
        guarded(|| Ok(self.lock().samples()))
    }

    // --- Frames -------------------------------------------------------------

    /// Whether a frame is due (something changed and there is a layer).
    pub fn needs_draw(&self) -> Result<bool, CoreError> {
        guarded(|| Ok(self.lock().needs_draw()))
    }

    /// Draw a frame if one is due. Call from the display link, on the main
    /// thread.
    pub fn draw(&self) -> Result<FrameReport, CoreError> {
        guarded(|| {
            let t0 = Instant::now();
            let drawn = self.lock().draw().map_err(failed)?;
            Ok(FrameReport {
                drawn: drawn == Drawn::Frame,
                deferred: drawn == Drawn::Deferred,
                cpu_ms: t0.elapsed().as_secs_f64() * 1000.0,
            })
        })
    }

    /// Draw a frame now and read it back (the layer must have been
    /// attached `readable`). Blocks until the GPU is done: tests only.
    pub fn read_pixels(&self) -> Result<ViewportImage, CoreError> {
        guarded(|| {
            let image = self.lock().read_pixels_blocking().map_err(failed)?;
            Ok(ViewportImage {
                width: image.width,
                height: image.height,
                rgba: image.rgba,
            })
        })
    }

    // --- The camera ---------------------------------------------------------

    /// A left drag by `dx`, `dy` points (y down): orbit.
    pub fn orbit(&self, dx: f64, dy: f64) -> Result<(), CoreError> {
        guarded(|| {
            self.lock().orbit(dx, dy);
            Ok(())
        })
    }

    /// A right drag or two-finger scroll by `dx`, `dy` points (y down):
    /// pan, the model following the pointer.
    pub fn pan(&self, dx: f64, dy: f64) -> Result<(), CoreError> {
        guarded(|| {
            self.lock().pan(dx, dy);
            Ok(())
        })
    }

    /// Mouse-wheel zoom by `notches` (positive: closer), a tenth of the
    /// distance a notch as in OpenSCAD.
    pub fn zoom(&self, notches: f64) -> Result<(), CoreError> {
        guarded(|| {
            self.lock().with_camera(|c| c.zoom(120.0 * notches));
            Ok(())
        })
    }

    /// Pinch zoom: the distance divided by `factor` (the gesture's
    /// `1 + magnification`).
    pub fn magnify(&self, factor: f64) -> Result<(), CoreError> {
        guarded(|| {
            self.lock().with_camera(|c| c.zoom_by(factor));
            Ok(())
        })
    }

    /// The trackpad rotate gesture: turn about z by `degrees`.
    pub fn turn(&self, degrees: f64) -> Result<(), CoreError> {
        guarded(|| {
            self.lock().with_camera(|c| c.turn(degrees));
            Ok(())
        })
    }

    pub fn set_view(&self, preset: ViewPreset) -> Result<(), CoreError> {
        guarded(|| {
            self.lock().set_view(preset.into());
            Ok(())
        })
    }

    /// View All: centre on the model and fit it.
    pub fn view_all(&self) -> Result<(), CoreError> {
        guarded(|| {
            self.lock().view_all();
            Ok(())
        })
    }

    /// View > Center: look at the origin.
    pub fn center(&self) -> Result<(), CoreError> {
        guarded(|| {
            self.lock().with_camera(render::Camera::center);
            Ok(())
        })
    }

    /// Reset View: OpenSCAD's default camera.
    pub fn reset_view(&self) -> Result<(), CoreError> {
        guarded(|| {
            self.lock().reset_view();
            Ok(())
        })
    }

    pub fn camera(&self) -> Result<CameraState, CoreError> {
        guarded(|| {
            let v = self.lock();
            let c = v.camera();
            Ok(CameraState {
                vpt: c.vpt().to_vec(),
                vpr: c.vpr().to_vec(),
                vpd: c.viewer_distance,
                vpf: c.fov,
            })
        })
    }

    // --- Settings -----------------------------------------------------------

    pub fn settings(&self) -> Result<ViewportSettings, CoreError> {
        guarded(|| {
            let v = self.lock();
            let s = v.settings();
            Ok(ViewportSettings {
                axes: s.axes,
                scales: s.scales,
                grid: s.grid,
                edges: s.edges,
                crosshairs: s.crosshairs,
                lighting: match s.lighting {
                    render::Lighting::OpenScad => LightingStyle::OpenScad,
                    render::Lighting::Headlight => LightingStyle::Headlight,
                },
                orthographic: v.camera().projection == render::Projection::Orthogonal,
            })
        })
    }

    pub fn set_settings(&self, s: ViewportSettings) -> Result<(), CoreError> {
        guarded(|| {
            let mut v = self.lock();
            v.set_settings(render::viewport::ViewSettings {
                axes: s.axes,
                scales: s.scales,
                grid: s.grid,
                edges: s.edges,
                crosshairs: s.crosshairs,
                lighting: match s.lighting {
                    LightingStyle::OpenScad => render::Lighting::OpenScad,
                    LightingStyle::Headlight => render::Lighting::Headlight,
                },
            });
            let projection = if s.orthographic {
                render::Projection::Orthogonal
            } else {
                render::Projection::Perspective
            };
            if v.camera().projection != projection {
                v.set_projection(projection);
            }
            Ok(())
        })
    }

    /// Draw in the scheme called `name` from now on. The background and
    /// lines change at once; the model's colours come from the scheme it
    /// was built in, so the app renders again after switching.
    pub fn set_color_scheme(&self, name: String) -> Result<(), CoreError> {
        guarded(|| {
            let s = scheme(&name)?;
            self.lock().set_scheme(s);
            Ok(())
        })
    }

    pub fn color_scheme(&self) -> Result<String, CoreError> {
        guarded(|| Ok(self.lock().scheme().name.clone()))
    }
}

#[uniffi::export]
impl Core {
    /// Render (or preview) a document and show the result in `viewport`:
    /// the scene is built in the viewport's colour scheme and uploaded on
    /// the calling thread, then swapped in; the next display refresh draws
    /// it. Returns what `render` returns. A render that fails without
    /// producing anything (a syntax error) leaves the last model on
    /// screen, as OpenSCAD keeps its last image; a newer request that
    /// finishes first wins.
    pub fn render_into(
        &self,
        path: String,
        mode: RenderMode,
        viewport: Arc<Viewport>,
    ) -> Result<RenderResult, CoreError> {
        guarded(|| {
            let run = self.run(&path)?;
            let generation = viewport.requests.fetch_add(1, Ordering::SeqCst) + 1;
            let scheme = viewport.lock().scheme().clone();
            let r = self.session().render(&run, mode.into(), &scheme)?;
            let scene = match (&r.tree, &r.geometry) {
                (Some(tree), _) => Some(render::preview::scene(
                    tree,
                    &scheme,
                    render::Previewer::OpenCsg,
                )),
                (None, Some(g)) => Some(render::Scene::new(Some(g), &scheme)),
                // An empty top level: show the empty view.
                (None, None) if r.exit_code == 0 => Some(render::Scene::new(None, &scheme)),
                (None, None) => None,
            };
            if let Some(scene) = scene {
                let model = viewport.gpu.upload(&scene).map_err(failed)?;
                viewport.lock().set_model(Arc::new(model), generation);
            }
            Ok(crate::render_result(&r, &scheme))
        })
    }
}

#[cfg(test)]
mod tests;
