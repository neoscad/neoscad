//! The document loop's host-neutral half: one run per pause in typing,
//! whose one evaluation feeds everything a window shows.
//!
//! [`Client::document_run`] fixes the text the run reads (so the editor's
//! markers are published for exactly that text), with the customizer's
//! values as `-D` assignments after it; the host runs it and shows the
//! model (the app's viewport, the web page's viewer). From the result,
//! [`Client::console_lines`] gives the console's lines with editor
//! positions (UTF-16, through `lang::source`) for click-to-jump, and
//! [`Client::run_files`] the files the run read, which a host with a disk
//! watches.
//!
//! The customizer's parameters ([`Client::parameters`]) come from the
//! document's own text (the main file's annotated top-level assignments),
//! and its parameter sets are OpenSCAD's JSON files beside the model
//! ([`Client::parameter_sets`], [`Client::parameter_set_file`]).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use lang::customizer::Parameters;
use lang::customizer::params::{EnumValue, ParamKind};
use lang::source::SourceFile;
use serde::{Deserialize, Serialize};

use crate::{Client, CoreError, RenderMode};

/// A customizer value: what a control edits, and what the run passes as
/// `name = value` after the text.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum ParameterValue {
    Bool { value: bool },
    Number { value: f64 },
    Text { value: String },
    Vector { value: Vec<f64> },
}

/// One customizer value to run with.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ParameterOverride {
    pub name: String,
    pub value: ParameterValue,
}

/// One entry of a dropdown: the label shown and the value it stands for.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ParameterOption {
    pub label: String,
    pub value: ParameterValue,
}

/// The control for a parameter, as OpenSCAD's customizer picks it
/// (`ParameterWidget::createParameterWidget`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ParameterGroup {
    pub name: String,
    pub parameters: Vec<Parameter>,
}

/// What a document run does.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DocumentRequest {
    pub mode: RenderMode,
    /// Customizer values, appended to the text as `-D` assignments are.
    #[serde(default)]
    pub overrides: Vec<ParameterOverride>,
    /// neoscad's `part()` extension (`--enable part`), the window's
    /// toggle: the check and measure panels name parts, and the view must
    /// accept the same text without warning that `part` is unknown.
    #[serde(default)]
    pub parts: bool,
    /// OpenSCAD's experimental features, as `--enable` names them
    /// (`textmetrics`, `object-function`, ...); off by default, as in
    /// OpenSCAD.
    #[serde(default)]
    pub enable: Vec<String>,
}

/// What a console line is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceRange {
    pub path: String,
    pub start_line: u32,
    pub start_character: u32,
    pub end_line: u32,
    pub end_character: u32,
}

/// One console line: the text OpenSCAD prints, and where it points.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConsoleLine {
    pub kind: ConsoleKind,
    pub text: String,
    pub location: Option<SourceRange>,
}

/// A value as OpenSCAD source (a literal).
pub fn literal(v: &ParameterValue) -> Option<String> {
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

/// The `-D` assignment of an override; `None` for a name that is not an
/// identifier or a value that is not finite.
pub fn define(o: &ParameterOverride) -> Option<String> {
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
pub fn g(v: f64, precision: usize) -> String {
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
    client: &'a Client,
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
                    self.client.session.fs().read(&path).ok()?
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

impl Client {
    /// A document's text as the session reads it now: its buffer, or the
    /// file.
    pub fn text_now(&self, doc: &Path) -> Result<Arc<[u8]>, CoreError> {
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

    /// A document run's request, ready for the session: the document's
    /// normalised path, the text the run reads (fixed now, so the markers
    /// are published for exactly this text whatever edits arrive
    /// meanwhile, and set as [`session::Run::text`]), and the run with
    /// `request`'s customizer values, parts and features. The host sets
    /// the camera and any hooks, then renders it in `request.mode`.
    pub fn document_run(
        &self,
        path: &str,
        request: &DocumentRequest,
    ) -> Result<(session::Run, PathBuf, Arc<[u8]>), CoreError> {
        let doc = self.doc_path(path)?;
        let mut run = self.run(path)?;
        let text = self.text_now(&doc)?;
        run.text = Some(text.clone());
        run.defines = request.overrides.iter().filter_map(define).collect();
        run.parts = request.parts;
        run.features = eval::Features::from_names(&request.enable);
        Ok((run, doc, text))
    }

    /// Every line of a run's console, in order, with where it points in
    /// editor positions: `doc`'s lines against `text` (the text the run
    /// read), other files' as the session reads them now.
    pub fn console_lines(
        &self,
        log: &session::Log,
        doc: &Path,
        text: Arc<[u8]>,
    ) -> Vec<ConsoleLine> {
        let mut positions = Positions {
            client: self,
            doc: doc.to_path_buf(),
            text,
            files: HashMap::new(),
        };
        log.lines
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
            .collect()
    }

    /// The files a run read that another program could change: not the
    /// document (the host writes it itself, and a save must not re-run
    /// it) and not other open documents (their buffers are what runs
    /// read). A host with a disk also leaves out what is not on it (the
    /// bundled MCAD exists only in memory).
    pub fn run_files<'r>(
        &self,
        r: &'r session::Rendered,
        doc: &Path,
    ) -> impl Iterator<Item = &'r PathBuf> {
        let doc = doc.to_path_buf();
        r.files
            .iter()
            .filter(move |f| **f != doc && self.session.buffer_text(f).is_none())
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

    /// The customizer's groups for a document's current text (see
    /// [`ParameterGroup`]).
    pub fn parameters(&self, path: &str) -> Result<Vec<ParameterGroup>, CoreError> {
        let doc = self.doc_path(path)?;
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
    }

    /// The names of the parameter sets in `json_path` (OpenSCAD's file
    /// beside the model, `model.json`), in the file's order. No file is no
    /// sets.
    pub fn parameter_sets(&self, json_path: &str) -> Result<Vec<String>, CoreError> {
        let p = self.doc_path(json_path)?;
        if !self.session.fs().exists(&p) {
            return Ok(Vec::new());
        }
        let sets = lang::customizer::read_parameter_sets(&*self.session.fs(), &p)
            .map_err(|d| CoreError::Failed { message: d.message })?;
        Ok(sets.into_iter().map(|s| s.name).collect())
    }

    /// The document's parameters with the set `name` of `json_path`
    /// applied as OpenSCAD applies one (`-p file -P name`): values checked
    /// against each parameter's type and range, and parameters the set
    /// does not name back at their defaults. Returns every parameter's
    /// value.
    pub fn apply_parameter_set(
        &self,
        path: &str,
        json_path: &str,
        name: &str,
    ) -> Result<Vec<ParameterOverride>, CoreError> {
        let doc = self.doc_path(path)?;
        let p = self.doc_path(json_path)?;
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
    }

    /// The text of `json_path` with the document's parameters, `values`
    /// over their defaults, saved as the set `name`, keeping the file's
    /// other sets (a set of the same name is replaced in place). `existing`
    /// says whether the file exists now (the host asks its disk); it is
    /// read through the session. The text is what OpenSCAD writes and
    /// reads (`ParameterSets::writeFile`): every value a string,
    /// `fileFormatVersion` "1". The host writes it.
    pub fn parameter_set_file(
        &self,
        path: &str,
        json_path: &str,
        name: &str,
        values: &[ParameterOverride],
        existing: bool,
    ) -> Result<String, CoreError> {
        let doc = self.doc_path(path)?;
        let target = self.doc_path(json_path)?;
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
        let mut sets: Vec<(String, Vec<(String, String)>)> = if existing {
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
            None => sets.push((name.to_string(), mine)),
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
        Ok(out)
    }
}
