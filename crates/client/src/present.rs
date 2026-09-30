//! What the panels say and offer, apart from any toolkit: the console's
//! summary line and filter groups, the export formats, the printer
//! presets and the check's summary, the customizer's edit rules, and the
//! small path rules a document window follows.
//!
//! Each of these was written in Swift for the macOS app and again in
//! JavaScript for the web demo (`docs/audits/shared-core.md`, steps 1 and
//! 2), and the Linux and Windows apps would have written them a third and
//! fourth time. Here they are one table or one function, so a console line
//! is filed under the same filter, a slider lands on the same value and an
//! export fails with the same sentence in every host.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::{
    CheckOptions, CheckReport, Client, ConsoleKind, CoreError, ExportResult, Parameter,
    ParameterControl, ParameterValue, RenderMode, RenderResult, Timings, g,
};

/// How long typing (or a customizer drag) must pause before a document
/// runs again, unless the host picks its own.
///
/// 150 ms, the macOS app's value: the run is also where the editor's
/// markers come from, so this is how soon they follow typing, and it is
/// the language server's own debounce (`crates/cli/src/lsp.rs`). The web
/// demo overrides it with 300 ms because its one worker cannot interrupt
/// a run, so a run started too eagerly holds up the next keystroke's.
pub const DEFAULT_PREVIEW_DELAY_MS: u64 = 150;

/// `%g`: six significant digits, as C's `printf` and the customizer's
/// number fields write numbers.
pub fn format_number(x: f64) -> String {
    g(x, 6)
}

// --- The console -----------------------------------------------------------

/// The one-line summary of a finished run: its time, then the geometry's
/// numbers. A preview has none (it builds the CSG products, not one mesh).
pub fn describe_render(r: &RenderResult, mode: RenderMode) -> String {
    let ms = format!("{:.1} ms", r.timings.total_ms);
    if mode == RenderMode::Preview {
        return if r.exit_code == 0 {
            format!("Previewed in {ms}.")
        } else {
            format!("Preview failed ({ms}).")
        };
    }
    if r.exit_code != 0 {
        return format!("Render failed ({ms}).");
    }
    let Some(geo) = &r.geometry else {
        return format!("Rendered in {ms}: empty result.");
    };
    let size = geo
        .bbox_max
        .iter()
        .zip(&geo.bbox_min)
        .map(|(hi, lo)| format_number(hi - lo))
        .collect::<Vec<_>>()
        .join(" × ");
    let mut parts = vec![format!("{}D", geo.dimensions), format!("bbox {size}")];
    if let Some(v) = geo.volume {
        parts.push(format!("volume {}", format_number(v)));
    }
    parts.push(format!("area {}", format_number(geo.area)));
    if let Some(t) = geo.triangles {
        parts.push(format!("{t} triangles"));
    }
    if let Some(c) = geo.components {
        parts.push(format!("{c} component{}", if c == 1 { "" } else { "s" }));
    }
    if let Some(m) = geo.manifold {
        parts.push(if m { "manifold" } else { "not manifold" }.into());
    }
    format!("Rendered in {ms}: {}", parts.join(", "))
}

/// The stages' times, for the summary's tooltip.
pub fn describe_timings(t: &Timings) -> String {
    format!(
        "Parse {:.1} ms, evaluate {:.1} ms, geometry {:.1} ms; total {:.1} ms",
        t.parse_ms, t.evaluate_ms, t.geometry_ms, t.total_ms
    )
}

/// The console's filters: which kinds of line each shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ConsoleGroup {
    Errors,
    Warnings,
    Echo,
    Other,
}

/// The filter a console line is under. A trace line goes with the error
/// it explains, a deprecation with the warnings.
pub fn console_group(kind: ConsoleKind) -> ConsoleGroup {
    match kind {
        ConsoleKind::Error | ConsoleKind::Trace => ConsoleGroup::Errors,
        ConsoleKind::Warning | ConsoleKind::Deprecated => ConsoleGroup::Warnings,
        ConsoleKind::Echo => ConsoleGroup::Echo,
        ConsoleKind::Info => ConsoleGroup::Other,
    }
}

/// One console filter as a host lists it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConsoleGroupInfo {
    pub group: ConsoleGroup,
    /// A stable id for settings and the web protocol.
    pub id: String,
    pub title: String,
    pub kinds: Vec<ConsoleKind>,
}

/// The console's filters, in the order a host shows them.
pub fn console_groups() -> Vec<ConsoleGroupInfo> {
    use ConsoleKind as K;
    [
        (ConsoleGroup::Errors, "errors", "Errors"),
        (ConsoleGroup::Warnings, "warnings", "Warnings"),
        (ConsoleGroup::Echo, "echo", "Echo"),
        (ConsoleGroup::Other, "other", "Other"),
    ]
    .into_iter()
    .map(|(group, id, title)| ConsoleGroupInfo {
        group,
        id: id.into(),
        title: title.into(),
        kinds: [
            K::Error,
            K::Warning,
            K::Deprecated,
            K::Echo,
            K::Trace,
            K::Info,
        ]
        .into_iter()
        .filter(|k| console_group(*k) == group)
        .collect(),
    })
    .collect()
}

// --- Export ----------------------------------------------------------------

/// What an export writes: the model's geometry, or a picture.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ExportKind {
    /// A geometry file; its `id` is the core's format id.
    Geometry,
    /// A PNG of the 3D view as it is.
    ViewImage,
    /// A PNG contact sheet (`neoscad snapshot`).
    Snapshot,
}

/// One entry of File > Export.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportFormatInfo {
    /// The core's format id (`--export-format`) for geometry;
    /// `view-image` and `snapshot` for the pictures.
    pub id: String,
    pub title: String,
    /// The file extension, without the dot.
    pub extension: String,
    /// The model's dimension the format needs; `None` for pictures.
    pub dimension: Option<u32>,
    pub kind: ExportKind,
}

/// File > Export's formats, in menu order. The geometry entries take their
/// ids and dimensions from the session's own table
/// (`session::export::Format`), so they cannot disagree with what
/// [`Client::export`] accepts.
pub fn export_formats() -> Vec<ExportFormatInfo> {
    let geometry = |id: &str, title: &str| {
        let f = session::export::Format::from_id(id).expect("a known format id");
        ExportFormatInfo {
            id: f.id().into(),
            title: title.into(),
            extension: match f.id() {
                "binstl" => "stl".into(),
                other => other.into(),
            },
            dimension: Some(f.dimension()),
            kind: ExportKind::Geometry,
        }
    };
    let image = |id: &str, title: &str, kind| ExportFormatInfo {
        id: id.into(),
        title: title.into(),
        extension: "png".into(),
        dimension: None,
        kind,
    };
    vec![
        geometry("binstl", "STL (binary)"),
        geometry("stl", "STL (ASCII)"),
        geometry("3mf", "3MF"),
        geometry("obj", "OBJ"),
        geometry("off", "OFF"),
        geometry("svg", "SVG"),
        geometry("dxf", "DXF"),
        geometry("pdf", "PDF"),
        image("view-image", "PNG image of the view", ExportKind::ViewImage),
        image("snapshot", "PNG snapshot sheet", ExportKind::Snapshot),
    ]
}

/// The entry with `id`, if File > Export offers it.
pub fn export_format_info(id: &str) -> Option<ExportFormatInfo> {
    export_formats().into_iter().find(|f| f.id == id)
}

/// Why a file export failed, as an alert says it: the console's last error
/// lines (every line but warnings when there is no `ERROR` line, such as
/// "Current top level object is not a 3D object."), else the exit code.
/// `None` when it succeeded.
pub fn export_failure_reason(r: &ExportResult) -> Option<String> {
    if r.exit_code == 0 {
        return None;
    }
    let lines: Vec<&str> = r.console.split('\n').filter(|l| !l.is_empty()).collect();
    let errors: Vec<&str> = lines
        .iter()
        .copied()
        .filter(|l| l.starts_with("ERROR"))
        .collect();
    let chosen: Vec<&str> = if errors.is_empty() {
        lines
            .into_iter()
            .filter(|l| !l.starts_with("WARNING"))
            .collect()
    } else {
        errors
    };
    let tail = &chosen[chosen.len().saturating_sub(6)..];
    Some(if tail.is_empty() {
        format!("The export failed (exit code {}).", r.exit_code)
    } else {
        tail.join("\n")
    })
}

/// The format File > Export starts on: `preferred` (the last one used),
/// unless the last render was 2D and it is a 3D format or the other way
/// round; then SVG or binary STL.
pub fn suggest_export_format(preferred: &str, last_dimensions: Option<u32>) -> String {
    let want = export_format_info(preferred).and_then(|f| f.dimension);
    match (last_dimensions, want) {
        (Some(have), Some(want)) if have != want => if have == 2 { "svg" } else { "binstl" }.into(),
        _ if export_format_info(preferred).is_none() => "binstl".into(),
        _ => preferred.into(),
    }
}

// --- Check -----------------------------------------------------------------

/// A printer's numbers for `check`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PrinterPreset {
    pub id: String,
    pub name: String,
    pub nozzle: f64,
    /// Width, depth, height in mm.
    pub bed: Vec<f64>,
}

/// The check panel's printers. Build volumes as the makers publish them,
/// written from memory and not yet checked against their spec sheets
/// (`docs/followups.md`); every one ships with a 0.4 mm nozzle. "Custom"
/// covers anything else.
pub fn printer_presets() -> Vec<PrinterPreset> {
    [
        ("prusa-mk4", "Prusa MK4", [250.0, 210.0, 220.0]),
        ("prusa-mini", "Prusa MINI+", [180.0, 180.0, 180.0]),
        ("bambu-x1", "Bambu Lab X1 / P1", [256.0, 256.0, 256.0]),
        ("bambu-a1-mini", "Bambu Lab A1 mini", [180.0, 180.0, 180.0]),
        ("ender-3", "Creality Ender-3", [220.0, 220.0, 250.0]),
        ("voron-350", "Voron 2.4 (350)", [350.0, 350.0, 340.0]),
    ]
    .into_iter()
    .map(|(id, name, bed)| PrinterPreset {
        id: id.into(),
        name: name.into(),
        nozzle: 0.4,
        bed: bed.to_vec(),
    })
    .collect()
}

/// The preset id that stands for "my own numbers".
pub const CUSTOM_PRINTER: &str = "custom";

/// The check panel's settings as a host edits and stores them (the keys
/// are the host's; the values and their bounds are these).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PrinterSettings {
    /// A preset's id, or [`CUSTOM_PRINTER`].
    pub preset: String,
    pub nozzle: f64,
    pub min_wall: f64,
    pub max_overhang: f64,
    pub use_bed: bool,
    pub bed: Vec<f64>,
}

impl Default for PrinterSettings {
    /// `check`'s own defaults, no bed (an Ender-3's volume ready for when
    /// the bed is turned on).
    fn default() -> Self {
        let d = CheckOptions::default();
        PrinterSettings {
            preset: CUSTOM_PRINTER.into(),
            nozzle: d.nozzle,
            min_wall: d.min_wall,
            max_overhang: d.max_overhang,
            use_bed: false,
            bed: vec![220.0, 220.0, 250.0],
        }
    }
}

impl PrinterSettings {
    /// Take a preset's nozzle and bed; walls follow as two perimeters.
    /// An unknown id changes nothing.
    pub fn apply_preset(mut self, id: &str) -> PrinterSettings {
        if let Some(p) = printer_presets().into_iter().find(|p| p.id == id) {
            self.preset = p.id;
            self.nozzle = p.nozzle;
            self.min_wall = 2.0 * p.nozzle;
            self.use_bed = true;
            self.bed = p.bed;
        }
        self
    }

    /// Stored settings made safe to use: each value out of its bounds
    /// (settings written by an older version, or edited by hand) falls
    /// back to the default rather than failing every check.
    pub fn validated(self) -> PrinterSettings {
        let d = PrinterSettings::default();
        let positive = |x: f64, or: f64| if x.is_finite() && x > 0.0 { x } else { or };
        PrinterSettings {
            nozzle: positive(self.nozzle, d.nozzle),
            min_wall: positive(self.min_wall, d.min_wall),
            max_overhang: if (0.0..=90.0).contains(&self.max_overhang) {
                self.max_overhang
            } else {
                d.max_overhang
            },
            bed: if self.bed.len() == 3 && self.bed.iter().all(|x| x.is_finite() && *x > 0.0) {
                self.bed
            } else {
                d.bed
            },
            preset: self.preset,
            use_bed: self.use_bed,
        }
    }

    /// What `check` runs with; the bed tolerance and the findings per code
    /// stay its defaults.
    pub fn check_options(&self) -> CheckOptions {
        CheckOptions {
            nozzle: self.nozzle,
            min_wall: self.min_wall,
            max_overhang: self.max_overhang,
            bed: self.use_bed.then(|| self.bed.clone()),
            ..CheckOptions::default()
        }
    }
}

/// The first error line of a failed request's console: why it failed, for
/// a panel to say (the document's own console keeps its run's).
pub fn first_error(console: &str) -> Option<String> {
    console
        .split('\n')
        .find(|l| l.starts_with("ERROR") || l.contains("Parser error"))
        .map(str::to_string)
}

/// The check panel's summary line.
pub fn check_summary(r: &CheckReport) -> String {
    if r.failed {
        return match first_error(&r.console) {
            Some(e) => format!("The model did not render: {e}"),
            None => "The model did not render.".into(),
        };
    }
    let mut s = format!(
        "{} errors, {} warnings, {} info",
        r.errors, r.warnings, r.info
    );
    if let Some(w) = r.min_wall {
        s += &format!(" · thinnest wall {} mm", format_number(w));
    }
    let more: u64 = r.truncated.iter().map(|t| u64::from(t.count)).sum();
    if more > 0 {
        s += &format!(" · {more} more not listed");
    }
    s
}

// --- The customizer --------------------------------------------------------

/// What a customizer control did.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum ParameterEdit {
    /// A value taken as it is (a checkbox, a dropdown entry, a text
    /// field; text is cut to the control's length).
    Set { value: ParameterValue },
    /// A slider moved: the value lands on the step grid from the minimum.
    Slide { value: f64 },
    /// A number typed into a field: clamped to the control's bounds (a
    /// slider's field takes it as typed, as OpenSCAD's does).
    Type { value: f64 },
    /// A stepper's arrow: one step (1 without a step) up or down, clamped.
    Step { up: bool },
    /// One element of a vector typed: clamped to the control's bounds.
    Item { index: u32, value: f64 },
}

/// A slider's value on its step grid (OpenSCAD's slider moves in steps
/// from the minimum), kept to the step's decimals: 0.1 steps give 0.3,
/// not 0.30000000000000004.
pub fn snap_to_step(x: f64, step: Option<f64>, min: f64) -> f64 {
    let Some(step) = step.filter(|s| *s > 0.0) else {
        return x;
    };
    let n = ((x - min) / step).round();
    let decimals = (-(step.log10().floor() as i32) + 1).clamp(0, 12);
    let scale = 10f64.powi(decimals);
    ((min + n * step) * scale).round() / scale
}

pub fn clamp_to(x: f64, min: Option<f64>, max: Option<f64>) -> f64 {
    let mut v = x;
    if let Some(lo) = min {
        v = v.max(lo);
    }
    if let Some(hi) = max {
        v = v.min(hi);
    }
    v
}

/// `s` cut to at most `max` UTF-8 bytes, on a character boundary.
fn cut(s: String, max: Option<u32>) -> String {
    let Some(max) = max else { return s };
    let max = max as usize;
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_string()
}

/// The override a control's edit makes: the new value, or `None` when it
/// equals the text's own (a value equal to the default is no override,
/// so the run passes nothing and the text's later changes show through).
/// `current` is the value shown before the edit.
pub fn edit_parameter(
    parameter: &Parameter,
    current: &ParameterValue,
    edit: ParameterEdit,
) -> Option<ParameterValue> {
    let number = match current {
        ParameterValue::Number { value } => *value,
        _ => 0.0,
    };
    let bounds = match &parameter.control {
        ParameterControl::Slider { min, max, step } => (Some(*min), Some(*max), *step, true),
        ParameterControl::SpinBox { min, max, step }
        | ParameterControl::Vector { min, max, step } => (*min, *max, *step, false),
        _ => (None, None, None, false),
    };
    let (min, max, step, slider) = bounds;
    let value = match edit {
        ParameterEdit::Set { value } => match (&parameter.control, value) {
            (ParameterControl::Text { max_length }, ParameterValue::Text { value }) => {
                ParameterValue::Text {
                    value: cut(value, *max_length),
                }
            }
            (_, v) => v,
        },
        ParameterEdit::Slide { value } => ParameterValue::Number {
            value: snap_to_step(value, step, min.unwrap_or(0.0)),
        },
        ParameterEdit::Type { value } => ParameterValue::Number {
            value: if slider {
                value
            } else {
                clamp_to(value, min, max)
            },
        },
        ParameterEdit::Step { up } => {
            let s = step.unwrap_or(1.0);
            ParameterValue::Number {
                value: clamp_to(if up { number + s } else { number - s }, min, max),
            }
        }
        ParameterEdit::Item { index, value } => {
            let mut items = match current {
                ParameterValue::Vector { value } => value.clone(),
                _ => Vec::new(),
            };
            if let Some(slot) = items.get_mut(index as usize) {
                *slot = clamp_to(value, min, max);
            }
            ParameterValue::Vector { value: items }
        }
    };
    (value != parameter.default_value).then_some(value)
}

/// The parameter sets file of a document: OpenSCAD's, `name.json` beside
/// `name.scad` (`ParameterWidget::getJsonFile`).
pub fn parameter_set_path(doc_path: &str) -> String {
    Path::new(doc_path)
        .with_extension("json")
        .to_string_lossy()
        .into_owned()
}

/// The colour schemes a viewport draws in, by the names
/// `Viewport::set_color_scheme` takes, in OpenSCAD's menu order.
pub fn color_scheme_names() -> Vec<String> {
    render::scheme::all().into_iter().map(|s| s.name).collect()
}

impl Client {
    /// A path for an untitled document in `dir`: `base.scad`, else
    /// `base 2.scad`, `base 3.scad`, ... The first that no other untitled
    /// document uses (`taken`) and no file has: the document's buffer
    /// would otherwise hide that file from other documents' includes. The
    /// directory is the host's to choose (the Mac's is the document
    /// controller's current directory), so an untitled document behaves
    /// as a file there: `include <parts.scad>` finds the parts beside it.
    pub fn untitled_path(
        &self,
        dir: &str,
        base: &str,
        taken: &[String],
    ) -> Result<String, CoreError> {
        let dir = self.doc_path(dir)?;
        let base = if base.is_empty() { "Untitled" } else { base };
        let mut n = 1u64;
        loop {
            let name = if n == 1 {
                format!("{base}.scad")
            } else {
                format!("{base} {n}.scad")
            };
            let path = dir.join(name);
            let s = path.to_string_lossy().into_owned();
            if !taken.contains(&s) && !self.session.fs().exists(&path) {
                return Ok(s);
            }
            n += 1;
        }
    }
}
