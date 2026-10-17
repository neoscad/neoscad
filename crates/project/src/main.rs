//! `scad-deps`: the dependency manager's command line, for stock OpenSCAD
//! and other tools (`neoscad` has the same commands built in).

use std::ffi::OsString;

use scad_deps::cli::{COMMANDS, Tool};

/// The top-level help: the commands and what each does.
fn command() -> clap::Command {
    let tool = Tool::SCAD_DEPS;
    let mut cmd = clap::Command::new(tool.name)
        .version(tool.version)
        .about("Dependencies for OpenSCAD projects: scad.toml, scad.lock and a shared store")
        .after_help(
            "Render with stock OpenSCAD:\n  OPENSCADPATH=$(scad-deps fetch --print-path) openscad -o main.stl main.scad\n  export $(scad-deps vendor)   # or copy them into ./libraries",
        )
        .subcommand_required(true)
        .disable_help_subcommand(true);
    for c in COMMANDS {
        cmd = cmd.subcommand((c.command)(tool).name(c.name));
    }
    cmd
}

fn main() -> std::process::ExitCode {
    let mut args = std::env::args_os().skip(1);
    let first = args.next();
    let code = match first
        .as_ref()
        .and_then(|f| f.to_str())
        .and_then(|f| COMMANDS.iter().find(|c| c.name == f))
    {
        Some(c) => (c.run)(Tool::SCAD_DEPS, args.collect()),
        None => {
            // No command, `--help`, `--version` or an unknown word: clap
            // prints what fits and exits 0 for help, 2 for an error.
            let argv: Vec<OsString> = std::iter::once(OsString::from("scad-deps"))
                .chain(first)
                .chain(args)
                .collect();
            match command().try_get_matches_from(argv) {
                Ok(_) => 0,
                Err(e) => {
                    let _ = e.print();
                    if e.use_stderr() { 1 } else { 0 }
                }
            }
        }
    };
    std::process::ExitCode::from(code)
}
