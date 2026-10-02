//! How deep a program may recurse before evaluation stops with OpenSCAD's
//! recursion error ("Recursion detected calling function 'f'") instead of
//! crashing the process.
//!
//! OpenSCAD measures its stack (`StackCheck`: 8 MiB minus 128 KiB) at each
//! function call and user module instantiation, so its depth is a property
//! of its build. neoscad runs statements and module instantiation on a heap
//! stack (`crate::heap`), and function calls past a few native levels with
//! the expressions around them (`crate::heap_expr`), so a recursion takes
//! no native stack per level. What stops it is a count: the user modules
//! being instantiated plus the user function calls in progress, against
//! [`crate::limits::Limits::depth`] (default
//! [`crate::limits::DEFAULT_DEPTH`]), the same in every build, profile and
//! browser.
//!
//! The native stack still grows with a few things, and two native checks
//! bound them:
//!
//! - **The stack measured** ([`crate::Options::stack_limit`]): the distance
//!   between the stack pointer at the start of evaluation and at the check.
//!   Natively this is exact: [`DEFAULT_STACK_LIMIT`] on a thread of
//!   [`DEFAULT_THREAD_STACK`] ([`crate::with_stack`]).
//! - **A frame budget** ([`crate::Options::frame_limit`]), which measures
//!   nothing: each nested expression, function call and list comprehension
//!   element holds a few frames ([`EXPRESSION_FRAMES`] and the like), and
//!   printing a nested vector one per level. On wasm32 the stack that
//!   overflows first is one the module cannot measure: a WebAssembly
//!   engine runs wasm functions on its own machine stack (about 1 MB in
//!   V8), and overflowing it throws `RangeError: Maximum call stack size
//!   exceeded` out of the module, which leaves the instance unusable. The
//!   budget there, [`DEFAULT_FRAME_LIMIT`], is calibrated so that a native
//!   recursion at the limit still evaluates within V8's default stack;
//!   natively the budget is unlimited.
//!
//! Both are checked at every function call (`Evaluator::call_exhausted`)
//! and while printing a nested value. What they still guard:
//!
//! - the first `heap_expr::NATIVE_CALLS` nested user calls, which run
//!   natively for speed (a bounded amount of stack);
//! - the few shapes that stay native and start a nested heap loop for each
//!   call they reach: ranges, callees that are expressions, methods,
//!   C-style `for` comprehensions, `object()` and `is_undef()` arguments,
//!   parameter defaults and `use`d libraries' assignments. A recursion
//!   through one of them at every level holds native stack per level, and
//!   the check at its calls is what stops it cleanly;
//! - printing, whose depth is the value's;
//! - the source's own nesting, which the parser bounds first with a
//!   counted limit (`lang::syntax::parser::NESTING_LIMIT`).
//!
//! On wasm32 the measured limit also guards the module's own stack in
//! linear memory (Rust's "shadow stack", where locals whose address is
//! taken live). rustc links wasm with the stack first in memory, growing
//! down towards address 0, so the address of a local at the start of
//! evaluation is exactly the stack left (checked: 1,048,571 with rustc's
//! default 1 MiB, 8,388,603 with 8 MiB). The limit is capped by it, less
//! [`WASM_STACK_RESERVE`], so a module linked with less than
//! [`WASM_STACK_SIZE`] still fails cleanly.
//!
//! Reachable depths of `function f(n) = n == 0 ? 0 : 1 + f(n - 1);` and
//! `module m(n) { if (n > 0) m(n - 1); else cube(1); }`, as the deepest
//! `n` that evaluates without an error (wasm32: and renders), measured
//! with `crates/wasm-check/run.js --depths` and by bisection natively
//! (`conformance depth` bisects the native ones for any binary; macOS
//! arm64 here):
//!
//! | | function | module |
//! |---|---|---|
//! | OpenSCAD nightly 2026.09.23, native | 9,192 | 7,052 |
//! | neoscad, native, plain and PGO builds alike | 99,999 | 99,999 |
//! | neoscad, wasm32 in node 18 | 99,999 | 99,999 |
//!
//! Natively neoscad recurses deeper than OpenSCAD, and that is the policy:
//! the limit exists to turn a crash into an error, not to reject programs
//! OpenSCAD could run on a bigger stack, and an infinite recursion prints
//! the same error and trace either way. There is no mode that matches the
//! nightly's depth, because that depth is a property of its build (stack
//! size, compiler, frame sizes) rather than of the language.

/// The default [`crate::Options::stack_limit`]: what the shapes that still
/// recurse natively (see the module documentation) may use before they
/// stop with the recursion error.
///
/// It was sized for the recursive evaluator, whose module and function
/// levels all held native stack: 48 MiB until a profile-guided build
/// (`scripts/pgo.sh`) inlined more and grew its frames by half, then
/// 64 MiB so that build cleared the nightly's depth by 25%. Neither
/// reason holds now that recursion runs on the heap, but it is what the
/// native shapes reach: 61,667 levels of a recursion through `is_undef()`
/// and 49,332 through a C-style `for`'s initialiser (plain release build,
/// macOS arm64), where the counted limit would allow 100,000. The stack
/// is only touched when a program recurses that way. Shrinking it, and
/// the thread [`crate::with_stack`] makes, to what source nesting and
/// those shapes need is a followup (`docs/followups.md`).
#[cfg(not(target_arch = "wasm32"))]
pub const DEFAULT_STACK_LIMIT: usize = 64 << 20;

/// The default [`crate::Options::stack_limit`] on wasm32: the linked stack
/// ([`WASM_STACK_SIZE`]) less the reserve. The frame budget is reached
/// first; this only matters for a module linked with a smaller stack.
#[cfg(target_arch = "wasm32")]
pub const DEFAULT_STACK_LIMIT: usize = WASM_STACK_SIZE - WASM_STACK_RESERVE;

/// Stack to give a thread running [`crate::evaluate`] with the default
/// limit: the limit plus room for the work between two checks and for
/// reporting the error.
pub const DEFAULT_THREAD_STACK: usize = DEFAULT_STACK_LIMIT + (16 << 20);

/// The default [`crate::Options::frame_limit`]: unlimited natively, where
/// the measured stack decides.
#[cfg(not(target_arch = "wasm32"))]
pub const DEFAULT_FRAME_LIMIT: u32 = u32::MAX;

/// The default [`crate::Options::frame_limit`] on wasm32, calibrated in
/// node 18 (V8's default 984 KiB stack) with `scripts/wasm-check.sh
/// --depths --all-programs --frames=4000000000` when every level of a
/// recursion held native stack: the deepest each kind reached under this
/// budget was at most 63% of the depth where V8 overflowed. It now only
/// meets the shapes that still recurse natively, whose frames are the
/// same expression and call frames it was calibrated on.
#[cfg(all(target_arch = "wasm32", not(debug_assertions)))]
pub const DEFAULT_FRAME_LIMIT: u32 = 2_000;

/// The default [`crate::Options::frame_limit`] in an unoptimised wasm32
/// build, whose frames are larger: calibrated the same way, the deepest
/// recursion it allowed reached at most 57% of V8's limit.
#[cfg(all(target_arch = "wasm32", debug_assertions))]
pub const DEFAULT_FRAME_LIMIT: u32 = 600;

/// The frame budget [`crate::Options::default`] starts from: on wasm32 the
/// one `set_default_frame_limit` (wasm32 only) chose, if any, else
/// [`DEFAULT_FRAME_LIMIT`].
pub fn default_frame_limit() -> u32 {
    #[cfg(target_arch = "wasm32")]
    {
        match FRAME_LIMIT.load(std::sync::atomic::Ordering::Relaxed) {
            0 => DEFAULT_FRAME_LIMIT,
            n => n,
        }
    }
    #[cfg(not(target_arch = "wasm32"))]
    DEFAULT_FRAME_LIMIT
}

/// Sets the frame budget every later [`crate::Options::default`] starts
/// from, for this instance (0 restores [`DEFAULT_FRAME_LIMIT`]): for a
/// host whose engine gives wasm frames more or less stack than V8. It is a
/// process-wide default rather than an option because every evaluation in
/// the instance runs on the same stack, whichever API starts it.
///
/// wasm32 only: a native process measures its stack exactly, and a global
/// here would leak between tests that run in parallel.
#[cfg(target_arch = "wasm32")]
pub fn set_default_frame_limit(limit: u32) {
    FRAME_LIMIT.store(limit, std::sync::atomic::Ordering::Relaxed);
}

/// [`set_default_frame_limit`]'s budget; 0 when none was set.
#[cfg(target_arch = "wasm32")]
static FRAME_LIMIT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// Always 0. A shim for the web core's `framesAtLastCheck`, kept until
/// `crates/web` drops it: the web worker read it from an instance whose
/// stack its start-up probe had overflowed, to learn how many frames that
/// engine's stack held. The probe never runs against the heap evaluator
/// (see [`HEAP_EVAL`]), so nothing records frames any more.
pub fn frames_at_last_check() -> u32 {
    0
}

/// Always true: recursion runs on the heap and ends at the counted depth
/// limit ([`crate::limits::Limits::depth`]), not at the frame budget.
///
/// A shim for the web core's `heapStatements`, kept until `crates/web`
/// drops it: the web worker skips its stack probes when this is set. Those
/// probes recurse until the stack overflows; on the heap they only stop at
/// the depth limit, which took about 7 s at start-up in a WebKit worker.
pub const HEAP_EVAL: bool = true;

/// What one nested frame of each kind adds to the frame budget's count:
/// the constants below. Statements add nothing, since they run on the
/// heap.
///
/// The web worker used to measure each kind's depth in its own engine and
/// set weights here; a shim keeps that API (`set_frame_weights`, wasm32
/// only) until `crates/web` drops it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FrameWeights {
    /// A statement instantiation: 0, statements take no native stack.
    pub statement: u32,
    /// A nested expression ([`EXPRESSION_FRAMES`]).
    pub expression: u32,
    /// A function call ([`CALL_FRAMES`]).
    pub call: u32,
    /// A list comprehension element ([`COMPREHENSION_FRAMES`]).
    pub comprehension: u32,
    /// Extra for a builtin module's children: 0, as for statements.
    pub geometry: u32,
}

/// The weights every evaluation counts frames with.
pub const DEFAULT_WEIGHTS: FrameWeights = FrameWeights {
    statement: 0,
    expression: EXPRESSION_FRAMES,
    call: CALL_FRAMES,
    comprehension: COMPREHENSION_FRAMES,
    geometry: 0,
};

/// The weights every evaluation counts frames with: always
/// [`DEFAULT_WEIGHTS`].
pub fn frame_weights() -> FrameWeights {
    DEFAULT_WEIGHTS
}

/// Accepted and ignored. A shim for the web core's `setFrameWeights`, kept
/// until `crates/web` drops it: the weights tuned the frame budget per
/// kind of recursion to a browser's stack, and recursion no longer reaches
/// that budget. The frames that still do are the same expression and call
/// frames in every kind, which [`DEFAULT_WEIGHTS`] counts.
#[cfg(target_arch = "wasm32")]
pub fn set_frame_weights(w: FrameWeights) {
    let _ = w;
}

/// Frames an expression holds.
pub const EXPRESSION_FRAMES: u32 = 1;

/// Frames a function call holds: calling (binding arguments, the tail-call
/// loop) takes about twice an expression's stack.
pub const CALL_FRAMES: u32 = 2;

/// Frames a list comprehension element (`for`, `let`, `if`, `each`
/// inside `[...]`) holds: its evaluation path is about four times as deep
/// in wasm frames as an expression's (`eval_element`, `eval_lc_frame`,
/// `for_each` and the closure it calls). It was 2, which made a
/// recursion through a comprehension the one that overflowed first per
/// budgeted frame: V8 in a Chromium worker overflowed at 1,656 frames of
/// the default 2,000.
pub const COMPREHENSION_FRAMES: u32 = 4;

/// The linear-memory stack a wasm32 build of neoscad should be linked
/// with: `-C link-arg=-zstack-size=8388608` (rustc's default is 1 MiB).
/// The evaluator stays safe with less, but the native shapes then stop
/// sooner. `crates/wasm-check/build.rs` links this way.
pub const WASM_STACK_SIZE: usize = 8 << 20;

/// Linear-memory stack kept free below the measured limit on wasm32, for
/// the work between two checks and for reporting the error.
pub const WASM_STACK_RESERVE: usize = 1 << 20;

/// The measured limit for an evaluation whose stack starts at `base`.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn stack_limit(limit: usize, base: usize) -> usize {
    let _ = base;
    limit
}

/// The measured limit for an evaluation whose stack starts at `base`:
/// with the stack first in linear memory, `base` is the stack left.
#[cfg(target_arch = "wasm32")]
pub(crate) fn stack_limit(limit: usize, base: usize) -> usize {
    limit.min(base.saturating_sub(WASM_STACK_RESERVE))
}
