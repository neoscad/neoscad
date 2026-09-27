//! `conformance`: runs OpenSCAD's regression suite against neoscad.
//!
//! - `conformance manifest` regenerates `conformance/manifest.json` from the
//!   reference checkout's `tests/CMakeLists.txt`.
//! - `conformance run` executes it, gates on `conformance/baseline.json` and
//!   with `--record` writes a progress snapshot.
//! - `conformance grid` renders snapshots' `grid.png` from their data.
//! - `conformance showcase` checks the showcase list.
//! - `conformance images` surveys neoscad's renderer on every render-mode
//!   PNG case.
//! - `conformance diff` compares neoscad with a reference OpenSCAD binary
//!   on a corpus of inputs.
//! - `conformance bench` times neoscad against reference binaries on
//!   `conformance/bench.json`; `conformance bench-chart` draws a result.
//! - `conformance video` stitches the recorded snapshots, benchmarks and
//!   agent eval into a progress video.
//!
//! See crates/conformance/README.md.

mod bench;
mod bench_chart;
mod cmake;
mod ctx;
mod diff;
mod edit_loop;
mod geometry;
mod grid;
mod image_compare;
mod manifest;
mod normalize;
mod prepare;
mod record;
mod run;
mod script;
mod sha256;
mod showcase;
mod validatestl;
mod video;

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use clap::{Parser, Subcommand};

use crate::ctx::{Ctx, REF_REL};

#[derive(Parser, Debug)]
#[command(
    name = "conformance",
    about = "OpenSCAD regression suite runner for neoscad"
)]
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
        /// Renderer for tier 3 geometry cases: draws the exported meshes.
        #[arg(long, default_value = diff::DEFAULT_REFERENCE)]
        renderer: PathBuf,
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
    /// Compare two PNGs as OpenSCAD's tests/image_compare.py does; exits 0
    /// when they match.
    ImageCompare { expected: PathBuf, actual: PathBuf },
    /// Survey neoscad's own renderer: draw every PNG case (tier 3's direct
    /// `--render` images and tier 4's image cases: render mode, previews,
    /// throwntogether, view options) with neoscad and compare with the
    /// expected image under tier 4's rules, reported per kind. A
    /// diagnostic only: tier 3 still checks geometry through the nightly,
    /// and nothing here touches the baseline.
    Images {
        /// Only cases whose id contains this substring.
        #[arg(long)]
        filter: Option<String>,
        /// List every case that fails both rules.
        #[arg(long, short)]
        verbose: bool,
        /// Per-case timeout in seconds.
        #[arg(long, default_value_t = 30.0)]
        timeout: f64,
        /// Parallel jobs (default: one per CPU).
        #[arg(long, short)]
        jobs: Option<usize>,
        /// Binary under test (default: target/release/neoscad).
        #[arg(long)]
        binary: Option<PathBuf>,
    },
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
        /// A library directory for both binaries, searched before the
        /// reference's libraries/ (repeatable): lets a library's own files
        /// and examples keep their `include <Lib/...>` lines.
        #[arg(long = "library-path", value_name = "DIR")]
        library_path: Vec<PathBuf>,
        /// Files or directories (searched for .scad). Default: the reference's
        /// tests/data/scad, examples and libraries/MCAD.
        paths: Vec<PathBuf>,
    },
    /// Benchmark neoscad against the reference binaries on the models in
    /// conformance/bench.json; writes progress/bench/<UTC>-<sha>.json.
    Bench {
        /// Only these models (ids from bench.json, or cold_start,
        /// eval_only); repeatable or comma separated.
        #[arg(long, value_delimiter = ',')]
        only: Vec<String>,
        /// Only these references (neoscad, nightly-manifold, nightly-cgal,
        /// openscad-2021.01); repeatable or comma separated.
        #[arg(long, value_delimiter = ',')]
        refs: Vec<String>,
        /// neoscad and the nightly's Manifold backend only.
        #[arg(long)]
        quick: bool,
        /// Per-run timeout in seconds (default: bench.json's).
        #[arg(long)]
        timeout: Option<f64>,
        /// Runs per model, the best kept (default: bench.json's).
        #[arg(long)]
        runs: Option<u32>,
        /// neoscad binary (default: target/release/neoscad).
        #[arg(long)]
        binary: Option<PathBuf>,
    },
    /// Draw a benchmark result as a 1920x1080 PNG.
    BenchChart {
        /// A result file (a path, or a name under progress/bench/).
        file: Option<PathBuf>,
        /// The newest result (the default without FILE).
        #[arg(long)]
        latest: bool,
        /// Where to write the PNG (default: next to the result).
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// Render the progress video: one scene per snapshot in
    /// progress/index.jsonl, with benchmark and agent-eval interludes,
    /// encoded to H.264 by ffmpeg.
    Video {
        /// Output file (default: progress/video/progress.mp4).
        #[arg(long)]
        out: Option<PathBuf>,
        /// Frames per second.
        #[arg(long, default_value_t = 30)]
        fps: u32,
        /// Seconds each snapshot is held after its transition.
        #[arg(long, default_value_t = 2.0)]
        hold: f64,
        /// Keep the PNG frames in this directory (default: a temporary
        /// directory, deleted after encoding).
        #[arg(long)]
        frames_dir: Option<PathBuf>,
        /// The recorded data to read (default: this checkout's progress/).
        #[arg(long)]
        progress: Option<PathBuf>,
        /// The ffmpeg to encode with.
        #[arg(long, default_value = "ffmpeg")]
        ffmpeg: PathBuf,
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
    if let Cmd::Video {
        out,
        fps,
        hold,
        frames_dir,
        progress,
        ffmpeg,
    } = cmd
    {
        let opts = video::VideoOptions {
            out,
            fps,
            hold,
            frames_dir,
            progress,
            ffmpeg,
        };
        return video::command(&Ctx::repo_only()?, &opts);
    }
    let ctx = Ctx::discover()?;
    match cmd {
        Cmd::Manifest { check } => manifest_cmd(&ctx, check),
        Cmd::Run {
            tier,
            filter,
            verbose,
            timeout,
            jobs,
            binary,
            renderer,
            update_baseline,
            record,
            grid,
        } => {
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
                renderer,
                update_baseline,
                record,
                grid,
            };
            run::run(&ctx, &opts).map(|c| u8::try_from(c).unwrap_or(1))
        }
        Cmd::Grid {
            dirs,
            all,
            out,
            force,
        } => grid::command(&ctx, &dirs, all, out.as_deref(), force),
        Cmd::Showcase => Ok(u8::from(showcase::check(&ctx)? > 0)),
        Cmd::Images {
            filter,
            verbose,
            timeout,
            jobs,
            binary,
        } => {
            if timeout.is_nan() || timeout <= 0.0 {
                return Err("--timeout must be positive".into());
            }
            run::survey_images(
                &ctx,
                filter.as_deref(),
                verbose,
                Duration::from_secs_f64(timeout),
                jobs,
                binary,
            )
        }
        Cmd::ImageCompare { expected, actual } => {
            let c = image_compare::compare_files(&expected, &actual)?;
            if c.passed() {
                println!("3x3 image block comparison successfully passed.");
                Ok(0)
            } else {
                println!("{}", c.describe());
                Ok(1)
            }
        }
        Cmd::Diff {
            format,
            binary_ref,
            binary,
            jobs,
            timeout,
            verbose,
            library_path,
            paths,
        } => {
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
                library_path,
            };
            diff::diff(&ctx, &opts)
        }
        Cmd::Bench {
            only,
            refs,
            quick,
            timeout,
            runs,
            binary,
        } => {
            if timeout.is_some_and(|t| t.is_nan() || t <= 0.0) {
                return Err("--timeout must be positive".into());
            }
            let opts = bench::BenchOptions {
                only,
                refs,
                quick,
                timeout,
                runs,
                binary,
            };
            bench::bench(&ctx, &opts)
        }
        Cmd::BenchChart { file, latest, out } => {
            bench_chart::command(&ctx, file.as_deref(), latest, out.as_deref())
        }
        Cmd::Video { .. } => unreachable!("handled before the reference is required"),
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
    println!(
        "{:<4} {:<9} {:>6} {:>6} {:>8} {:>6} {:>8} {:>6} {:>8}",
        "tier", "name", "total", "text", "geometry", "script", "pending", "skip", "no-exp"
    );
    for (t, c) in &m.counts {
        let name = t
            .parse::<usize>()
            .ok()
            .and_then(|i| manifest::TIER_NAMES.get(i))
            .copied()
            .unwrap_or("?");
        println!(
            "{:<4} {:<9} {:>6} {:>6} {:>8} {:>6} {:>8} {:>6} {:>8}",
            t, name, c.total, c.text, c.geometry, c.script, c.pending, c.skip, c.missing_expected
        );
    }
    println!("skip reasons:");
    for (r, n) in &m.skip_reasons {
        println!("  {n:>5}  {r}");
    }

    if check {
        let current = std::fs::read_to_string(&path).unwrap_or_default();
        if current != text {
            eprintln!(
                "{} is out of date; run `conformance manifest`",
                path.display()
            );
            return Ok(1);
        }
        println!("{} is up to date", path.display());
        return Ok(0);
    }
    std::fs::write(&path, text).map_err(|e| format!("{}: {e}", path.display()))?;
    println!("wrote {} ({} tests)", path.display(), m.tests.len());
    Ok(0)
}
