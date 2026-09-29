//! The window's check and measure panels, File > Export and the App
//! Intents (`docs/audits/macos-prep.md`, step 8i): `check`, `measure`,
//! exports with options, and snapshots, each as a request that does not
//! take part in the document's own superseding.
//!
//! # Why these requests are detached
//!
//! A document run ([`Core::run_document`]) supersedes the older requests on
//! its document, and edits cancel them: that is how a burst of typing
//! leaves one run going. A check, a measurement or an export made through
//! the same mechanism would cancel the live preview when it started, and
//! be cancelled by the next keystroke or the next auto-preview, so a long
//! export could never finish while the user kept typing. These requests
//! run with `supersede` off and their own [`CancelToken`] instead: the app
//! cancels them when their panel asks again (or the user presses Cancel),
//! and closing the document still stops them (`Session::cancel` stops
//! every request on a path).
//!
//! # Numbers
//!
//! Findings, statistics and sections come from the session's own JSON
//! (`docs/cli-json.md`, "check" and "measure"), as the other results do
//! (`types.rs`): the panels show the numbers `neoscad check` and
//! `neoscad measure` print, rounded the same way.
//!
//! The shaping is `crates/client`'s (`inspect.rs`), shared with the web
//! worker; its records are declared to UniFFI here as remote types. This
//! file adds the app's half: cancel tokens, progress listeners, writing
//! exports to disk, snapshots and the viewport's annotations.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use session::snapshot::{SnapshotError, SnapshotRequest};

pub use client::{
    BetweenResult, CheckFinding, CheckOptions, CheckReport, ExportOptions, FindingSeverity,
    PartStats, RunOptions, SectionAxis, SectionResult, SolidStats, ThreeMfColorMode,
    ThreeMfMaterial, TruncatedFindings,
};

use crate::{
    Core, CoreError, Diagnostic, ExportResult, GeometryStats, ParameterOverride, SnapshotOptions,
    SnapshotResult, Viewport, guarded, types,
};

// --- Cancelling and progress ---------------------------------------------

/// Stops one detached request (see the module documentation). Made by the
/// app per request; `cancel` from any thread.
#[derive(Debug, Default, uniffi::Object)]
pub struct CancelToken {
    flag: Arc<AtomicBool>,
}

#[uniffi::export]
impl CancelToken {
    #[uniffi::constructor]
    pub fn new() -> Result<Arc<CancelToken>, CoreError> {
        guarded(|| Ok(Arc::new(CancelToken::default())))
    }

    /// Stop the request at its next check (evaluation and geometry check
    /// often; it then fails with [`CoreError::Cancelled`]).
    pub fn cancel(&self) -> Result<(), CoreError> {
        guarded(|| {
            self.flag.store(true, Ordering::Relaxed);
            Ok(())
        })
    }

    pub fn is_cancelled(&self) -> Result<bool, CoreError> {
        guarded(|| Ok(self.flag.load(Ordering::Relaxed)))
    }
}

/// Told each stage of a long request as it starts (`parse`, `evaluate`,
/// `geometry`, `draw`), on the engine's thread: an export's progress.
#[uniffi::export(with_foreign)]
pub trait ProgressListener: Send + Sync {
    fn stage(&self, stage: String);
}

/// What a detached request runs with besides the document's text.
#[uniffi::remote(Record)]
pub struct RunOptions {
    /// The customizer's values, as the document's runs pass them.
    #[uniffi(default = [])]
    pub overrides: Vec<ParameterOverride>,
    /// neoscad's `part()` extension (`--enable part`).
    #[uniffi(default = false)]
    pub parts: bool,
    /// OpenSCAD's experimental features, as `--enable` names them.
    #[uniffi(default = [])]
    pub enable: Vec<String>,
}

impl Core {
    /// A request on `path` that neither supersedes the document's runs nor
    /// is superseded by them (see the module documentation).
    fn detached(
        &self,
        path: &str,
        options: &RunOptions,
        cancel: Option<&Arc<CancelToken>>,
        progress: Option<Arc<dyn ProgressListener>>,
    ) -> Result<session::Run, CoreError> {
        let progress = progress.map(|p| -> session::Progress {
            Arc::new(move |s: session::Stage| p.stage(s.name().to_string()))
        });
        self.client
            .detached(path, options, cancel.map(|c| c.flag.clone()), progress)
    }
}

// --- Check ------------------------------------------------------------------

/// What `check` counts as a problem (`docs/cli-json.md`, "check").
#[uniffi::remote(Record)]
pub struct CheckOptions {
    /// mm; walls thinner than this are errors.
    pub nozzle: f64,
    /// mm; walls thinner than this are warnings.
    pub min_wall: f64,
    /// Degrees from vertical.
    pub max_overhang: f64,
    /// The build volume `[width, depth, height]` in mm; `None` skips the
    /// bed-fit check.
    pub bed: Option<Vec<f64>>,
    pub bed_tolerance: f64,
    pub max_findings: u32,
}

/// `check`'s defaults (a 0.4 mm nozzle, 0.8 mm walls, 45°, no bed), so the
/// app does not keep a second copy of them.
#[uniffi::export]
pub fn default_check_options() -> Result<CheckOptions, CoreError> {
    guarded(|| Ok(CheckOptions::default()))
}

/// How bad a finding is.
#[uniffi::remote(Enum)]
pub enum FindingSeverity {
    Error,
    Warning,
    Info,
}

/// One problem `check` found (`FINDING` in `docs/cli-json.md`).
#[uniffi::remote(Record)]
pub struct CheckFinding {
    /// From 1, errors first; `snapshot --issues` numbers its markers so.
    pub id: u32,
    pub severity: FindingSeverity,
    /// Stable: `thin-wall`, `overhang`, `floating`, ...
    pub code: String,
    pub message: String,
    pub part: Option<String>,
    /// The worst point, and the box of the whole problem (`None` for a
    /// finding about no place, such as an empty model).
    pub point: Vec<f64>,
    pub bbox_min: Option<Vec<f64>>,
    pub bbox_max: Option<Vec<f64>>,
    pub fix: String,
    pub value: Option<f64>,
    pub limit: Option<f64>,
}

/// Findings of one code left out past `max_findings`.
#[uniffi::remote(Record)]
pub struct TruncatedFindings {
    pub code: String,
    pub count: u32,
}

/// The result of [`Core::check`].
#[uniffi::remote(Record)]
pub struct CheckReport {
    /// 0 when nothing is an error; 1 for errors, or when the model failed
    /// to load, evaluate or render (then `failed`, and no findings).
    pub exit_code: u8,
    pub failed: bool,
    /// Counts before truncation.
    pub errors: u32,
    pub warnings: u32,
    pub info: u32,
    pub findings: Vec<CheckFinding>,
    pub truncated: Vec<TruncatedFindings>,
    /// The thinnest wall any sample measured (mm).
    pub min_wall: Option<f64>,
    /// The model's parts (with `parts` on and a 3D model that has any).
    pub parts: Vec<String>,
    /// `neoscad check`'s report as it prints it: a summary line, then one
    /// line and its fix per finding.
    pub text: String,
    /// `neoscad check --format json`'s object.
    pub summary_json: String,
    pub diagnostics: Vec<Diagnostic>,
    pub console: String,
}

#[uniffi::export]
impl Core {
    /// Check a document for FDM printing (`neoscad check`): its current
    /// text with `run`'s customizer values, under `options`.
    pub fn check(
        &self,
        path: String,
        options: CheckOptions,
        run: RunOptions,
        cancel: Option<Arc<CancelToken>>,
    ) -> Result<CheckReport, CoreError> {
        guarded(|| {
            // The options are checked before the path, as they always were.
            options.to_session()?;
            let run = self.detached(&path, &run, cancel.as_ref(), None)?;
            self.client.check(run, &options)
        })
    }
}

// --- Measure ----------------------------------------------------------------

/// Volume, area, box and centre of mass of a solid (`SOLID` in
/// `docs/cli-json.md`, "measure").
#[uniffi::remote(Record)]
pub struct SolidStats {
    /// mm³, and mm² of surface.
    pub volume: f64,
    pub area: f64,
    pub bbox_min: Vec<f64>,
    pub bbox_max: Vec<f64>,
    /// The centre of mass of the enclosed volume, at uniform density.
    pub centroid: Vec<f64>,
    pub triangles: u64,
}

/// One part's own solid.
#[uniffi::remote(Record)]
pub struct PartStats {
    /// Dotted for nested parts (`lid.hinge`).
    pub name: String,
    pub instances: u32,
    /// The operation above the part that changes it (`difference`,
    /// `hull`, ...), when there is one.
    pub context: Option<String>,
    /// `None` for a part that is not a solid (2D).
    pub solid: Option<SolidStats>,
}

/// The result of [`Core::measure`].
#[derive(Debug, Clone, uniffi::Record)]
pub struct MeasureResult {
    /// 0, or the exit code of a model that failed.
    pub exit_code: u8,
    /// The model's solid (3D), with its pieces and whether it is valid.
    pub model: Option<SolidStats>,
    pub components: Option<u64>,
    pub manifold: Option<bool>,
    /// A 2D model's statistics (its area and outline count).
    pub model_2d: Option<GeometryStats>,
    pub parts: Vec<PartStats>,
    /// The solids, kept for sections, distances and picking without
    /// rendering again; `None` when there is nothing to measure.
    pub measurement: Option<Arc<Measurement>>,
    pub diagnostics: Vec<Diagnostic>,
    pub console: String,
}

/// A measured model's solids: the model's and each part's
/// ([`client::Measurement`]). Sections and distances work from these, so
/// the measure panel's slider cuts the same solid again without
/// evaluating or rendering the document.
#[derive(Debug, uniffi::Object)]
pub struct Measurement {
    inner: client::Measurement,
}

/// An axis-aligned cutting plane's axis.
#[uniffi::remote(Enum)]
pub enum SectionAxis {
    X,
    Y,
    Z,
}

/// A cross-section (`section` in `docs/cli-json.md`, "measure").
#[uniffi::remote(Record)]
pub struct SectionResult {
    /// `z=5`.
    pub plane: String,
    /// mm² (holes subtracted) and mm (every contour).
    pub area: f64,
    pub perimeter: f64,
    pub contours: u32,
    /// In model coordinates; `None` when the plane misses the solid.
    pub bbox_min: Option<Vec<f64>>,
    pub bbox_max: Option<Vec<f64>>,
    /// Each contour's points in model coordinates, flattened
    /// (`x0, y0, z0, x1, ...`), closed implicitly: what the viewport
    /// draws.
    pub outline: Vec<Vec<f64>>,
}

/// The distance between two parts (`between` in `docs/cli-json.md`).
#[uniffi::remote(Record)]
pub struct BetweenResult {
    pub a: String,
    pub b: String,
    /// mm; 0 when they overlap, `None` when either is not a solid.
    pub distance: Option<f64>,
    /// Within 1 µm.
    pub touching: bool,
    pub overlapping: bool,
    pub overlap_volume: f64,
    /// The closest points on `a` and on `b` (not for overlapping parts).
    pub point_a: Option<Vec<f64>>,
    pub point_b: Option<Vec<f64>>,
}

#[uniffi::export]
impl Measurement {
    /// The parts measured, in the model's order.
    pub fn part_names(&self) -> Result<Vec<String>, CoreError> {
        guarded(|| Ok(self.inner.part_names()))
    }

    /// Cut the model (or `part`'s solid) with the plane `axis = offset`.
    pub fn section(
        &self,
        axis: SectionAxis,
        offset: f64,
        part: Option<String>,
    ) -> Result<SectionResult, CoreError> {
        guarded(|| self.inner.section(axis, offset, part.as_deref()))
    }

    /// The smallest distance between two parts, or their overlap.
    pub fn between(&self, a: String, b: String) -> Result<BetweenResult, CoreError> {
        guarded(|| self.inner.between(a, b))
    }

    /// Where a ray (from `origin` along `direction`, model coordinates)
    /// first meets the model's surface: click-to-measure's point. `None`
    /// when it misses.
    pub fn pick(
        &self,
        origin: Vec<f64>,
        direction: Vec<f64>,
    ) -> Result<Option<Vec<f64>>, CoreError> {
        guarded(|| self.inner.pick(&origin, &direction))
    }
}

#[uniffi::export]
impl Core {
    /// Measure a document (`neoscad measure`): the model's and each part's
    /// volume, area, box and centroid, and a [`Measurement`] to take
    /// sections and distances from.
    pub fn measure(
        &self,
        path: String,
        run: RunOptions,
        cancel: Option<Arc<CancelToken>>,
    ) -> Result<MeasureResult, CoreError> {
        guarded(|| {
            let run = self.detached(&path, &run, cancel.as_ref(), None)?;
            let (r, m) = self.client.measure(run)?;
            Ok(MeasureResult {
                exit_code: r.exit_code,
                model: r.model,
                components: r.components,
                manifold: r.manifold,
                model_2d: r.model_2d,
                parts: r.parts,
                measurement: m.map(|inner| Arc::new(Measurement { inner })),
                diagnostics: r.diagnostics,
                console: r.console,
            })
        })
    }
}

// --- Export -----------------------------------------------------------------

/// `export-3mf/color-mode`.
#[uniffi::remote(Enum)]
pub enum ThreeMfColorMode {
    /// The model's own colours, over the default one.
    Model,
    /// No colour at all.
    NoColor,
    /// One colour ([`ExportOptions::threemf_color`]) for everything.
    SelectedOnly,
}

/// `export-3mf/material-type`: where the colours go.
#[uniffi::remote(Enum)]
pub enum ThreeMfMaterial {
    /// A colour group.
    Color,
    /// Base materials (OpenSCAD's default).
    BaseMaterial,
}

/// How to write an export: OpenSCAD's `-O` options the app offers, each
/// `None` for OpenSCAD's default.
#[uniffi::remote(Record)]
pub struct ExportOptions {
    /// OpenSCAD's format id (`stl` for ASCII STL, `binstl`, `3mf`, `obj`,
    /// `off`, `wrl`, `pov`, `svg`, `dxf`, `pdf`); `None` to go by the
    /// output's extension.
    #[uniffi(default = None)]
    pub format: Option<String>,
    #[uniffi(default = None)]
    pub threemf_color_mode: Option<ThreeMfColorMode>,
    /// A colour name or `#rrggbb`, for [`ThreeMfColorMode::SelectedOnly`].
    #[uniffi(default = None)]
    pub threemf_color: Option<String>,
    #[uniffi(default = None)]
    pub threemf_material: Option<ThreeMfMaterial>,
}

/// Writes an export's one output through a temporary file beside it,
/// renamed over the target once complete: a failed or cancelled export
/// never leaves a truncated file where the user's old one was.
struct AtomicFile {
    bytes: u64,
}

impl session::ExportSink for AtomicFile {
    fn write(&mut self, target: &str, data: &[u8]) -> Result<(), String> {
        let target_path = Path::new(target);
        let name = target_path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let temp = target_path.with_file_name(format!(".{name}.neoscad-export"));
        let result = std::fs::write(&temp, data).and_then(|()| std::fs::rename(&temp, target));
        if let Err(e) = result {
            let _ = std::fs::remove_file(&temp);
            return Err(format!("ERROR: Can't write to '{target}': {e}"));
        }
        self.bytes = data.len() as u64;
        Ok(())
    }

    fn summary(&mut self, _: &session::SummaryFacts<'_>, _: &mut eval::Console<Vec<u8>>) -> bool {
        true
    }
}

#[uniffi::export]
impl Core {
    /// Render a document and write it to `output` (an absolute path),
    /// with `options`. Unlike `export`, this runs detached (a long export
    /// survives typing), takes the customizer's values, reports its
    /// stages to `progress`, and stops with `cancel`. A failure (a 2D
    /// model to a 3D format, an empty model, an unwritable folder) is an
    /// `exit_code` of 1 with the reason in `console` and `diagnostics`.
    pub fn export_file(
        &self,
        path: String,
        output: String,
        options: ExportOptions,
        run: RunOptions,
        cancel: Option<Arc<CancelToken>>,
        progress: Option<Arc<dyn ProgressListener>>,
    ) -> Result<ExportResult, CoreError> {
        guarded(|| {
            let target = PathBuf::from(&output);
            if !target.is_absolute() {
                return Err(CoreError::InvalidArgument {
                    message: format!("the output path must be absolute (got '{output}')"),
                });
            }
            let fmt = client::export_format(options.format.as_deref(), &output)?;
            let run = self.detached(&path, &run, cancel.as_ref(), progress)?;
            let mut sink = AtomicFile { bytes: 0 };
            let mut r =
                self.client
                    .export(run, &output, fmt, &options, crate::iso8601_now(), &mut sink)?;
            r.bytes = sink.bytes;
            Ok(r)
        })
    }

    /// A contact sheet (`neoscad snapshot`) of a document, detached like
    /// [`Core::export_file`] and with the customizer's values.
    pub fn snapshot_file(
        &self,
        path: String,
        options: SnapshotOptions,
        run: RunOptions,
        cancel: Option<Arc<CancelToken>>,
    ) -> Result<SnapshotResult, CoreError> {
        guarded(|| {
            let run = self.detached(&path, &run, cancel.as_ref(), None)?;
            let name = Path::new(&run.input)
                .with_extension("png")
                .to_string_lossy()
                .into_owned();
            let mut req = SnapshotRequest::new(run, name);
            req.size = (options.width, options.height);
            req.views = options.views;
            req.dims = options.dims;
            req.preview = options.preview;
            let s = self.session().snapshot(&req).map_err(|e| match e {
                SnapshotError::Cancelled => CoreError::Cancelled,
                SnapshotError::Failed(message) => CoreError::Failed { message },
            })?;
            Ok(SnapshotResult {
                exit_code: s.exit_code,
                png: s.png,
                summary_json: s.summary.to_string(),
                diagnostics: types::diagnostics(&s.log),
                console: types::console(&s.log),
            })
        })
    }
}

// --- The viewport's annotations and picking ----------------------------------

/// A polyline for [`Viewport::set_annotations`]: points flattened
/// (`x0, y0, z0, x1, ...`) in model coordinates, and an RGBA colour
/// (0 to 1 each).
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct ViewLine {
    pub points: Vec<f64>,
    pub closed: bool,
    pub color: Vec<f32>,
}

/// A marked point for [`Viewport::set_annotations`].
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct ViewMarker {
    pub point: Vec<f64>,
    pub label: String,
    pub color: Vec<f32>,
}

/// A ray into the scene, in model coordinates (unit direction).
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct PickRay {
    pub origin: Vec<f64>,
    pub direction: Vec<f64>,
}

fn rgba(c: &[f32]) -> [f32; 4] {
    match c {
        [r, g, b, a] => [*r, *g, *b, *a],
        [r, g, b] => [*r, *g, *b, 1.0],
        _ => [1.0, 0.0, 1.0, 1.0],
    }
}

#[uniffi::export]
impl Viewport {
    /// Draw these lines and markers over the model from now on, replacing
    /// the ones before (empty lists clear them).
    pub fn set_annotations(
        &self,
        lines: Vec<ViewLine>,
        markers: Vec<ViewMarker>,
    ) -> Result<(), CoreError> {
        guarded(|| {
            let lines = lines
                .iter()
                .map(|l| render::viewport::AnnotationLine {
                    // A trailing partial point is dropped.
                    points: l.points.as_chunks::<3>().0.to_vec(),
                    closed: l.closed,
                    color: rgba(&l.color),
                })
                .collect();
            let markers = markers
                .iter()
                .map(|m| {
                    Ok(render::viewport::AnnotationMarker {
                        point: client::point3(&m.point, "a marker's point")?,
                        label: m.label.clone(),
                        color: rgba(&m.color),
                    })
                })
                .collect::<Result<_, CoreError>>()?;
            self.lock()
                .set_annotations(render::viewport::Annotations { lines, markers });
            Ok(())
        })
    }

    /// The ray under a point of the view (points from the top left), for
    /// picking; `None` before the view has a size.
    pub fn ray_at(&self, x: f64, y: f64) -> Result<Option<PickRay>, CoreError> {
        guarded(|| {
            Ok(self.lock().ray_at(x, y).map(|(o, d)| PickRay {
                origin: o.to_vec(),
                direction: d.to_vec(),
            }))
        })
    }

    /// Look at `point` (keeping the rotation and the distance).
    pub fn look_at(&self, point: Vec<f64>) -> Result<(), CoreError> {
        guarded(|| {
            let p = client::point3(&point, "the point")?;
            self.lock().look_at(p);
            Ok(())
        })
    }

    /// The current view as a PNG of `width` by `height` pixels (File >
    /// Export's image), drawn offscreen with the same model, camera and
    /// view options, without the grid and the annotations.
    pub fn image(&self, width: u32, height: u32) -> Result<Vec<u8>, CoreError> {
        guarded(|| {
            if !(16..=8192).contains(&width) || !(16..=8192).contains(&height) {
                return Err(CoreError::InvalidArgument {
                    message: format!("image size {width}x{height} is outside 16 to 8192"),
                });
            }
            let failed = |e: render::offscreen::Error| CoreError::Failed {
                message: e.to_string(),
            };
            // The copy is made under the lock and drawn outside it, so the
            // window's frames do not wait for the GPU readback.
            let mut copy = self.lock().copy_for_image(width, height).map_err(failed)?;
            let image = copy.read_pixels_blocking().map_err(failed)?;
            Ok(render::encode_png(image.width, image.height, &image.rgba))
        })
    }
}

#[cfg(test)]
mod tests;
