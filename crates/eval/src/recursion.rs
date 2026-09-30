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
//!   stack; natively the budget is unlimited.
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

/// Frames an expression holds.
pub const EXPRESSION_FRAMES: u32 = 1;

/// Frames a function call holds: calling (binding arguments, the tail-call
/// loop) takes about twice an expression's stack.
pub const CALL_FRAMES: u32 = 2;

/// Frames a list comprehension element (`for`, `let`, `if`, `each`
/// inside `[...]`) holds: its evaluation path is about twice as deep in
/// Rust frames as an expression's.
pub const COMPREHENSION_FRAMES: u32 = 2;

/// Frames a statement instantiation holds. Each nested statement is also a
/// level of the node tree, which rendering, the cache keys and freeing the
/// tree walk recursively after evaluation; per level that walk costs about
/// four times the stack of evaluating an expression, and it is what limits
/// recursive modules on WASM.
///
/// The weights only matter where the frame budget is finite (wasm32);
/// they were measured there, as V8 stack per level of the recursions in
/// `crates/wasm-check/run.js` (`--depths --all-programs`) divided by the
/// frames each level holds.
pub const STATEMENT_FRAMES: u32 = 4;

/// The linear-memory stack a wasm32 build of neoscad should be linked
/// with: `-C link-arg=-zstack-size=8388608` (rustc's default is 1 MiB).
/// The evaluator stays safe with less, but recursion then stops sooner,
/// and the geometry walk over a deep tree, which has no check of its own,
/// needs the room too. `crates/wasm-check/build.rs` links this way.
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
