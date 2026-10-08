//! The NeoSCAD desktop app for Linux (GTK 4, libadwaita): see
//! docs/linux-app.md.
//!
//! This library is the app's host logic that is not GTK glue, so it builds
//! and is tested on every platform: AI agents' consent, setup and requests
//! (`agent`), the editor bridge's state machine
//! (`bridge`), the editor page's resources (`resources`), its language
//! server and where a definition opens (`language`), the machine's side
//! of the session (`host`), a window's document (`document`), runs and
//! exports off the main thread (`run`), the 3D view's drawing and input
//! (`view`), the side panels' logic (`customizer`, `inspect`), which
//! files a window watches (`watch`), the language extensions on
//! (`extensions`) and the update check's settings and
//! notice (`update`). Everything else a front end does comes from
//! `crates/client`, the port boundary (docs/architecture.md).
//! The window itself (`src/app/`) is compiled only with the `gtk` feature.

pub mod agent;
pub mod bridge;
pub mod customizer;
pub mod document;
pub mod extensions;
pub mod host;
pub mod inspect;
pub mod language;
pub mod resources;
pub mod run;
pub mod update;
pub mod view;
pub mod watch;
