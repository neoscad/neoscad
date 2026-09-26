//! `neoscad`: the command-line front end.
//!
//! The flags mirror OpenSCAD's own (`src/openscad.cc`, the
//! `desc.add_options()` block) so that OpenSCAD's regression suite can drive
//! this binary unchanged: the conformance harness passes exactly the
//! arguments `tests/CMakeLists.txt` registers. Nothing is implemented yet;
//! every output mode reports that it is missing and exits with
//! [`EXIT_NOT_IMPLEMENTED`], which the harness can tell apart from a crash or
//! a usage error.
//!
//! Cold start is a tracked benchmark (docs/architecture.md, "Agent surface"),
//! so `main` does nothing before argument parsing and nothing expensive after.

use std::path::Path;
use std::process::ExitCode;

use clap::{ArgAction, Parser};

/// Exit code for "this output mode exists in OpenSCAD but not in neoscad yet".
/// OpenSCAD itself exits with 1 for every error, and clap exits with 2 on a
/// usage error, so 3 is unambiguous in conformance results.
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
    /// csg, dxf, svg, pdf, png, echo, ast, term, nef3, nefdbg, param, pov).
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

    /// Enable an experimental feature ('all' enables every one).
    #[arg(long, value_name = "FEATURE", action = ArgAction::Append)]
    enable: Vec<String>,

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

    /// Export N animated frames.
    #[arg(long, value_name = "N")]
    animate: Option<u32>,

    /// Print a summary of the rendered model.
    #[arg(long, value_name = "WHAT", action = ArgAction::Append)]
    summary: Vec<String>,

    /// Write the summary to a file.
    #[arg(long = "summary-file", value_name = "FILE")]
    summary_file: Option<String>,

    /// Write a make-style dependency file.
    #[arg(short = 'd', value_name = "DEPS_FILE")]
    deps_file: Option<String>,

    /// Run this make command for missing files.
    #[arg(short = 'm', value_name = "MAKE_CMD")]
    make_command: Option<String>,

    /// Quiet mode: print nothing except errors.
    #[arg(short = 'q', long)]
    quiet: bool,

    /// Stop on the first warning.
    #[arg(long)]
    hardwarnings: bool,

    /// Maximum number of trace messages.
    #[arg(long = "trace-depth", value_name = "N")]
    trace_depth: Option<u32>,

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
}

fn main() -> ExitCode {
    let cli = Cli::parse();

    if cli.version {
        println!("neoscad {}", env!("CARGO_PKG_VERSION"));
        return ExitCode::SUCCESS;
    }

    // OpenSCAD opens its GUI when no -o is given. neoscad has no GUI in this
    // binary, so a missing -o is a usage error rather than a silent no-op.
    if cli.output.is_empty() {
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

    // An unknown --export-format is rejected before any output is attempted,
    // with OpenSCAD's wording (openscad.cc, "Unknown --export-format option").
    if let Some(fmt) = &cli.export_format
        && lookup_format(fmt).is_none()
    {
        eprintln!("Unknown --export-format option '{fmt}'.  Use -h to list available options.");
        return ExitCode::from(EXIT_ERROR);
    }

    let mut formats = Vec::with_capacity(cli.output.len());
    for output in &cli.output {
        match resolve_format(output, cli.export_format.as_deref()) {
            Ok(f) => formats.push(f),
            Err(suffix) => {
                // Same text as OpenSCAD's cmdline() so scripts see one message.
                eprintln!(
                    "Invalid suffix {suffix}. Either add a valid suffix or specify one using the --export-format option."
                );
                return ExitCode::from(EXIT_ERROR);
            }
        }
    }

    for (id, name) in &formats {
        eprintln!("neoscad: {name} export ({id}) is not implemented yet");
    }
    ExitCode::from(EXIT_NOT_IMPLEMENTED)
}

fn lookup_format(id: &str) -> Option<(&'static str, &'static str)> {
    FORMATS.iter().copied().find(|(name, _)| *name == id)
}

/// Pick the output format the way OpenSCAD's `cmdline()` does: an explicit
/// `--export-format` wins, otherwise the lower-cased extension of the output
/// path. `-` (stdout) has no extension, so it needs `--export-format`.
/// On failure returns the suffix that was not recognised.
fn resolve_format(
    output: &str,
    export_format: Option<&str>,
) -> Result<(&'static str, &'static str), String> {
    if let Some(id) = export_format {
        return lookup_format(id).ok_or_else(|| id.to_string());
    }
    let suffix = Path::new(output)
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    lookup_format(&suffix).ok_or(suffix)
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
        assert_eq!(resolve_format("out/x-actual.ECHO", None).unwrap().0, "echo");
        assert_eq!(resolve_format("x.stl", None).unwrap().1, "STL (ascii)");
        assert_eq!(resolve_format("-", None), Err(String::new()));
        assert_eq!(resolve_format("-", Some("ast")).unwrap().0, "ast");
        assert!(resolve_format("x.txt", None).is_err());
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
}
