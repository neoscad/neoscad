//! `neoscad`: the command-line front end.
//!
//! The flags mirror OpenSCAD's own (`src/openscad.cc`, the
//! `desc.add_options()` block) so that OpenSCAD's regression suite can drive
//! this binary unchanged: the conformance harness passes exactly the
//! arguments `tests/CMakeLists.txt` registers. The `.ast` export (with
//! customizer parameter sets), the `.echo` export (evaluation messages),
//! the `.csg` and `.term` node-tree exports, the `.param` customizer
//! export, the 3D mesh exports (`.stl` ASCII and binary, `.off`, `.obj`,
//! `.3mf`, `.wrl`, `.pov`) and the 2D exports (`.svg`, `.dxf`, `.pdf`) are
//! implemented, and `.png` ([`png`], drawn by the `render` crate: the
//! rendered geometry with `--render`, otherwise the OpenCSG or
//! throwntogether preview, with the `--view` options); every other output
//! mode (`.nef3`, `.nefdbg`)
//! reports that it is missing and exits with [`EXIT_NOT_IMPLEMENTED`],
//! which the harness can tell apart from a crash or a usage error.
//!
//! `-d`/`-m` are in [`deps`], `--summary`/`--summary-file` in [`summary`]
//! (its JSON is documented in `docs/cli-json.md`), `--info` and
//! `--help-export` in [`info`].
//!
//! `neoscad check` ([`check`]) and `neoscad measure` ([`measure`]) are
//! neoscad's printability checks and measurements, on the session like
//! snapshots. `neoscad snapshot` is neoscad's own subcommand, with its own flags
//! ([`snapshot`]): a contact sheet of a model for agents. `neoscad serve`
//! ([`serve`]) keeps a session's caches warm behind JSON-RPC; exports and
//! snapshots use a running one automatically ([`client`], [`delegate`]).
//! `neoscad lsp --stdio` ([`lsp`]) is the language server for editors.
//! `neoscad generate` ([`generate`]) prints the manual page and shell
//! completions, from these same clap definitions; [`SUBCOMMANDS`] lists
//! every subcommand for both dispatch and generation.
//! `--format json` prints one JSON object for the run ([`report`]).
//!
//! Cold start is a tracked benchmark (docs/architecture.md, "Agent surface"),
//! so `main` does nothing before argument parsing and nothing expensive after.
//! For the same reason the GPU frameworks are linked delay-init (`build.rs`).

// The linker warns on every link that it ignores delay-init for
// CoreGraphics, then records it anyway, and dyld honours it (see
// `delay_gpu_frameworks` in `build.rs`; a test checks that it still does).
// A warning that is known and checked would only train readers to skip the
// linker's output.
#![allow(linker_messages)]

mod bench;
mod check;
mod client;
mod delegate;
mod deps;
mod docs;
mod export_options;
mod format;
mod generate;
mod host;
mod info;
mod limits;
mod lsp;
mod mcp;
mod measure;
mod memory;
mod modeltest;
mod outcome;
mod param_json;
mod png;
mod report;
mod rpc;
mod run;
mod serve;
mod snapshot;
mod summary;
mod transport;
mod update;

use std::path::Path;
use std::process::ExitCode;

use clap::{ArgAction, Parser};

/// mimalloc rather than the system allocator, as OpenSCAD ships
/// (`USE_MIMALLOC`, on by default). Evaluation and geometry allocate
/// heavily (small values, then large vertex buffers); measured in the
/// performance audit's O1, mimalloc makes allocation-heavy models 7-15%
/// faster and cuts peak RSS by about a fifth. Allocation order never reaches
/// results, so output is unchanged. The WASM build keeps Rust's allocator.
#[cfg(all(feature = "mimalloc", not(target_arch = "wasm32")))]
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

/// Exit code for "this output mode exists in OpenSCAD but not in neoscad yet".
/// OpenSCAD itself exits with 1 for every error, usage errors included, and
/// neoscad's only exit 2 is a missing `-o`, so 3 is unambiguous in
/// conformance results.
const EXIT_NOT_IMPLEMENTED: u8 = 3;

/// OpenSCAD's general failure code (`return 1` throughout `openscad.cc`).
const EXIT_ERROR: u8 = 1;

/// Output formats OpenSCAD accepts, by identifier, in the order
/// `src/io/export.cc` registers them. `stl` is OpenSCAD's alias for
/// `asciistl`. An identifier is both an `-o` file extension and an
/// `--export-format` value.
const FORMATS: &[(&str, &str)] = &[
    ("asciistl", "STL (ascii)"),
    ("binstl", "STL (binary)"),
    ("stl", "STL (ascii)"),
    ("obj", "OBJ"),
    ("off", "OFF"),
    ("wrl", "VRML"),
    ("3mf", "3MF"),
    ("dxf", "DXF"),
    ("svg", "SVG"),
    ("nefdbg", "nefdbg"),
    ("nef3", "nef3"),
    ("csg", "CSG"),
    ("param", "param"),
    ("ast", "AST"),
    ("term", "term"),
    ("echo", "echo"),
    ("png", "PNG"),
    ("pdf", "PDF"),
    ("pov", "POV"),
];

/// Formats only NeoSCAD's extensions add: `--enable exact` makes `step`
/// (and its other common suffix `stp`) an output format. Without it they
/// are not formats at all, so `-o x.step` fails with OpenSCAD's own
/// "Invalid suffix" message, exactly as before the extension existed.
const EXACT_FORMATS: &[(&str, &str)] = &[("step", "STEP"), ("stp", "STEP")];

#[derive(Parser, Debug)]
#[command(
    name = "neoscad",
    about = "NeoSCAD: an OpenSCAD reimplementation (command-line mode)",
    disable_version_flag = true
)]
struct Cli {
    /// Input .scad file, or '-' for stdin.
    #[arg(value_name = "INPUT")]
    input: Vec<String>,

    /// Output file; its extension selects the format (stl, off, wrl, 3mf,
    /// csg, dxf, svg, pdf, png, echo, ast, term, nef3, nefdbg, param, pov;
    /// with `--enable exact` also step and stp: STEP AP214 with exact
    /// planes, cylinders, cones and spheres).
    /// Use '-' for stdout. May be given more than once.
    #[arg(short = 'o', value_name = "FILE", action = ArgAction::Append, allow_hyphen_values = true)]
    output: Vec<String>,

    /// Overrides the format chosen by the -o extension ('asciistl' and
    /// 'binstl' select the STL flavour).
    #[arg(long = "export-format", value_name = "FORMAT")]
    export_format: Option<String>,

    /// Pre-define a variable, e.g. -D 'a=3;'. May be given more than once.
    #[arg(short = 'D', value_name = "VAR=VAL", action = ArgAction::Append, allow_hyphen_values = true)]
    define: Vec<String>,

    /// Export setting as section/key=value, e.g. export-pdf/paper-size=a3.
    #[arg(short = 'O', value_name = "SECTION/KEY=VALUE", action = ArgAction::Append)]
    export_option: Vec<String>,

    /// Customizer parameter file (JSON).
    #[arg(short = 'p', value_name = "FILE")]
    parameter_file: Option<String>,

    /// Customizer parameter set name within the -p file.
    #[arg(short = 'P', value_name = "NAME")]
    parameter_set: Option<String>,

    /// `part` turns on NeoSCAD's `part("name") { ... }` extension (named
    /// parts for `check` and `measure`), `sketch` its constrained 2D
    /// sketches (`sketch() { ... }`), `query` its geometry queries
    /// (`anchor()`, `child_anchors()`, `child_bounds()`,
    /// `child_measure()`, `child_distance()`), `exact` STEP export
    /// (`-o x.step`) with exact surfaces where `$fn` is not set, `fillet`
    /// its `fillet_edges()` and `chamfer_edges()` (in development: the
    /// arguments are checked and the children pass through unchanged). `all`
    /// turns on OpenSCAD's experiments only, never NeoSCAD's extensions.
    /// OpenSCAD's experimental features
    /// `textmetrics`, `object-function`, `import-function`,
    /// `vector-swizzle` and `predictible-output` (sorted mesh exports) work
    /// as in OpenSCAD; the others are accepted for compatibility, with a
    /// warning for each one named.
    #[arg(long, value_name = "FEATURE", action = ArgAction::Append)]
    enable: Vec<String>,

    /// Print the -O export settings and their values, then exit.
    #[arg(long = "help-export")]
    help_export: bool,

    /// Print information about the build and the search paths, then exit.
    #[arg(long)]
    info: bool,

    /// Accepted for compatibility ('all' or source file names): neoscad
    /// prints OpenSCAD's "Debug on" line but has no debug output.
    #[arg(long, value_name = "WHAT")]
    debug: Option<String>,

    /// Camera: translate_x,y,z,rot_x,y,z,dist or eye_x,y,z,center_x,y,z.
    #[arg(long, value_name = "PARAMS", allow_hyphen_values = true)]
    camera: Option<String>,

    /// Adjust the camera to fit the object.
    #[arg(long)]
    viewall: bool,

    /// Adjust the camera to look at the object's centre.
    #[arg(long)]
    autocenter: bool,

    /// (o)rtho or (p)erspective projection for PNG export.
    #[arg(long, value_name = "o|p")]
    projection: Option<String>,

    /// Width,height of an exported PNG.
    #[arg(long, value_name = "W,H")]
    imgsize: Option<String>,

    /// Full geometry evaluation for PNG export ('--render=force' also for
    /// other formats). Takes its value only in '=' form, as OpenSCAD's
    /// implicit_value option does, so '--render input.scad' keeps the input.
    #[arg(long, value_name = "MODE", num_args = 0..=1, require_equals = true, default_missing_value = "")]
    render: Option<String>,

    /// Preview mode for PNG export ('--preview=throwntogether').
    #[arg(long, value_name = "MODE", num_args = 0..=1, require_equals = true, default_missing_value = "")]
    preview: Option<String>,

    /// 3D geometry backend: manifold or cgal.
    #[arg(long, value_name = "BACKEND")]
    backend: Option<String>,

    /// View options: axes, crosshairs, edges, scales (comma separated).
    #[arg(long, value_name = "OPTS", value_delimiter = ',', action = ArgAction::Append)]
    view: Vec<String>,

    /// Colour scheme for PNG export.
    #[arg(long, value_name = "SCHEME")]
    colorscheme: Option<String>,

    /// Stop rendering at N CSG elements when exporting PNG.
    #[arg(long, value_name = "N")]
    csglimit: Option<u32>,

    /// Export N animated frames ($t = frame / N), each to the output name
    /// with the frame number before the extension (x00000.stl, ...).
    #[arg(long, value_name = "N")]
    animate: Option<u32>,

    /// SHARD/NUM_SHARDS: export only the SHARD-th of NUM_SHARDS equal
    /// parts of the --animate frames (e.g. 2/5), to split the work.
    #[arg(long = "animate_sharding", value_name = "SHARD/NUM")]
    animate_sharding: Option<String>,

    /// Extra render summary: all, cache, time, camera, geometry,
    /// bounding-box or area. May be given more than once.
    #[arg(long, value_name = "WHAT", action = ArgAction::Append)]
    summary: Vec<String>,

    /// Write the summary as JSON to FILE ('-' for stdout) instead of
    /// printing it (schema: docs/cli-json.md).
    #[arg(long = "summary-file", value_name = "FILE")]
    summary_file: Option<String>,

    /// Write a make-style dependency file.
    #[arg(short = 'd', value_name = "DEPS_FILE")]
    deps_file: Option<String>,

    /// Run MAKE_CMD 'file' for a missing imported file before reading it.
    #[arg(short = 'm', value_name = "MAKE_CMD")]
    make_command: Option<String>,

    /// Quiet mode: print nothing except errors.
    #[arg(short = 'q', long)]
    quiet: bool,

    /// Stop on the first warning.
    #[arg(long)]
    hardwarnings: bool,

    /// A resource limit, NAME=VALUE (repeatable): time (s), memory (MiB,
    /// or 4G), fragments, slices, list, string, rands, triangles,
    /// sketch_unknowns, queries; 'off' for none. Unlimited by default, as
    /// OpenSCAD is. Also depth: nested module calls before 'Recursion
    /// detected' (default 100000; it cannot be off).
    #[arg(long = "limit", value_name = "NAME=VALUE", action = ArgAction::Append)]
    limit: Vec<String>,

    /// Maximum number of trace messages.
    #[arg(long = "trace-depth", value_name = "N")]
    trace_depth: Option<u32>,

    /// The seed of unseeded `rands()` (a 32-bit unsigned integer).
    /// Without it the seed comes from the clock and the process ID, as
    /// OpenSCAD's does, so two runs of a model that calls `rands()`
    /// without a seed differ; with it they repeat. OpenSCAD has no such
    /// option.
    #[arg(long, value_name = "N")]
    seed: Option<u32>,

    /// Trace user module parameters (true/false).
    #[arg(long = "trace-usermodule-parameters", value_name = "BOOL")]
    trace_usermodule_parameters: Option<String>,

    /// Check user module parameters (true/false).
    #[arg(long = "check-parameters", value_name = "BOOL")]
    check_parameters: Option<String>,

    /// Check parameter ranges for builtin modules (on/off).
    #[arg(long = "check-parameter-ranges", value_name = "BOOL")]
    check_parameter_ranges: Option<String>,

    /// Print the version and exit.
    #[arg(short = 'v', long)]
    version: bool,

    /// `json`: print one JSON object describing the run on stdout (on
    /// stderr when an output is '-') instead of the messages
    /// (docs/cli-json.md).
    #[arg(long, value_name = "FORMAT")]
    format: Option<String>,

    /// Export in this process even when a `neoscad serve` is running.
    #[arg(long = "no-server")]
    no_server: bool,
}

/// One of neoscad's own subcommands: the word that selects it, the function
/// that runs it on the arguments after that word, and its clap definition.
pub(crate) struct Subcommand {
    pub name: &'static str,
    pub run: fn(Vec<std::ffi::OsString>) -> u8,
    pub command: fn() -> clap::Command,
}

/// neoscad's own subcommands, in the order the manual page lists them.
///
/// This one table both dispatches them (`main`) and describes them to
/// `neoscad generate` (the manual page and the shell completions), so a
/// subcommand cannot be added to one without the other: when the dispatch
/// was a `match` of its own, nothing tied it to a second list, and the
/// generated documentation would have silently lost any command added
/// later.
pub(crate) const SUBCOMMANDS: &[Subcommand] = {
    use clap::CommandFactory;
    &[
        Subcommand {
            name: "check",
            run: check::main,
            command: check::Args::command,
        },
        Subcommand {
            name: "measure",
            run: measure::main,
            command: measure::Args::command,
        },
        Subcommand {
            name: "snapshot",
            run: snapshot::main,
            command: snapshot::Args::command,
        },
        Subcommand {
            name: "fmt",
            run: format::main,
            command: format::Args::command,
        },
        Subcommand {
            name: "test",
            run: modeltest::main,
            command: modeltest::Args::command,
        },
        Subcommand {
            name: "docs",
            run: docs::main,
            command: docs::Args::command,
        },
        Subcommand {
            name: "serve",
            run: serve::main,
            command: serve::Args::command,
        },
        Subcommand {
            name: "mcp",
            run: mcp::main,
            command: mcp::Args::command,
        },
        Subcommand {
            name: "lsp",
            run: lsp::main,
            command: lsp::Args::command,
        },
        Subcommand {
            name: "bench",
            run: bench::main,
            command: bench::Args::command,
        },
        Subcommand {
            name: "generate",
            run: generate::main,
            command: generate::Args::command,
        },
    ]
};

fn main() -> ExitCode {
    // The update notice (`update.rs`) checks in the background while the
    // command runs and prints after it, never delaying it.
    let notice = update::start(std::env::args_os().nth(1).as_deref());
    let code = run_main();
    update::finish(notice);
    code
}

fn run_main() -> ExitCode {
    // `neoscad snapshot ...` and the rest of `SUBCOMMANDS` are neoscad's
    // own commands, each with its own flags; everything else is OpenSCAD's
    // command line.
    let mut args = std::env::args_os();
    let first = args.nth(1);
    if first.as_deref() == Some(std::ffi::OsStr::new(update::CHILD_ARG)) {
        return ExitCode::from(update::child(args.collect()));
    }
    if let Some(first) = first
        && let Some(sub) = SUBCOMMANDS.iter().find(|s| first == s.name)
    {
        return ExitCode::from((sub.run)(args.collect()));
    }
    // OpenSCAD answers every command-line error (an unknown option, a
    // repeated single-valued one such as `--export-format`) with its usage
    // text and exit status 1 (`help(..., true)` in `openscad.cc`); clap
    // would exit with 2. Help and version requests still exit 0.
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(e) => {
            let _ = e.print();
            return if e.use_stderr() {
                ExitCode::from(EXIT_ERROR)
            } else {
                ExitCode::SUCCESS
            };
        }
    };
    // Parsing and evaluation recurse over the program; OpenSCAD programs
    // (and OpenSCAD's own tests) recurse deeper than the main thread's
    // default stack allows, and the evaluator's recursion limit assumes
    // this much stack (see `eval::Options::stack_limit`).
    eval::with_stack(eval::DEFAULT_THREAD_STACK, move || run_cli(cli))
}

/// The environment variable [`rich_diagnostics`] reads (named here for
/// the manual page's ENVIRONMENT section too).
pub const DIAGNOSTICS_ENV: &str = "NEOSCAD_DIAGNOSTICS";

/// Whether human-readable diagnostics on stderr get the source line and a
/// caret under the span.
///
/// Only when stderr is a terminal: OpenSCAD prints no such lines, and
/// everything that compares neoscad's output with OpenSCAD's (the
/// conformance harness, scripts, agents) captures stderr into a file or a
/// pipe, so detecting the terminal keeps them on the exact OpenSCAD text
/// with no flag to remember. A person at a terminal gets the helpful form
/// by default. `NEOSCAD_DIAGNOSTICS=openscad` forces the exact text on a
/// terminal too, `=rich` the excerpts in a pipe. Agents that want spans
/// should use `--format json`, which carries them as data.
pub fn rich_diagnostics() -> bool {
    use std::io::IsTerminal;
    match std::env::var(DIAGNOSTICS_ENV).as_deref() {
        Ok("openscad") => false,
        Ok("rich") => true,
        _ => std::io::stderr().is_terminal(),
    }
}

/// NeoSCAD's own extensions `--enable` names (`part`, `sketch`, `query`;
/// `eval::Options::extensions`). Only the exact names turn them on:
/// `--enable all` means OpenSCAD's experiments and leaves them off, so a
/// program run with OpenSCAD's flags behaves as it does in OpenSCAD.
pub fn extensions(names: &[String]) -> eval::Extensions {
    eval::Extensions::from_names(names)
}

/// OpenSCAD's experimental features `--enable` turns on (`all` is every
/// one of them), as the one set every host passes on
/// (`eval::Options::features`).
pub fn features(names: &[String]) -> eval::Features {
    eval::Features::from_names(names)
}

/// `--enable`: OpenSCAD switches the named features on (`all` switches on
/// every one and ends the list) and warns about unknown names. A feature
/// neoscad implements is switched on silently, as in OpenSCAD; one it
/// does not gets a warning instead of silently doing nothing, and an
/// unknown one gets OpenSCAD's own warning. Like OpenSCAD's, these are
/// warnings (dropped by `--quiet`), not errors.
pub fn enable_warnings(names: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    for name in names {
        // NeoSCAD's own names are accepted silently, including those
        // that do nothing yet: they are not OpenSCAD's, so its "unknown
        // feature" warning would be wrong for them.
        if eval::Extension::from_name(name).is_some() {
            continue;
        }
        if name == "all" {
            let missing: Vec<&str> = eval::Feature::ALL
                .iter()
                .filter(|f| !f.supported())
                .map(|f| f.name())
                .collect();
            if !missing.is_empty() {
                out.push(format!(
                    "WARNING: --enable all: experimental features not supported by neoscad are ignored: {}.",
                    missing.join(", ")
                ));
            }
            break;
        }
        match eval::Feature::from_name(name) {
            Some(f) if f.supported() => {}
            Some(_) => out.push(format!(
                "WARNING: Experimental feature '{name}' is not supported by neoscad; ignoring it."
            )),
            None => out.push(format!(
                "WARNING: Ignoring request to enable unknown feature '{name}'."
            )),
        }
    }
    out
}

/// `get_animate`: the frame range, or OpenSCAD's message for a bad
/// `--animate_sharding` (which exits 1).
fn animate_args(
    frames: Option<u32>,
    sharding: Option<&str>,
) -> Result<Option<run::Animate>, String> {
    let (mut shard, mut shards) = (1u32, 1u32);
    if let Some(s) = sharding {
        let parts: Vec<&str> = s.split('/').collect();
        if parts.len() != 2 {
            return Err("--animate_sharding requires <shard>/<num_shards>".into());
        }
        // `boost::lexical_cast<unsigned>`: digits only.
        let num = |p: &str| {
            (!p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
                .then(|| p.parse::<u32>().ok())
                .flatten()
        };
        match (num(parts[0]), num(parts[1])) {
            (Some(a), Some(b)) => (shard, shards) = (a, b),
            _ => return Err("--animate_sharding parameters need to be positive integers".into()),
        }
        if shard > shards || shard == 0 {
            return Err("--animate_sharding: shard needs to be in range <1..num_shards>".into());
        }
    }
    Ok(frames.filter(|&n| n > 0).map(|n| {
        let (n64, s, k) = (u64::from(n), u64::from(shard), u64::from(shards));
        run::Animate {
            frames: n,
            start: ((s - 1) * n64 / k) as u32,
            limit: (s * n64 / k) as u32,
        }
    }))
}

fn run_cli(cli: Cli) -> ExitCode {
    // Printed before anything else, and before --quiet takes effect.
    if let Some(d) = &cli.debug {
        eprintln!("Debug on. --debug={d}");
    }
    if cli.help_export {
        eprint!("{}", info::help_export());
        return ExitCode::SUCCESS;
    }
    if cli.version {
        println!("neoscad {}", env!("CARGO_PKG_VERSION"));
        return ExitCode::SUCCESS;
    }
    if cli.info {
        if cli.input.len() > 1 {
            eprintln!(
                "neoscad: expected at most one input file, got {}",
                cli.input.len()
            );
            return ExitCode::from(EXIT_ERROR);
        }
        print!("{}", info::info());
        return ExitCode::SUCCESS;
    }
    if !cli.quiet {
        for w in enable_warnings(&cli.enable) {
            eprintln!("{w}");
        }
    }

    // An unknown --export-format is rejected before any output is attempted,
    // with OpenSCAD's wording (openscad.cc, "Unknown --export-format option").
    let exact = extensions(&cli.enable).has(eval::Extension::Exact);
    if let Some(fmt) = &cli.export_format
        && lookup_format(fmt, exact).is_none()
    {
        eprintln!("Unknown --export-format option '{fmt}'.  Use -h to list available options.");
        return ExitCode::from(EXIT_ERROR);
    }
    let animate = match animate_args(cli.animate, cli.animate_sharding.as_deref()) {
        Ok(a) => a,
        Err(msg) => {
            eprintln!("{msg}");
            return ExitCode::from(EXIT_ERROR);
        }
    };
    let mut outputs = cli.output.clone();
    if animate.is_some() {
        if outputs.iter().any(|o| o == "-") {
            eprintln!("Option --animate is not supported when exporting to stdout.");
            return ExitCode::from(EXIT_ERROR);
        }
        // OpenSCAD's default animation target.
        if outputs.is_empty() {
            outputs.push("frame.png".into());
        }
    }

    // OpenSCAD opens its GUI when no -o is given. neoscad has no GUI in this
    // binary, so a missing -o is a usage error rather than a silent no-op.
    if outputs.is_empty() {
        eprintln!("neoscad: no output file given; use -o FILE (the extension selects the format)");
        return ExitCode::from(2);
    }
    // OpenSCAD's cmd-line mode takes exactly one input (`help(..., true)`).
    if cli.input.len() != 1 {
        eprintln!(
            "neoscad: expected exactly one input file, got {}",
            cli.input.len()
        );
        return ExitCode::from(2);
    }

    let json = match cli.format.as_deref() {
        None => false,
        Some("json") => true,
        Some(f) => {
            eprintln!("neoscad: unknown --format '{f}' (only json)");
            return ExitCode::from(EXIT_ERROR);
        }
    };
    if json {
        report::enable();
    }
    let started = std::time::Instant::now();
    deps::set_make_command(cli.make_command.clone());
    let code = export(&cli, &outputs, animate);
    if json && report::enabled() {
        let text = report::finish(&report::Run {
            command: "export",
            input: &cli.input[0],
            outputs: outputs
                .iter()
                .map(|o| {
                    let id =
                        resolve_format(o, cli.export_format.as_deref(), exact).map_or("", |f| f.0);
                    (o.clone(), id.to_string())
                })
                .collect(),
            exit_code: code,
            timings: serde_json::json!({
                "total": (started.elapsed().as_secs_f64() * 10000.0).round() / 10.0
            }),
            served: false,
        });
        if outputs.iter().any(|o| o == "-") {
            eprint!("{text}");
        } else {
            print!("{text}");
        }
    }
    // `write_deps` runs after every export, whatever their outcome.
    if let Some(d) = &cli.deps_file
        && !deps::write(d, &outputs)
    {
        return ExitCode::from(EXIT_ERROR);
    }
    ExitCode::from(code)
}

/// Every `-o` output, grouped the way neoscad evaluates them: formats that
/// share an evaluation run together.
fn export(cli: &Cli, outputs: &[String], animate: Option<run::Animate>) -> u8 {
    let mut formats = Vec::with_capacity(outputs.len());
    for output in outputs {
        match resolve_format(
            output,
            cli.export_format.as_deref(),
            extensions(&cli.enable).has(eval::Extension::Exact),
        ) {
            Ok(f) => formats.push(f),
            Err(suffix) => {
                // Same text as OpenSCAD's cmdline() so scripts see one message.
                eprintln!(
                    "Invalid suffix {suffix}. Either add a valid suffix or specify one using the --export-format option."
                );
                return EXIT_ERROR;
            }
        }
    }

    // `set_render_color_scheme(arg_colorscheme, true)` runs for every
    // export: the scheme also colours exported Manifold meshes.
    let scheme = match png::scheme(cli.colorscheme.as_deref()) {
        Ok(s) => s,
        Err(code) => return code,
    };
    let png_settings = if formats.iter().any(|(id, _)| *id == "png") {
        let camera = match png::camera(
            cli.camera.as_deref(),
            cli.viewall,
            cli.autocenter,
            cli.projection.as_deref(),
            cli.imgsize.as_deref(),
        ) {
            Ok(c) => c,
            Err(code) => return code,
        };
        Some(png::Settings {
            camera,
            scheme: scheme.clone(),
            previewer: png::previewer(cli.render.as_deref(), cli.preview.as_deref()),
            view: png::view_options(&cli.view, cli.quiet),
            csg_limit: cli
                .csglimit
                .map_or(geom::csg::DEFAULT_TERM_LIMIT, |n| n as usize),
        })
    } else {
        None
    };
    let export_options = export_options::ExportOptions::parse(&cli.export_option);
    let summary = summary::Request {
        options: cli.summary.clone(),
        file: cli.summary_file.clone(),
    };
    let job = run::Job {
        input: &cli.input[0],
        outputs,
        defines: &cli.define,
        parameter_file: cli.parameter_file.as_deref(),
        parameter_set: cli.parameter_set.as_deref(),
        quiet: cli.quiet,
        hardwarnings: cli.hardwarnings,
        export_options: &export_options,
        summary: &summary,
        animate,
        scheme: &scheme,
        png: png_settings.as_ref(),
        json: cli.format.as_deref() == Some("json"),
        rich: rich_diagnostics(),
    };
    if formats.iter().all(|(id, _)| *id == "ast") {
        return run::export_ast(&job);
    }
    if formats.iter().all(|(id, _)| *id == "echo") {
        return match eval_options(cli) {
            Ok(o) => run::export_echo(&job, &o),
            Err(code) => code,
        };
    }
    if formats.iter().all(|(id, _)| *id == "param") {
        return match eval_options(cli) {
            Ok(o) => run::export_param(&job, &o),
            Err(code) => code,
        };
    }
    let tree: Option<Vec<run::TreeFormat>> = formats
        .iter()
        .map(|(id, _)| match *id {
            "csg" => Some(run::TreeFormat::Csg),
            "term" => Some(run::TreeFormat::Term),
            _ => None,
        })
        .collect();
    if let Some(tree) = tree {
        return match eval_options(cli) {
            Ok(o) => run::export_tree(&job, &o, &tree),
            Err(code) => code,
        };
    }
    let mesh: Option<Vec<run::MeshFormat>> = formats
        .iter()
        .map(|(id, _)| match *id {
            "stl" | "asciistl" => Some(run::MeshFormat::AsciiStl),
            "binstl" => Some(run::MeshFormat::BinaryStl),
            "off" => Some(run::MeshFormat::Off),
            "obj" => Some(run::MeshFormat::Obj),
            "3mf" => Some(run::MeshFormat::ThreeMf),
            "wrl" => Some(run::MeshFormat::Wrl),
            "pov" => Some(run::MeshFormat::Pov),
            "svg" => Some(run::MeshFormat::Svg),
            "dxf" => Some(run::MeshFormat::Dxf),
            "pdf" => Some(run::MeshFormat::Pdf),
            "png" => Some(run::MeshFormat::Png),
            "step" => Some(run::MeshFormat::Step),
            _ => None,
        })
        .collect();
    if let Some(mesh) = mesh {
        let mut options = match eval_options(cli) {
            Ok(o) => o,
            Err(code) => return code,
        };
        // `$preview` is false for every geometry export, and for a PNG
        // drawn from the rendered geometry (`fileformat::canPreview`,
        // `openscad.cc:646-650`).
        options.preview = png::previewer(cli.render.as_deref(), cli.preview.as_deref()).is_some()
            && mesh.iter().all(|f| *f == run::MeshFormat::Png);
        if let Some(b) = cli.backend.as_deref()
            && !b.eq_ignore_ascii_case("manifold")
        {
            eprintln!("neoscad: only the manifold backend is implemented (got --backend={b})");
            return EXIT_NOT_IMPLEMENTED;
        }
        // `--render=force` (and the legacy `--render=cgal`) converts a mesh
        // result to a solid before export (`openscad.cc:1040-1041`).
        // `--preview` wins over it, as it does for images.
        let force =
            cli.preview.is_none() && matches!(cli.render.as_deref(), Some("force" | "cgal"));
        if let Some(code) = served_export(cli, outputs, &formats, &options, force, &scheme) {
            return code;
        }
        return run::export_mesh(&job, &options, &mesh, force);
    }

    for (id, name) in &formats {
        eprintln!("neoscad: {name} export ({id}) is not implemented yet");
    }
    EXIT_NOT_IMPLEMENTED
}

/// Run a plain geometry export on a running server (see [`delegate`]):
/// `Some(exit code)` when the server did it, `None` to run it here.
fn served_export(
    cli: &Cli,
    outputs: &[String],
    formats: &[(&'static str, &'static str)],
    options: &eval::Options,
    force: bool,
    scheme: &render::ColorScheme,
) -> Option<u8> {
    let local_only = cli.input.len() != 1
        || cli.input[0] == "-"
        || cli.animate.is_some()
        || cli.parameter_file.is_some()
        || cli.parameter_set.is_some()
        || cli.deps_file.is_some()
        || cli.make_command.is_some()
        || cli.summary_file.is_some()
        || cli.hardwarnings
        || !cli.limit.is_empty()
        || cli.trace_depth.is_some()
        || cli.trace_usermodule_parameters.is_some()
        || cli.check_parameters.is_some()
        || cli.check_parameter_ranges.is_some()
        // STEP runs here: its `--format json` report (`exact`, with the
        // normal render's time beside the export's stages, which
        // `conformance exact` reads) is the local run's, and a delegated
        // reply has no place for it.
        || formats.iter().any(|(id, _)| *id == "step")
        || !(formats
            .iter()
            .all(|(id, _)| session::export::Format::from_id(id).is_some())
            || formats.iter().all(|(id, _)| *id == "png"));
    if local_only {
        return None;
    }
    let socket = client::available(cli.no_server)?;
    let params = delegate::params(&delegate::Plan {
        input: &cli.input[0],
        outputs,
        formats: formats.iter().map(|(id, _)| *id).collect(),
        defines: &cli.define,
        export_options: &cli.export_option,
        summary: &cli.summary,
        scheme: &scheme.name,
        force,
        camera: options.camera,
        quiet: cli.quiet,
        json: cli.format.as_deref() == Some("json"),
        rich: rich_diagnostics(),
        seed: options.rng_seed,
        extensions: options.extensions,
        enable: &cli.enable,
        png: formats
            .iter()
            .all(|(id, _)| *id == "png")
            .then(|| delegate::PngArgs {
                camera: cli.camera.as_deref(),
                viewall: cli.viewall,
                autocenter: cli.autocenter,
                projection: cli.projection.as_deref(),
                imgsize: cli.imgsize.as_deref(),
                render: cli.render.as_deref(),
                preview: cli.preview.as_deref(),
                view: &cli.view,
                csglimit: cli.csglimit,
            }),
    });
    let outcome = client::run(&socket, "cli.export", params)?;
    // The server wrote the report: nothing more to print here.
    report::disable();
    Some(outcome.emit())
}

/// `flagConvert` in openscad.cc: the accepted spellings of a boolean flag.
fn flag(value: &Option<String>, default: bool, name: &str) -> Result<bool, u8> {
    let Some(v) = value else { return Ok(default) };
    let l = v.to_ascii_lowercase();
    match l.as_str() {
        "1" | "on" | "true" => Ok(true),
        "0" | "off" | "false" => Ok(false),
        _ => {
            eprintln!("neoscad: invalid value '{v}' for --{name} (use true/false, on/off or 1/0)");
            Err(EXIT_ERROR)
        }
    }
}

/// Evaluation settings from the command line, as `openscad.cc` derives
/// them (`RenderVariables`, `get_camera`, the `OpenSCAD::` flags).
fn eval_options(cli: &Cli) -> Result<eval::Options, u8> {
    let mut o = eval::Options {
        // `$preview` is true for preview-capable exports unless they draw
        // the rendered geometry (`--render` without `--preview`).
        preview: png::previewer(cli.render.as_deref(), cli.preview.as_deref()).is_some(),
        trace_usermodule_parameters: flag(
            &cli.trace_usermodule_parameters,
            true,
            "trace-usermodule-parameters",
        )?,
        check_parameters: flag(&cli.check_parameters, true, "check-parameters")?,
        check_parameter_ranges: flag(&cli.check_parameter_ranges, false, "check-parameter-ranges")?,
        hardwarnings: cli.hardwarnings,
        rng_seed: cli.seed.unwrap_or_else(host::entropy_seed),
        extensions: extensions(&cli.enable),
        features: features(&cli.enable),
        ..Default::default()
    };
    if let Some(d) = cli.trace_depth {
        o.trace_depth = d;
    }
    match limits::from_flags(limits::Limits::NONE, &cli.limit) {
        Ok(l) => {
            if let Some((guard, flag)) = limits::guard(l) {
                o.interrupt = Some(flag);
                o.guard = Some(guard);
            }
        }
        Err(e) => {
            eprintln!("neoscad: {e}");
            return Err(EXIT_ERROR);
        }
    }
    if let Some(cam) = &cli.camera {
        let nums: Result<Vec<f64>, _> = cam.split(',').map(|s| s.trim().parse::<f64>()).collect();
        let n = cam.split(',').count();
        if n != 6 && n != 7 {
            eprintln!(
                "Camera setup requires either 7 numbers for Gimbal Camera or 6 numbers for Vector Camera"
            );
            return Err(EXIT_ERROR);
        }
        match nums.ok().and_then(|v| eval::Camera::from_args(&v)) {
            Some(c) => o.camera = c,
            None => eprintln!("Camera setup requires numbers as parameters"),
        }
    }
    if cli.viewall || cli.autocenter {
        o.camera.auto = true;
    }
    Ok(o)
}

fn lookup_format(id: &str, exact: bool) -> Option<(&'static str, &'static str)> {
    FORMATS
        .iter()
        .copied()
        .find(|(name, _)| *name == id)
        .or_else(|| {
            exact
                .then(|| EXACT_FORMATS.iter().copied().find(|(name, _)| *name == id))
                .flatten()
                // One identifier for both suffixes.
                .map(|(_, desc)| ("step", desc))
        })
}

/// Pick the output format the way OpenSCAD's `cmdline()` does: an explicit
/// `--export-format` wins, otherwise the lower-cased extension of the output
/// path. `-` (stdout) has no extension, so it needs `--export-format`.
/// On failure returns the suffix that was not recognised.
fn resolve_format(
    output: &str,
    export_format: Option<&str>,
    exact: bool,
) -> Result<(&'static str, &'static str), String> {
    if let Some(id) = export_format {
        return lookup_format(id, exact).ok_or_else(|| id.to_string());
    }
    let suffix = Path::new(output)
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    lookup_format(&suffix, exact).ok_or(suffix)
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn cli_definition_is_consistent() {
        Cli::command().debug_assert();
    }

    #[test]
    fn extension_selects_format_case_insensitively() {
        let f = |o: &str, e: Option<&str>| resolve_format(o, e, false);
        assert_eq!(f("out/x-actual.ECHO", None).unwrap().0, "echo");
        assert_eq!(f("x.stl", None).unwrap().1, "STL (ascii)");
        assert_eq!(f("-", None), Err(String::new()));
        assert_eq!(f("-", Some("ast")).unwrap().0, "ast");
        assert!(f("x.txt", None).is_err());
    }

    /// `.step` is a format only with `--enable exact`: without it the
    /// suffix is unknown, as in OpenSCAD, and gets its "Invalid suffix".
    #[test]
    fn step_is_a_format_only_with_exact() {
        assert_eq!(resolve_format("x.step", None, false), Err("step".into()));
        assert_eq!(resolve_format("x.STP", None, false), Err("stp".into()));
        assert_eq!(resolve_format("-", Some("step"), false), Err("step".into()));
        assert_eq!(resolve_format("x.step", None, true).unwrap().0, "step");
        assert_eq!(resolve_format("x.STP", None, true).unwrap().0, "step");
        assert_eq!(resolve_format("-", Some("stp"), true).unwrap().0, "step");
        assert_eq!(resolve_format("x.stl", None, true).unwrap().0, "stl");
    }

    #[test]
    fn animate_sharding_splits_frames_as_openscad_does() {
        let a = |n, s: Option<&str>| animate_args(n, s);
        assert_eq!(a(None, None), Ok(None));
        assert_eq!(
            a(Some(4), None),
            Ok(Some(run::Animate {
                frames: 4,
                start: 0,
                limit: 4
            }))
        );
        // `--animate 4 --animate_sharding 2/2` exports frames 2 and 3 in
        // the nightly; 10 frames in 3 shards split 0..3, 3..6, 6..10.
        assert_eq!(
            a(Some(4), Some("2/2")).unwrap().map(|x| (x.start, x.limit)),
            Some((2, 4))
        );
        assert_eq!(
            a(Some(10), Some("3/3"))
                .unwrap()
                .map(|x| (x.start, x.limit)),
            Some((6, 10))
        );
        assert_eq!(
            a(Some(4), Some("3/2")),
            Err("--animate_sharding: shard needs to be in range <1..num_shards>".into())
        );
        assert_eq!(
            a(Some(4), Some("a/2")),
            Err("--animate_sharding parameters need to be positive integers".into())
        );
        assert_eq!(
            a(None, Some("2")),
            Err("--animate_sharding requires <shard>/<num_shards>".into())
        );
    }

    #[test]
    fn enable_warns_instead_of_silently_ignoring() {
        let w = |names: &[&str]| {
            enable_warnings(&names.iter().map(|s| s.to_string()).collect::<Vec<_>>())
        };
        // The nightly's own text for an unknown name.
        assert_eq!(
            w(&["foo"]),
            ["WARNING: Ignoring request to enable unknown feature 'foo'."]
        );
        assert_eq!(
            w(&["roof"]),
            ["WARNING: Experimental feature 'roof' is not supported by neoscad; ignoring it."]
        );
        // Implemented features are switched on without a word.
        assert!(w(&["textmetrics", "object-function", "vector-swizzle"]).is_empty());
        // `all` ends the list, as in OpenSCAD.
        assert_eq!(w(&["all", "foo"]).len(), 1);
        // NeoSCAD's own names are not OpenSCAD's unknown features.
        assert!(
            w(&[
                "predictible-output",
                "part",
                "sketch",
                "query",
                "exact",
                "fillet"
            ])
            .is_empty()
        );
        let names = |n: &[&str]| n.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let sorted = |n: &[&str]| features(&names(n)).has(eval::Feature::PredictibleOutput);
        assert!(sorted(&["predictible-output"]));
        assert!(sorted(&["all"]));
        assert!(!sorted(&["roof", "part"]));
        // `all` is OpenSCAD's experiments, never NeoSCAD's extensions.
        assert_eq!(extensions(&names(&["all"])), eval::Extensions::NONE);
        let on = extensions(&names(&["all", "sketch", "query"]));
        assert!(on.has(eval::Extension::Sketch) && on.has(eval::Extension::Query));
        assert!(!on.has(eval::Extension::Part));
    }

    #[test]
    fn frame_targets_number_before_the_extension() {
        assert_eq!(run::frame_target("out/x.stl", Some(3)), "out/x00003.stl");
        assert_eq!(run::frame_target("a.b.stl", Some(12)), "a.b00012.stl");
        assert_eq!(run::frame_target("x", Some(0)), "x00000");
        assert_eq!(run::frame_target("x.stl", None), "x.stl");
    }

    /// The exact argument shapes tests/CMakeLists.txt passes for tiers 0-2.
    #[test]
    fn parses_openscad_test_argument_shapes() {
        let cli = Cli::try_parse_from([
            "neoscad",
            "/abs/in.scad",
            "--camera=0,0,100,0,0,0",
            "--viewall",
            "--autocenter",
            "--projection=ortho",
            "-D",
            "a=3;",
            "-p",
            "x.json",
            "-P",
            "Name.dot",
            "--trace-usermodule-parameters=false",
            "--check-parameter-ranges=on",
            "--quiet",
            "--enable",
            "object-function",
            "--render",
            "--backend=manifold",
            "-o",
            "-",
            "--export-format",
            "echo",
        ])
        .unwrap();
        assert_eq!(cli.input, ["/abs/in.scad"]);
        assert_eq!(cli.output, ["-"]);
        assert_eq!(cli.define, ["a=3;"]);
        assert_eq!(cli.render.as_deref(), Some(""));
        assert_eq!(cli.parameter_set.as_deref(), Some("Name.dot"));

        let cli = Cli::try_parse_from([
            "neoscad",
            "in.scad",
            "--camera",
            "10,20,30,40,50,60",
            "-Dfile=\"a.svg\";",
            "--render=force",
            "--view",
            "axes,scales",
            "-o",
            "x.png",
        ])
        .unwrap();
        assert_eq!(cli.camera.as_deref(), Some("10,20,30,40,50,60"));
        assert_eq!(cli.define, ["file=\"a.svg\";"]);
        assert_eq!(cli.render.as_deref(), Some("force"));
        assert_eq!(cli.view, ["axes", "scales"]);
    }

    /// `--seed` reaches unseeded `rands()`: PGO training (scripts/pgo-train.py)
    /// relies on it to run BOSL2's unseeded tests the same way every time.
    #[test]
    fn seed_flag_sets_the_rands_seed() {
        let cli = Cli::try_parse_from(["neoscad", "in.scad", "--seed", "4242"]).unwrap();
        assert_eq!(cli.seed, Some(4242));
        assert_eq!(eval_options(&cli).ok().map(|o| o.rng_seed), Some(4242));
        assert!(Cli::try_parse_from(["neoscad", "in.scad", "--seed", "-1"]).is_err());
    }
}
