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
//!   `context`. Ordinary names are resolved ahead of time, as each
//!   definition is first used, to the few contexts that can bind them,
//!   where they sit in numbered slots; see `resolve`.
//! - **Errors** unwind as `Err(Box<Unwind>)`, collecting OpenSCAD's
//!   `TRACE:` lines at the same call sites; see `message`.
//! - **Recursion limits** are a measured stack budget, like OpenSCAD's
//!   `StackCheck`, and a frame budget for where the stack cannot be
//!   measured (WASM), so deep recursion reports an error instead of
//!   crashing; see [`recursion`]. Run evaluation on a thread with enough
//!   stack ([`with_stack`]).
//! - **No ambient platform access.** Files are read through
//!   [`Options::fs`], and unseeded `rands()` starts from
//!   [`Options::rng_seed`], which the host chooses; nothing reads the
//!   clock or the environment.
//! - **Cancellation**: set [`Options::interrupt`] and evaluation stops at
//!   the next call or loop iteration.
//! - **Resource limits** ([`limits`]): a host running models it did not
//!   write sets [`Options::guard`], and a list, string, `rands()` or
//!   evaluation that would pass a limit stops with a `resource-limit`
//!   error before it allocates.

mod builtins;
mod call;
mod context;
pub mod dump;
mod eval;
pub mod fma;
mod inst;
pub mod limits;
pub mod message;
pub mod node;
mod ops;
mod print;
pub mod recursion;
mod resolve;
pub use resolve::Stats as ResolveStats;
pub mod rng;
mod sym;
pub mod text_props;
/// Degree trigonometry lives in `io`, the lowest crate that needs it
/// (its DXF and SVG readers); re-exported so evaluator code keeps its path.
pub use io::trig;
mod utf8;
pub mod value;

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use lang::Program;
use lang::loader::{FileSystem, StdFs};

pub use message::{Collect, Console, Location, Logged, LoggedHint, Message, Output, excerpt};
pub use node::Node;
pub use value::Value;

/// `OpenSCAD::parse_color`: a CSS or `xkcd:` colour name, or `#rgb[a]` /
/// `#rrggbb[aa]`, as RGBA in 0..=1. Exports that take colour options (PDF)
/// resolve them with the same table `color()` uses.
pub fn parse_color(s: &str) -> Option<[f32; 4]> {
    builtins::modules::parse_color(s.as_bytes())
}

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
        Camera {
            vpt: [0.0; 3],
            vpr: [55.0, 0.0, 25.0],
            vpd: 140.0,
            vpf: 22.5,
            auto: true,
            locked: false,
        }
    }
}

impl Camera {
    /// `Camera::setup` from `--camera` numbers: 7 for a gimbal camera
    /// (translate, rotate, distance) or 6 for eye and centre points.
    pub fn from_args(p: &[f64]) -> Option<Camera> {
        let wrap = |a: f64| (360.0 + a) % 360.0;
        let rot = |x: f64, y: f64, z: f64| {
            [
                wrap(90.0 - wrap(90.0 - x)),
                wrap(-wrap(-y)),
                wrap(-wrap(-z)),
            ]
        };
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
                Some(Camera {
                    vpt: center,
                    vpr,
                    vpd: dist,
                    vpf: 22.5,
                    auto: false,
                    locked: true,
                })
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
    /// (OpenSCAD's `StackCheck`, 8 MiB minus 128 KiB on macOS and Linux);
    /// see [`recursion`].
    pub stack_limit: usize,
    /// Nested function calls and statement instantiations allowed before
    /// reporting recursion, whatever the stack; see [`recursion`].
    pub frame_limit: u32,
    /// `version()`: the OpenSCAD release this evaluator matches.
    pub version: [f64; 3],
    /// The seed unseeded `rands()` starts from. OpenSCAD seeds from the
    /// clock and its process ID, and the command line does the same; other
    /// hosts choose (a WASM host from its own entropy). The same seed
    /// always gives the same numbers, so the default, 0, is repeatable.
    pub rng_seed: u32,
    /// Where `dxf_dim()` and `dxf_cross()` read their files.
    pub fs: Arc<dyn FileSystem + Send + Sync>,
    /// Checked at every call and loop iteration; when set, evaluation stops.
    pub interrupt: Option<Arc<AtomicBool>>,
    /// The request's resource limits ([`limits`]); `None` is unlimited, as
    /// OpenSCAD is. Its interrupt flag should be [`Options::interrupt`].
    pub guard: Option<Arc<limits::Guard>>,
    /// `--hardwarnings`: stop at the first warning, with the `TRACE:` lines
    /// of an evaluation error (see [`Evaluation::hard_warning`]).
    pub hardwarnings: bool,
    /// neoscad's `part("name") { ... }` extension (`--enable part`). Off,
    /// `part` is an unknown module exactly as in OpenSCAD, with its
    /// warning; on, it is a builtin that a program's own `part` module
    /// still shadows (see [`node::NodeKind::Part`]).
    pub parts: bool,
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
            frame_limit: recursion::DEFAULT_FRAME_LIMIT,
            version: [2026.0, 9.0, 23.0],
            rng_seed: 0,
            fs: Arc::new(StdFs),
            interrupt: None,
            guard: None,
            hardwarnings: false,
            parts: false,
        }
    }
}

pub use recursion::{DEFAULT_STACK_LIMIT, DEFAULT_THREAD_STACK};

/// What kind of name a builtin is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum BuiltinKind {
    Module,
    Function,
    /// A special variable (`$fn`) or constant (`PI`) the evaluator sets.
    Variable,
}

/// Whether a builtin is on by default.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuiltinStatus {
    Stable,
    /// Known but disabled, as in OpenSCAD without `--enable` (`roof`,
    /// `textmetrics`, ...).
    Experimental,
    /// neoscad's own, off by default (`part` with `--enable part`).
    Extension,
}

/// A builtin the evaluator registers, for documentation and completion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BuiltinName {
    pub name: &'static str,
    pub kind: BuiltinKind,
    pub status: BuiltinStatus,
}

/// The special variables and constants every evaluation starts with, or
/// that module calls set (`$children`, `$parent_modules`).
pub const BUILTIN_VARIABLES: [&str; 12] = [
    "$fn",
    "$fa",
    "$fs",
    "$t",
    "$preview",
    "$vpt",
    "$vpr",
    "$vpd",
    "$vpf",
    "$children",
    "$parent_modules",
    "PI",
];

/// Every builtin module, function and variable the evaluator knows, from
/// the tables it evaluates with (so a new builtin cannot be missed by what
/// lists them).
pub fn builtins() -> Vec<BuiltinName> {
    use BuiltinKind::*;
    let status = |enabled: bool| {
        if enabled {
            BuiltinStatus::Stable
        } else {
            BuiltinStatus::Experimental
        }
    };
    let mut out: Vec<BuiltinName> = builtins::modules::ALL
        .iter()
        .map(|(n, b)| BuiltinName {
            name: n,
            kind: Module,
            status: status(b.enabled()),
        })
        .collect();
    out.push(BuiltinName {
        name: "part",
        kind: Module,
        status: BuiltinStatus::Extension,
    });
    out.extend(builtins::functions::ALL.iter().map(|(n, b)| BuiltinName {
        name: n,
        kind: Function,
        status: status(b.enabled()),
    }));
    out.extend(BUILTIN_VARIABLES.iter().map(|n| BuiltinName {
        name: n,
        kind: Variable,
        status: BuiltinStatus::Stable,
    }));
    out
}

/// Run `f` on a thread with `bytes` of stack and wait for it.
///
/// On wasm32 this calls `f` directly: wasm32-unknown-unknown cannot spawn
/// threads (the spawn fails with "operation not supported"), and a wasm
/// module's stack size is fixed when it is linked (see [`recursion`]), so
/// a new thread would buy nothing.
#[cfg(target_arch = "wasm32")]
pub fn with_stack<T: Send>(_bytes: usize, f: impl FnOnce() -> T + Send) -> T {
    f()
}

/// Run `f` on a thread with `bytes` of stack and wait for it.
#[cfg(not(target_arch = "wasm32"))]
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
    /// Whether [`Options::hardwarnings`] was set and a warning was printed:
    /// evaluation stopped there (or ended just after it), and OpenSCAD
    /// would exit with status 1.
    pub hard_warning: bool,
    /// [`Options::camera`] after `Camera::updateView`: top-level `$vpt`,
    /// `$vpr`, `$vpd` and `$vpf` assignments replace its values unless it
    /// is locked by `--camera`, and clear [`Camera::auto`] (with OpenSCAD's
    /// warning). A PNG export draws with this view, so the file's own
    /// camera settings reach the image as they do in OpenSCAD.
    pub camera: Camera,
    /// Which of those the file itself assigned (and converted): the GUI
    /// moves its view to exactly these (`Camera::updateView`).
    pub camera_assigned: CameraAssigned,
    /// How the program's names were resolved: how many references are
    /// left to a dynamic lookup.
    pub resolution: ResolveStats,
}

/// Which of `$vpt`, `$vpr`, `$vpd` and `$vpf` a program assigned at its
/// top level, with a value `Camera::updateView` accepts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CameraAssigned {
    pub vpt: bool,
    pub vpr: bool,
    pub vpd: bool,
    pub vpf: bool,
}

impl CameraAssigned {
    pub fn any(self) -> bool {
        self.vpt || self.vpr || self.vpd || self.vpf
    }
}

/// Evaluate `main` with its `use`d `libraries`. `main_uses` are the keys of
/// the libraries `main` uses (`lang::deps::resolve_uses`). `main_dir` is the
/// main file's directory: messages print paths relative to it.
///
/// Call this on a thread with at least [`Options::stack_limit`] plus some
/// headroom of stack ([`with_stack`] with [`DEFAULT_THREAD_STACK`] for the
/// default limit): the recursion check measures the real stack and
/// assumes it is there. On wasm32 the module must be linked with the stack
/// [`recursion`] describes.
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
