//! Project dependencies (`docs/dependencies.md`): `neoscad add`,
//! `remove`, `fetch`, `update`, `tree`, `vendor` and `store` are the
//! `scad-deps` crate's command line (`scad_deps::cli`) under neoscad's
//! name and version, so they print and do exactly what the standalone
//! `scad-deps` binary does. A project is the nearest directory holding a
//! `scad.toml`; its locked dependencies are linked into
//! `.scad/libraries`, which every host puts on the library path of the
//! project's files, after the including file's directory and before
//! `OPENSCADPATH` (`crate::host::project_libs`).

use std::ffi::OsString;

use scad_deps::cli::{self, Tool};

/// The commands as `neoscad <word>`.
const NEOSCAD: Tool = Tool {
    name: "neoscad",
    version: env!("CARGO_PKG_VERSION"),
};

macro_rules! commands {
    ($($main:ident, $command:ident;)*) => {$(
        pub fn $main(args: Vec<OsString>) -> u8 {
            cli::$main(NEOSCAD, args)
        }

        pub fn $command() -> clap::Command {
            cli::$command(NEOSCAD)
        }
    )*};
}

commands! {
    add_main, add_command;
    remove_main, remove_command;
    fetch_main, fetch_command;
    update_main, update_command;
    tree_main, tree_command;
    vendor_main, vendor_command;
    store_main, store_command;
}
