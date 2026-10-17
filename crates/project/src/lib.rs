//! A dependency manager for OpenSCAD-language projects: `scad.toml`
//! declares the libraries a project includes, `scad.lock` pins each to a
//! commit and a content hash, and a fetch makes them appear under
//! `<project>/.scad/libraries`, which a host searches for every file in
//! the project. The formats, the search order and the store are specified
//! in the crate's README; NeoSCAD's user guide is its
//! `docs/dependencies.md`.
//!
//! Every revision is fetched and checked out once per machine, into a
//! shared store ([`store`]): one bare git mirror per remote and one
//! read-only tree per resolved commit, linked into each project that uses
//! it. A path dependency is linked to its directory directly.
//!
//! The crate runs `git` and reads and writes the disk and the
//! environment. What a host needs to evaluate a file is
//! [`search::project_for`]: the project's library directories, in
//! search order, and its declared dependencies. The command line
//! (`scad-deps`, and `neoscad`'s subcommands) is [`cli`], behind the `cli`
//! feature.

#[cfg(feature = "cli")]
pub mod cli;
mod env;
mod error;
pub mod git;
pub mod hash;
pub mod link;
pub mod lock;
pub mod manifest;
mod ops;
mod resolve;
pub mod search;
pub mod store;
pub mod version;

pub use error::{Error, Result};
pub use ops::{
    AddSpec, FetchOptions, FetchReport, Package, Update, VendorReport, add, find_root, remove,
    tree, vendor,
};
pub use ops::{fetch, managed_dir};
