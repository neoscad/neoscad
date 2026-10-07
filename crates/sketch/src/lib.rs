//! A deterministic 2D geometric constraint solver for CAD sketches.
//!
//! Build a [`Sketch`] of points, lines, arcs and circles, each point with a
//! guess (where it is drawn), add [`Constraint`]s, and [`Sketch::solve`]
//! it. The [`Solution`] has the solved coordinates and a diagnosis: free
//! degrees of freedom and which coordinates can still move, redundant
//! constraints, conflicting constraints, and any place where the solution
//! flipped away from the drawing.
//!
//! ```
//! use sketch_solver::{Along, Constraint, Pair, Sketch, Status};
//!
//! // A right triangle: a horizontal base of 40 and a vertical side of 30.
//! let mut s = Sketch::new();
//! let o = s.point(Some([0.0, 0.0]))?;
//! let a = s.point(Some([38.0, 1.0]))?;  // drawn roughly
//! let b = s.point(Some([39.0, 28.0]))?;
//! let base = s.line(o, a)?;
//! let side = s.line(a, b)?;
//! let hyp = s.line(b, o)?;
//! s.add(Constraint::Fix { entity: o, at: None })?;
//! s.add(Constraint::Horizontal(Pair::Line(base)))?;
//! s.add(Constraint::Vertical(Pair::Line(side)))?;
//! s.add(Constraint::Length { line: base, value: 40.0 })?;
//! s.add(Constraint::Distance { a, b, value: 30.0, along: Along::Y })?;
//!
//! let sol = s.solve();
//! assert_eq!(sol.status, Status::Solved);
//! assert_eq!(sol.dof, 0);
//! assert_eq!(sol.point(b), Some([40.0, 30.0]));
//! # let _ = hyp;
//! # Ok::<(), sketch_solver::ModelError>(())
//! ```
//!
//! # How it solves
//!
//! 1. The unknowns and equations are split into connected components,
//!    each solved on its own.
//! 2. *Clean the drawing*: solve with every dimension set to its drawn
//!    value, so only the geometric constraints (horizontal, tangent, ...)
//!    move points, by minimal-norm steps that leave unconstrained
//!    directions where they were drawn.
//! 3. *Solve*: Levenberg–Marquardt with a fixed damping schedule, from the
//!    cleaned drawing to the real dimensions.
//! 4. *Check the branch*: if the solve failed, or the solution's
//!    orientation (which way corners turn, which side of 180° an arc
//!    sweeps) differs from the drawing's, *continuation* moves the
//!    dimensions from drawn to wanted in steps, each from the last, which
//!    follows the drawing's branch through large changes.
//! 5. *Exactify*: coordinates that equations make equal get the same bits,
//!    and fixed ones their exact values.
//! 6. *Diagnose*: QR of the Jacobian's rows in constraint order gives the
//!    rank, the free coordinates (from the null space), and each dependent
//!    equation with the earlier ones it depends on: redundant if its
//!    residual agrees with theirs, a conflict if not.
//!
//! # Determinism
//!
//! The result depends only on the sketch: the same bits on every platform
//! (aarch64, x86_64, wasm32), at any thread count, and never on an earlier
//! solve.
//!
//! - It is single-threaded and allocation order does not matter: no hash
//!   maps, and every loop has a fixed order.
//! - Arithmetic is IEEE `+ - * /` and `sqrt`, which are correctly rounded
//!   everywhere. No fused multiply-add (`mul_add` fuses on some targets
//!   only), and Rust never contracts on its own.
//! - The only trigonometry (an angle constraint's cosine and sine, the
//!   drawn angle, the placement of unguessed points) goes through the
//!   pure-Rust `libm` crate, not the platform's maths library, whose last
//!   bits differ between platforms.
//! - Iteration counts and stopping depend only on those values.
//!
//! The crate uses no file system, environment, clock or threads, and
//! builds for `wasm32-unknown-unknown`.

mod equations;
mod linalg;
mod model;
mod report;
mod solve;
mod trig;

pub use equations::{Coordinate, Source};
pub use model::{
    Along, Constraint, ConstraintId, Entity, EntityId, EntityKind, ModelError, Pair, Sketch,
};
pub use report::{Dependency, FreeCoordinate, Orientation, Solution, SolveError, Status};
pub use solve::SolveOptions;

/// The README's example, run as a doctest so it cannot go stale.
#[cfg(doctest)]
#[doc = include_str!("../README.md")]
struct ReadmeDoctests;
