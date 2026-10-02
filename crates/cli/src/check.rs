//! `neoscad check MODEL.scad`: printability checks (`session::check`),
//! printed as a short report, or with `--format json` as one JSON object
//! on stdout (`docs/cli-json.md`, "check"). The model's own messages go to
//! stderr as for an export. Exit status: 0 when no finding is an error, 1
//! for errors or a model that fails.
//!
//! As with `snapshot`, a running `neoscad serve` does the work when there
//! is one (`cli.check`), with the same output.

use std::ffi::OsString;
use std::path::Path;

use clap::Parser;
use serde_json::{Value, json};
use session::check::{CheckRequest, CheckSettings};

use crate::outcome::Outcome;

const EXIT_ERROR: u8 = 1;

/// `neoscad check`: printability checks for FDM printing.
#[derive(Parser, Debug)]
#[command(
    name = "neoscad check",
    about = "Check a model for FDM printing: manifold, floating pieces, thin walls, \
             overhangs, bed fit, tiny features, intersecting parts, and objects a \
             difference() removes entirely",
    version
)]
pub(crate) struct Args {
    /// The model.
    model: String,

    /// Build volume in mm, e.g. 220x220x250 (checked only when given).
    #[arg(long, value_name = "WxDxH")]
    bed: Option<String>,

    /// Nozzle diameter in mm: thinner walls are errors.
    #[arg(long, value_name = "MM", default_value_t = 0.4)]
    nozzle: f64,

    /// Thinner walls are warnings (default: twice the nozzle, two
    /// perimeters).
    #[arg(long = "min-wall", value_name = "MM")]
    min_wall: Option<f64>,

    /// Steepest printable overhang, degrees from vertical.
    #[arg(long = "max-overhang", value_name = "DEG", default_value_t = 45.0)]
    max_overhang: f64,

    /// `json` prints the whole result as one JSON object.
    #[arg(long, value_name = "FORMAT")]
    format: Option<String>,

    /// Set a top-level variable (`-D var=value`).
    #[arg(short = 'D', value_name = "var=val", action = clap::ArgAction::Append)]
    define: Vec<String>,

    /// `part`: neoscad's `part("name") { ... }` extension, so findings name
    /// parts and parts are checked on their own.
    #[arg(long, value_name = "FEATURE", action = clap::ArgAction::Append)]
    enable: Vec<String>,

    /// Only errors on stderr.
    #[arg(short = 'q', long)]
    quiet: bool,

    /// Check in this process even when a `neoscad serve` is running.
    #[arg(long = "no-server")]
    no_server: bool,
}

/// `WxDxH` in mm, each positive.
pub fn parse_bed(s: &str) -> Option<[f64; 3]> {
    let v: Vec<f64> = s
        .split(['x', 'X', ','])
        .map(|p| p.trim().parse::<f64>())
        .collect::<Result<_, _>>()
        .ok()?;
    match v[..] {
        [w, d, h] if [w, d, h].iter().all(|x| x.is_finite() && *x > 0.0) => Some([w, d, h]),
        _ => None,
    }
}

/// The check settings a request's parameters give (`bed`, `nozzle`,
/// `min_wall`, `max_overhang`, `max_findings`), defaults for the rest.
pub fn settings_of(params: &Value) -> Result<CheckSettings, String> {
    let mut s = CheckSettings::default();
    let num = |k: &str| params.get(k).and_then(Value::as_f64);
    if let Some(n) = num("nozzle") {
        if !(n.is_finite() && n > 0.0) {
            return Err(format!(
                "--nozzle must be a positive number of mm (got {n})"
            ));
        }
        s.nozzle = n;
    }
    s.min_wall = 2.0 * s.nozzle;
    if let Some(w) = num("min_wall") {
        if !(w.is_finite() && w >= 0.0) {
            return Err(format!("--min-wall must be a number of mm (got {w})"));
        }
        s.min_wall = w;
    }
    if let Some(a) = num("max_overhang") {
        if !(0.0..=90.0).contains(&a) {
            return Err(format!("--max-overhang must be 0 to 90 degrees (got {a})"));
        }
        s.max_overhang = a;
    }
    if let Some(n) = params.get("max_findings").and_then(Value::as_u64) {
        s.max_findings = n as usize;
    }
    match params.get("bed") {
        None | Some(Value::Null) => {}
        Some(Value::String(b)) => {
            s.bed = Some(parse_bed(b).ok_or_else(|| {
                format!("--bed must be WxDxH in mm, e.g. 220x220x250 (got '{b}')")
            })?);
        }
        Some(Value::Array(a)) => {
            let v: Vec<f64> = a.iter().filter_map(Value::as_f64).collect();
            match v[..] {
                [w, d, h] if [w, d, h].iter().all(|x| *x > 0.0) => s.bed = Some([w, d, h]),
                _ => return Err("bed must be [width, depth, height] in mm".into()),
            }
        }
        Some(_) => return Err("bed must be \"WxDxH\" or [width, depth, height]".into()),
    }
    Ok(s)
}

/// The session's run for a command's parameters (`model`, `defines`,
/// `quiet`, `rich`, `enable`/`parts`, `supersede`).
pub fn run_of(params: &Value, model: &str, cwd: &Path) -> session::Run {
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
    let mut run = session::Run::new(model);
    run.cwd = Some(cwd.to_path_buf());
    run.defines = strings("defines");
    run.quiet = b("quiet");
    run.rich = b("rich");
    run.supersede = b("supersede");
    run.rng_seed = Some(0);
    run.parts = b("parts") || crate::parts_enabled(&strings("enable"));
    run.features = crate::features(&strings("enable"));
    // A server resolves a request's limits into a whole `limits` object
    // (`crate::limits`), so nothing here depends on its defaults.
    run.limits = crate::limits::of_params(params, session::Limits::NONE)
        .ok()
        .flatten();
    run
}

/// Run `neoscad check` with the arguments after `check`.
pub fn main(args: Vec<OsString>) -> u8 {
    let argv = std::iter::once(OsString::from("neoscad check")).chain(args);
    let a = match Args::try_parse_from(argv) {
        Ok(a) => a,
        Err(e) => {
            let _ = e.print();
            return if e.use_stderr() { EXIT_ERROR } else { 0 };
        }
    };
    let params = json!({
        "model": a.model,
        "bed": a.bed,
        "nozzle": a.nozzle,
        "min_wall": a.min_wall,
        "max_overhang": a.max_overhang,
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
        && let Some(o) = crate::client::run(&socket, "cli.check", params.clone())
    {
        return o.emit();
    }
    let host = crate::host::Host::from_env();
    let session = session::Session::new(host.session_config(0));
    let cwd = std::env::current_dir().unwrap_or_default();
    execute(&session, &params, &cwd, None).emit()
}

fn fail(msg: impl std::fmt::Display) -> Outcome {
    Outcome::fail(EXIT_ERROR, format!("neoscad check: {msg}"))
}

/// Check a model as the command line asked (`params` as `main` builds
/// them): what `neoscad check` prints, here or in the server.
pub fn execute(
    session: &session::Session,
    params: &Value,
    cwd: &Path,
    progress: Option<session::Progress>,
) -> Outcome {
    let Some(model) = params.get("model").and_then(Value::as_str) else {
        return fail("no model given");
    };
    let settings = match settings_of(params) {
        Ok(s) => s,
        Err(e) => return fail(e),
    };
    let mut run = run_of(params, model, cwd);
    run.progress = progress;
    let checked = match session.check(&CheckRequest { run, settings }) {
        Ok(c) => c,
        Err(e) => return fail(e),
    };
    let json = params.get("json").and_then(Value::as_bool).unwrap_or(false);
    Outcome {
        exit_code: checked.exit_code,
        stderr: checked.log.stderr,
        stdout: if json {
            format!("{}\n", checked.summary).into_bytes()
        } else {
            session::check::text(&checked.summary).into_bytes()
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn beds_parse() {
        assert_eq!(parse_bed("220x220x250"), Some([220.0, 220.0, 250.0]));
        assert_eq!(parse_bed("200,200"), None);
        assert_eq!(parse_bed("0x1x1"), None);
    }

    #[test]
    fn min_wall_follows_the_nozzle() {
        let s = settings_of(&json!({"nozzle": 0.6})).unwrap();
        assert_eq!(s.min_wall, 1.2);
        let s = settings_of(&json!({"nozzle": 0.6, "min_wall": 1.0})).unwrap();
        assert_eq!(s.min_wall, 1.0);
        assert!(settings_of(&json!({"max_overhang": 120})).is_err());
    }
}
