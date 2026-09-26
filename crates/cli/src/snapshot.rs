//! `neoscad snapshot MODEL.scad`: one contact-sheet PNG of a model for
//! agents, and with `--format json` a small summary of it on stdout
//! (`docs/cli-json.md`, "snapshot"). The sheet itself is the session's
//! (`session::snapshot`); this is the command line around it.
//!
//! When a `neoscad serve` is running (and `--no-server` is not given) the
//! server draws the sheet with its warm caches ([`crate::client`]); the
//! output is the same either way, because both run [`execute`].

use std::ffi::OsString;
use std::path::Path;

use clap::Parser;
use serde_json::{Value, json};
use session::snapshot::{SnapshotError, SnapshotRequest, parse_size};

use crate::outcome::Outcome;

/// OpenSCAD's general failure exit code.
const EXIT_ERROR: u8 = 1;

/// `neoscad snapshot`: a contact sheet of a model's standard views.
#[derive(Parser, Debug)]
#[command(
    name = "neoscad snapshot",
    about = "Draw a model's standard views as one PNG contact sheet",
    version
)]
struct Args {
    /// The model.
    model: String,

    /// The PNG to write (default: MODEL's name with `-snapshot.png`, in the
    /// working directory).
    #[arg(short = 'o', long = "output", value_name = "FILE")]
    output: Option<String>,

    /// Views, comma separated: iso, front, back, left, right, top, bottom.
    #[arg(long, value_delimiter = ',', value_name = "LIST")]
    views: Vec<String>,

    /// Size of the whole sheet in pixels.
    #[arg(long, value_name = "WxH", default_value = "1024x1024")]
    size: String,

    /// Annotate the bounding box's size (mm) in every view.
    #[arg(long)]
    dims: bool,

    /// Draw the rendered geometry (the default).
    #[arg(long, conflicts_with = "preview")]
    render: bool,

    /// Draw OpenSCAD's preview (the CSG products; shows `%` and `#`).
    #[arg(long)]
    preview: bool,

    /// Compare with another version of the model: added (green),
    /// removed (red), unchanged (grey).
    #[arg(long, value_name = "OTHER.scad", conflicts_with = "preview")]
    diff: Option<String>,

    /// `headlight` (the default: a light at the camera, so every visible
    /// face is legible) or `openscad` (OpenSCAD's fixed lights, as PNG
    /// export draws).
    #[arg(long, value_name = "MODE", default_value = "headlight")]
    lighting: String,

    /// Parts to draw in colour, comma separated; the others are ghosted
    /// (needs `--enable part`; a name includes the parts nested in it).
    #[arg(long, value_delimiter = ',', value_name = "PART[,PART]")]
    highlight: Vec<String>,

    /// Run `neoscad check` (default settings) and mark its findings: thin
    /// walls red, overhangs amber, and a numbered marker per finding.
    #[arg(long)]
    issues: bool,

    /// `part`: neoscad's `part("name") { ... }` extension; parts are then
    /// drawn in colours with a legend.
    #[arg(long, value_name = "FEATURE", action = clap::ArgAction::Append)]
    enable: Vec<String>,

    /// `json` also writes a summary to stdout.
    #[arg(long, value_name = "FORMAT")]
    format: Option<String>,

    /// Set a top-level variable (`-D var=value`), as for export.
    #[arg(short = 'D', value_name = "var=val", action = clap::ArgAction::Append)]
    define: Vec<String>,

    /// Only errors on stderr.
    #[arg(short = 'q', long)]
    quiet: bool,

    /// Draw in this process even when a `neoscad serve` is running.
    #[arg(long = "no-server")]
    no_server: bool,
}

/// Run `neoscad snapshot` with the arguments after `snapshot`.
pub fn main(args: Vec<OsString>) -> u8 {
    let argv = std::iter::once(OsString::from("neoscad snapshot")).chain(args);
    let a = match Args::try_parse_from(argv) {
        Ok(a) => a,
        Err(e) => {
            let _ = e.print();
            return if e.use_stderr() { EXIT_ERROR } else { 0 };
        }
    };
    let params = json!({
        "model": a.model,
        "output": a.output,
        "views": a.views,
        "size": a.size,
        "dims": a.dims,
        "preview": a.preview,
        "diff": a.diff,
        "lighting": a.lighting,
        "highlight": a.highlight,
        "issues": a.issues,
        "enable": a.enable,
        "json": match a.format.as_deref() {
            None => false,
            Some("json") => true,
            Some(f) => return fail(format!("unknown --format '{f}' (only json)")).emit(),
        },
        "defines": a.define,
        "quiet": a.quiet,
        "rich": crate::rich_diagnostics(),
    });
    if let Some(socket) = crate::client::available(a.no_server)
        && let Some(o) = crate::client::run(&socket, "cli.snapshot", params.clone())
    {
        return o.emit();
    }
    let host = crate::host::Host::from_env();
    let session = session::Session::new(host.session_config(0));
    let cwd = std::env::current_dir().unwrap_or_default();
    execute(&session, &params, &cwd, None).emit()
}

fn fail(msg: impl std::fmt::Display) -> Outcome {
    Outcome::fail(EXIT_ERROR, format!("neoscad snapshot: {msg}"))
}

/// The session's snapshot request for `params` (as `main` builds them;
/// the server and `neoscad mcp` pass theirs), relative to `cwd`, or what
/// is wrong with them.
pub fn request(
    params: &Value,
    cwd: &Path,
    progress: Option<session::Progress>,
) -> Result<SnapshotRequest, String> {
    let s = |k: &str| params.get(k).and_then(Value::as_str);
    let b = |k: &str| params.get(k).and_then(Value::as_bool).unwrap_or(false);
    let strings = |k: &str| -> Vec<String> {
        params
            .get(k)
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect()
    };
    let Some(model) = s("model") else {
        return Err("no model given".into());
    };
    let size = s("size").unwrap_or("1024x1024");
    let Some(size) = parse_size(size) else {
        return Err(format!(
            "--size must be WxH, 64 to 8192 each (got '{size}')"
        ));
    };
    let lighting = match s("lighting").unwrap_or("headlight") {
        "headlight" => render::Lighting::Headlight,
        "openscad" => render::Lighting::OpenScad,
        l => return Err(format!("unknown --lighting '{l}' (headlight or openscad)")),
    };
    let output = s("output").map(str::to_string).unwrap_or_else(|| {
        let stem = Path::new(model)
            .file_stem()
            .map_or("model".into(), |s| s.to_string_lossy().into_owned());
        format!("{stem}-snapshot.png")
    });
    let mut run = session::Run::new(model);
    run.cwd = Some(cwd.to_path_buf());
    run.defines = strings("defines");
    run.quiet = b("quiet");
    run.rich = b("rich");
    // A command-line snapshot is one-shot and never cancels another; the
    // server's `snapshot` method is an agent's, and supersedes.
    run.supersede = b("supersede");
    run.progress = progress;
    // Unseeded `rands()` repeats from snapshot to snapshot.
    run.rng_seed = Some(0);
    run.parts = b("parts") || crate::parts_enabled(&strings("enable"));
    run.limits = crate::limits::of_params(params, session::Limits::NONE)?;
    // `issues`: true for the default check settings (or those given
    // alongside, as for `check`), or an object of settings.
    let issues = match params.get("issues") {
        None | Some(Value::Null) | Some(Value::Bool(false)) => None,
        Some(Value::Bool(true)) => Some(crate::check::settings_of(params)?),
        Some(o @ Value::Object(_)) => Some(crate::check::settings_of(o)?),
        Some(_) => return Err("issues must be true or an object of check settings".into()),
    };
    Ok(SnapshotRequest {
        run,
        output,
        views: strings("views"),
        size,
        dims: b("dims"),
        preview: b("preview"),
        diff: s("diff").map(str::to_string),
        lighting,
        highlight: strings("highlight"),
        issues,
    })
}

/// Draw a snapshot as the command line asked (`params` as `main` builds
/// them), writing the PNG relative to `cwd`: what `neoscad snapshot`
/// prints, whether it runs here or in the server.
pub fn execute(
    session: &session::Session,
    params: &Value,
    cwd: &Path,
    progress: Option<session::Progress>,
) -> Outcome {
    let req = match request(params, cwd, progress) {
        Ok(r) => r,
        Err(m) => return fail(m),
    };
    let output = req.output.clone();
    let b = |k: &str| params.get(k).and_then(Value::as_bool).unwrap_or(false);
    let snap = match session.snapshot(&req) {
        Ok(s) => s,
        Err(SnapshotError::Failed(m)) => return fail(m),
        Err(e @ SnapshotError::Cancelled) => return fail(e),
    };
    let mut out = Outcome {
        exit_code: snap.exit_code,
        stderr: snap.log.stderr,
        stdout: Vec::new(),
    };
    if let Some(png) = &snap.png
        && let Err(e) = std::fs::write(cwd.join(&output), png)
    {
        out.stderr.extend_from_slice(
            format!("neoscad snapshot: cannot write '{output}': {e}\n").as_bytes(),
        );
        out.exit_code = EXIT_ERROR;
        return out;
    }
    if b("json") {
        out.stdout = format!("{}\n", snap.summary).into_bytes();
    }
    out
}
