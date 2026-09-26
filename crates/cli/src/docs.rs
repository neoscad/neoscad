//! `neoscad docs [NAME] [--in FILE] [--format json]`: the reference of a
//! builtin module, function or special variable, or with `--in FILE` of a
//! module or function the file defines, includes or `use`s, from its
//! comment block (`session::docs`). No name: a compact index. Terse by
//! default; `--full` shows a definition's whole comment block.

use std::ffi::OsString;

use clap::Parser;
use session::docs::DocsRequest;

use crate::outcome::Outcome;

const EXIT_ERROR: u8 = 1;

#[derive(Parser, Debug)]
#[command(
    name = "neoscad docs",
    about = "Reference for builtins, and for the modules and functions of a file (--in)",
    version
)]
struct Args {
    /// A builtin, or a module or function of --in FILE; none for an index.
    name: Option<String>,

    /// Also look in this file, what it includes and the libraries it uses.
    #[arg(long = "in", value_name = "FILE")]
    file: Option<String>,

    /// A definition's whole comment block, not the compact form.
    #[arg(long)]
    full: bool,

    /// `json`: the entries as one JSON object.
    #[arg(long, value_name = "FORMAT")]
    format: Option<String>,
}

/// Run `neoscad docs` with the arguments after `docs`.
pub fn main(args: Vec<OsString>) -> u8 {
    let argv = std::iter::once(OsString::from("neoscad docs")).chain(args);
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
        Some(f) => {
            return Outcome::fail(
                EXIT_ERROR,
                format!("neoscad docs: unknown --format '{f}' (only json)"),
            )
            .emit();
        }
    };
    let host = crate::host::Host::from_env();
    let session = session::Session::new(host.session_config(0));
    let r = session.docs(&DocsRequest {
        name: a.name,
        file: a.file,
        cwd: Some(std::env::current_dir().unwrap_or_default()),
        full: a.full,
        brief: false,
    });
    let (stdout, stderr) = if json {
        (format!("{}\n", r.json).into_bytes(), Vec::new())
    } else if r.exit_code == 0 {
        (r.text.into_bytes(), Vec::new())
    } else {
        (Vec::new(), r.text.into_bytes())
    };
    Outcome {
        exit_code: r.exit_code,
        stderr,
        stdout,
    }
    .emit()
}
