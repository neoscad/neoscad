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
//!   element holds a few frames ([`EXPRESSION_FRAMES`] and the like), a
//!   and a heap loop started from native code many
//!   ([`HEAP_LOOP_FRAMES`]). On wasm32 the
//!   stack that overflows first is one the module cannot measure: a
//!   WebAssembly engine runs wasm functions on its own machine stack
//!   (about 1 MB in V8), and overflowing it throws `RangeError: Maximum
//!   call stack size exceeded` out of the module, which leaves the
//!   instance unusable. The
//!   budget there, [`DEFAULT_FRAME_LIMIT`], and the weights are constants
//!   that keep every shape below within the smallest engine stack, a
//!   WebKit worker's (about 512 KiB); natively the budget is unlimited.
//!
//! Both are checked at every function call (`Evaluator::call_exhausted`).
//! What they still guard:
//!
//! - the first `heap_expr::NATIVE_CALLS` nested user calls, which run
//!   natively for speed (a bounded amount of stack);
//! - the one shape that stays native and starts a nested heap loop for
//!   each call it reaches: a `use`d library's assignments (see
//!   `heap_expr`). A recursion through them at every level holds native
//!   stack per level, and the check at its calls is what stops it
//!   cleanly;
//! - the source's own nesting, which the parser bounds first with a
//!   counted limit (`lang::syntax::parser::NESTING_LIMIT`).
//!
//! Values take none: printing, comparing, hashing, the element-wise
//! operators, `chr()` and freeing walk a nested value with a stack of
//! their own, so a value nested as deep as a recursion can build it needs
//! no native stack per level (`tests/value_depth.rs` runs them on a
//! thread of 512 KiB). Printing still fails with OpenSCAD's "Stack
//! exhausted" error, at a counted depth (`print.rs`).
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
/// reason holds now that recursion runs on the heap. What still uses the
/// native stack, measured as the smallest evaluation thread that runs it
/// (plain release build, and debug, macOS arm64; October 2026):
/// - source nested to the parser's limit: at most 2.7 MiB (`{` blocks),
///   4.3 MiB in a debug build at its lower limit;
/// - every file of the BOSL2 corpus, examples and tests: 64 KiB;
/// - the one shape that stays native, a recursion through two `use`d
///   libraries' assignments: 3.3 KiB a level (13 KiB in debug), so this
///   limit stops it at about 20,000 levels, where OpenSCAD's nightly stops
///   between 3,000 and 6,000.
///
/// The stack is only touched when a program needs it. A smaller limit
/// and thread are proposed in `docs/followups.md`.
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
/// meets the shape that still recurses natively, and a heap loop's weight
/// ([`HEAP_LOOP_FRAMES`]) is set so that this budget stops it short of a
/// WebKit worker's stack, the smallest of the three engines'. (Printing a
/// nested vector had a weight too, which stopped it at 250 levels in a
/// browser, where WebKit's stack overflowed at 518; it holds no native
/// stack per level now.)
#[cfg(all(target_arch = "wasm32", not(debug_assertions)))]
pub const DEFAULT_FRAME_LIMIT: u32 = 2_000;

/// The default [`crate::Options::frame_limit`] in an unoptimised wasm32
/// build, whose frames are larger: calibrated the same way, the deepest
/// recursion it allowed reached at most 57% of V8's limit.
#[cfg(all(target_arch = "wasm32", debug_assertions))]
pub const DEFAULT_FRAME_LIMIT: u32 = 600;

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

/// Frames a nested heap loop holds: `heap_expr`'s `heap_eval`, which a
/// call past the native call levels starts from native code, and which
/// each level of a recursion through the shape that stays native (a
/// `use`d library's assignments; a range's bounds, as measured below, and
/// a C-style `for`, `object()` and parameter defaults, before they moved
/// to the heap) starts again. Its native
/// frames are large. When this was
/// [`CALL_FRAMES`], the release budget let such a recursion run to
/// 280-660 levels, and the web core trapped instead: measured without
/// the weight (October 2026, Playwright's browsers on macOS arm64), a
/// WebKit worker's stack overflowed at 57-60 levels, about 8 KiB a level,
/// in its default tiers and with its interpreter or its optimising tier
/// turned off; Chromium's at 223-280 and Firefox's at 393-724. At this
/// weight [`DEFAULT_FRAME_LIMIT`] stops them at 34-37 levels in all three.
pub const HEAP_LOOP_FRAMES: u32 = 64;

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
