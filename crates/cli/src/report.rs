//! `--format json`: one JSON object describing a command-line run
//! (`docs/cli-json.md`, "`--format json`"), written after the run instead
//! of the human-readable messages on stderr.
//!
//! The run's consoles record every line they print with its tool view
//! (`eval::Logged`); the export code hands them here, with the names the
//! program defines (for "did you mean" hints) and the geometry, and `main`
//! prints the object at the end. Like `deps`, it is process-wide state:
//! one command line, one report.

use std::sync::Mutex;

use eval::Logged;
use serde_json::{Value, json};
use session::Names;

#[derive(Debug, Default)]
struct Report {
    lines: Vec<Logged>,
    names: Names,
    geometry: Option<Value>,
    /// `-o x.step`'s exact export (`--enable exact`).
    exact: Option<Value>,
}

static REPORT: Mutex<Option<Report>> = Mutex::new(None);

/// Start collecting (`--format json`).
pub fn enable() {
    *REPORT.lock().expect("report") = Some(Report::default());
}

/// Stop collecting without printing (a server wrote the report).
pub fn disable() {
    *REPORT.lock().expect("report") = None;
}

pub fn enabled() -> bool {
    REPORT.lock().expect("report").is_some()
}

/// Lines a console printed.
pub fn add(lines: Vec<Logged>) {
    if let Some(r) = REPORT.lock().expect("report").as_mut() {
        r.lines.extend(lines);
    }
}

/// The names of the loaded programs.
pub fn set_names(names: Names) {
    if let Some(r) = REPORT.lock().expect("report").as_mut() {
        r.names = names;
    }
}

pub fn set_geometry(g: Value) {
    if let Some(r) = REPORT.lock().expect("report").as_mut() {
        r.geometry = Some(g);
    }
}

/// The exact STEP export's numbers (`run::exact_json`).
pub fn set_exact(v: Value) {
    if let Some(r) = REPORT.lock().expect("report").as_mut() {
        r.exact = Some(v);
    }
}

/// What the JSON says about the run besides its messages.
#[derive(Debug)]
pub struct Run<'a> {
    pub command: &'a str,
    pub input: &'a str,
    /// Each output and its format identifier.
    pub outputs: Vec<(String, String)>,
    pub exit_code: u8,
    pub timings: Value,
    /// Whether a running `neoscad serve` did the work.
    pub served: bool,
}

/// The object, from recorded lines.
pub fn json(run: &Run<'_>, lines: &[Logged], names: &Names, geometry: Option<Value>) -> Value {
    json_with(run, lines, names, geometry, None)
}

/// [`json`] with an exact export's section, which is present only when a
/// `.step` file was asked for, so other runs' JSON is unchanged.
pub fn json_with(
    run: &Run<'_>,
    lines: &[Logged],
    names: &Names,
    geometry: Option<Value>,
    exact: Option<Value>,
) -> Value {
    use lang::diag::Severity;
    let count = |s: Severity| lines.iter().filter(|l| l.severity == Some(s)).count();
    let log: Vec<&str> = lines
        .iter()
        .filter(|l| l.severity.is_none())
        .map(|l| l.text.as_str())
        .collect();
    let mut out = json!({
        "schema": 1,
        "command": run.command,
        "input": run.input,
        "outputs": run.outputs.iter().map(|(f, id)| json!({"file": f, "format": id})).collect::<Vec<_>>(),
        "exit_code": run.exit_code,
        "served": run.served,
        "counts": {
            "errors": count(Severity::Error),
            "warnings": count(Severity::Warning),
            "echoes": count(Severity::Echo),
        },
        "diagnostics": session::diag::list_json(lines, names),
        "echo": lines.iter().filter(|l| l.severity == Some(Severity::Echo)).map(|l| l.text.as_str()).collect::<Vec<_>>(),
        "log": log,
        "geometry": geometry.unwrap_or(Value::Null),
        "timings_ms": run.timings,
    });
    if let (Some(e), Some(o)) = (exact, out.as_object_mut()) {
        o.insert("exact".into(), e);
    }
    out
}

/// The collected report as JSON text (one line), and stop collecting.
pub fn finish(run: &Run<'_>) -> String {
    let r = REPORT.lock().expect("report").take().unwrap_or_default();
    format!(
        "{}\n",
        json_with(run, &r.lines, &r.names, r.geometry, r.exact)
    )
}
