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

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};

use geom::Geometry;
use geom::manifold_geom::ManifoldGeometry;
use serde_json::Value;
use session::check::{CheckRequest, CheckSettings};
use session::measure::Plane;
use session::mesh::{Bvh, Mesh};
use session::snapshot::{SnapshotError, SnapshotRequest};

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
#[derive(Debug, Clone, Default, PartialEq, uniffi::Record)]
pub struct RunOptions {
    /// The customizer's values, as the document's runs pass them.
    #[uniffi(default = [])]
    pub overrides: Vec<ParameterOverride>,
    /// neoscad's `part()` extension (`--enable part`).
    #[uniffi(default = false)]
    pub parts: bool,
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
        let mut run = self.run(path)?;
        run.supersede = false;
        run.parts = options.parts;
        run.defines = options
            .overrides
            .iter()
            .filter_map(crate::document::define)
            .collect();
        run.interrupt = cancel.map(|c| c.flag.clone());
        if let Some(p) = progress {
            run.progress = Some(Arc::new(move |s: session::Stage| {
                p.stage(s.name().to_string())
            }));
        }
        Ok(run)
    }
}

// --- Check ------------------------------------------------------------------

/// What `check` counts as a problem (`docs/cli-json.md`, "check").
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
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

impl From<CheckSettings> for CheckOptions {
    fn from(s: CheckSettings) -> Self {
        CheckOptions {
            nozzle: s.nozzle,
            min_wall: s.min_wall,
            max_overhang: s.max_overhang,
            bed: s.bed.map(|b| b.to_vec()),
            bed_tolerance: s.bed_tolerance,
            max_findings: u32::try_from(s.max_findings).unwrap_or(u32::MAX),
        }
    }
}

impl CheckOptions {
    fn to_session(&self) -> Result<CheckSettings, CoreError> {
        let bad = |what: &str| CoreError::InvalidArgument {
            message: format!("check: {what}"),
        };
        let positive = |x: f64| x.is_finite() && x > 0.0;
        if !positive(self.nozzle) || !positive(self.min_wall) {
            return Err(bad("the nozzle and the minimum wall must be positive"));
        }
        if !(self.max_overhang.is_finite() && (0.0..=90.0).contains(&self.max_overhang)) {
            return Err(bad("the maximum overhang must be 0 to 90 degrees"));
        }
        if !(self.bed_tolerance.is_finite() && self.bed_tolerance >= 0.0) {
            return Err(bad("the bed tolerance must not be negative"));
        }
        let bed = match &self.bed {
            None => None,
            Some(b) if b.len() == 3 && b.iter().all(|&x| positive(x)) => Some([b[0], b[1], b[2]]),
            Some(_) => return Err(bad("the bed is three positive sizes, width, depth, height")),
        };
        Ok(CheckSettings {
            bed,
            nozzle: self.nozzle,
            min_wall: self.min_wall,
            max_overhang: self.max_overhang,
            bed_tolerance: self.bed_tolerance,
            max_findings: self.max_findings as usize,
        })
    }
}

/// `check`'s defaults (a 0.4 mm nozzle, 0.8 mm walls, 45°, no bed), so the
/// app does not keep a second copy of them.
#[uniffi::export]
pub fn default_check_options() -> Result<CheckOptions, CoreError> {
    guarded(|| Ok(CheckSettings::default().into()))
}

/// How bad a finding is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum FindingSeverity {
    Error,
    Warning,
    Info,
}

/// One problem `check` found (`FINDING` in `docs/cli-json.md`).
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
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
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct TruncatedFindings {
    pub code: String,
    pub count: u32,
}

/// The result of [`Core::check`].
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
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

fn f64s(v: &Value) -> Option<Vec<f64>> {
    v.as_array()
        .map(|a| a.iter().map(|x| x.as_f64().unwrap_or(0.0)).collect())
}

fn u32_of(v: &Value) -> u32 {
    v.as_u64()
        .map_or(0, |n| u32::try_from(n).unwrap_or(u32::MAX))
}

fn finding(f: &Value) -> CheckFinding {
    let loc = &f["location"];
    CheckFinding {
        id: u32_of(&f["id"]),
        severity: match f["severity"].as_str() {
            Some("error") => FindingSeverity::Error,
            Some("warning") => FindingSeverity::Warning,
            _ => FindingSeverity::Info,
        },
        code: f["code"].as_str().unwrap_or_default().to_string(),
        message: f["message"].as_str().unwrap_or_default().to_string(),
        part: f["part"].as_str().map(str::to_string),
        point: f64s(&loc["point"]).unwrap_or_default(),
        bbox_min: f64s(&loc["bbox"]["min"]),
        bbox_max: f64s(&loc["bbox"]["max"]),
        fix: f["fix"].as_str().unwrap_or_default().to_string(),
        value: f["value"].as_f64(),
        limit: f["limit"].as_f64(),
    }
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
            let settings = options.to_session()?;
            let run = self.detached(&path, &run, cancel.as_ref(), None)?;
            let c = self.session.check(&CheckRequest { run, settings })?;
            let s = &c.summary;
            let failed = s["failed"] == Value::Bool(true);
            Ok(CheckReport {
                exit_code: c.exit_code,
                failed,
                errors: u32_of(&s["counts"]["errors"]),
                warnings: u32_of(&s["counts"]["warnings"]),
                info: u32_of(&s["counts"]["info"]),
                findings: s["findings"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(finding)
                    .collect(),
                truncated: s["truncated"]
                    .as_object()
                    .into_iter()
                    .flatten()
                    .map(|(code, n)| TruncatedFindings {
                        code: code.clone(),
                        count: u32_of(n),
                    })
                    .collect(),
                min_wall: s["model"]["min_wall"]["thickness"].as_f64(),
                parts: s["parts"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|p| p["name"].as_str().map(str::to_string))
                    .collect(),
                text: session::check::text(s),
                summary_json: s.to_string(),
                diagnostics: types::diagnostics(&c.log),
                console: types::console(&c.log),
            })
        })
    }
}

// --- Measure ----------------------------------------------------------------

/// Volume, area, box and centre of mass of a solid (`SOLID` in
/// `docs/cli-json.md`, "measure").
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
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

fn solid_stats(m: &ManifoldGeometry) -> Option<SolidStats> {
    let v = session::measure::solid_json(m);
    Some(SolidStats {
        volume: v["volume"].as_f64()?,
        area: v["area"].as_f64()?,
        bbox_min: f64s(&v["bbox"]["min"])?,
        bbox_max: f64s(&v["bbox"]["max"])?,
        centroid: f64s(&v["centroid"])?,
        triangles: v["triangles"].as_u64().unwrap_or(0),
    })
}

/// One part's own solid.
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
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

/// A solid, with its mesh and hierarchy for picking made on first use.
struct Solid {
    geometry: ManifoldGeometry,
    picking: OnceLock<(Mesh, Bvh)>,
}

impl Solid {
    fn new(geometry: ManifoldGeometry) -> Solid {
        Solid {
            geometry,
            picking: OnceLock::new(),
        }
    }
}

/// A measured model's solids: the model's and each part's. Sections and
/// distances work from these, so the measure panel's slider cuts the same
/// solid again without evaluating or rendering the document.
#[derive(uniffi::Object)]
pub struct Measurement {
    model: Option<Solid>,
    parts: Vec<(String, Option<Solid>)>,
}

impl std::fmt::Debug for Measurement {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Measurement")
            .field(
                "parts",
                &self.parts.iter().map(|p| &p.0).collect::<Vec<_>>(),
            )
            .finish_non_exhaustive()
    }
}

/// An axis-aligned cutting plane's axis.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum SectionAxis {
    X,
    Y,
    Z,
}

/// A cross-section (`section` in `docs/cli-json.md`, "measure").
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
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
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
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

impl Measurement {
    fn solid(&self, part: Option<&str>) -> Result<&Solid, CoreError> {
        let missing = |what: String| CoreError::InvalidArgument { message: what };
        match part {
            None => self
                .model
                .as_ref()
                .ok_or_else(|| missing("the model is not a solid".into())),
            Some(name) => match self.parts.iter().find(|(n, _)| n == name) {
                Some((_, Some(s))) => Ok(s),
                Some((_, None)) => Err(missing(format!("part '{name}' is not a solid"))),
                None => Err(missing(if self.parts.is_empty() {
                    format!("no part '{name}': the model has no parts (they need parts enabled)")
                } else {
                    format!(
                        "no part '{name}' (parts: {})",
                        self.parts
                            .iter()
                            .map(|p| p.0.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    )
                })),
            },
        }
    }
}

fn point3(v: &[f64], what: &str) -> Result<[f64; 3], CoreError> {
    match v {
        [x, y, z] if v.iter().all(|c| c.is_finite()) => Ok([*x, *y, *z]),
        _ => Err(CoreError::InvalidArgument {
            message: format!("{what} must be three finite numbers"),
        }),
    }
}

#[uniffi::export]
impl Measurement {
    /// The parts measured, in the model's order.
    pub fn part_names(&self) -> Result<Vec<String>, CoreError> {
        guarded(|| Ok(self.parts.iter().map(|p| p.0.clone()).collect()))
    }

    /// Cut the model (or `part`'s solid) with the plane `axis = offset`.
    pub fn section(
        &self,
        axis: SectionAxis,
        offset: f64,
        part: Option<String>,
    ) -> Result<SectionResult, CoreError> {
        guarded(|| {
            if !offset.is_finite() {
                return Err(CoreError::InvalidArgument {
                    message: "the section's offset must be finite".into(),
                });
            }
            let plane = match axis {
                SectionAxis::X => Plane::X(offset),
                SectionAxis::Y => Plane::Y(offset),
                SectionAxis::Z => Plane::Z(offset),
            };
            let solid = self.solid(part.as_deref())?;
            let (v, poly) = session::measure::section(&solid.geometry, plane);
            Ok(SectionResult {
                plane: plane.name(),
                area: v["area"].as_f64().unwrap_or(0.0),
                perimeter: v["perimeter"].as_f64().unwrap_or(0.0),
                contours: u32_of(&v["contours"]),
                bbox_min: f64s(&v["bbox"]["min"]),
                bbox_max: f64s(&v["bbox"]["max"]),
                outline: poly
                    .outlines
                    .iter()
                    .map(|o| o.vertices.iter().flat_map(|p| plane.to_model(*p)).collect())
                    .collect(),
            })
        })
    }

    /// The smallest distance between two parts, or their overlap.
    pub fn between(&self, a: String, b: String) -> Result<BetweenResult, CoreError> {
        guarded(|| {
            let (sa, sb) = (self.solid(Some(&a))?, self.solid(Some(&b))?);
            let v = session::measure::between(&sa.geometry, &sb.geometry);
            let points = v["points"].as_array();
            Ok(BetweenResult {
                distance: v["distance"].as_f64(),
                touching: v["touching"] == Value::Bool(true),
                overlapping: v["overlapping"] == Value::Bool(true),
                overlap_volume: v["overlap_volume"].as_f64().unwrap_or(0.0),
                point_a: points.and_then(|p| p.first()).and_then(f64s),
                point_b: points.and_then(|p| p.get(1)).and_then(f64s),
                a,
                b,
            })
        })
    }

    /// Where a ray (from `origin` along `direction`, model coordinates)
    /// first meets the model's surface: click-to-measure's point. `None`
    /// when it misses.
    pub fn pick(
        &self,
        origin: Vec<f64>,
        direction: Vec<f64>,
    ) -> Result<Option<Vec<f64>>, CoreError> {
        guarded(|| {
            let o = point3(&origin, "the ray's origin")?;
            let d = point3(&direction, "the ray's direction")?;
            let n = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt();
            if n == 0.0 {
                return Err(CoreError::InvalidArgument {
                    message: "the ray's direction must not be zero".into(),
                });
            }
            let d = d.map(|c| c / n);
            let Some(solid) = &self.model else {
                return Ok(None);
            };
            let (mesh, bvh) = solid.picking.get_or_init(|| {
                let mesh = Mesh::of_solid(&solid.geometry);
                let bvh = Bvh::new(&mesh);
                (mesh, bvh)
            });
            Ok(bvh
                .ray(mesh, o, d, 0.0, f64::INFINITY, |_| false)
                .map(|(t, _)| vec![o[0] + t * d[0], o[1] + t * d[1], o[2] + t * d[2]]))
        })
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
            let scheme = render::ColorScheme::cornfield();
            let (model, parts) = self.session.render_parts(&run, &scheme)?;
            let mut out = MeasureResult {
                exit_code: model.exit_code,
                model: None,
                components: None,
                manifold: None,
                model_2d: None,
                parts: Vec::new(),
                measurement: None,
                diagnostics: types::diagnostics(&model.log),
                console: types::console(&model.log),
            };
            if model.exit_code != 0 {
                return Ok(out);
            }
            let solid = match &model.geometry {
                None => None,
                Some(g @ Geometry::Polygon2d(_)) => {
                    out.model_2d = Some(types::geometry_stats(g, &scheme.geometry_scheme()));
                    None
                }
                Some(g) => {
                    let solid = session::stats::solid(g);
                    out.model = solid_stats(&solid);
                    out.components = Some(Mesh::of_solid(&solid).components().1 as u64);
                    out.manifold = Some(solid.is_valid());
                    Some(Solid::new(solid))
                }
            };
            out.parts = parts
                .iter()
                .map(|p| PartStats {
                    name: p.name.clone(),
                    instances: u32::try_from(p.instances).unwrap_or(u32::MAX),
                    context: p.context.map(str::to_string),
                    solid: p.solid.as_ref().and_then(solid_stats),
                })
                .collect();
            if solid.is_some() || !parts.is_empty() {
                out.measurement = Some(Arc::new(Measurement {
                    model: solid,
                    parts: parts
                        .into_iter()
                        .map(|p| (p.name, p.solid.map(Solid::new)))
                        .collect(),
                }));
            }
            Ok(out)
        })
    }
}

// --- Export -----------------------------------------------------------------

/// `export-3mf/color-mode`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum ThreeMfColorMode {
    /// The model's own colours, over the default one.
    Model,
    /// No colour at all.
    NoColor,
    /// One colour ([`ExportOptions::threemf_color`]) for everything.
    SelectedOnly,
}

/// `export-3mf/material-type`: where the colours go.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum ThreeMfMaterial {
    /// A colour group.
    Color,
    /// Base materials (OpenSCAD's default).
    BaseMaterial,
}

/// How to write an export: OpenSCAD's `-O` options the app offers, each
/// `None` for OpenSCAD's default.
#[derive(Debug, Clone, Default, PartialEq, uniffi::Record)]
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
            let id = options.format.clone().unwrap_or_else(|| {
                target
                    .extension()
                    .map(|e| e.to_string_lossy().to_lowercase())
                    .unwrap_or_default()
            });
            let fmt = session::export::Format::from_id(&id).ok_or_else(|| {
                CoreError::InvalidArgument {
                    message: format!(
                        "unknown export format '{id}' (stl, binstl, off, obj, 3mf, wrl, pov, svg, dxf or pdf)"
                    ),
                }
            })?;
            let run = self.detached(&path, &run, cancel.as_ref(), progress)?;
            let scheme = render::ColorScheme::cornfield();
            let mut settings = crate::export_settings(&run, scheme.geometry_scheme());
            apply_threemf(&mut settings, &options, &scheme);
            let req = session::ExportRequest {
                run,
                outputs: vec![(output.clone(), fmt)],
                force: false,
                scheme,
                settings,
            };
            let mut sink = AtomicFile { bytes: 0 };
            let r = self.session.export(&req, &mut sink)?;
            Ok(ExportResult {
                exit_code: r.exit_code,
                format: fmt.id().to_string(),
                bytes: sink.bytes,
                geometry: r
                    .geometry
                    .as_ref()
                    .map(|g| types::geometry_stats(g, &req.scheme.geometry_scheme())),
                diagnostics: types::diagnostics(&r.log),
                console: types::console(&r.log),
                timings: r.timings.into(),
            })
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
            let s = self.session.snapshot(&req).map_err(|e| match e {
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

/// The 3MF options into the encoder's settings, the colour resolved as
/// the command line resolves `export-3mf/color` (a name it cannot parse
/// warns and falls back to the default colour).
fn apply_threemf(
    settings: &mut session::export::Settings,
    options: &ExportOptions,
    scheme: &render::ColorScheme,
) {
    use io::threemf::{ColorMode, MaterialType};
    let t = &mut settings.threemf;
    if let Some(m) = options.threemf_color_mode {
        t.color_mode = match m {
            ThreeMfColorMode::Model => ColorMode::Model,
            ThreeMfColorMode::NoColor => ColorMode::None,
            ThreeMfColorMode::SelectedOnly => ColorMode::SelectedOnly,
        };
    }
    if let Some(m) = options.threemf_material {
        t.material_type = match m {
            ThreeMfMaterial::Color => MaterialType::Color,
            ThreeMfMaterial::BaseMaterial => MaterialType::BaseMaterial,
        };
    }
    if t.color_mode == ColorMode::SelectedOnly {
        let name = options.threemf_color.as_deref().unwrap_or("#f9d72c");
        t.color = Some(match eval::parse_color(name) {
            Some(c) => io::Color(c),
            None => {
                settings.threemf_warning = Some(format!(
                    "Unable to parse color \"{name}\", reverting to default color."
                ));
                scheme.geometry_scheme().face_front
            }
        });
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
                        point: point3(&m.point, "a marker's point")?,
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
            let p = point3(&point, "the point")?;
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
