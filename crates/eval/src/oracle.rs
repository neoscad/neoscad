//! The geometry oracle: how `child_bounds()` and `child_measure()`
//! (`--enable query`, `docs/language-extensions.md` section 5.4) get
//! numbers about geometry while the program is still being evaluated.
//!
//! Values exist before geometry: `geom` renders the node tree after
//! evaluation, and it depends on this crate, not the other way round. So
//! the evaluator cannot render anything itself. A host that renders hands
//! it a [`GeometryOracle`] in [`crate::Options::geometry`]; a query
//! instantiates the child in its sandbox (`crate::query`), asks the oracle
//! about that subtree, and goes on with the answer. With no oracle (a host
//! that never renders, or a library test) a query warns
//! `query-unavailable` and is `undef`.
//!
//! The answer must be what a full render of the child gives, in preview
//! too (`%` children left out, `#` ones kept), or a model would change
//! shape between preview and render. It must also be the same on every
//! platform and at any thread count, since it becomes ordinary numbers in
//! the program: bounds are minima and maxima over the result's vertices,
//! and the sums (area, volume) are taken serially in mesh order.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use crate::node::Node;

/// What a query learns about a subtree's rendered geometry.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Facts {
    /// The subtree renders to nothing.
    Empty,
    /// A 2D result: its bounding box and its area (holes subtracted).
    Flat {
        min: [f64; 2],
        max: [f64; 2],
        area: f64,
    },
    /// A 3D result: its bounding box, the volume it encloses and the area
    /// of its surface.
    Solid {
        min: [f64; 3],
        max: [f64; 3],
        volume: f64,
        surface_area: f64,
    },
}

/// Why the oracle gave no facts.
#[derive(Debug, Clone, PartialEq)]
pub enum OracleError {
    /// The render stopped: the request was cancelled, or a resource limit
    /// was passed (recorded on the guard, as a render records it).
    Interrupted,
    /// The child uses something the renderer cannot build; the text says
    /// what, as the command line's "not implemented" line does.
    Unsupported(String),
}

/// Renders a subtree for a query, as a full render of the model would
/// render it there. Implemented by `session` over the request's geometry
/// cache, so the final render finds the queried subtree already built.
pub trait GeometryOracle: std::fmt::Debug + Send + Sync {
    /// The facts of `subtree` (a group of the children a query asked
    /// about, numbered from 0), rendered under the evaluation's interrupt
    /// flag and limits, which stop a long query render as they stop the
    /// final one.
    fn measure(
        &self,
        subtree: &Node,
        interrupt: Option<&Arc<AtomicBool>>,
        guard: Option<&Arc<crate::limits::Guard>>,
    ) -> Result<Facts, OracleError>;
}
