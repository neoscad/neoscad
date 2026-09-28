//! `neoscad measure MODEL.scad`: volume, area, bounding box and centre of
//! mass of the model and its parts, the distance between two parts, and
//! cross-sections (`session::measure`). A short report, or with `--format
//! json` one JSON object on stdout (`docs/cli-json.md`, "measure").
//!
//! As with `snapshot`, a running `neoscad serve` does the work when there
//! is one (`cli.measure`), with the same output.

use std::ffi::OsString;
use std::path::Path;

use clap::Parser;
use serde_json::{Value, json};
use session::measure::{Axis, MeasureRequest, Plane, Profile};

use crate::outcome::Outcome;

const EXIT_ERROR: u8 = 1;

/// `neoscad measure`: a model's numbers.
#[derive(Parser, Debug)]
#[command(
    name = "neoscad measure",
    about = "Measure a model and its parts: volume, area, bounding box, centre of mass, \
             distances and cross-sections",
    version
)]
struct Args {
    /// The model.
    model: String,

    /// Only this part (and the parts nested in it); a section cuts its
    /// solid rather than the model's.
    #[arg(long, value_name = "PART")]
    part: Option<String>,

    /// The smallest distance between two parts, and whether they touch or
    /// overlap.
    #[arg(long, num_args = 2, value_names = ["A", "B"])]
    between: Option<Vec<String>>,

    /// A cross-section at an axis plane: z=H, x=H or y=H (mm).
    #[arg(long, value_name = "AXIS=MM")]
    section: Option<String>,

    /// The axis radii are measured about (a section's contours and a
    /// profile): x, y or z.
    #[arg(long, value_name = "AXIS", default_value = "z")]
    axis: String,

    /// Where the axis is, in the other two coordinates (x,y for z; y,z for
    /// x; x,z for y) [default: 0,0].
    #[arg(long, value_name = "A,B", allow_hyphen_values = true)]
    center: Option<String>,

    /// A radius profile along the axis: the outer radii every STEP from
    /// FROM to TO, and the crests on one side (a thread's pitch).
    #[arg(long, value_name = "FROM:TO:STEP", allow_hyphen_values = true)]
    profile: Option<String>,

    /// Also write the section's outline as SVG.
    #[arg(long, value_name = "FILE", requires = "section")]
    svg: Option<String>,

    /// `json` prints the whole result as one JSON object.
    #[arg(long, value_name = "FORMAT")]
    format: Option<String>,

    /// Set a top-level variable (`-D var=value`).
    #[arg(short = 'D', value_name = "var=val", action = clap::ArgAction::Append)]
    define: Vec<String>,

    /// `part`: neoscad's `part("name") { ... }` extension.
    #[arg(long, value_name = "FEATURE", action = clap::ArgAction::Append)]
    enable: Vec<String>,

    /// Only errors on stderr.
    #[arg(short = 'q', long)]
    quiet: bool,

    /// Measure in this process even when a `neoscad serve` is running.
    #[arg(long = "no-server")]
    no_server: bool,
}

/// Run `neoscad measure` with the arguments after `measure`.
pub fn main(args: Vec<OsString>) -> u8 {
    let argv = std::iter::once(OsString::from("neoscad measure")).chain(args);
    let a = match Args::try_parse_from(argv) {
        Ok(a) => a,
        Err(e) => {
            let _ = e.print();
            return if e.use_stderr() { EXIT_ERROR } else { 0 };
        }
    };
    let params = json!({
        "model": a.model,
        "part": a.part,
        "between": a.between,
        "section": a.section,
        "axis": a.axis,
        "center": a.center,
        "profile": a.profile,
        "svg": a.svg,
        "json": match a.format.as_deref() {
            None => false,
            Some("json") => true,
            Some(f) => return fail(format!("unknown --format '{f}' (only json)")).emit(),
        },
        "defines": a.define,
        "enable": a.enable,
        "quiet": a.quiet,
        "rich": crate::rich_diagnostics(),
    });
    if let Some(socket) = crate::client::available(a.no_server)
        && let Some(o) = crate::client::run(&socket, "cli.measure", params.clone())
    {
        return o.emit();
    }
    let host = crate::host::Host::from_env();
    let session = session::Session::new(host.session_config(0));
    let cwd = std::env::current_dir().unwrap_or_default();
    execute(&session, &params, &cwd, None).emit()
}

fn fail(msg: impl std::fmt::Display) -> Outcome {
    Outcome::fail(EXIT_ERROR, format!("neoscad measure: {msg}"))
}

/// The measure request a command's parameters give (`part`, `between`
/// as two names, `section` as `axis=mm`, `axis` as a letter, `center` as
/// `a,b` or `[a, b]`, `profile` as `from:to:step` or `[from, to, step]`,
/// `svg`).
pub fn request_of(params: &Value, run: session::Run) -> Result<MeasureRequest, String> {
    let mut req = MeasureRequest::new(run);
    req.part = params
        .get("part")
        .and_then(Value::as_str)
        .map(str::to_string);
    match params.get("between") {
        None | Some(Value::Null) => {}
        Some(Value::Array(v)) => match &v[..] {
            [Value::String(a), Value::String(b)] => req.between = Some((a.clone(), b.clone())),
            _ => return Err("--between takes two part names".into()),
        },
        Some(_) => return Err("between must be [A, B]".into()),
    }
    if let Some(s) = params.get("section").and_then(Value::as_str) {
        req.section = Some(
            Plane::parse(s)
                .ok_or_else(|| format!("--section must be z=MM, x=MM or y=MM (got '{s}')"))?,
        );
    }
    let numbers = |key: &str, sep: char, n: usize| -> Result<Option<Vec<f64>>, String> {
        let v: Vec<f64> = match params.get(key) {
            None | Some(Value::Null) => return Ok(None),
            Some(Value::String(s)) => s
                .split(sep)
                .map(|x| x.trim().parse::<f64>())
                .collect::<Result<_, _>>()
                .map_err(|_| format!("--{key} takes {n} numbers (got '{s}')"))?,
            Some(Value::Array(a)) => a
                .iter()
                .map(Value::as_f64)
                .collect::<Option<_>>()
                .ok_or_else(|| format!("--{key} takes {n} numbers"))?,
            Some(v) => return Err(format!("--{key} takes {n} numbers (got {v})")),
        };
        if v.len() != n {
            return Err(format!("--{key} takes {n} numbers (got {})", v.len()));
        }
        Ok(Some(v))
    };
    let center = numbers("center", ',', 2)?.map(|v| [v[0], v[1]]);
    let letter = params.get("axis").and_then(Value::as_str).unwrap_or("z");
    req.axis = Axis::parse(letter, center)
        .ok_or_else(|| format!("--axis must be x, y or z (got '{letter}')"))?;
    if let Some(v) = numbers("profile", ':', 3)? {
        req.profile = Some(Profile::new(v[0], v[1], v[2]).map_err(|e| format!("--{e}"))?);
    }
    req.svg = matches!(
        params.get("svg"),
        Some(Value::String(_)) | Some(Value::Bool(true))
    );
    Ok(req)
}

/// Measure as the command line asked (`params` as `main` builds them),
/// writing the SVG relative to `cwd`: what `neoscad measure` prints.
pub fn execute(
    session: &session::Session,
    params: &Value,
    cwd: &Path,
    progress: Option<session::Progress>,
) -> Outcome {
    let Some(model) = params.get("model").and_then(Value::as_str) else {
        return fail("no model given");
    };
    let mut run = crate::check::run_of(params, model, cwd);
    run.progress = progress;
    let req = match request_of(params, run) {
        Ok(r) => r,
        Err(e) => return fail(e),
    };
    let mut m = match session.measure(&req) {
        Ok(m) => m,
        Err(e) => return fail(e),
    };
    let mut out = Outcome {
        exit_code: m.exit_code,
        stderr: m.log.stderr.clone(),
        stdout: Vec::new(),
    };
    if let (Some(file), Some(svg)) = (params.get("svg").and_then(Value::as_str), &m.svg) {
        if let Err(e) = std::fs::write(cwd.join(file), svg) {
            out.stderr.extend_from_slice(
                format!("neoscad measure: cannot write '{file}': {e}\n").as_bytes(),
            );
            out.exit_code = EXIT_ERROR;
            return out;
        }
        if let Some(s) = m.summary.get_mut("section").filter(|s| !s.is_null()) {
            s["svg"] = json!(file);
        }
    }
    let json = params.get("json").and_then(Value::as_bool).unwrap_or(false);
    if !json && let Some(e) = m.summary.get("error").and_then(Value::as_str) {
        out.stderr
            .extend_from_slice(format!("neoscad measure: {e}\n").as_bytes());
        return out;
    }
    out.stdout = if json {
        format!("{}\n", m.summary).into_bytes()
    } else {
        session::measure::text(&m.summary).into_bytes()
    };
    out
}
