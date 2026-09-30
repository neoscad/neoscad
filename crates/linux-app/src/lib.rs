//! The NeoSCAD desktop app for Linux (GTK 4, libadwaita), milestone 1:
//! see docs/linux-app.md.
//!
//! This library is the app's host logic that is not GTK glue, so it builds
//! and is tested on every platform: the editor bridge's state machine
//! (`bridge`), the editor page's resources (`resources`), the machine's
//! side of the session (`host`), a window's document (`document`), runs
//! and exports off the main thread (`run`) and the 3D view's drawing and
//! input (`view`). Everything else a front end does comes from
//! `crates/client`, the port boundary (docs/architecture.md). The window
//! itself (`src/app/`) is compiled only with the `gtk` feature.

pub mod bridge;
pub mod document;
pub mod host;
pub mod resources;
pub mod run;
pub mod view;
