//! NeoSCAD's 3D view in a browser: the renderer's [`render::viewport`]
//! drawing into an HTML canvas, on the page's main thread.
//!
//! The model is built elsewhere. The web worker evaluates the program,
//! builds a [`render::Scene`] and packs it ([`render::packed`]); its two
//! byte arrays arrive here as transferables with a line of JSON, and
//! `Viewer.setModel` uploads them as they are. This module never
//! evaluates, builds geometry or triangulates, so a long render in the
//! worker never stalls orbiting the last model.
//!
//! - [`input`] turns pointer and wheel events into camera moves, and
//!   [`wire`] holds the JSON shapes of the JavaScript API. Both are plain
//!   Rust and tested natively.
//! - `web` (wasm32 only) is the wasm-bindgen class `Viewer`: the canvas
//!   surface (WebGPU, or WebGL2 where WebGPU is missing), the event
//!   listeners, sizing to the canvas and its device pixel ratio, and
//!   drawing on an animation frame only when something changed.
//!
//! See `crates/web-view/demo/` for a page that drives it alone, and
//! `scripts/web/build-view.sh` for the build.

pub mod input;
pub mod wire;

#[cfg(target_arch = "wasm32")]
mod web;
