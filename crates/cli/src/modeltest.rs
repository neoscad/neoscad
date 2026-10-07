//! `neoscad test [PATHS...] [--filter S] [--format json]`: run model
//! tests (`session::modeltest`, `docs/model-tests.md`). Each `module
//! test_*()` of a `*_test.scad` or `test_*.scad` file is a test; it passes
//! when evaluation has no error (a failed `assert()` is one) and its
//! `// @expect` lines hold. Exit status 1 when a test fails or none is
//! found.

use std::ffi::OsString;

use clap::Parser;
use session::modeltest::TestRequest;

use crate::outcome::Outcome;

const EXIT_ERROR: u8 = 1;

#[derive(Parser, Debug)]
#[command(
    name = "neoscad test",
    about = "Run model tests: `module test_*()` in *_test.scad / test_*.scad files, with `// @expect` lines",
    version
)]
pub(crate) struct Args {
    /// Test files, or directories searched for them; default: the
    /// current directory.
    paths: Vec<String>,

    /// Only tests whose id (`file::test_name`) contains this.
    #[arg(long, value_name = "S")]
    filter: Option<String>,

    /// `json`: one JSON object with every test's result.
    #[arg(long, value_name = "FORMAT")]
    format: Option<String>,

    /// NeoSCAD's extensions (`part`: the `part("name") { ... }` module; a
    /// test with `@expect parts` has it anyway) and OpenSCAD's experimental
    /// features, for every test.
    #[arg(long, value_name = "FEATURE", action = clap::ArgAction::Append)]
    enable: Vec<String>,

    /// Tests run at once (default: one per CPU).
    #[arg(short = 'j', long, value_name = "N")]
    jobs: Option<usize>,
}

/// Run `neoscad test` with the arguments after `test`.
pub fn main(args: Vec<OsString>) -> u8 {
    let argv = std::iter::once(OsString::from("neoscad test")).chain(args);
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
                format!("neoscad test: unknown --format '{f}' (only json)"),
            )
            .emit();
        }
    };
    let host = crate::host::Host::from_env();
    let session = session::Session::new(host.session_config(0));
    let jobs = a.jobs.unwrap_or_else(|| {
        std::thread::available_parallelism().map_or(1, std::num::NonZeroUsize::get)
    });
    let req = TestRequest {
        paths: a.paths,
        cwd: Some(std::env::current_dir().unwrap_or_default()),
        filter: a.filter,
        extensions: crate::extensions(&a.enable),
        features: crate::features(&a.enable),
        jobs: jobs.max(1),
    };
    let report = match session.test(&req) {
        Ok(r) => r,
        Err(e) => return Outcome::fail(EXIT_ERROR, format!("neoscad test: {e}")).emit(),
    };
    Outcome {
        exit_code: report.exit_code,
        stderr: Vec::new(),
        stdout: if json {
            format!("{}\n", report.json).into_bytes()
        } else {
            session::modeltest::text(&report.json).into_bytes()
        },
    }
    .emit()
}
