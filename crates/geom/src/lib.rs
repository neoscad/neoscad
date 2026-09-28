//! Geometry for NeoSCAD: builds OpenSCAD's geometry from the node tree the
//! evaluator produces, and writes it out as mesh files.
//!
//! - [`primitives`]: `cube`, `sphere`, `cylinder`, `polyhedron`, `square`,
//!   `circle` and `polygon`, tessellated exactly as OpenSCAD does.
//! - [`manifold_geom`]: 3D solids on `manifold-rust`, the pure-Rust port of
//!   the Manifold kernel OpenSCAD uses by default, with OpenSCAD's colour
//!   bookkeeping, plus the slices and outlines `projection()` takes.
//! - [`polygon2d`] and [`clipper`]: 2D shapes and the 2D kernel on
//!   `clipper2-rust` (sanitizing, booleans, `offset`, `fill`).
//! - [`extrude`]: `linear_extrude` and `rotate_extrude`.
//! - [`hull`] and [`minkowski`]: `hull()` and `minkowski()` in 2D and 3D.
//! - [`evaluate`]: the tree walk, with a cache keyed by
//!   [`eval::dump::Keys`] and optional parallelism (feature `parallel`).
//! - [`export`]: the mesh a result exports as, handed to the `io` crate's
//!   STL, OFF, OBJ, 3MF, SVG and DXF writers, and the render summary.
//! - `import`: `import()` and `surface()` over the `io` crate's readers.
//! - `text()` is built by the `text` crate (fonts, shaping, outlines) and
//!   unioned here; the fonts come in [`RenderOptions::fonts`].
//!
//! `.nef3` imports belong to a later phase; the evaluator reports them as
//! [`evaluate::Unsupported`].

pub mod clipper;
pub mod color;
pub mod csg;
pub mod evaluate;
pub mod export;
pub mod extrude;
pub mod fragments;
pub mod hull;
mod import;
pub mod libtess2;
pub mod manifold_geom;
pub mod minkowski;
pub mod polygon2d;
pub mod polyset;
pub mod primitives;

use std::sync::Arc;

pub use eval::node::{IDENTITY, Matrix};
pub use evaluate::{
    CACHE_BUDGET, CacheStats, INTERRUPTED, Msg, MsgLoc, RenderOptions, Rendered, Renderer,
    Unsupported, result_key,
};

use manifold_geom::ManifoldGeometry;
use polygon2d::Polygon2d;
use polyset::PolySet;

/// A node's geometry: a mesh, a Manifold solid, or 2D outlines. Shared, so
/// the cache and several parents can hold one result.
#[derive(Debug, Clone)]
pub enum Geometry {
    PolySet(Arc<PolySet>),
    Manifold(Arc<ManifoldGeometry>),
    Polygon2d(Arc<Polygon2d>),
}

impl Geometry {
    pub fn dimension(&self) -> u32 {
        match self {
            Geometry::PolySet(_) | Geometry::Manifold(_) => 3,
            Geometry::Polygon2d(_) => 2,
        }
    }

    pub fn is_empty(&self) -> bool {
        match self {
            Geometry::PolySet(p) => p.is_empty(),
            Geometry::Manifold(m) => m.is_empty(),
            Geometry::Polygon2d(p) => p.is_empty(),
        }
    }
}
