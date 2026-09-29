//! The records and enums a front end receives, and their conversions from
//! the session's results. Moved from `crates/ffi` so the web worker shares
//! them: the app sees them through UniFFI (`crates/ffi/src/types.rs`
//! declares them as remote types, field for field), the worker as JSON
//! (serde, camelCase; `docs/web-protocol.md`).
//!
//! Most results are typed: diagnostics, geometry statistics, timings and
//! limits are small and fixed in shape, and typed records give Swift
//! exhaustive `switch`es and no parsing code of its own.
//!
//! Diagnostics and geometry statistics are converted from the session's
//! own JSON ([`session::Log::diagnostics_json`], [`session::stats::geometry`])
//! rather than rebuilt from the underlying structures, so a front end sees
//! the exact codes, hints and numbers the CLI, `serve` and MCP report: one
//! source of truth for the "did you mean" hints and the statistics.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Why a call failed. Every front-end call returns this on failure: Swift
/// sees a thrown error (never a trap that would lose unsaved work), the web
/// page an `{ kind, message }` error.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum CoreError {
    /// A newer request on the same document (an edit, a newer render) or
    /// an explicit `cancel` stopped this one. Not a failure of the model:
    /// the front end drops the stale result.
    Cancelled,
    /// A bad argument from the caller (an unknown export format, an edit
    /// outside the text, a bad snapshot size).
    InvalidArgument { message: String },
    /// The request could not be carried out (no GPU for a snapshot, an
    /// unreadable document).
    Failed { message: String },
    /// A bug in the core: the request panicked. The core caught it; the
    /// session stays usable.
    Panicked { message: String },
}

impl std::fmt::Display for CoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CoreError::Cancelled => f.write_str("cancelled by a newer request"),
            CoreError::InvalidArgument { message } => write!(f, "invalid argument: {message}"),
            CoreError::Failed { message } => f.write_str(message),
            CoreError::Panicked { message } => {
                write!(f, "internal error: the request panicked: {message}")
            }
        }
    }
}

impl std::error::Error for CoreError {}

impl From<session::Cancelled> for CoreError {
    fn from(_: session::Cancelled) -> Self {
        CoreError::Cancelled
    }
}

/// An open document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DocInfo {
    /// The path as the session keys it (absolute, normalised).
    pub path: String,
    /// Bumped by every change; 0 for a document read from disk.
    pub version: u64,
    /// Bytes of text, when the session holds it.
    pub length: Option<u64>,
}

impl From<session::DocInfo> for DocInfo {
    fn from(d: session::DocInfo) -> Self {
        DocInfo {
            path: d.path.to_string_lossy().into_owned(),
            version: d.version,
            length: d.len.map(|n| n as u64),
        }
    }
}

/// A replacement of `start..end` by `text`. Offsets are **UTF-8 byte**
/// offsets into the current text (the session's unit); an editor that
/// counts UTF-16 units converts first. Edits apply in order, each to the
/// result of the previous one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TextEdit {
    pub start: u64,
    pub end: u64,
    pub text: String,
}

/// A diagnostic's severity. Echo and trace lines are not diagnostics:
/// echo output is in `echo`, and trace lines in their error's `trace`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Severity {
    Error,
    Warning,
    Deprecated,
}

/// A range of source text: 1-based lines, 1-based byte columns, the end
/// exclusive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceSpan {
    pub start_line: u32,
    pub start_column: u32,
    pub end_line: u32,
    pub end_column: u32,
}

/// Replacement text for a span, as a fix.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Replacement {
    pub span: SourceSpan,
    pub text: String,
}

/// How to fix a diagnostic.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Hint {
    pub message: String,
    pub replacement: Option<Replacement>,
}

/// One diagnostic (`docs/cli-json.md`, "Diagnostics").
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Diagnostic {
    /// The stable code, e.g. `syntax-error`, `unknown-module`,
    /// `resource-limit`.
    pub code: String,
    pub severity: Severity,
    /// The message alone.
    pub message: String,
    /// OpenSCAD's line, word for word.
    pub text: String,
    pub file: Option<String>,
    /// The line OpenSCAD reports (not always the span's first line).
    pub line: Option<u32>,
    pub span: Option<SourceSpan>,
    pub hints: Vec<Hint>,
    /// The `TRACE:` lines that followed an error: the call stack.
    pub trace: Vec<String>,
}

/// Timings of one request, in milliseconds.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Timings {
    pub parse_ms: f64,
    pub evaluate_ms: f64,
    pub geometry_ms: f64,
    pub total_ms: f64,
}

impl From<session::Timings> for Timings {
    fn from(t: session::Timings) -> Self {
        Timings {
            parse_ms: t.parse,
            evaluate_ms: t.evaluate,
            geometry_ms: t.geometry,
            total_ms: t.total,
        }
    }
}

/// Statistics of a rendered geometry (`docs/cli-json.md`, `geometry`).
/// 3D results fill `volume`, `triangles`, `vertices`, `manifold` and
/// `components`; 2D results fill `contours`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GeometryStats {
    /// 2 or 3.
    pub dimensions: u8,
    pub bbox_min: Vec<f64>,
    pub bbox_max: Vec<f64>,
    /// The surface area (3D), or the enclosed area (2D).
    pub area: f64,
    pub volume: Option<f64>,
    pub triangles: Option<u64>,
    pub vertices: Option<u64>,
    pub manifold: Option<bool>,
    pub components: Option<u64>,
    pub contours: Option<u64>,
}

/// What a render builds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RenderMode {
    /// The full geometry (`--render`).
    Render,
    /// Full geometry, a mesh converted to a solid (`--render=force`).
    Force,
    /// OpenSCAD's preview: the leaves and the CSG products (no statistics).
    Preview,
}

impl From<RenderMode> for session::Mode {
    fn from(m: RenderMode) -> Self {
        match m {
            RenderMode::Render => session::Mode::Render,
            RenderMode::Force => session::Mode::Force,
            RenderMode::Preview => session::Mode::Preview,
        }
    }
}

/// The result of `evaluate`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Evaluation {
    /// 0, or the command line's exit code for the failure.
    pub exit_code: u8,
    /// Whether an evaluation error stopped evaluation early.
    pub aborted: bool,
    pub diagnostics: Vec<Diagnostic>,
    /// `echo()` output, one line each, as printed.
    pub echo: Vec<String>,
    /// Every line the command line would print on stderr, in order: what a
    /// console panel shows.
    pub console: String,
    pub timings: Timings,
}

/// The result of `render`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RenderResult {
    pub exit_code: u8,
    pub diagnostics: Vec<Diagnostic>,
    pub echo: Vec<String>,
    pub console: String,
    /// `None` for an empty result or a preview.
    pub geometry: Option<GeometryStats>,
    /// Entries in the geometry cache after the render.
    pub cache_entries: u64,
    pub timings: Timings,
}

/// The result of `export`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportResult {
    /// 0, or the exit code of the failure (the file is then not written,
    /// or incomplete; `console` says why).
    pub exit_code: u8,
    /// OpenSCAD's format id (`stl`, `3mf`, `svg`, ...).
    pub format: String,
    /// Bytes written.
    pub bytes: u64,
    pub geometry: Option<GeometryStats>,
    pub diagnostics: Vec<Diagnostic>,
    pub console: String,
    pub timings: Timings,
}

/// Resource limits for every request (`eval::limits`); `None` is
/// unlimited. A core starts with the agent defaults
/// ([`session::Limits::AGENT`]), because live editing runs half-typed
/// code: one keystroke can turn `$fn=10` into `$fn=100000`.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourceLimits {
    pub time_seconds: Option<f64>,
    /// Estimated bytes.
    pub memory_bytes: Option<u64>,
    /// Segments of one circle, sphere, cylinder, `rotate_extrude` or round
    /// `offset`.
    pub fragments: Option<u64>,
    /// Slices of one `linear_extrude`.
    pub slices: Option<u64>,
    /// Elements of one list.
    pub list: Option<u64>,
    /// Bytes of one string.
    pub string: Option<u64>,
    /// Numbers one `rands()` call returns.
    pub rands: Option<u64>,
    /// Triangles (2D: vertices) of one geometry result.
    pub triangles: Option<u64>,
}

impl From<session::Limits> for ResourceLimits {
    fn from(l: session::Limits) -> Self {
        ResourceLimits {
            time_seconds: l.time,
            memory_bytes: l.memory,
            fragments: l.fragments,
            slices: l.slices,
            list: l.list,
            string: l.string,
            rands: l.rands,
            triangles: l.triangles,
        }
    }
}

impl ResourceLimits {
    /// The session's limits, or why these are not valid: a time that is
    /// not a positive, finite number of seconds would make every request
    /// fail at once (or never time out), which is a caller's bug.
    pub fn to_session(self) -> Result<session::Limits, CoreError> {
        if let Some(t) = self.time_seconds
            && !(t.is_finite() && t > 0.0)
        {
            return Err(CoreError::InvalidArgument {
                message: format!("time_seconds must be a positive number (got {t})"),
            });
        }
        Ok(session::Limits {
            time: self.time_seconds,
            memory: self.memory_bytes,
            fragments: self.fragments,
            slices: self.slices,
            list: self.list,
            string: self.string,
            rands: self.rands,
            triangles: self.triangles,
        })
    }
}

// --- From the session's JSON ---------------------------------------------

fn u32_of(v: &Value) -> Option<u32> {
    v.as_u64().and_then(|n| u32::try_from(n).ok())
}

fn span_of(v: &Value) -> Option<SourceSpan> {
    Some(SourceSpan {
        start_line: u32_of(&v["start"]["line"])?,
        start_column: u32_of(&v["start"]["column"])?,
        end_line: u32_of(&v["end"]["line"])?,
        end_column: u32_of(&v["end"]["column"])?,
    })
}

fn string_of(v: &Value) -> String {
    v.as_str().unwrap_or_default().to_string()
}

/// One diagnostic from its JSON (`session::diag::to_json`). `None` for a
/// severity that is not a diagnostic's (the list holds none).
fn diagnostic_of(v: &Value) -> Option<Diagnostic> {
    let severity = match v["severity"].as_str()? {
        "error" => Severity::Error,
        "warning" => Severity::Warning,
        "deprecated" => Severity::Deprecated,
        _ => return None,
    };
    let hints = v["hints"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|h| Hint {
            message: string_of(&h["message"]),
            replacement: span_of(&h["replace"]["span"]).map(|span| Replacement {
                span,
                text: string_of(&h["replace"]["text"]),
            }),
        })
        .collect();
    let trace = v["trace"]
        .as_array()
        .into_iter()
        .flatten()
        .map(string_of)
        .collect();
    Some(Diagnostic {
        code: string_of(&v["code"]),
        severity,
        message: string_of(&v["message"]),
        text: string_of(&v["text"]),
        file: v["file"].as_str().map(str::to_string),
        line: u32_of(&v["line"]),
        span: span_of(&v["span"]),
        hints,
        trace,
    })
}

/// A log's errors, warnings and deprecations, in order.
pub fn diagnostics(log: &session::Log) -> Vec<Diagnostic> {
    log.diagnostics_json()
        .iter()
        .filter_map(diagnostic_of)
        .collect()
}

/// A log's lines as the command line prints them.
pub fn console(log: &session::Log) -> String {
    String::from_utf8_lossy(&log.stderr).into_owned()
}

fn f64s(v: &Value) -> Vec<f64> {
    v.as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_f64)
        .collect()
}

/// Statistics from the `geometry` object (`session::stats::geometry`).
pub fn geometry_stats(g: &geom::Geometry, scheme: &geom::color::Scheme) -> GeometryStats {
    let v = session::stats::geometry(g, scheme);
    GeometryStats {
        dimensions: v["dimensions"].as_u64().unwrap_or(3) as u8,
        bbox_min: f64s(&v["bbox"]["min"]),
        bbox_max: f64s(&v["bbox"]["max"]),
        area: v["area"].as_f64().unwrap_or(0.0),
        volume: v["volume"].as_f64(),
        triangles: v["triangles"].as_u64(),
        vertices: v["vertices"].as_u64(),
        manifold: v["manifold"].as_bool(),
        components: v["components"].as_u64(),
        contours: v["contours"].as_u64(),
    }
}

/// What `render` (and a document run) report about a finished render.
pub fn render_result(r: &session::Rendered, scheme: &render::ColorScheme) -> RenderResult {
    RenderResult {
        exit_code: r.exit_code,
        diagnostics: diagnostics(&r.log),
        echo: r.log.echo(),
        console: console(&r.log),
        geometry: r
            .geometry
            .as_ref()
            .map(|g| geometry_stats(g, &scheme.geometry_scheme())),
        cache_entries: r.cache_entries as u64,
        timings: r.timings.into(),
    }
}
