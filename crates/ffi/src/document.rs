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

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::Ordering;

use lang::customizer::Parameters;
use lang::customizer::params::{EnumValue, ParamKind};
use lang::source::SourceFile;

use crate::{
    CameraState, Core, CoreError, LanguageServer, RenderMode, RenderResult, Viewport, guarded,
};

/// A customizer value: what a control edits, and what the run passes as
/// `name = value` after the text.
#[derive(Debug, Clone, PartialEq, uniffi::Enum)]
pub enum ParameterValue {
    Bool { value: bool },
    Number { value: f64 },
    Text { value: String },
    Vector { value: Vec<f64> },
}

/// One customizer value to run with.
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct ParameterOverride {
    pub name: String,
    pub value: ParameterValue,
}

/// One entry of a dropdown: the label shown and the value it stands for.
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct ParameterOption {
    pub label: String,
    pub value: ParameterValue,
}

/// The control for a parameter, as OpenSCAD's customizer picks it
/// (`ParameterWidget::createParameterWidget`).
#[derive(Debug, Clone, PartialEq, uniffi::Enum)]
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
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
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
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct ParameterGroup {
    pub name: String,
    pub parameters: Vec<Parameter>,
}

/// What a document run does.
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
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
}

/// What a console line is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
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
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct SourceRange {
    pub path: String,
    pub start_line: u32,
    pub start_character: u32,
    pub end_line: u32,
    pub end_character: u32,
}

/// One console line: the text OpenSCAD prints, and where it points.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
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

/// A value as OpenSCAD source (a literal).
fn literal(v: &ParameterValue) -> Option<String> {
    let number = |x: f64| x.is_finite().then(|| format!("{x}"));
    Some(match v {
        ParameterValue::Bool { value } => value.to_string(),
        ParameterValue::Number { value } => number(*value)?,
        ParameterValue::Text { value } => {
            let mut s = String::with_capacity(value.len() + 2);
            s.push('"');
            for c in value.chars() {
                match c {
                    '"' => s.push_str("\\\""),
                    '\\' => s.push_str("\\\\"),
                    '\n' => s.push_str("\\n"),
                    '\r' => s.push_str("\\r"),
                    '\t' => s.push_str("\\t"),
                    c => s.push(c),
                }
            }
            s.push('"');
            s
        }
        ParameterValue::Vector { value } => {
            let items: Option<Vec<String>> = value.iter().map(|&x| number(x)).collect();
            format!("[{}]", items?.join(", "))
        }
    })
}

/// A parameter name as an identifier the parser takes back: anything
/// else would put arbitrary text after the model.
fn identifier(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|c| c == '$' || c == '_' || c.is_alphabetic())
        && chars.all(|c| c == '_' || c.is_alphanumeric())
}

/// The `-D` assignment of an override.
pub(crate) fn define(o: &ParameterOverride) -> Option<String> {
    identifier(&o.name).then_some(())?;
    Some(format!("{}={}", o.name, literal(&o.value)?))
}

fn enum_value(v: &EnumValue) -> ParameterValue {
    match v {
        EnumValue::Number(x) => ParameterValue::Number { value: *x },
        EnumValue::String(s) => ParameterValue::Text {
            value: String::from_utf8_lossy(s).into_owned(),
        },
    }
}

fn parameter(p: &lang::customizer::params::Parameter) -> Parameter {
    let text = |b: &[u8]| String::from_utf8_lossy(b).into_owned();
    let (control, default_value) = match &p.kind {
        ParamKind::Bool { default, .. } => (
            ParameterControl::Checkbox,
            ParameterValue::Bool { value: *default },
        ),
        ParamKind::String {
            default, max_len, ..
        } => (
            ParameterControl::Text {
                max_length: max_len.map(|m| u32::try_from(m).unwrap_or(u32::MAX)),
            },
            ParameterValue::Text {
                value: text(default),
            },
        ),
        ParamKind::Number {
            default,
            min,
            max,
            step,
            ..
        } => (
            match (min, max) {
                (Some(min), Some(max)) => ParameterControl::Slider {
                    min: *min,
                    max: *max,
                    step: *step,
                },
                _ => ParameterControl::SpinBox {
                    min: *min,
                    max: *max,
                    step: *step,
                },
            },
            ParameterValue::Number { value: *default },
        ),
        ParamKind::Vector {
            default,
            min,
            max,
            step,
            ..
        } => (
            ParameterControl::Vector {
                min: *min,
                max: *max,
                step: *step,
            },
            ParameterValue::Vector {
                value: default.clone(),
            },
        ),
        ParamKind::Enum { default, items, .. } => (
            ParameterControl::Dropdown {
                options: items
                    .iter()
                    .map(|i| ParameterOption {
                        label: i.key.clone(),
                        value: enum_value(&i.value),
                    })
                    .collect(),
            },
            items
                .get(*default)
                .map(|i| enum_value(&i.value))
                .unwrap_or(ParameterValue::Number { value: 0.0 }),
        ),
    };
    Parameter {
        name: p.name.clone(),
        description: p.description.clone(),
        control,
        default_value,
    }
}

/// A parameter's current value, as a set file stores it: a string, as
/// Boost's property tree writes every value (`ParameterSets::writeFile`):
/// numbers with 16 significant digits, vectors as OpenSCAD's stream
/// prints them (6), text as it is.
fn set_value(p: &lang::customizer::params::Parameter) -> String {
    let g16 = |x: f64| g(x, 16);
    let g6 = |x: f64| g(x, 6);
    match &p.kind {
        ParamKind::Bool { value, .. } => value.to_string(),
        ParamKind::String { value, .. } => String::from_utf8_lossy(value).into_owned(),
        ParamKind::Number { value, .. } => g16(*value),
        ParamKind::Vector { value, .. } => format!(
            "[{}]",
            value.iter().map(|&x| g6(x)).collect::<Vec<_>>().join(", ")
        ),
        ParamKind::Enum { index, items, .. } => match items.get(*index).map(|i| &i.value) {
            Some(EnumValue::Number(x)) => g16(*x),
            Some(EnumValue::String(s)) => String::from_utf8_lossy(s).into_owned(),
            None => String::new(),
        },
    }
}

/// A parameter's value as the customizer edits it.
fn current(p: &lang::customizer::params::Parameter) -> ParameterValue {
    match &p.kind {
        ParamKind::Bool { value, .. } => ParameterValue::Bool { value: *value },
        ParamKind::String { value, .. } => ParameterValue::Text {
            value: String::from_utf8_lossy(value).into_owned(),
        },
        ParamKind::Number { value, .. } => ParameterValue::Number { value: *value },
        ParamKind::Vector { value, .. } => ParameterValue::Vector {
            value: value.clone(),
        },
        ParamKind::Enum { index, items, .. } => items
            .get(*index)
            .map(|i| enum_value(&i.value))
            .unwrap_or(ParameterValue::Number { value: 0.0 }),
    }
}

/// C++ `ostream << double` at `precision` significant digits (`%g`):
/// how Boost's property tree (16 digits) and OpenSCAD's vector export (the
/// stream's default 6) write numbers into parameter set files.
fn g(v: f64, precision: usize) -> String {
    if !v.is_finite() {
        return if v.is_nan() {
            "nan".into()
        } else if v < 0.0 {
            "-inf".into()
        } else {
            "inf".into()
        };
    }
    if v == 0.0 {
        return if v.is_sign_negative() { "-0" } else { "0" }.into();
    }
    let p = precision.max(1);
    let e = format!("{v:.*e}", p - 1);
    let (mant, exp) = e.split_once('e').unwrap_or((&e, "0"));
    let x: i32 = exp.parse().unwrap_or(0);
    let trim = |s: &str| -> String {
        if s.contains('.') {
            s.trim_end_matches('0').trim_end_matches('.').to_string()
        } else {
            s.to_string()
        }
    };
    if x >= -4 && x < p as i32 {
        let decimals = (p as i32 - 1 - x).max(0) as usize;
        trim(&format!("{v:.decimals$}"))
    } else {
        let sign = if x < 0 { '-' } else { '+' };
        format!("{}e{sign}{:02}", trim(mant), x.abs())
    }
}

/// JSON string contents with the escapes JSON needs.
fn json_string(s: &str) -> String {
    serde_json::Value::String(s.to_string()).to_string()
}

impl Core {
    /// A document's text as the session reads it now: its buffer, or the
    /// file.
    fn text_now(&self, doc: &Path) -> Result<Arc<[u8]>, CoreError> {
        if let Some(t) = self.session.buffer_text(doc) {
            return Ok(t);
        }
        self.session
            .fs()
            .read(doc)
            .map(Arc::from)
            .map_err(|e| CoreError::Failed {
                message: format!("cannot read '{}': {e}", doc.display()),
            })
    }

    /// The customizer parameters of `doc`'s text, with the values
    /// `overrides` give (validated and clamped as a parameter set's are).
    fn customizer(
        &self,
        doc: &Path,
        overrides: &[(String, String)],
    ) -> Result<Parameters, CoreError> {
        let text = self.text_now(doc)?;
        let program = lang::parse_file_annotated(doc.to_path_buf(), text.to_vec());
        let mut params = Parameters::from_ast(&program.ast, &mut Vec::new());
        if !overrides.is_empty() {
            let set = lang::customizer::ParameterSet {
                name: String::new(),
                values: overrides
                    .iter()
                    .map(|(k, v)| {
                        (
                            k.clone(),
                            lang::customizer::json::JsonNode {
                                data: v.clone(),
                                children: Vec::new(),
                            },
                        )
                    })
                    .collect(),
            };
            params.import(&set);
        }
        Ok(params)
    }
}

/// An override as a parameter set file's string.
fn override_string(v: &ParameterValue) -> String {
    match v {
        ParameterValue::Bool { value } => value.to_string(),
        ParameterValue::Number { value } => g(*value, 16),
        ParameterValue::Text { value } => value.clone(),
        ParameterValue::Vector { value } => format!(
            "[{}]",
            value
                .iter()
                .map(|&x| g(x, 16))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

/// Editor positions of the session's located lines, reading each file
/// once: the document from the text the run read, others through the
/// session.
struct Positions<'a> {
    core: &'a Core,
    doc: PathBuf,
    text: Arc<[u8]>,
    files: HashMap<PathBuf, Option<SourceFile>>,
}

impl Positions<'_> {
    fn range(&mut self, l: &eval::Location) -> Option<SourceRange> {
        let path = session::normal(&l.file);
        let src = self
            .files
            .entry(path.clone())
            .or_insert_with(|| {
                let text = if path == self.doc {
                    self.text.to_vec()
                } else {
                    self.core.session.fs().read(&path).ok()?
                };
                Some(SourceFile::new(path.clone(), text))
            })
            .as_ref()?;
        let at = |(line, col): (u32, u32)| {
            src.utf16_position(src.line_start(line) + col.saturating_sub(1))
        };
        let (start_line, start_character) = at(l.start);
        let (end_line, end_character) = at(l.end);
        Some(SourceRange {
            path: path.to_string_lossy().into_owned(),
            start_line,
            start_character,
            end_line,
            end_character,
        })
    }
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
            let doc = self.doc_path(&path)?;
            let mut run = self.run(&path)?;
            // The text the run reads, fixed now: the markers are published
            // for exactly this text, whatever edits arrive meanwhile.
            let text = self.text_now(&doc)?;
            run.text = Some(text.clone());
            run.defines = request.overrides.iter().filter_map(define).collect();
            run.parts = request.parts;
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
                    let out = ls
                        .server
                        .supply(&ls.core.session, &doc, text.clone(), diags.clone());
                    *published
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(diags);
                    if !out.is_empty() {
                        listener.language(out);
                    }
                }));
            }
            let r = self.session.render(&run, request.mode.into(), &scheme)?;
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
                        l.server.supply(&self.session, &doc, text.clone(), diags)
                    }
                }
                None => Vec::new(),
            };
            let mut shown = false;
            let mut file_view = None;
            if let Some(v) = &viewport
                && v.requests.load(Ordering::SeqCst) == generation
            {
                let scene = match (&r.tree, &r.geometry) {
                    (Some(tree), _) => Some(render::preview::scene(
                        tree,
                        &scheme,
                        render::Previewer::OpenCsg,
                    )),
                    (None, Some(g)) => Some(render::Scene::new(Some(g), &scheme)),
                    (None, None) if r.exit_code == 0 => Some(render::Scene::new(None, &scheme)),
                    (None, None) => None,
                };
                if let Some(scene) = scene {
                    let model = v.gpu.upload(&scene).map_err(|e| CoreError::Failed {
                        message: e.to_string(),
                    })?;
                    file_view = v.apply_file_view(&r, generation);
                    shown = v.lock().set_model(Arc::new(model), generation);
                }
            }
            let mut positions = Positions {
                core: self,
                doc: doc.clone(),
                text,
                files: HashMap::new(),
            };
            let console = r
                .log
                .lines
                .iter()
                .map(|l| ConsoleLine {
                    kind: match l.severity {
                        Some(lang::diag::Severity::Error) => ConsoleKind::Error,
                        Some(lang::diag::Severity::Warning) => ConsoleKind::Warning,
                        Some(lang::diag::Severity::Deprecated) => ConsoleKind::Deprecated,
                        Some(lang::diag::Severity::Echo) => ConsoleKind::Echo,
                        Some(lang::diag::Severity::Trace) => ConsoleKind::Trace,
                        None => ConsoleKind::Info,
                    },
                    text: l.text.clone(),
                    location: l.location.as_ref().and_then(|at| positions.range(at)),
                })
                .collect();
            // Only files another program can change: not the document
            // (the app writes it itself, and a save must not re-run it),
            // not other open documents (their buffers are what runs read),
            // and not what exists only in memory (the bundled MCAD).
            let files = r
                .files
                .iter()
                .filter(|f| **f != doc && self.session.buffer_text(f).is_none() && f.is_file())
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
        guarded(|| {
            let doc = self.doc_path(&path)?;
            let params = self.customizer(&doc, &[])?;
            let mut groups: Vec<ParameterGroup> = Vec::new();
            let mut global = Vec::new();
            for p in &params.params {
                match p.group.as_str() {
                    "Hidden" => {}
                    "Global" => global.push(parameter(p)),
                    g => match groups.iter_mut().find(|x| x.name == g) {
                        Some(x) => x.parameters.push(parameter(p)),
                        None => groups.push(ParameterGroup {
                            name: g.to_string(),
                            parameters: vec![parameter(p)],
                        }),
                    },
                }
            }
            if groups.is_empty() {
                if !global.is_empty() {
                    groups.push(ParameterGroup {
                        name: "Global".into(),
                        parameters: global,
                    });
                }
            } else {
                for g in &mut groups {
                    g.parameters.extend(global.iter().cloned());
                }
            }
            Ok(groups)
        })
    }

    /// The names of the parameter sets in `json_path` (OpenSCAD's file
    /// beside the model, `model.json`), in the file's order. No file is no
    /// sets.
    pub fn parameter_sets(&self, json_path: String) -> Result<Vec<String>, CoreError> {
        guarded(|| {
            let p = self.doc_path(&json_path)?;
            if !self.session.fs().exists(&p) {
                return Ok(Vec::new());
            }
            let sets = lang::customizer::read_parameter_sets(&*self.session.fs(), &p)
                .map_err(|d| CoreError::Failed { message: d.message })?;
            Ok(sets.into_iter().map(|s| s.name).collect())
        })
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
        guarded(|| {
            let doc = self.doc_path(&path)?;
            let p = self.doc_path(&json_path)?;
            let sets = lang::customizer::read_parameter_sets(&*self.session.fs(), &p)
                .map_err(|d| CoreError::Failed { message: d.message })?;
            let set =
                sets.iter()
                    .find(|s| s.name == name)
                    .ok_or_else(|| CoreError::InvalidArgument {
                        message: format!("no parameter set '{name}' in '{json_path}'"),
                    })?;
            let mut params = self.customizer(&doc, &[])?;
            params.import(set);
            Ok(params
                .params
                .iter()
                .map(|p| ParameterOverride {
                    name: p.name.clone(),
                    value: current(p),
                })
                .collect())
        })
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
            let doc = self.doc_path(&path)?;
            let target = self.doc_path(&json_path)?;
            let overrides: Vec<(String, String)> = values
                .iter()
                .map(|o| (o.name.clone(), override_string(&o.value)))
                .collect();
            // Unnamed parameters keep their defaults: import resets them.
            let params = self.customizer(&doc, &overrides)?;
            let mine: Vec<(String, String)> = params
                .params
                .iter()
                .map(|p| (p.name.clone(), set_value(p)))
                .collect();
            let mut sets: Vec<(String, Vec<(String, String)>)> = if target.is_file() {
                lang::customizer::read_parameter_sets(&*self.session.fs(), &target)
                    .map_err(|d| CoreError::Failed { message: d.message })?
                    .into_iter()
                    .map(|s| {
                        let values = s.values.into_iter().map(|(k, v)| (k, v.data)).collect();
                        (s.name, values)
                    })
                    .collect()
            } else {
                Vec::new()
            };
            match sets.iter_mut().find(|(n, _)| *n == name) {
                Some(s) => s.1 = mine,
                None => sets.push((name, mine)),
            }
            // Boost's `write_json` layout: four spaces, `"key": "value"`.
            let mut out =
                String::from("{\n    \"fileFormatVersion\": \"1\",\n    \"parameterSets\": {\n");
            for (i, (set, values)) in sets.iter().enumerate() {
                out.push_str(&format!("        {}: {{\n", json_string(set)));
                for (j, (k, v)) in values.iter().enumerate() {
                    let comma = if j + 1 < values.len() { "," } else { "" };
                    out.push_str(&format!(
                        "            {}: {}{comma}\n",
                        json_string(k),
                        json_string(v)
                    ));
                }
                let comma = if i + 1 < sets.len() { "," } else { "" };
                out.push_str(&format!("        }}{comma}\n"));
            }
            out.push_str("    }\n}\n");
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
        guarded(|| {
            let doc = self.doc_path(&path)?;
            Ok(u32::try_from(self.session.running(&doc)).unwrap_or(u32::MAX))
        })
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
