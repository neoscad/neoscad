//! `conformance`: runs OpenSCAD's regression suite against neoscad.
//!
//! - `conformance manifest` regenerates `conformance/manifest.json` from the
//!   reference checkout's `tests/CMakeLists.txt`.
//! - `conformance run` executes it, gates on `conformance/baseline.json` and
//!   with `--record` writes a progress snapshot.
//! - `conformance grid` renders snapshots' `grid.png` from their data.
//! - `conformance showcase` checks the showcase list.
//! - `conformance diff` compares neoscad with a reference OpenSCAD binary
//!   on a corpus of inputs.
//!
//! See crates/conformance/README.md.

mod cmake;
mod ctx;
mod diff;
mod grid;
mod manifest;
mod normalize;
mod prepare;
mod record;
mod run;
mod sha256;
mod showcase;

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use clap::{Parser, Subcommand};

use crate::ctx::{Ctx, REF_REL};

#[derive(Parser, Debug)]
#[command(name = "conformance", about = "OpenSCAD regression suite runner for neoscad")]
struct Cli {
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// Regenerate conformance/manifest.json from the reference checkout.
    Manifest {
        /// Only check that the committed manifest is up to date.
        #[arg(long)]
        check: bool,
    },
    /// Run the runnable cases and compare outputs.
    Run {
        /// Only run these tiers (repeatable).
        #[arg(long)]
        tier: Vec<u8>,
        /// Only run tests whose id contains this substring.
        #[arg(long)]
        filter: Option<String>,
        /// Show the first differing lines of each failure.
        #[arg(long, short)]
        verbose: bool,
        /// Per-case timeout in seconds.
        #[arg(long, default_value_t = 30.0)]
        timeout: f64,
        /// Parallel jobs (default: one per CPU).
        #[arg(long, short)]
        jobs: Option<usize>,
        /// Binary under test (default: target/release/neoscad). Pointing this
        /// at an OpenSCAD build checks the harness itself.
        #[arg(long)]
        binary: Option<PathBuf>,
        /// Rewrite conformance/baseline.json from the current passes.
        #[arg(long)]
        update_baseline: bool,
        /// Write a progress snapshot under progress/.
        #[arg(long)]
        record: bool,
        /// With --record, also render the snapshot's grid.png now.
        #[arg(long, requires = "record")]
        grid: bool,
    },
    /// Render grid.png for progress snapshots from their recorded data.
    Grid {
        /// Snapshot directories (a path, or a name under progress/).
        dirs: Vec<PathBuf>,
        /// Every snapshot listed in progress/index.jsonl.
        #[arg(long)]
        all: bool,
        /// Write here instead of <dir>/grid.png (one snapshot only).
        #[arg(long)]
        out: Option<PathBuf>,
        /// Re-render snapshots that already have a grid.png.
        #[arg(long)]
        force: bool,
    },
    /// Check that every showcase input and expected image exists.
    Showcase,
    /// Differential test: run a reference OpenSCAD and neoscad on each input
    /// and compare exit status, output and the format's diagnostics.
    Diff {
        /// Output format to compare: ast, echo or csg.
        #[arg(long, default_value = "ast")]
        format: String,
        /// Reference binary (default: the pinned nightly).
        #[arg(long, default_value = diff::DEFAULT_REFERENCE)]
        binary_ref: PathBuf,
        /// Binary under test (default: target/release/neoscad).
        #[arg(long)]
        binary: Option<PathBuf>,
        /// Parallel jobs (default: one per CPU).
        #[arg(long, short)]
        jobs: Option<usize>,
        /// Per-run timeout in seconds.
        #[arg(long, default_value_t = 60.0)]
        timeout: f64,
        /// List every mismatch, not just the first few per category.
        #[arg(long, short)]
        verbose: bool,
        /// Files or directories (searched for .scad). Default: the reference's
        /// tests/data/scad, examples and libraries/MCAD.
        paths: Vec<PathBuf>,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match dispatch(cli.command) {
        Ok(code) => ExitCode::from(code),
        Err(e) => {
            eprintln!("conformance: {e}");
            ExitCode::from(2)
        }
    }
}

fn dispatch(cmd: Cmd) -> Result<u8, String> {
    let ctx = Ctx::discover()?;
    match cmd {
        Cmd::Manifest { check } => manifest_cmd(&ctx, check),
        Cmd::Run { tier, filter, verbose, timeout, jobs, binary, update_baseline, record, grid } => {
            if timeout.is_nan() || timeout <= 0.0 {
                return Err("--timeout must be positive".into());
            }
            let opts = run::RunOptions {
                tiers: tier,
                filter,
                verbose,
                timeout: Duration::from_secs_f64(timeout),
                jobs,
                binary,
                update_baseline,
                record,
                grid,
            };
            run::run(&ctx, &opts).map(|c| u8::try_from(c).unwrap_or(1))
        }
        Cmd::Grid { dirs, all, out, force } => grid::command(&ctx, &dirs, all, out.as_deref(), force),
        Cmd::Showcase => Ok(u8::from(showcase::check(&ctx)? > 0)),
        Cmd::Diff { format, binary_ref, binary, jobs, timeout, verbose, paths } => {
            if timeout.is_nan() || timeout <= 0.0 {
                return Err("--timeout must be positive".into());
            }
            let opts = diff::DiffOptions {
                format: diff::Format::parse(&format)?,
                reference: binary_ref,
                binary,
                paths,
                jobs,
                timeout: Duration::from_secs_f64(timeout),
                verbose,
            };
            diff::diff(&ctx, &opts)
        }
    }
}

/// Variables a configured OpenSCAD build would give tests/CMakeLists.txt:
/// a macOS in-tree build (`PROJECT_IS_TOP_LEVEL` false) with Manifold,
/// lib3mf and EXPERIMENTAL on, as OpenSCAD's CI and snapshots use.
/// Experimental tests are registered so they can be listed as skipped.
fn cmake_vars(ctx: &Ctx) -> HashMap<String, String> {
    let r = ctx.ref_str();
    [
        ("CMAKE_SOURCE_DIR", r.clone()),
        ("CMAKE_BINARY_DIR", format!("{r}/build")),
        ("CMAKE_CURRENT_SOURCE_DIR", format!("{r}/tests")),
        ("CMAKE_CURRENT_BINARY_DIR", format!("{r}/build/tests")),
        ("CMAKE_COMMAND", "cmake".into()),
        ("PROJECT_IS_TOP_LEVEL", "OFF".into()),
        ("APPLE", "1".into()),
        ("UNIX", "1".into()),
        ("EXPERIMENTAL", "ON".into()),
        ("ENABLE_MANIFOLD", "ON".into()),
        ("LIB3MF_FOUND", "TRUE".into()),
        ("Python3_EXECUTABLE", "python3".into()),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v))
    .collect()
}

fn manifest_cmd(ctx: &Ctx, check: bool) -> Result<u8, String> {
    let eval = cmake::Interpreter::new(cmake_vars(ctx))
        .run_file(&ctx.ref_root.join("tests/CMakeLists.txt"))?;
    let m = manifest::build(&eval, &ctx.ref_str(), REF_REL, &ctx.reference_commit());
    let text = m.to_text()?;
    let path = ctx.manifest_path();

    for d in eval.messages.iter().chain(&m.diagnostics) {
        eprintln!("note: {d}");
    }
    println!("{:<4} {:<9} {:>6} {:>6} {:>8} {:>6} {:>8}", "tier", "name", "total", "text", "pending", "skip", "no-exp");
    for (t, c) in &m.counts {
        let name = t.parse::<usize>().ok().and_then(|i| manifest::TIER_NAMES.get(i)).copied().unwrap_or("?");
        println!(
            "{:<4} {:<9} {:>6} {:>6} {:>8} {:>6} {:>8}",
            t, name, c.total, c.text, c.pending, c.skip, c.missing_expected
        );
    }
    println!("skip reasons:");
    for (r, n) in &m.skip_reasons {
        println!("  {n:>5}  {r}");
    }

    if check {
        let current = std::fs::read_to_string(&path).unwrap_or_default();
        if current != text {
            eprintln!("{} is out of date; run `conformance manifest`", path.display());
            return Ok(1);
        }
        println!("{} is up to date", path.display());
        return Ok(0);
    }
    std::fs::write(&path, text).map_err(|e| format!("{}: {e}", path.display()))?;
    println!("wrote {} ({} tests)", path.display(), m.tests.len());
    Ok(0)
}
