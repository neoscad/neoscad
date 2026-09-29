//! `neoscad fmt [PATHS...] [--check] [--diff] [--stdin]`: format OpenSCAD
//! files in place (`session::format`, the `scadfmt` crate). Directories
//! are searched for `.scad` files (hidden ones skipped); no path means
//! the current directory. A file with a syntax error is left as it is and
//! reported. `--check` and `--diff` write nothing and exit 1 when a file
//! would change; `--stdin` formats standard input to standard output.

use std::ffi::OsString;
use std::io::Read;
use std::path::Path;

use clap::Parser;
use serde_json::{Value, json};
use session::format::{FormatRequest, Formatted};

use crate::outcome::Outcome;

const EXIT_ERROR: u8 = 1;

#[derive(Parser, Debug)]
#[command(
    name = "neoscad fmt",
    about = "Format OpenSCAD files: comments kept, the program unchanged (checked on every file)",
    version
)]
pub(crate) struct Args {
    /// Files or directories (searched for .scad files); default: the
    /// current directory. With --stdin, at most one: the name the text is
    /// configured and reported under.
    paths: Vec<String>,

    /// Write nothing; list the files that would change and exit 1 if any.
    #[arg(long)]
    check: bool,

    /// Write nothing; print a unified diff of each change and exit 1 if
    /// any.
    #[arg(long)]
    diff: bool,

    /// Format standard input to standard output.
    #[arg(long)]
    stdin: bool,

    /// Spaces per indent (overrides .neoscad-fmt.toml; default 4).
    #[arg(long, value_name = "N")]
    indent: Option<usize>,

    /// Line width (overrides .neoscad-fmt.toml; default 100).
    #[arg(long, value_name = "N")]
    width: Option<usize>,

    /// `json`: one JSON object describing the run.
    #[arg(long, value_name = "FORMAT")]
    format: Option<String>,
}

fn fail(msg: impl std::fmt::Display) -> Outcome {
    Outcome::fail(EXIT_ERROR, format!("neoscad fmt: {msg}"))
}

/// Run `neoscad fmt` with the arguments after `fmt`.
pub fn main(args: Vec<OsString>) -> u8 {
    let argv = std::iter::once(OsString::from("neoscad fmt")).chain(args);
    let a = match Args::try_parse_from(argv) {
        Ok(a) => a,
        Err(e) => {
            let _ = e.print();
            return if e.use_stderr() { EXIT_ERROR } else { 0 };
        }
    };
    let json = match a.format.as_deref() {
        None => false,
        Some("json") => true,
        Some(f) => return fail(format!("unknown --format '{f}' (only json)")).emit(),
    };
    let host = crate::host::Host::from_env();
    let session = session::Session::new(host.session_config(0));
    let cwd = std::env::current_dir().unwrap_or_default();
    let base = FormatRequest {
        cwd: Some(cwd.clone()),
        indent: a.indent,
        width: a.width,
        ..FormatRequest::default()
    };
    if a.stdin {
        if a.paths.len() > 1 {
            return fail("--stdin takes at most one path (the name of the text)").emit();
        }
        let mut text = Vec::new();
        if let Err(e) = std::io::stdin().read_to_end(&mut text) {
            return fail(format!("cannot read standard input: {e}")).emit();
        }
        let req = FormatRequest {
            input: a.paths.first().cloned(),
            text: Some(text),
            ..base
        };
        let f = session.format(&req);
        return stdin_outcome(&f, &a, json).emit();
    }
    let paths = if a.paths.is_empty() {
        vec![".".to_string()]
    } else {
        a.paths.clone()
    };
    let files = match session.find_files(&paths, &cwd, &session::format::is_scad) {
        Ok(f) => f,
        Err(e) => return fail(e).emit(),
    };
    let mut out = Outcome::default();
    let mut results = Vec::new();
    let (mut changed, mut errors) = (0usize, 0usize);
    for (shown, abs) in &files {
        let req = FormatRequest {
            input: Some(shown.clone()),
            ..base.clone()
        };
        let f = session.format(&req);
        match &f.result {
            Err(e) => {
                errors += 1;
                if !json {
                    out.stderr
                        .extend_from_slice(format!("neoscad fmt: {shown}: {e}\n").as_bytes());
                }
            }
            Ok(text) if f.changed() => {
                changed += 1;
                if a.diff {
                    if !json {
                        out.stdout.extend_from_slice(f.diff().as_bytes());
                    }
                } else if a.check {
                    if !json {
                        out.stdout
                            .extend_from_slice(format!("would reformat {shown}\n").as_bytes());
                    }
                } else if let Err(e) = write(abs, text) {
                    errors += 1;
                    out.stderr.extend_from_slice(
                        format!("neoscad fmt: cannot write '{shown}': {e}\n").as_bytes(),
                    );
                }
            }
            Ok(_) => {}
        }
        results.push(f.json(false, a.diff));
    }
    out.exit_code = if errors > 0 || ((a.check || a.diff) && changed > 0) {
        EXIT_ERROR
    } else {
        0
    };
    if json {
        out.stdout = format!(
            "{}\n",
            json!({
                "schema": 1,
                "exit_code": out.exit_code,
                "mode": mode(&a),
                "counts": {"files": files.len(), "changed": changed, "errors": errors},
                "files": results,
            })
        )
        .into_bytes();
    } else if a.check && changed == 0 && errors == 0 {
        out.stderr
            .extend_from_slice(format!("{} files already formatted\n", files.len()).as_bytes());
    }
    out.emit()
}

fn mode(a: &Args) -> &'static str {
    if a.diff {
        "diff"
    } else if a.check {
        "check"
    } else if a.stdin {
        "stdin"
    } else {
        "write"
    }
}

/// Replace a file's contents (not through a temporary file: the file keeps
/// its permissions and any hard links).
fn write(path: &Path, text: &[u8]) -> std::io::Result<()> {
    std::fs::write(path, text)
}

fn stdin_outcome(f: &Formatted, a: &Args, json: bool) -> Outcome {
    let failed = f.result.is_err();
    let exit_code = if failed || ((a.check || a.diff) && f.changed()) {
        EXIT_ERROR
    } else {
        0
    };
    if json {
        let mut v: Value = f.json(!a.check && !a.diff, a.diff);
        v["schema"] = json!(1);
        v["exit_code"] = json!(exit_code);
        v["mode"] = json!(mode(a));
        return Outcome {
            exit_code,
            stderr: Vec::new(),
            stdout: format!("{v}\n").into_bytes(),
        };
    }
    match &f.result {
        Err(e) => fail(format!("{}: {e}", f.display)),
        Ok(text) => Outcome {
            exit_code,
            stderr: Vec::new(),
            stdout: if a.diff {
                f.diff().into_bytes()
            } else if a.check {
                if f.changed() {
                    format!("would reformat {}\n", f.display).into_bytes()
                } else {
                    Vec::new()
                }
            } else {
                text.clone()
            },
        },
    }
}
