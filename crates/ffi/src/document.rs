//! The document loop (`docs/audits/macos-prep.md`, step 8f): one run per
//! pause in typing, whose one evaluation feeds everything the window
//! shows.
//!
//! [`Core::run_document`] takes the document's text as the session holds
//! it at that moment, evaluates and builds it once (a preview or a
//! render, with the customizer's values as `-D` assignments after the
//! text), and from that one result
//!
//! - hands the diagnostics to the editor's language server, which
//!   publishes them as the editor's markers for the client's version with
//!   the same text (`lsp::Server::supply`): the evaluation's as soon as it
//!   ends, through the listener, before any geometry is built, and again
//!   with the geometry stage's warnings when it adds any. The server does
//!   not evaluate again;
//! - swaps the model into the viewport (skipped when a newer run has
//!   started, whose model would replace it anyway), moving the view to
//!   the `$vp*` the file assigned;
//! - returns the console's lines with editor positions (UTF-16, through
//!   `lang::source`) for click-to-jump, and the files the run read, which
//!   the app watches on disk.
//!
//! A newer run of the same document supersedes an older one at the
//! session (its evaluation or geometry stops at the next check), so a
//! burst of typing leaves one run going.
//!
//! The customizer's parameters ([`Core::parameters`]) come from the
//! document's own text (the main file's annotated top-level assignments),
//! and its parameter sets are OpenSCAD's JSON files beside the model
//! ([`Core::parameter_sets`], [`Core::save_parameter_set`]).
//!
//! The host-neutral half (the run's request, console positions, the
//! customizer and parameter set files) is `crates/client`'s, shared with
//! the web worker; its records are declared to UniFFI here as remote
//! types. This file adds the app's half: the language server's hand-over,
//! the viewport, the disk.

use std::sync::Arc;
use std::sync::atomic::Ordering;

pub use client::{
    ConsoleKind, ConsoleLine, DocumentRequest, Parameter, ParameterControl, ParameterGroup,
    ParameterOption, ParameterOverride, ParameterValue, SourceRange,
};

use crate::{
    CameraState, Core, CoreError, LanguageServer, RenderMode, RenderResult, Viewport, guarded,
};

/// A customizer value: what a control edits, and what the run passes as
/// `name = value` after the text.
#[uniffi::remote(Enum)]
pub enum ParameterValue {
    Bool { value: bool },
    Number { value: f64 },
    Text { value: String },
    Vector { value: Vec<f64> },
}

/// One customizer value to run with.
#[uniffi::remote(Record)]
pub struct ParameterOverride {
    pub name: String,
    pub value: ParameterValue,
}

/// One entry of a dropdown: the label shown and the value it stands for.
#[uniffi::remote(Record)]
pub struct ParameterOption {
    pub label: String,
    pub value: ParameterValue,
}

/// The control for a parameter, as OpenSCAD's customizer picks it
/// (`ParameterWidget::createParameterWidget`).
#[uniffi::remote(Enum)]
pub enum ParameterControl {
    Checkbox,
    /// A number with both a minimum and a maximum.
    Slider {
        min: f64,
        max: f64,
        step: Option<f64>,
    },
    /// A number without both bounds.
    SpinBox {
        min: Option<f64>,
        max: Option<f64>,
        step: Option<f64>,
    },
    Text {
        max_length: Option<u32>,
    },
    /// A vector of numbers, one field each.
    Vector {
        min: Option<f64>,
        max: Option<f64>,
        step: Option<f64>,
    },
    Dropdown {
        options: Vec<ParameterOption>,
    },
}

/// One customizer parameter.
#[uniffi::remote(Record)]
pub struct Parameter {
    pub name: String,
    pub description: String,
    pub control: ParameterControl,
    /// The value in the text.
    pub default_value: ParameterValue,
}

/// A group of the customizer (`/* [Name] */`), in the file's order.
/// Parameters of the "Global" group appear in every group, and those of
/// "Hidden" in none, as in OpenSCAD's customizer
/// (`ParameterWidget::getParameterGroups`).
#[uniffi::remote(Record)]
pub struct ParameterGroup {
    pub name: String,
    pub parameters: Vec<Parameter>,
}

/// What a document run does.
#[uniffi::remote(Record)]
pub struct DocumentRequest {
    pub mode: RenderMode,
    /// Customizer values, appended to the text as `-D` assignments are.
    #[uniffi(default = [])]
    pub overrides: Vec<ParameterOverride>,
    /// neoscad's `part()` extension (`--enable part`), the window's
    /// toggle: the check and measure panels name parts, and the view must
    /// accept the same text without warning that `part` is unknown.
    #[uniffi(default = false)]
    pub parts: bool,
    /// OpenSCAD's experimental features, as `--enable` names them
    /// (`textmetrics`, `object-function`, ...); off by default, as in
    /// OpenSCAD.
    #[uniffi(default = [])]
    pub enable: Vec<String>,
}

/// What a console line is.
#[uniffi::remote(Enum)]
pub enum ConsoleKind {
    Error,
    Warning,
    Deprecated,
    Echo,
    Trace,
    /// A line without a severity (the render's own notes).
    Info,
}

/// A span of a file in editor positions: 0-based lines and UTF-16
/// columns, the end exclusive.
#[uniffi::remote(Record)]
pub struct SourceRange {
    pub path: String,
    pub start_line: u32,
    pub start_character: u32,
    pub end_line: u32,
    pub end_character: u32,
}

/// One console line: the text OpenSCAD prints, and where it points.
#[uniffi::remote(Record)]
pub struct ConsoleLine {
    pub kind: ConsoleKind,
    pub text: String,
    pub location: Option<SourceRange>,
}

/// The result of [`Core::run_document`].
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct DocumentResult {
    pub render: RenderResult,
    /// Every line of the console, in order.
    pub console: Vec<ConsoleLine>,
    /// The files on disk the run read besides the document itself (its
    /// includes, used libraries, imports and fonts), sorted. Open
    /// documents are left out: their text comes from their buffers.
    pub files: Vec<String>,
    /// The editor's language-server notifications (the markers) to hand
    /// to its client now; empty when it has not sent this text yet (they
    /// go out when it does).
    pub language: Vec<String>,
    /// Whether the model was swapped into the viewport (false when a
    /// newer run started meanwhile, or there is no viewport).
    pub shown: bool,
    /// The view the file's `$vp*` moved the viewport to, when it did.
    pub file_view: Option<CameraState>,
}

/// Told what a document run has to show before it ends. Called on the
/// engine's thread.
#[uniffi::export(with_foreign)]
pub trait DocumentListener: Send + Sync {
    /// The editor's language-server notifications for the evaluation's
    /// diagnostics, sent once evaluation ends and before the geometry
    /// stage (which a large preview can spend seconds in).
    fn language(&self, messages: Vec<String>);
}

#[uniffi::export]
impl Core {
    /// Run a document once for everything its window shows (see the
    /// module documentation): evaluate and build it as `request` asks,
    /// publish its diagnostics through `language`, show the model in
    /// `viewport`, and report the console and the files it read. A newer
    /// run of the same document cancels this one.
    pub fn run_document(
        &self,
        path: String,
        request: DocumentRequest,
        viewport: Option<Arc<Viewport>>,
        language: Option<Arc<LanguageServer>>,
        listener: Option<Arc<dyn DocumentListener>>,
    ) -> Result<DocumentResult, CoreError> {
        guarded(|| {
            // The text the run reads is fixed now: the markers are
            // published for exactly this text, whatever edits arrive
            // meanwhile.
            let (mut run, doc, text) = self.client.document_run(&path, &request)?;
            let (scheme, generation) = match &viewport {
                Some(v) => {
                    let generation = v.requests.fetch_add(1, Ordering::SeqCst) + 1;
                    let inner = v.lock();
                    // The program sees the view it is shown in, as in
                    // OpenSCAD's GUI (`setRenderVariables`); no
                    // "Viewall and autocenter disabled" warning, which
                    // only the command line's `--viewall` earns.
                    let c = inner.camera();
                    run.camera = eval::Camera {
                        vpt: c.vpt(),
                        vpr: c.vpr(),
                        vpd: c.viewer_distance,
                        vpf: c.fov,
                        auto: false,
                        locked: false,
                    };
                    (inner.scheme().clone(), generation)
                }
                None => {
                    run.camera.auto = false;
                    (render::ColorScheme::cornfield(), 0)
                }
            };
            // The evaluation's diagnostics go out before the geometry is
            // built; the finished run's go out again only if the geometry
            // stage added to them.
            let published: Arc<std::sync::Mutex<Option<Vec<serde_json::Value>>>> = Arc::default();
            if let (Some(ls), Some(listener)) = (&language, &listener) {
                let (ls, listener, published) = (ls.clone(), listener.clone(), published.clone());
                let (doc, text) = (doc.clone(), text.clone());
                run.on_evaluated = Some(Arc::new(move |log: &session::Log| {
                    let diags = log.diagnostics_json();
                    let out =
                        ls.server
                            .supply(ls.core.session(), &doc, text.clone(), diags.clone());
                    *published
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(diags);
                    if !out.is_empty() {
                        listener.language(out);
                    }
                }));
            }
            let r = self.session().render(&run, request.mode.into(), &scheme)?;
            let language = match &language {
                Some(l) => {
                    let diags = r.log.diagnostics_json();
                    let early = published
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .take();
                    if early.as_ref() == Some(&diags) {
                        Vec::new()
                    } else {
                        l.server.supply(self.session(), &doc, text.clone(), diags)
                    }
                }
                None => Vec::new(),
            };
            let mut shown = false;
            let mut file_view = None;
            if let Some(v) = &viewport
                && v.requests.load(Ordering::SeqCst) == generation
            {
                let scene = client::run_scene(&r, &scheme, render::Previewer::OpenCsg)?;
                if let Some(scene) = scene {
                    let model = v.gpu.upload(&scene).map_err(|e| CoreError::Failed {
                        message: e.to_string(),
                    })?;
                    file_view = v.apply_file_view(&r, generation);
                    shown = v.lock().set_model(Arc::new(model), generation);
                }
            }
            let console = self.client.console_lines(&r.log, &doc, text);
            // Only files another program can change (see
            // `Client::run_files`), and only those on disk: not what
            // exists only in memory (the bundled MCAD).
            let files = self
                .client
                .run_files(&r, &doc)
                .filter(|f| f.is_file())
                .map(|f| f.to_string_lossy().into_owned())
                .collect();
            Ok(DocumentResult {
                render: crate::render_result(&r, &scheme),
                console,
                files,
                language,
                shown,
                file_view,
            })
        })
    }

    /// The customizer's groups for a document's current text (see
    /// [`ParameterGroup`]).
    pub fn parameters(&self, path: String) -> Result<Vec<ParameterGroup>, CoreError> {
        guarded(|| self.client.parameters(&path))
    }

    /// The names of the parameter sets in `json_path` (OpenSCAD's file
    /// beside the model, `model.json`), in the file's order. No file is no
    /// sets.
    pub fn parameter_sets(&self, json_path: String) -> Result<Vec<String>, CoreError> {
        guarded(|| self.client.parameter_sets(&json_path))
    }

    /// The document's parameters with the set `name` of `json_path`
    /// applied as OpenSCAD applies one (`-p file -P name`): values checked
    /// against each parameter's type and range, and parameters the set
    /// does not name back at their defaults. Returns every parameter's
    /// value.
    pub fn apply_parameter_set(
        &self,
        path: String,
        json_path: String,
        name: String,
    ) -> Result<Vec<ParameterOverride>, CoreError> {
        guarded(|| self.client.apply_parameter_set(&path, &json_path, &name))
    }

    /// Save the document's parameters, with `values` over their defaults,
    /// as the set `name` in `json_path`, keeping the file's other sets
    /// (a set of the same name is replaced in place). The file is what
    /// OpenSCAD writes and reads (`ParameterSets::writeFile`): every value
    /// a string, `fileFormatVersion` "1".
    pub fn save_parameter_set(
        &self,
        path: String,
        json_path: String,
        name: String,
        values: Vec<ParameterOverride>,
    ) -> Result<(), CoreError> {
        guarded(|| {
            let target = self.doc_path(&json_path)?;
            let out = self.client.parameter_set_file(
                &path,
                &json_path,
                &name,
                &values,
                target.is_file(),
            )?;
            std::fs::write(&target, out).map_err(|e| CoreError::Failed {
                message: format!(
                    "Cannot open Parameter Set '{}' for writing: {e}",
                    target.display()
                ),
            })
        })
    }

    /// How many requests are running on a document (the app's tests check
    /// that closing a window stops its work).
    pub fn running(&self, path: String) -> Result<u32, CoreError> {
        guarded(|| self.client.running(&path))
    }
}

impl Viewport {
    /// Move the view to the `$vp*` the run's file assigned, when they
    /// differ from what the last run applied (or none was applied yet).
    ///
    /// OpenSCAD's GUI applies them after every evaluation; here the view
    /// moves only when the file's values change, so live preview after
    /// each pause in typing does not throw away an orbit the user made
    /// meanwhile, while editing `$vpr` still turns the view. Returns the
    /// view it moved to.
    fn apply_file_view(&self, r: &session::Rendered, generation: u64) -> Option<CameraState> {
        let a = r.camera_assigned;
        if !a.any() {
            return None;
        }
        let c = &r.camera;
        let wanted = (
            a.vpt.then_some(c.vpt),
            a.vpr.then_some(c.vpr),
            a.vpd.then_some(c.vpd),
            a.vpf.then_some(c.vpf),
        );
        let mut last = self
            .file_view
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if last.as_ref().is_some_and(|(w, _)| *w == wanted) {
            return None;
        }
        *last = Some((wanted, generation));
        let mut v = self.lock();
        v.set_file_view(wanted.0, wanted.1, wanted.2, wanted.3);
        let cam = v.camera();
        Some(CameraState {
            vpt: cam.vpt().to_vec(),
            vpr: cam.vpr().to_vec(),
            vpd: cam.viewer_distance,
            vpf: cam.fov,
        })
    }
}

#[cfg(test)]
mod tests;
