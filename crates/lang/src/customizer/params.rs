//! Customizer parameters and parameter sets (`-p file.json -P name`).
//!
//! A port of src/core/customizer/ParameterObject.cc and ParameterSet.cc:
//! each annotated top-level assignment becomes a typed parameter (bool,
//! string, number, vector or enum), values from the chosen set are imported
//! with that type's validation and clamping, and every parameter is then
//! written back into its assignment as a literal.

use std::path::Path;

use crate::ast::{Assignment, Ast, ExprId, ExprKind};
use crate::customizer::json::{self, JsonNode};
use crate::diag::{DiagCode, Diagnostic, PathBase, Severity};
use crate::number::fmt_g;
use crate::source::Span;

/// An enum option's value.
#[derive(Debug, Clone, PartialEq)]
pub enum EnumValue {
    Number(f64),
    String(Vec<u8>),
}

#[derive(Debug, Clone, PartialEq)]
pub struct EnumItem {
    pub key: String,
    pub value: EnumValue,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ParamKind {
    Bool { value: bool, default: bool },
    String { value: Vec<u8>, default: Vec<u8>, max_len: Option<usize> },
    Number { value: f64, default: f64, min: Option<f64>, max: Option<f64>, step: Option<f64> },
    Vector { value: Vec<f64>, default: Vec<f64>, min: Option<f64>, max: Option<f64>, step: Option<f64> },
    Enum { index: usize, default: usize, items: Vec<EnumItem> },
}

#[derive(Debug, Clone, PartialEq)]
pub struct Parameter {
    pub name: String,
    pub description: String,
    pub group: String,
    pub kind: ParamKind,
}

/// One named set from a parameter file: `(name, value)` pairs in file order.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ParameterSet {
    pub name: String,
    pub values: Vec<(String, JsonNode)>,
}

impl ParameterSet {
    fn get(&self, name: &str) -> Option<&JsonNode> {
        // `set[key] = value` in readFile: the last duplicate wins.
        self.values.iter().rev().find(|(k, _)| k == name).map(|(_, v)| v)
    }
}

/// Read the `parameterSets` object of a parameter file. Errors carry
/// OpenSCAD's message text.
pub fn read_parameter_sets(path: &Path) -> Result<Vec<ParameterSet>, Diagnostic> {
    let err = |msg: String| Diagnostic::new(DiagCode::ParameterFile, Severity::Error, msg);
    let text = std::fs::read(path)
        .map_err(|_| err(format!("Cannot open Parameter Set '{}' for reading", path.display())))?;
    let root = json::parse(&text).map_err(|e| err(format!("Cannot open Parameter Set '{}': {e}", path.display())))?;
    let Some(sets) = root.child("parameterSets") else { return Ok(Vec::new()) };
    Ok(sets
        .children
        .iter()
        .map(|(name, set)| ParameterSet { name: name.clone(), values: set.children.clone() })
        .collect())
}

fn lossy(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

fn number(ast: &Ast, e: ExprId) -> Option<f64> {
    match ast.expr(e).kind {
        ExprKind::Number(v) => Some(v),
        _ => None,
    }
}

fn string(ast: &Ast, e: ExprId) -> Option<&[u8]> {
    match &ast.expr(e).kind {
        ExprKind::String(s) => Some(s),
        _ => None,
    }
}

fn is_literal_node(ast: &Ast, e: ExprId) -> bool {
    matches!(ast.expr(e).kind, ExprKind::Undef | ExprKind::Bool(_) | ExprKind::Number(_) | ExprKind::String(_))
}

/// `parseEnumItems`: `None` when the annotation is not an enum list.
fn enum_items(ast: &Ast, param: ExprId, default_key: &str, default: &EnumValue) -> Option<(Vec<EnumItem>, usize)> {
    let ExprKind::Vector(elements) = &ast.expr(param).kind else { return None };
    let mut items = Vec::new();
    for &el in elements {
        let item = match &ast.expr(el).kind {
            ExprKind::Number(v) => {
                // A single number is a maximum, not an enum.
                if elements.len() == 1 {
                    return None;
                }
                EnumItem { key: fmt_g(*v), value: EnumValue::Number(*v) }
            }
            ExprKind::String(s) => EnumItem { key: lossy(s), value: EnumValue::String(s.to_vec()) },
            ExprKind::Vector(pair) => {
                if pair.len() != 2 || !is_literal_node(ast, pair[0]) || !is_literal_node(ast, pair[1]) {
                    return None;
                }
                let key = match (&ast.expr(pair[1]).kind, number(ast, pair[1])) {
                    (_, Some(v)) => fmt_g(v),
                    (ExprKind::String(s), _) => lossy(s),
                    _ => return None,
                };
                let value = match (&ast.expr(pair[0]).kind, number(ast, pair[0])) {
                    (_, Some(v)) => EnumValue::Number(v),
                    (ExprKind::String(s), _) => EnumValue::String(s.to_vec()),
                    _ => return None,
                };
                EnumItem { key, value }
            }
            _ => return None,
        };
        items.push(item);
    }
    if let Some(i) = items.iter().position(|it| it.value == *default) {
        return Some((items, i));
    }
    items.insert(0, EnumItem { key: default_key.to_string(), value: default.clone() });
    Some((items, 0))
}

struct Limits {
    min: Option<f64>,
    max: Option<f64>,
    step: Option<f64>,
}

/// `parseNumericLimits`, including its warnings when the default lies
/// outside the declared range (which then widens the range).
fn numeric_limits(ast: &Ast, a: &Assignment, param: ExprId, values: &[f64], diags: &mut Vec<Diagnostic>) -> Limits {
    let mut l = Limits { min: None, max: None, step: None };
    match &ast.expr(param).kind {
        ExprKind::Number(v) => l.step = Some(*v),
        ExprKind::String(_) | ExprKind::Bool(_) | ExprKind::Undef => {}
        ExprKind::Vector(v) => {
            if v.len() == 1 {
                l.max = number(ast, v[0]);
            }
        }
        ExprKind::Range { begin, step, end } => {
            if let (Some(b), Some(e)) = (number(ast, *begin), number(ast, *end)) {
                l.min = Some(b);
                l.max = Some(e);
                l.step = step.and_then(|s| number(ast, s));
            }
        }
        _ => {}
    }
    let (dmin, dmax) = (l.min, l.max);
    let name = ast.name(a.name);
    for &v in values {
        if let Some(m) = dmin
            && v < m
        {
            diags.push(range_warning(a, format!(
                "Parameter \"{name}\": value {} is below the declared minimum {}, adjusting minimum value",
                fmt_g(v),
                fmt_g(m)
            )));
            if l.min.is_some_and(|x| v < x) {
                l.min = Some(v);
            }
        }
        if let Some(m) = dmax
            && v > m
        {
            diags.push(range_warning(a, format!(
                "Parameter \"{name}\": value {} is above the declared maximum {}, adjusting maximum value",
                fmt_g(v),
                fmt_g(m)
            )));
            if l.max.is_some_and(|x| v > x) {
                l.max = Some(v);
            }
        }
    }
    l
}

fn range_warning(a: &Assignment, message: String) -> Diagnostic {
    Diagnostic::new(DiagCode::ParameterRange, Severity::Warning, message)
        .at(a.loc.span, a.loc.line)
        .with_base(PathBase::MainFileDir)
}

/// `ParameterObject::fromAssignment`.
fn from_assignment(ast: &Ast, a: &Assignment, diags: &mut Vec<Diagnostic>) -> Option<Parameter> {
    let param = a.annotation("Parameter")?;
    let description = a.annotation("Description").and_then(|e| string(ast, e)).map(lossy).unwrap_or_default();
    let mut group = "Parameters".to_string();
    if let Some(g) = a.annotation("Group") {
        if let Some(s) = string(ast, g) {
            group = lossy(s.trim_ascii());
        }
        if group == "Hidden" {
            return None;
        }
    }
    let name = ast.name(a.name).to_string();
    let make = |kind| Some(Parameter { name: name.clone(), description: description.clone(), group: group.clone(), kind });
    match &ast.expr(a.expr).kind {
        ExprKind::Bool(b) => make(ParamKind::Bool { value: *b, default: *b }),
        ExprKind::Number(v) => {
            if let Some((items, i)) = enum_items(ast, param, &fmt_g(*v), &EnumValue::Number(*v)) {
                return make(ParamKind::Enum { index: i, default: i, items });
            }
            let l = numeric_limits(ast, a, param, &[*v], diags);
            make(ParamKind::Number { value: *v, default: *v, min: l.min, max: l.max, step: l.step })
        }
        ExprKind::String(s) => {
            if let Some((items, i)) = enum_items(ast, param, &lossy(s), &EnumValue::String(s.to_vec())) {
                return make(ParamKind::Enum { index: i, default: i, items });
            }
            // StringParameter's constructor means to widen the limit to fit
            // the default but assigns to its argument, not the member, so
            // the declared limit stands (and truncates imported values).
            let max_len = number(ast, param).map(|m| m as usize);
            make(ParamKind::String { value: s.to_vec(), default: s.to_vec(), max_len })
        }
        ExprKind::Vector(v) => {
            if v.is_empty() || v.len() > 4 {
                return None;
            }
            let values: Option<Vec<f64>> = v.iter().map(|&e| number(ast, e)).collect();
            let values = values?;
            let l = numeric_limits(ast, a, param, &values, diags);
            make(ParamKind::Vector { value: values.clone(), default: values, min: l.min, max: l.max, step: l.step })
        }
        _ => None,
    }
}

/// `stream >> double` on the whole text: optional leading whitespace, a
/// number, then nothing but whitespace (boost's `get_value_optional`).
fn decode_double(s: &str) -> Option<f64> {
    let t = s.trim_start_matches([' ', '\t', '\n', '\r', '\x0b', '\x0c']);
    let t = t.trim_end_matches([' ', '\t', '\n', '\r', '\x0b', '\x0c']);
    if t.is_empty() || t.starts_with('+') && t[1..].starts_with(['+', '-']) {
        return None;
    }
    stream_double(t)
}

/// The number syntax `operator>>(double&)` accepts, on text already free of
/// surrounding whitespace. Rust's parser also takes `inf` and `nan`, which
/// the stream does not.
fn stream_double(t: &str) -> Option<f64> {
    let lower = t.to_ascii_lowercase();
    if t.is_empty() || lower.contains("inf") || lower.contains("nan") {
        return None;
    }
    t.parse::<f64>().ok()
}

fn decode_bool(s: &str) -> Option<bool> {
    match s.trim() {
        "1" | "true" => Some(true),
        "0" | "false" => Some(false),
        _ => None,
    }
}

impl Parameter {
    fn reset(&mut self) {
        match &mut self.kind {
            ParamKind::Bool { value, default } => *value = *default,
            ParamKind::String { value, default, .. } => *value = default.clone(),
            ParamKind::Number { value, default, .. } => *value = *default,
            ParamKind::Vector { value, default, .. } => *value = default.clone(),
            ParamKind::Enum { index, default, .. } => *index = *default,
        }
    }

    /// `importValue(..., store = true)`; returns whether it was accepted.
    fn import(&mut self, v: &JsonNode) -> bool {
        let data = v.data.as_str();
        match &mut self.kind {
            ParamKind::Bool { value, .. } => match decode_bool(data) {
                Some(b) => {
                    *value = b;
                    true
                }
                None => false,
            },
            ParamKind::String { value, max_len, .. } => {
                *value = data.as_bytes().to_vec();
                // `substr(0, max)`: a byte count, even mid-character.
                if let Some(m) = *max_len {
                    value.truncate(m);
                }
                true
            }
            ParamKind::Number { value, min, max, .. } => {
                let Some(mut d) = decode_double(data) else { return false };
                if let Some(m) = *min
                    && d < m
                {
                    d = m;
                }
                if let Some(m) = *max
                    && d > m
                {
                    d = m;
                }
                *value = d;
                true
            }
            ParamKind::Vector { value, min, max, .. } => {
                let enc: String = data.chars().filter(|&c| c != ' ').collect();
                if enc.len() < 2 || !enc.starts_with('[') || !enc.ends_with(']') {
                    return false;
                }
                let inner = &enc[1..enc.len() - 1];
                let mut decoded = Vec::new();
                for item in inner.split(',') {
                    let t = item.trim_start_matches(['\t', '\n', '\r', '\x0b', '\x0c']);
                    match stream_double(t) {
                        Some(x) => decoded.push(x),
                        None => return false,
                    }
                }
                if decoded.len() != value.len() {
                    return false;
                }
                for (slot, mut x) in value.iter_mut().zip(decoded) {
                    if let Some(m) = *min
                        && x < m
                    {
                        x = m;
                    }
                    if let Some(m) = *max
                        && x > m
                    {
                        x = m;
                    }
                    *slot = x;
                }
                true
            }
            ParamKind::Enum { index, items, .. } => {
                let d = decode_double(data);
                let found = items.iter().position(|it| {
                    d.is_some_and(|d| it.value == EnumValue::Number(d)) || it.value == EnumValue::String(data.as_bytes().to_vec())
                });
                match found {
                    Some(i) => {
                        *index = i;
                        true
                    }
                    None => false,
                }
            }
        }
    }

    fn value_expr(&self, ast: &mut Ast) -> ExprId {
        let sp = Span::default();
        match &self.kind {
            ParamKind::Bool { value, .. } => ast.add(ExprKind::Bool(*value), sp),
            ParamKind::String { value, .. } => ast.add(ExprKind::String(value.as_slice().into()), sp),
            ParamKind::Number { value, .. } => ast.add(ExprKind::Number(*value), sp),
            ParamKind::Vector { value, .. } => {
                let items = value.iter().map(|&v| ast.add(ExprKind::Number(v), sp)).collect();
                ast.add(ExprKind::Vector(items), sp)
            }
            ParamKind::Enum { index, items, .. } => match &items[*index].value {
                EnumValue::Number(v) => ast.add(ExprKind::Number(*v), sp),
                EnumValue::String(s) => ast.add(ExprKind::String(s.as_slice().into()), sp),
            },
        }
    }
}

/// The customizer parameters of a program (`ParameterObjects`).
#[derive(Debug, Clone, Default)]
pub struct Parameters {
    pub params: Vec<Parameter>,
}

impl Parameters {
    /// `ParameterObjects::fromSourceFile`. Range warnings go to `diags`.
    pub fn from_ast(ast: &Ast, diags: &mut Vec<Diagnostic>) -> Self {
        let params = ast.root.assignments.iter().filter_map(|a| from_assignment(ast, a, diags)).collect();
        Self { params }
    }

    /// `importValues`: parameters missing from the set return to their
    /// defaults; values that fail validation leave the parameter unchanged.
    pub fn import(&mut self, set: &ParameterSet) {
        for p in &mut self.params {
            match set.get(&p.name) {
                None => p.reset(),
                Some(v) => {
                    p.import(v);
                }
            }
        }
    }

    /// `apply`: write every parameter's value into its assignment.
    pub fn apply(&self, ast: &mut Ast) {
        let mut root = std::mem::take(&mut ast.root);
        for a in &mut root.assignments {
            // `namedParameters[name]` keeps the last parameter of a name.
            if let Some(p) = self.params.iter().rev().find(|p| p.name == ast.name(a.name)) {
                a.expr = p.value_expr(ast);
            }
        }
        ast.root = root;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_like_iostreams() {
        assert_eq!(decode_double("30"), Some(30.0));
        assert_eq!(decode_double(" 1.5 "), Some(1.5));
        assert_eq!(decode_double("wrong type"), None);
        assert_eq!(decode_double("1e3"), Some(1000.0));
        assert_eq!(decode_bool("false"), Some(false));
        assert_eq!(decode_bool("yes"), None);
    }
}
