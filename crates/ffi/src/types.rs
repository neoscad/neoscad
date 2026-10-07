//! The records and enums that cross the bridge.
//!
//! The host-neutral ones (diagnostics, geometry statistics, timings,
//! limits, the errors) live in `crates/client`, which the web worker
//! shares; they are declared to UniFFI here as remote types, field for
//! field, so Swift sees exactly the records it always did. A field added
//! there must be added here too (the declaration does not compile
//! otherwise). The records only this host has (its configuration and
//! snapshots, which need the GPU) are defined here.
//!
//! Most results are typed: diagnostics, geometry statistics, timings and
//! limits are small and fixed in shape, and typed records give Swift
//! exhaustive `switch`es and no parsing code of its own. The one exception
//! is the snapshot summary (`docs/cli-json.md`, "snapshot"): a large,
//! nested object that grows with each snapshot feature (views, parts,
//! issues, diffs) and that the app only shows or forwards. Mirroring it
//! would mean a dozen records to keep in step with the CLI's JSON for no
//! gain, so it crosses as the same JSON string the CLI prints.

pub use client::{
    CoreError, Diagnostic, DocInfo, Evaluation, ExportResult, GeometryStats, Hint, RenderMode,
    RenderResult, Replacement, ResourceLimits, Severity, SourceSpan, TextEdit, Timings,
};

/// Why a call failed. Every exported function returns this on failure, so
/// Swift sees a thrown error and never a trap (UniFFI generates
/// `try! rustCall` for functions that cannot fail, and a panic there would
/// end the app with the user's unsaved work).
#[uniffi::remote(Error)]
pub enum CoreError {
    /// A newer request on the same document (an edit, a newer render) or
    /// an explicit `cancel` stopped this one. Not a failure of the model:
    /// the app drops the stale result.
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

/// How the core is set up.
#[derive(Debug, Clone, Default, uniffi::Record)]
pub struct CoreConfig {
    /// Where the bundled libraries (MCAD) are mounted, as OpenSCAD's
    /// `<resources>/libraries`: the app passes its bundle's resource
    /// directory. Default: `/NeoSCAD.resources`, a path that exists only
    /// in memory.
    #[uniffi(default = None)]
    pub resource_dir: Option<String>,
    /// Enables [`crate::Core::debug_panic`], which the Swift tests use to
    /// check that a panic comes back as an error. The app leaves it off.
    #[uniffi(default = false)]
    pub test_hooks: bool,
}

/// An open document.
#[uniffi::remote(Record)]
pub struct DocInfo {
    /// The path as the session keys it (absolute, normalised).
    pub path: String,
    /// Bumped by every change; 0 for a document read from disk.
    pub version: u64,
    /// Bytes of text, when the session holds it.
    pub length: Option<u64>,
}

/// A replacement of `start..end` by `text`. Offsets are **UTF-8 byte**
/// offsets into the current text (the session's unit); an editor that
/// counts UTF-16 units converts first. Edits apply in order, each to the
/// result of the previous one.
#[uniffi::remote(Record)]
pub struct TextEdit {
    pub start: u64,
    pub end: u64,
    pub text: String,
}

/// A diagnostic's severity. Echo and trace lines are not diagnostics:
/// echo output is in `echo`, and trace lines in their error's `trace`.
#[uniffi::remote(Enum)]
pub enum Severity {
    Error,
    Warning,
    Deprecated,
    /// NeoSCAD's notes that need no action (a sketch's free degrees of
    /// freedom).
    Info,
}

/// A range of source text: 1-based lines, 1-based byte columns, the end
/// exclusive.
#[uniffi::remote(Record)]
pub struct SourceSpan {
    pub start_line: u32,
    pub start_column: u32,
    pub end_line: u32,
    pub end_column: u32,
}

/// Replacement text for a span, as a fix.
#[uniffi::remote(Record)]
pub struct Replacement {
    pub span: SourceSpan,
    pub text: String,
}

/// How to fix a diagnostic.
#[uniffi::remote(Record)]
pub struct Hint {
    pub message: String,
    pub replacement: Option<Replacement>,
}

/// One diagnostic (`docs/cli-json.md`, "Diagnostics").
#[uniffi::remote(Record)]
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
#[uniffi::remote(Record)]
pub struct Timings {
    pub parse_ms: f64,
    pub evaluate_ms: f64,
    pub geometry_ms: f64,
    pub total_ms: f64,
}

/// Statistics of a rendered geometry (`docs/cli-json.md`, `geometry`).
/// 3D results fill `volume`, `triangles`, `vertices`, `manifold` and
/// `components`; 2D results fill `contours`.
#[uniffi::remote(Record)]
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
#[uniffi::remote(Enum)]
pub enum RenderMode {
    /// The full geometry (`--render`).
    Render,
    /// Full geometry, a mesh converted to a solid (`--render=force`).
    Force,
    /// OpenSCAD's preview: the leaves and the CSG products (no statistics).
    Preview,
}

/// The result of `evaluate`.
#[uniffi::remote(Record)]
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
#[uniffi::remote(Record)]
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

/// What a snapshot draws.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct SnapshotOptions {
    /// The whole sheet, 64 to 8192 pixels each way.
    #[uniffi(default = 1024)]
    pub width: u32,
    #[uniffi(default = 1024)]
    pub height: u32,
    /// View names (iso, front, back, left, right, top, bottom); empty for
    /// the default set.
    #[uniffi(default = [])]
    pub views: Vec<String>,
    /// Label the bounding box's size.
    #[uniffi(default = false)]
    pub dims: bool,
    /// Draw OpenSCAD's preview instead of the rendered geometry.
    #[uniffi(default = false)]
    pub preview: bool,
}

/// The result of `snapshot`.
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct SnapshotResult {
    /// 0, or the exit code of a model that failed (then no PNG).
    pub exit_code: u8,
    pub png: Option<Vec<u8>>,
    /// The summary as `neoscad snapshot --format json` prints it
    /// (`docs/cli-json.md`, "snapshot"); see the module documentation
    /// for why this one result is JSON.
    pub summary_json: String,
    pub diagnostics: Vec<Diagnostic>,
    pub console: String,
}

/// The result of `export`.
#[uniffi::remote(Record)]
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
/// unlimited. The core starts with the agent defaults
/// ([`session::Limits::AGENT`]), because live editing runs half-typed
/// code: one keystroke can turn `$fn=10` into `$fn=100000`.
#[uniffi::remote(Record)]
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
    /// User modules (with the heap evaluator, user function calls too)
    /// in progress inside one another before evaluation stops with
    /// OpenSCAD's "Recursion detected" error. Unlike the others, `None`
    /// is the default (100,000), not unlimited: this limit cannot be
    /// turned off. 0 is an invalid argument.
    pub depth: Option<u64>,
    /// Unknowns of one constrained sketch (`--enable sketch`): two per
    /// point, one per circle.
    pub sketch_unknowns: Option<u64>,
    /// Geometry queries (`child_bounds()`, `child_measure()`; `--enable
    /// query`) in one evaluation, each a render of its child.
    pub queries: Option<u64>,
}

pub use client::{console, diagnostics, geometry_stats};
