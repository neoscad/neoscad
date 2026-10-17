//! Project dependencies: `scad.toml` declares the libraries a project
//! includes, `scad.lock` pins each to a commit and a content hash, and
//! `neoscad fetch` makes them appear under `<project>/.scad/libraries`,
//! which hosts put on the library path of every file in the project
//! (`docs/dependencies.md` is the user guide and the design record).
//!
//! Every revision is fetched and checked out once per machine, into a
//! shared store ([`store`]): one bare git mirror per remote and one
//! read-only tree per resolved commit, linked into each project that uses
//! it. A path dependency is linked to its directory directly.
//!
//! This is a host crate: it runs `git`, reads and writes the disk and the
//! environment. The library crates only ever see the answer, a
//! [`lang::project::ProjectLibs`] that [`search::project_for`] builds for
//! a main file.

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
