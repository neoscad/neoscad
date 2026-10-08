//! The presentation tables, the customizer's edit rules and the editor's
//! UTF-16 edits, from `client` (`present.rs`, `text.rs`), for every host
//! that takes the core through UniFFI: the macOS app today, the Linux and
//! Windows apps next (`docs/audits/shared-core.md`, steps 1 to 3).
//!
//! Shapes kept to what both generators take (Swift's and
//! `uniffi-bindgen-cs`, which targets uniffi 0.31): free functions and
//! synchronous methods, records of plain fields, enums with named fields,
//! no `async` and no defaulted record fields.

use std::sync::{Arc, Mutex, PoisonError};

pub use client::{
    ConsoleGroup, ConsoleGroupInfo, Example, ExportFormatInfo, ExportKind, OverlayState,
    ParameterEdit, PreviewOutcome, PrinterPreset, PrinterSettings, Utf16Edit, ViewOverlay,
};

use crate::{
    BetweenResult, CheckFinding, CheckOptions, CheckReport, ConsoleKind, Core, CoreError, DocInfo,
    ExportResult, Parameter, ParameterValue, PartStats, RenderMode, RenderResult, SectionAxis,
    SectionResult, SolidStats, TextEdit, Timings, ViewLine, ViewMarker, Viewport, guarded,
};

/// The console's filters.
#[uniffi::remote(Enum)]
pub enum ConsoleGroup {
    Errors,
    Warnings,
    Echo,
    Other,
}

/// One console filter as a host lists it.
#[uniffi::remote(Record)]
pub struct ConsoleGroupInfo {
    pub group: ConsoleGroup,
    pub id: String,
    pub title: String,
    pub kinds: Vec<ConsoleKind>,
}

/// What an export writes.
#[uniffi::remote(Enum)]
pub enum ExportKind {
    Geometry,
    ViewImage,
    Snapshot,
}

/// One entry of File > Export.
#[uniffi::remote(Record)]
pub struct ExportFormatInfo {
    pub id: String,
    pub title: String,
    pub extension: String,
    pub dimension: Option<u32>,
    pub kind: ExportKind,
}

/// A printer's numbers for `check`.
#[uniffi::remote(Record)]
pub struct PrinterPreset {
    pub id: String,
    pub name: String,
    pub nozzle: f64,
    pub bed: Vec<f64>,
}

/// The check panel's settings as a host edits and stores them.
#[uniffi::remote(Record)]
pub struct PrinterSettings {
    pub preset: String,
    pub nozzle: f64,
    pub min_wall: f64,
    pub max_overhang: f64,
    pub use_bed: bool,
    pub bed: Vec<f64>,
}

/// What a customizer control did ([`client::edit_parameter`]).
#[uniffi::remote(Enum)]
pub enum ParameterEdit {
    Set { value: ParameterValue },
    Slide { value: f64 },
    Type { value: f64 },
    Step { up: bool },
    Item { index: u32, value: f64 },
}

/// A replacement in an editor's UTF-16 offsets.
#[uniffi::remote(Record)]
pub struct Utf16Edit {
    pub from: u64,
    pub to: u64,
    pub insert: String,
}

/// One example (File > Examples).
#[uniffi::remote(Record)]
pub struct Example {
    pub id: String,
    pub title: String,
    pub file_name: String,
    pub origin: String,
    pub license: String,
    pub note: String,
    pub libraries: Vec<String>,
    pub parts: bool,
    pub autorun: bool,
}

/// The examples, in menu order: the web demo's list, embedded in the core.
#[uniffi::export]
pub fn examples() -> Result<Vec<Example>, CoreError> {
    guarded(|| Ok(client::examples()))
}

/// An example's text.
#[uniffi::export]
pub fn example_source(id: String) -> Result<String, CoreError> {
    guarded(|| client::example_source(&id))
}

/// How long typing must pause before a document runs again, unless the
/// host picks its own ([`client::DEFAULT_PREVIEW_DELAY_MS`]).
#[uniffi::export]
pub fn default_preview_delay_ms() -> Result<u64, CoreError> {
    guarded(|| Ok(client::DEFAULT_PREVIEW_DELAY_MS))
}

/// `%g`, as the customizer's number fields show numbers.
#[uniffi::export]
pub fn format_number(value: f64) -> Result<String, CoreError> {
    guarded(|| Ok(client::format_number(value)))
}

/// The console's one-line summary of a finished run.
#[uniffi::export]
pub fn describe_render(result: RenderResult, mode: RenderMode) -> Result<String, CoreError> {
    guarded(|| Ok(client::describe_render(&result, mode)))
}

/// The stages' times, for the summary's tooltip.
#[uniffi::export]
pub fn describe_timings(timings: Timings) -> Result<String, CoreError> {
    guarded(|| Ok(client::describe_timings(&timings)))
}

#[uniffi::export]
pub fn console_group(kind: ConsoleKind) -> Result<ConsoleGroup, CoreError> {
    guarded(|| Ok(client::console_group(kind)))
}

#[uniffi::export]
pub fn console_groups() -> Result<Vec<ConsoleGroupInfo>, CoreError> {
    guarded(|| Ok(client::console_groups()))
}

#[uniffi::export]
pub fn export_formats() -> Result<Vec<ExportFormatInfo>, CoreError> {
    guarded(|| Ok(client::export_formats()))
}

/// File > Export's formats for documents run with `enable` (the app's
/// `--enable` names): STEP only with `exact`.
#[uniffi::export]
pub fn export_formats_with(enable: Vec<String>) -> Result<Vec<ExportFormatInfo>, CoreError> {
    guarded(|| Ok(client::export_formats_with(&enable)))
}

/// Why a file export failed, as an alert says it; `None` when it did not.
#[uniffi::export]
pub fn export_failure_reason(result: ExportResult) -> Result<Option<String>, CoreError> {
    guarded(|| Ok(client::export_failure_reason(&result)))
}

/// The format File > Export starts on.
#[uniffi::export]
pub fn suggest_export_format(
    preferred: String,
    last_dimensions: Option<u32>,
) -> Result<String, CoreError> {
    guarded(|| Ok(client::suggest_export_format(&preferred, last_dimensions)))
}

#[uniffi::export]
pub fn printer_presets() -> Result<Vec<PrinterPreset>, CoreError> {
    guarded(|| Ok(client::printer_presets()))
}

#[uniffi::export]
pub fn default_printer_settings() -> Result<PrinterSettings, CoreError> {
    guarded(|| Ok(PrinterSettings::default()))
}

/// `settings` with a preset's nozzle and bed (walls two perimeters).
#[uniffi::export]
pub fn apply_printer_preset(
    settings: PrinterSettings,
    id: String,
) -> Result<PrinterSettings, CoreError> {
    guarded(|| Ok(settings.apply_preset(&id)))
}

/// Stored settings with each out-of-bounds value back at its default.
#[uniffi::export]
pub fn validated_printer_settings(settings: PrinterSettings) -> Result<PrinterSettings, CoreError> {
    guarded(|| Ok(settings.validated()))
}

#[uniffi::export]
pub fn printer_check_options(settings: PrinterSettings) -> Result<CheckOptions, CoreError> {
    guarded(|| Ok(settings.check_options()))
}

#[uniffi::export]
pub fn check_summary(report: CheckReport) -> Result<String, CoreError> {
    guarded(|| Ok(client::check_summary(&report)))
}

/// The first error line of a console.
#[uniffi::export]
pub fn first_error(console: String) -> Result<Option<String>, CoreError> {
    guarded(|| Ok(client::first_error(&console)))
}

/// The override a customizer edit makes (`None`: the text's own value).
#[uniffi::export]
pub fn edit_parameter(
    parameter: Parameter,
    current: ParameterValue,
    edit: ParameterEdit,
) -> Result<Option<ParameterValue>, CoreError> {
    guarded(|| Ok(client::edit_parameter(&parameter, &current, edit)))
}

/// `name.json` beside `name.scad`.
#[uniffi::export]
pub fn parameter_set_path(doc_path: String) -> Result<String, CoreError> {
    guarded(|| Ok(client::parameter_set_path(&doc_path)))
}

/// The colour schemes a viewport can draw in.
#[uniffi::export]
pub fn color_scheme_names() -> Result<Vec<String>, CoreError> {
    guarded(|| Ok(client::color_scheme_names()))
}

#[uniffi::export]
impl Core {
    /// Apply an editor's edits (UTF-16 offsets) to a document's buffer.
    /// With `utf16_length`, a text that no longer agrees is refused (send
    /// the whole text with `update` then).
    pub fn edit_utf16(
        &self,
        path: String,
        edits: Vec<Utf16Edit>,
        utf16_length: Option<u64>,
    ) -> Result<DocInfo, CoreError> {
        guarded(|| self.client.edit_utf16(&path, &edits, utf16_length))
    }

    /// A path for an untitled document in `dir` that no other untitled
    /// document (`taken`) and no file uses.
    pub fn untitled_path(
        &self,
        dir: String,
        base: String,
        taken: Vec<String>,
    ) -> Result<String, CoreError> {
        guarded(|| self.client.untitled_path(&dir, &base, &taken))
    }
}

/// A host's own copy of a document's text, edited in the editor's UTF-16
/// offsets ([`client::EditorText`]): what the host saves, with the edits
/// it takes handed back in bytes for `Core::edit`.
#[derive(Debug, uniffi::Object)]
pub struct EditorText {
    inner: Mutex<client::EditorText>,
}

impl EditorText {
    pub(crate) fn lock(&self) -> std::sync::MutexGuard<'_, client::EditorText> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

#[uniffi::export]
impl EditorText {
    /// Not a `Result`, unlike every other export: it only copies the text
    /// and counts its UTF-16 units, which cannot panic short of running
    /// out of memory (an abort that `guarded` could not catch either), and
    /// a throwing constructor would make each host wrap a copy of its text
    /// in error handling that never runs.
    #[uniffi::constructor]
    pub fn new(text: String) -> Arc<EditorText> {
        Arc::new(EditorText {
            inner: Mutex::new(client::EditorText::new(text)),
        })
    }

    pub fn replace(&self, text: String) -> Result<(), CoreError> {
        guarded(|| {
            self.lock().replace(text);
            Ok(())
        })
    }

    /// Apply the editor's edits; the same edits in UTF-8 byte offsets. An
    /// error means the copies disagree (the text keeps the edits before
    /// the failing one); replace it with the editor's text then.
    pub fn apply(&self, edits: Vec<Utf16Edit>) -> Result<Vec<TextEdit>, CoreError> {
        guarded(|| self.lock().apply(&edits))
    }

    pub fn text(&self) -> Result<String, CoreError> {
        guarded(|| Ok(self.lock().text()))
    }

    pub fn utf16_length(&self) -> Result<u64, CoreError> {
        guarded(|| Ok(self.lock().utf16_length()))
    }

    pub fn byte_length(&self) -> Result<u64, CoreError> {
        guarded(|| Ok(self.lock().byte_length()))
    }
}

// --- The view's overlay (step 4) --------------------------------------------

/// What the check and measure panels draw over the model.
#[uniffi::remote(Record)]
pub struct ViewOverlay {
    pub lines: Vec<ViewLine>,
    pub markers: Vec<ViewMarker>,
}

/// The panels' state the overlay is drawn from.
#[uniffi::remote(Record)]
pub struct OverlayState {
    pub findings: Vec<CheckFinding>,
    pub selected: Option<u32>,
    pub section: Option<SectionResult>,
    pub between: Option<BetweenResult>,
    pub picks: Vec<Vec<f64>>,
}

/// The lines and markers for the panels' state, for a host that draws
/// them itself (the web demo's canvas); a `Viewport` takes the state
/// directly with `set_overlay`.
#[uniffi::export]
pub fn view_overlay(state: OverlayState) -> Result<ViewOverlay, CoreError> {
    guarded(|| Ok(client::view_overlay(&state)))
}

/// The distance between two picked points.
#[uniffi::export]
pub fn pick_distance(picks: Vec<Vec<f64>>) -> Result<Option<f64>, CoreError> {
    guarded(|| Ok(client::pick_distance(&picks)))
}

/// The section slider's range `[lo, hi]` along `axis` for `part` (the
/// whole model without one).
#[uniffi::export]
pub fn section_range(
    model: Option<SolidStats>,
    parts: Vec<PartStats>,
    axis: SectionAxis,
    part: Option<String>,
) -> Result<Vec<f64>, CoreError> {
    guarded(|| Ok(client::section_range(model.as_ref(), &parts, axis, part.as_deref()).to_vec()))
}

#[uniffi::export]
impl Viewport {
    /// Draw the panels' state over the model from now on (findings with
    /// the selected one's box, the section, the distance, the picks),
    /// replacing what was drawn before.
    pub fn set_overlay(&self, state: OverlayState) -> Result<(), CoreError> {
        let o = client::view_overlay(&state);
        self.set_annotations(o.lines, o.markers)
    }
}

// --- The file-manager preview (step 6) --------------------------------------

/// What a Quick Look preview (or a thumbnailer) shows.
#[uniffi::remote(Record)]
pub struct PreviewOutcome {
    pub png: Option<Vec<u8>>,
    pub source: String,
    pub notes: Vec<String>,
    pub timed_out: bool,
    pub unreadable: Vec<String>,
}

/// The tight limits a preview renders under (5 s, 512 MiB; the rest the
/// agent defaults).
#[uniffi::export]
pub fn preview_limits() -> Result<crate::ResourceLimits, CoreError> {
    guarded(|| Ok(client::preview_limits(session::Limits::AGENT.into())))
}

/// Files larger than this many bytes are previewed as text only.
#[uniffi::export]
pub fn preview_max_source_bytes() -> Result<u64, CoreError> {
    guarded(|| Ok(client::PREVIEW_MAX_SOURCE_BYTES))
}

/// A preview that stops at a note (the host could not read the file).
#[uniffi::export]
pub fn preview_failed(source: String, note: String) -> Result<PreviewOutcome, CoreError> {
    guarded(|| Ok(client::preview_failed(source, note)))
}

/// The host's watchdog fired first.
#[uniffi::export]
pub fn preview_timed_out(
    source: String,
    deadline_seconds: u32,
) -> Result<PreviewOutcome, CoreError> {
    guarded(|| Ok(client::preview_timed_out(source, deadline_seconds)))
}

/// The preview page: picture (the `cid:` attachment `preview_image_id`),
/// notes, then the source.
#[uniffi::export]
pub fn preview_html(outcome: PreviewOutcome, title: String) -> Result<String, CoreError> {
    guarded(|| Ok(client::preview_html(&outcome, &title)))
}

#[uniffi::export]
pub fn preview_image_id() -> Result<String, CoreError> {
    guarded(|| Ok(client::PREVIEW_IMAGE_ID.into()))
}

#[uniffi::export]
impl Core {
    /// Preview `text` as the document at `path`: open it, draw it, and say
    /// what went wrong. Never an error for a model or file problem (those
    /// become notes); a host's watchdog calls `cancel(path)` and
    /// `preview_timed_out` if this takes too long. Run it on a core made
    /// for the request, under `preview_limits`.
    pub fn preview_picture(
        &self,
        path: String,
        text: String,
        options: crate::PictureOptions,
    ) -> Result<PreviewOutcome, CoreError> {
        guarded(|| {
            if text.len() as u64 > client::PREVIEW_MAX_SOURCE_BYTES {
                return Ok(client::preview_too_large(text));
            }
            if let Err(e) = self.client.open(&path, Some(text.clone())) {
                return Ok(client::preview_failed(text, e.user_message()));
            }
            match self.picture(path.clone(), options) {
                Ok(p) => {
                    let dir = std::path::Path::new(&path)
                        .parent()
                        .map(|d| d.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    Ok(client::preview_outcome(
                        text,
                        p.png,
                        p.exit_code,
                        p.empty,
                        &p.diagnostics,
                        &p.console,
                        &dir,
                    ))
                }
                Err(e) => Ok(client::preview_failed(text, e.user_message())),
            }
        })
    }
}
