//! Diagnostics as JSON (`docs/cli-json.md`, "Diagnostics"): the stable
//! code, the severity, the span, OpenSCAD's own line, and hints that say
//! how to fix the problem.
//!
//! OpenSCAD's text stays exactly as it prints it (`text`); everything else
//! is added around it. Hints come from the diagnostic itself when the
//! front end attached one, otherwise from a short table by code, and for
//! unknown names a "did you mean" against the names the program and its
//! libraries define and OpenSCAD's builtins.

use std::collections::BTreeSet;

use eval::{Location, Logged};
use lang::Program;
use lang::diag::{DiagCode, Severity};
use serde_json::{Value, json};

/// The JSON name of a severity.
pub fn severity_name(s: Option<Severity>) -> &'static str {
    match s {
        Some(Severity::Error) => "error",
        Some(Severity::Warning) => "warning",
        Some(Severity::Deprecated) => "deprecated",
        Some(Severity::Echo) => "echo",
        Some(Severity::Trace) => "trace",
        Some(Severity::Info) | None => "info",
    }
}

fn location_json(l: &Location) -> Value {
    json!({
        "file": l.file.to_string_lossy(),
        "line": l.line,
        "span": {
            "start": {"line": l.start.0, "column": l.start.1},
            "end": {"line": l.end.0, "column": l.end.1},
        },
    })
}

/// One diagnostic. `names` are the candidates for "did you mean".
pub fn to_json(d: &Logged, names: &Names) -> Value {
    let mut o = serde_json::Map::new();
    o.insert("code".into(), json!(d.code.map_or("log", DiagCode::as_str)));
    o.insert("severity".into(), json!(severity_name(d.severity)));
    o.insert("message".into(), json!(d.message));
    o.insert("text".into(), json!(d.text));
    if let Some(l) = &d.location
        && let Value::Object(m) = location_json(l)
    {
        o.extend(m);
    }
    let mut hints: Vec<Value> = d
        .hints
        .iter()
        .map(|h| {
            let mut v = json!({"message": h.message});
            if let Some((l, t)) = &h.replacement {
                v["replace"] = json!({"span": location_json(l)["span"], "text": t});
            }
            v
        })
        .collect();
    if hints.is_empty()
        && let Some(h) = hint(d, names)
    {
        hints.push(json!({"message": h}));
    }
    if !hints.is_empty() {
        o.insert("hints".into(), Value::Array(hints));
    }
    Value::Object(o)
}

/// Names a program can refer to, for "did you mean".
#[derive(Debug, Default)]
pub struct Names {
    pub modules: BTreeSet<String>,
    pub functions: BTreeSet<String>,
    pub variables: BTreeSet<String>,
}

/// OpenSCAD's builtin modules: the `Builtins::init` registrations under
/// `src/core` in the reference checkout that are modules.
const BUILTIN_MODULES: &[&str] = &[
    "cube",
    "sphere",
    "cylinder",
    "polyhedron",
    "square",
    "circle",
    "polygon",
    "text",
    "import",
    "surface",
    "linear_extrude",
    "rotate_extrude",
    "projection",
    "union",
    "difference",
    "intersection",
    "hull",
    "minkowski",
    "offset",
    "fill",
    "render",
    "color",
    "translate",
    "rotate",
    "scale",
    "mirror",
    "multmatrix",
    "resize",
    "group",
    "children",
    "echo",
    "assert",
    "for",
    "intersection_for",
    "let",
    "if",
    "roof",
];

/// OpenSCAD's builtin functions: the other `Builtins::init` registrations
/// (`builtin_functions.cc`, plus `echo`, `assert` and `let`, which are
/// both).
const BUILTIN_FUNCTIONS: &[&str] = &[
    "abs",
    "sign",
    "rands",
    "min",
    "max",
    "sin",
    "cos",
    "asin",
    "acos",
    "tan",
    "atan",
    "atan2",
    "pow",
    "round",
    "ceil",
    "floor",
    "sqrt",
    "exp",
    "len",
    "log",
    "ln",
    "str",
    "chr",
    "ord",
    "concat",
    "lookup",
    "search",
    "version",
    "version_num",
    "norm",
    "cross",
    "parent_module",
    "is_undef",
    "is_bool",
    "is_num",
    "is_string",
    "is_list",
    "is_function",
    "is_object",
    "echo",
    "assert",
    "let",
    "dxf_dim",
    "dxf_cross",
    "textmetrics",
    "fontmetrics",
    "object",
    "has_key",
    "import",
];

impl Names {
    /// The top-level definitions of `programs`.
    pub fn of<'a>(programs: impl IntoIterator<Item = &'a Program>) -> Names {
        let mut n = Names::default();
        for p in programs {
            let root = &p.ast.root;
            n.modules
                .extend(root.modules.iter().map(|m| p.ast.name(m.name).to_string()));
            n.functions.extend(
                root.functions
                    .iter()
                    .map(|f| p.ast.name(f.name).to_string()),
            );
            n.variables.extend(
                root.assignments
                    .iter()
                    .map(|a| p.ast.name(a.name).to_string()),
            );
        }
        n
    }
}

/// Edit distance, for "did you mean".
fn distance(a: &str, b: &str) -> usize {
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for i in 1..=a.len() {
        let mut cur = vec![i; b.len() + 1];
        for j in 1..=b.len() {
            let sub = prev[j - 1] + usize::from(a[i - 1] != b[j - 1]);
            cur[j] = sub.min(prev[j] + 1).min(cur[j - 1] + 1);
        }
        prev = cur;
    }
    prev[b.len()]
}

/// The closest candidate within a third of the name's length (at least 1).
fn closest<'a>(name: &str, candidates: impl Iterator<Item = &'a str>) -> Option<&'a str> {
    let limit = (name.chars().count() / 3).max(1);
    candidates
        .filter(|c| *c != name)
        .map(|c| (distance(name, c), c))
        .filter(|(d, _)| *d <= limit)
        .min()
        .map(|(_, c)| c)
}

/// "did you mean" for `name` among `candidates`: the closest within a
/// third of its length, as the diagnostics' hints choose.
pub fn did_you_mean<'a>(name: &str, candidates: impl Iterator<Item = &'a str>) -> Option<&'a str> {
    closest(name, candidates)
}

/// The quoted name in a message such as `Ignoring unknown module 'cub'`.
fn quoted(message: &str) -> Option<&str> {
    let start = message.find('\'')? + 1;
    let end = start + message[start..].find('\'')?;
    Some(&message[start..end])
}

fn hint(d: &Logged, names: &Names) -> Option<String> {
    let code = d.code?;
    let did_you_mean = |pool: Vec<&str>| {
        let name = quoted(&d.message).or_else(|| d.message.rsplit(' ').next())?;
        closest(name, pool.into_iter()).map(|c| format!("did you mean '{c}'?"))
    };
    let text = match code {
        DiagCode::UnknownModule if quoted(&d.message) == Some("part") => {
            "`part()` is neoscad's named-parts extension: turn it on with `--enable part` \
             (the `parts` request option), or define a module called `part`"
        }
        DiagCode::UnknownModule if quoted(&d.message) == Some("sketch") => {
            "`sketch()` is a NeoSCAD extension (constrained 2D sketches): enable it with \
             `--enable sketch`, or define a module called `sketch`"
        }
        DiagCode::UnknownModule if quoted(&d.message) == Some("anchor") => {
            "`anchor()` is a NeoSCAD extension (geometry queries): enable it with \
             `--enable query`, or define a module called `anchor`"
        }
        DiagCode::UnknownFunction
            if matches!(
                quoted(&d.message),
                Some("child_anchors" | "child_bounds" | "child_measure")
            ) =>
        {
            let name = quoted(&d.message).unwrap_or_default();
            return Some(format!(
                "`{name}()` is a NeoSCAD extension (geometry queries): enable it with \
                 `--enable query`, or define a function called `{name}`"
            ));
        }
        DiagCode::DuplicatePart => {
            "give each part a unique name: parts with one name are measured and checked as one"
        }
        DiagCode::UnknownModule => {
            let pool = names
                .modules
                .iter()
                .map(String::as_str)
                .chain(BUILTIN_MODULES.iter().copied())
                .collect();
            return Some(did_you_mean(pool).unwrap_or_else(|| {
                "define the module, or `use`/`include` the file that defines it".into()
            }));
        }
        DiagCode::UnknownFunction => {
            let pool = names
                .functions
                .iter()
                .map(String::as_str)
                .chain(BUILTIN_FUNCTIONS.iter().copied())
                .collect();
            return Some(did_you_mean(pool).unwrap_or_else(|| {
                "define the function, or `use`/`include` the file that defines it".into()
            }));
        }
        DiagCode::UnknownVariable => {
            let pool = names.variables.iter().map(String::as_str).collect();
            return Some(did_you_mean(pool).unwrap_or_else(|| {
                "assign the variable before this point, or pass it as a parameter; \
                 OpenSCAD looks variables up lexically"
                    .into()
            }));
        }
        DiagCode::SyntaxError => {
            "look just before this point for a missing ';', ')', ']' or '}', or an unbalanced bracket"
        }
        DiagCode::IncludeNotFound | DiagCode::LibraryNotFound | DiagCode::FontNotFound => {
            "paths are relative to the including file, then each library directory (OPENSCADPATH); check the name"
        }
        DiagCode::Reassignment => {
            "OpenSCAD keeps the last value at the first assignment's position; remove one of them"
        }
        DiagCode::ArgumentMismatch => "check the parameter names and count against the definition",
        DiagCode::AssertionFailed => "the assert's condition is false for these arguments",
        DiagCode::RecursionLimit => "add or fix the recursion's base case",
        DiagCode::IterationLimit => "reduce the range or its step",
        DiagCode::InputNotFound => {
            "check the file name: a relative path is relative to the working directory (MCP: `base_dir`, or the server's directory)"
        }
        DiagCode::OutputNotWritable => {
            "check that the output's directory exists and is writable, and that the name is not a directory"
        }
        DiagCode::UndefinedOperation => {
            "an operand is undef or of the wrong type; check the values reaching this expression"
        }
        _ => return None,
    };
    Some(text.to_string())
}

/// Errors, warnings and deprecations, in order; the `TRACE:` lines after
/// an error go into its `trace` array (the call stack that led to it).
pub fn list_json(lines: &[Logged], names: &Names) -> Vec<Value> {
    let mut out: Vec<Value> = Vec::new();
    let mut last_error: Option<usize> = None;
    for l in lines {
        match l.severity {
            // Info is NeoSCAD's own (an under-constrained sketch): not a
            // problem, but a fact a tool or an agent should see.
            Some(Severity::Error | Severity::Warning | Severity::Deprecated | Severity::Info) => {
                last_error = (l.severity == Some(Severity::Error)).then_some(out.len());
                out.push(to_json(l, names));
            }
            Some(Severity::Trace) => {
                if let Some(i) = last_error
                    && let Value::Object(o) = &mut out[i]
                {
                    let t = o.entry("trace").or_insert_with(|| json!([]));
                    if let Value::Array(a) = t {
                        a.push(json!(l.text));
                    }
                }
            }
            _ => {}
        }
    }
    out
}

/// Messages sorted by kind, as `neoscad snapshot --format json` has always
/// reported them (`docs/cli-json.md`, "snapshot"), plus the structured
/// diagnostics (`items`).
pub fn summary_json(log: &[Logged], names: &Names) -> Value {
    // At most this many lines of each kind: the summary stays small.
    const KEEP: usize = 20;
    let of = |s: Severity| log.iter().filter(move |l| l.severity == Some(s));
    let errors: Vec<&Logged> = of(Severity::Error).collect();
    let warnings: Vec<&Logged> = of(Severity::Warning).collect();
    let echoes: Vec<&Logged> = of(Severity::Echo).collect();
    let first = |v: &[&Logged]| {
        v.iter()
            .take(KEEP)
            .map(|l| l.text.clone())
            .collect::<Vec<_>>()
    };
    let items: Vec<Value> = errors
        .iter()
        .take(KEEP)
        .chain(warnings.iter().take(KEEP))
        .map(|l| to_json(l, names))
        .collect();
    json!({
        "errors": errors.len(),
        "warnings": warnings.len(),
        "echoes": echoes.len(),
        "messages": first(&errors).into_iter().chain(first(&warnings)).collect::<Vec<_>>(),
        "echo": first(&echoes),
        "items": items,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn did_you_mean_finds_near_names() {
        assert_eq!(
            closest("cub", BUILTIN_MODULES.iter().copied()),
            Some("cube")
        );
        assert_eq!(
            closest("sphre", BUILTIN_MODULES.iter().copied()),
            Some("sphere")
        );
        assert_eq!(closest("xyzzy", BUILTIN_MODULES.iter().copied()), None);
        assert_eq!(quoted("Ignoring unknown module 'cub'"), Some("cub"));
    }
}
