//! How deep a program may recurse before evaluation stops with OpenSCAD's
//! recursion error ("Recursion detected calling function 'f'") instead of
//! crashing the process.
//!
//! OpenSCAD measures its stack (`StackCheck`: 8 MiB minus 128 KiB) at each
//! function call and user module instantiation. neoscad has two limits,
//! and whichever is reached first stops the evaluation:
//!
//! - **The stack measured** ([`crate::Options::stack_limit`]): the distance
//!   between the stack pointer at the start of evaluation and at the check.
//!   Natively this is the machine stack and the measurement is exact, so
//!   it is the limit that decides: [`DEFAULT_STACK_LIMIT`] on a thread of
//!   [`DEFAULT_THREAD_STACK`] ([`crate::with_stack`]).
//! - **A frame budget** ([`crate::Options::frame_limit`]), which measures
//!   nothing: each nested expression, function call, list comprehension
//!   element and statement holds a few frames ([`EXPRESSION_FRAMES`] and
//!   the like, weighted by how much stack each costs), and printing a
//!   nested vector one per level. It is checked where OpenSCAD checks its
//!   stack (function calls, user modules), and at builtin modules with a
//!   quarter more room, since a chain of `children()` nests builtins only.
//!   On wasm32 the stack that overflows first
//!   is one the module cannot measure: a WebAssembly engine runs wasm
//!   functions on its own machine stack (about 1 MB in V8), and overflowing
//!   it throws `RangeError: Maximum call stack size exceeded` out of the
//!   module, which leaves the instance unusable. The budget there,
//!   [`DEFAULT_FRAME_LIMIT`], is calibrated so that a program at the limit
//!   still evaluates, renders and frees its tree within V8's default
//!   stack; natively the budget is unlimited. Browsers differ far more
//!   than that calibration covers (a WebKit worker's stack holds about a
//!   fifth of the frames of node's), and disagree about which kind of
//!   recursion is expensive, so the web worker probes its own engine at
//!   start-up and sets a weight per kind of frame ([`FrameWeights`],
//!   [`set_frame_weights`]) under one large budget
//!   ([`set_default_frame_limit`]).
//!
//! On wasm32 the measured limit still guards the module's own stack in
//! linear memory (Rust's "shadow stack", where locals whose address is
//! taken live). rustc links wasm with the stack first in memory, growing
//! down towards address 0, so the address of a local at the start of
//! evaluation is exactly the stack left (checked: 1,048,571 with rustc's
//! default 1 MiB, 8,388,603 with 8 MiB). The limit is capped by it, less
//! [`WASM_STACK_RESERVE`], so a module linked with less than
//! [`WASM_STACK_SIZE`] recurses less deeply but still fails cleanly.
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
//! | neoscad, native | 110,361 | 16,842 |
//! | neoscad, native, PGO build (`scripts/pgo.sh`) | 54,465 | 11,183 |
//! | neoscad, wasm32 in node 18 | 498 | 249 |
//! | wasm32 without the budget: V8 overflows at | 1,076 | 527 |
//!
//! Natively neoscad recurses deeper than OpenSCAD, and that is the policy:
//! the limit exists to turn a crash into an error, not to reject programs
//! OpenSCAD could run on a bigger stack, and an infinite recursion prints
//! the same error and trace either way. There is no mode that matches the
//! nightly's depth, because that depth is a property of its build (stack
//! size, compiler, frame sizes) rather than of the language. On WASM the
//! engine's stack makes neoscad much shallower than native OpenSCAD; a
//! host whose engine has more stack can raise the budget, and smaller
//! evaluator and renderer frames would raise the default.

/// The default [`crate::Options::stack_limit`]. Rust frames for one
/// OpenSCAD call are larger than OpenSCAD's own, so this is scaled up from
/// OpenSCAD's 8 MiB so programs recurse at least as deep as they do there.
///
/// It was 48 MiB until a profile-guided build (`scripts/pgo.sh`) inlined
/// more into the evaluator: its frames grew by half, and module recursion
/// cleared the nightly by only 13% (`recursion-test-module`, 34,353
/// excluded frames against 30,261) and 19% (the table's module, 8,387),
/// under the 25% margin `conformance depth` holds every build to, since
/// frame sizes move with the target, compiler and profile. At 64 MiB the
/// PGO build clears it by 51% and 59%. The stack is only touched when a
/// program recurses that deep: a runaway recursion to the limit peaks at
/// about 85 MB and 0.02 s (macOS arm64).
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
/// --depths --all-programs --frames=4000000000`: over seven kinds of
/// recursion (plain, nested expressions, list comprehensions, builtin
/// calls, transforms, `children()`), the deepest each reaches under this
/// budget is at most 63% of the depth where V8 overflowed (38% to 63%),
/// which leaves room for engines and embeddings with somewhat less stack.
#[cfg(all(target_arch = "wasm32", not(debug_assertions)))]
pub const DEFAULT_FRAME_LIMIT: u32 = 2_000;

/// The default [`crate::Options::frame_limit`] in an unoptimised wasm32
/// build, whose frames are larger: calibrated the same way, the deepest
/// recursion it allows reaches at most 57% of V8's limit.
#[cfg(all(target_arch = "wasm32", debug_assertions))]
pub const DEFAULT_FRAME_LIMIT: u32 = 600;

/// The frame budget [`crate::Options::default`] starts from: on wasm32 the
/// one [`set_default_frame_limit`] chose, if any, else
/// [`DEFAULT_FRAME_LIMIT`].
pub fn default_frame_limit() -> u32 {
    #[cfg(target_arch = "wasm32")]
    {
        match wasm_host::FRAME_LIMIT.load(std::sync::atomic::Ordering::Relaxed) {
            0 => DEFAULT_FRAME_LIMIT,
            n => n,
        }
    }
    #[cfg(not(target_arch = "wasm32"))]
    DEFAULT_FRAME_LIMIT
}

/// Sets the frame budget every later [`crate::Options::default`] starts
/// from, for this instance (0 restores [`DEFAULT_FRAME_LIMIT`]).
///
/// A browser's engine decides how much native stack a wasm frame costs,
/// and they differ by far more than any single default can cover:
/// JavaScriptCore's baseline tier (BBQ) gives every frame of this code
/// about a kilobyte, and a worker on macOS has about 512 KiB of stack, so
/// a recursive module that stops cleanly at the default budget in V8
/// overflowed WebKit's stack at a fifth of the depth, and the trap killed
/// the instance. The web worker measures its engine once at start-up
/// (`crates/web/js/worker.js`) and sets the budget here, with the weights
/// ([`set_frame_weights`]) that make each kind of recursion stop at its
/// share of the depth that engine's stack holds. It is a
/// process-wide default rather than an option because every evaluation in
/// the instance runs on the same stack, whichever API starts it.
///
/// wasm32 only: a native process measures its stack exactly, and a global
/// here would leak between tests that run in parallel.
#[cfg(target_arch = "wasm32")]
pub fn set_default_frame_limit(limit: u32) {
    wasm_host::FRAME_LIMIT.store(limit, std::sync::atomic::Ordering::Relaxed);
}

/// The frames in use at the last recursion check, on wasm32: what the web
/// worker reads from an instance whose stack overflowed (the call is a
/// load of a static, which is safe after the trap) to learn how many
/// frames that engine's stack holds. Always 0 natively.
pub fn frames_at_last_check() -> u32 {
    #[cfg(target_arch = "wasm32")]
    {
        wasm_host::FRAMES_SEEN.load(std::sync::atomic::Ordering::Relaxed)
    }
    #[cfg(not(target_arch = "wasm32"))]
    0
}

/// Records `frames` for [`frames_at_last_check`]. A no-op natively, where
/// the check is on the hot path of every call and nothing reads it.
#[inline(always)]
pub(crate) fn note_frames(frames: u32) {
    #[cfg(target_arch = "wasm32")]
    wasm_host::FRAMES_SEEN.store(frames, std::sync::atomic::Ordering::Relaxed);
    #[cfg(not(target_arch = "wasm32"))]
    let _ = frames;
}

/// Whether this build runs recursion on the heap (the `heap-eval`
/// feature): statements and calls past a few native levels then hold no
/// native stack, so a recursion ends at the counted depth limit
/// ([`crate::limits::Limits::depth`]) rather than at the frame budget.
///
/// A host that tunes itself to the evaluator's stack use must ask this
/// crate, not its own features. The web worker's stack probes recurse
/// until the stack overflows; on the heap they only stop at the depth
/// limit, which took about 7 s at start-up in a WebKit worker. When the
/// heap evaluator came on through this crate's default features while
/// the web core's own `heap-eval` feature stayed off, the core told the
/// worker it was recursive, the probes ran, and the first preview and
/// the language server's requests waited behind them.
pub const HEAP_EVAL: bool = cfg!(feature = "heap-eval");

/// What one nested frame of each kind adds to the frame budget's count.
///
/// The defaults are [`STATEMENT_FRAMES`] and its neighbours: one set of
/// weights, measured in V8. Engines disagree about which kind is
/// expensive (in JavaScriptCore a function level costs more stack per
/// default weight than a module level, in V8 a comprehension does), so on
/// wasm32 the web worker measures each kind's depth in its own engine and
/// sets weights that make each kind stop at a share of the depth its stack
/// holds ([`set_frame_weights`]), under one large budget. A program that
/// mixes kinds then sums its levels' real costs, and a module chain is not
/// held to the depth of the most expensive kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FrameWeights {
    /// A statement instantiation ([`STATEMENT_FRAMES`]).
    pub statement: u32,
    /// A nested expression ([`EXPRESSION_FRAMES`]).
    pub expression: u32,
    /// A function call ([`CALL_FRAMES`]).
    pub call: u32,
    /// A list comprehension element ([`COMPREHENSION_FRAMES`]).
    pub comprehension: u32,
    /// Extra for a builtin module's children (`translate() ...`), on top of
    /// its statement's weight ([`GEOMETRY_FRAMES`]).
    pub geometry: u32,
}

/// The weights natively, and on wasm32 until a host sets its own.
pub const DEFAULT_WEIGHTS: FrameWeights = FrameWeights {
    statement: STATEMENT_FRAMES,
    expression: EXPRESSION_FRAMES,
    call: CALL_FRAMES,
    comprehension: COMPREHENSION_FRAMES,
    geometry: GEOMETRY_FRAMES,
};

/// The weights an evaluation starts with: [`DEFAULT_WEIGHTS`], or on wasm32
/// the ones [`set_frame_weights`] chose.
pub fn frame_weights() -> FrameWeights {
    #[cfg(target_arch = "wasm32")]
    {
        use std::sync::atomic::Ordering::Relaxed;
        let w = &wasm_host::WEIGHTS;
        // Stored plus one, so that 0 means "not set" and a weight of 0
        // (the probe counts one kind at a time) can still be set.
        let get = |i: usize, d: u32| match w[i].load(Relaxed) {
            0 => d,
            n => n - 1,
        };
        FrameWeights {
            statement: get(0, STATEMENT_FRAMES),
            expression: get(1, EXPRESSION_FRAMES),
            call: get(2, CALL_FRAMES),
            comprehension: get(3, COMPREHENSION_FRAMES),
            geometry: get(4, GEOMETRY_FRAMES),
        }
    }
    #[cfg(not(target_arch = "wasm32"))]
    DEFAULT_WEIGHTS
}

/// Sets the weights every later evaluation in this instance counts frames
/// with (a weight of 0 counts nothing for that kind; `u32::MAX` keeps its
/// default). Like [`set_default_frame_limit`],
/// process-wide and wasm32 only: one web worker is one instance, and every
/// evaluation in it runs on the same engine stack.
#[cfg(target_arch = "wasm32")]
pub fn set_frame_weights(w: FrameWeights) {
    use std::sync::atomic::Ordering::Relaxed;
    let v = [
        w.statement,
        w.expression,
        w.call,
        w.comprehension,
        w.geometry,
    ];
    for (slot, n) in wasm_host::WEIGHTS.iter().zip(v) {
        slot.store(n.wrapping_add(1), Relaxed);
    }
}

#[cfg(target_arch = "wasm32")]
mod wasm_host {
    use std::sync::atomic::AtomicU32;

    /// [`super::set_default_frame_limit`]'s budget; 0 when none was set.
    pub(super) static FRAME_LIMIT: AtomicU32 = AtomicU32::new(0);
    /// [`super::frames_at_last_check`].
    pub(super) static FRAMES_SEEN: AtomicU32 = AtomicU32::new(0);
    /// [`super::set_frame_weights`]: statement, expression, call,
    /// comprehension, geometry, each plus one; 0 for the default.
    pub(super) static WEIGHTS: [AtomicU32; 5] = [
        AtomicU32::new(0),
        AtomicU32::new(0),
        AtomicU32::new(0),
        AtomicU32::new(0),
        AtomicU32::new(0),
    ];
}

/// Extra frames a builtin module's children hold, on top of the statement's
/// [`STATEMENT_FRAMES`]: 0 by default, where the V8 calibration counted
/// every statement alike. In JavaScriptCore a `translate()` level costs
/// about twice the stack of a user module or `children()` level
/// (`geometry_module`'s frame), and the web worker's probe sets a weight
/// for it so that the cheaper statements are not held to its depth.
pub const GEOMETRY_FRAMES: u32 = 0;

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
/// the default 2,000. (In the browser the web worker's probe sets its own
/// weights; this default is what node and other wasm hosts use.)
pub const COMPREHENSION_FRAMES: u32 = 4;

/// Frames a statement instantiation holds. Each nested statement is also a
/// level of the node tree, but the walks over the finished tree (the dump,
/// the cache keys, rendering, copying and freeing it) keep their pending
/// nodes on the heap and add no stack per level: what limits recursive
/// modules on WASM is instantiation itself. Making those walks iterative
/// left V8's overflow depths unchanged (1,611 module levels, 1,712
/// function levels in node 22 without a budget), so this weight is
/// conservative: a module level costs about what a function level does.
///
/// The weights only matter where the frame budget is finite (wasm32);
/// they were measured there, as V8 stack per level of the recursions in
/// `crates/wasm-check/run.js` (`--depths --all-programs`) divided by the
/// frames each level holds.
pub const STATEMENT_FRAMES: u32 = 4;

/// The linear-memory stack a wasm32 build of neoscad should be linked
/// with: `-C link-arg=-zstack-size=8388608` (rustc's default is 1 MiB).
/// The evaluator stays safe with less, but recursion then stops sooner.
/// (The walks over the finished node tree no longer need stack in
/// proportion to its depth.) `crates/wasm-check/build.rs` links this way.
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
