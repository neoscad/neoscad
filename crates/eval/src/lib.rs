//! The OpenSCAD evaluator.
//!
//! [`evaluate`] runs a parsed program (and the libraries it `use`s) the way
//! OpenSCAD's `SourceFile::instantiate` does: it evaluates top-level
//! assignments, instantiates modules into a geometry-agnostic [`Node`] tree
//! and reports `echo()` output, warnings and errors through an [`Output`],
//! in OpenSCAD's order and wording.
//!
//! Design:
//!
//! - **No global state.** An evaluation is a value built per call, so any
//!   number can run in parallel threads or in WASM. The parsed programs are
//!   only borrowed.
//! - **Values** ([`Value`]) are small and cheap to clone; see `value`.
//! - **Scoping** follows OpenSCAD's contexts: lexical lookup through
//!   parents, and `$` variables through the stack of live contexts; see
//!   `context`.
//! - **Errors** unwind as `Err(Box<Unwind>)`, collecting OpenSCAD's
//!   `TRACE:` lines at the same call sites; see `message`.
//! - **Recursion limits** are measured on the real stack, like OpenSCAD's
//!   `StackCheck`, so deep recursion reports an error instead of crashing.
//!   Run evaluation on a thread with enough stack ([`with_stack`]).
//! - **Cancellation**: set [`Options::interrupt`] and evaluation stops at
//!   the next call or loop iteration.

mod builtins;
mod call;
mod context;
mod dxf;
pub mod dump;
mod eval;
mod inst;
pub mod message;
pub mod node;
mod ops;
mod print;
pub mod rng;
mod sym;
pub mod text_props;
pub mod trig;
mod utf8;
pub mod value;

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use lang::Program;

pub use message::{Collect, Console, Message, Output};
pub use node::Node;
pub use value::Value;

/// A `use`d library, already parsed (see `lang::deps::load_dependencies`).
#[derive(Debug, Clone, Copy)]
pub struct Library<'a> {
    /// The key other files refer to it by (`lang::deps::Library::path`).
    pub path: &'a str,
    /// `None` if the file could not be read or failed to parse; lookups then
    /// skip it, as OpenSCAD does.
    pub program: Option<&'a Program>,
    /// Keys of the libraries it uses, in search order.
    pub uses: &'a [String],
}

/// The view variables OpenSCAD derives from `--camera` (`$vpt`, `$vpr`,
/// `$vpd`, `$vpf`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Camera {
    pub vpt: [f64; 3],
    pub vpr: [f64; 3],
    pub vpd: f64,
    pub vpf: f64,
    /// `--viewall`/`--autocenter` in effect (the default without
    /// `--camera`): top-level `$vp*` assignments then disable them, with a
    /// warning.
    pub auto: bool,
    /// Set by `--camera`: top-level `$vp*` assignments are then ignored.
    pub locked: bool,
}

impl Default for Camera {
    /// OpenSCAD's `Camera()` after `resetView()`.
    fn default() -> Self {
        Camera { vpt: [0.0; 3], vpr: [55.0, 0.0, 25.0], vpd: 140.0, vpf: 22.5, auto: true, locked: false }
    }
}

impl Camera {
    /// `Camera::setup` from `--camera` numbers: 7 for a gimbal camera
    /// (translate, rotate, distance) or 6 for eye and centre points.
    pub fn from_args(p: &[f64]) -> Option<Camera> {
        let wrap = |a: f64| (360.0 + a) % 360.0;
        let rot = |x: f64, y: f64, z: f64| [wrap(90.0 - wrap(90.0 - x)), wrap(-wrap(-y)), wrap(-wrap(-z))];
        match p.len() {
            7 => Some(Camera {
                vpt: [p[0], p[1], p[2]],
                vpr: rot(p[3], p[4], p[5]),
                vpd: p[6],
                vpf: 22.5,
                auto: false,
                locked: true,
            }),
            6 => {
                let (eye, center) = ([p[0], p[1], p[2]], [p[3], p[4], p[5]]);
                let dir = [center[0] - eye[0], center[1] - eye[1], center[2] - eye[2]];
                let dist = (dir[0] * dir[0] + dir[1] * dir[1] + dir[2] * dir[2]).sqrt();
                let rz = if dir[1] == 0.0 && dir[0] == 0.0 {
                    if dir[2] < 0.0 { 0.0 } else { 180.0 }
                } else {
                    -trig::atan2_degrees(dir[1], dir[0]) + 90.0
                };
                let proj = (dir[0] * dir[0] + dir[1] * dir[1]).sqrt();
                let rx = -trig::atan2_degrees(dir[2], proj);
                // object_rot = (rx, 0, rz); $vpr = wrap(90 - rx), wrap(-0), wrap(-rz).
                let vpr = [wrap(90.0 - rx), wrap(-0.0), wrap(-rz)];
                Some(Camera { vpt: center, vpr, vpd: dist, vpf: 22.5, auto: false, locked: true })
            }
            _ => None,
        }
    }
}

/// Evaluation settings, mirroring OpenSCAD's command-line flags.
#[derive(Debug, Clone)]
pub struct Options {
    /// `$preview`: true for preview-style exports (echo, csg, png without
    /// `--render`).
    pub preview: bool,
    /// `$t`.
    pub time: f64,
    pub camera: Camera,
    /// `--trace-depth`.
    pub trace_depth: u32,
    /// `--trace-usermodule-parameters`.
    pub trace_usermodule_parameters: bool,
    /// `--check-parameters`: warn about unknown named arguments and extra
    /// positional ones in user calls.
    pub check_parameters: bool,
    /// `--check-parameter-ranges`: warn about degenerate primitive sizes.
    pub check_parameter_ranges: bool,
    /// Bytes of stack evaluation may use before reporting recursion
    /// (OpenSCAD's `StackCheck`, 8 MiB minus 128 KiB on macOS and Linux).
    pub stack_limit: usize,
    /// `version()`: the OpenSCAD release this evaluator matches.
    pub version: [f64; 3],
    /// The seed for unseeded `rands()`; `None` seeds from the clock.
    pub rng_seed: Option<u32>,
    /// Checked at every call and loop iteration; when set, evaluation stops.
    pub interrupt: Option<Arc<AtomicBool>>,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            preview: true,
            time: 0.0,
            camera: Camera::default(),
            trace_depth: 12,
            trace_usermodule_parameters: true,
            check_parameters: true,
            check_parameter_ranges: false,
            stack_limit: DEFAULT_STACK_LIMIT,
            version: [2026.0, 9.0, 23.0],
            rng_seed: None,
            interrupt: None,
        }
    }
}

/// The default [`Options::stack_limit`]. Rust frames for one OpenSCAD call
/// are larger than OpenSCAD's own, so this is scaled up from OpenSCAD's
/// 8 MiB so programs recurse at least as deep as they do there.
pub const DEFAULT_STACK_LIMIT: usize = 48 << 20;

/// Stack to give a thread running [`evaluate`] with the default limit.
pub const DEFAULT_THREAD_STACK: usize = DEFAULT_STACK_LIMIT + (16 << 20);

/// Run `f` on a thread with `bytes` of stack and wait for it.
pub fn with_stack<T: Send>(bytes: usize, f: impl FnOnce() -> T + Send) -> T {
    std::thread::scope(|s| {
        std::thread::Builder::new()
            .stack_size(bytes)
            .spawn_scoped(s, f)
            .map(|h| h.join().unwrap_or_else(|e| std::panic::resume_unwind(e)))
            .unwrap_or_else(|_| panic!("cannot spawn the evaluation thread"))
    })
}

/// The result of an evaluation.
#[derive(Debug)]
pub struct Evaluation {
    /// The instantiated tree (partial if evaluation stopped on an error).
    pub root: Node,
    /// Whether an evaluation error (failed assertion, recursion limit, ...)
    /// stopped evaluation early.
    pub aborted: bool,
    /// Whether [`Options::interrupt`] stopped it.
    pub interrupted: bool,
}

/// Evaluate `main` with its `use`d `libraries`. `main_uses` are the keys of
/// the libraries `main` uses (`lang::deps::resolve_uses`). `main_dir` is the
/// main file's directory: messages print paths relative to it.
///
/// Call this on a thread with at least [`Options::stack_limit`] plus some
/// headroom of stack ([`with_stack`] with [`DEFAULT_THREAD_STACK`] for the
/// default limit): the recursion check measures the real stack and
/// assumes it is there.
pub fn evaluate(
    main: &Program,
    main_uses: &[String],
    libraries: &[Library<'_>],
    main_dir: PathBuf,
    options: &Options,
    out: &mut dyn Output,
) -> Evaluation {
    let mut ev = eval::Evaluator::new(main, main_uses, libraries, main_dir, options.clone(), out);
    ev.run()
}
