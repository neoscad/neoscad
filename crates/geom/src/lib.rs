//! Geometry for NeoSCAD: builds OpenSCAD's geometry from the node tree the
//! evaluator produces, and writes it out as mesh files.
//!
//! - [`primitives`]: `cube`, `sphere`, `cylinder`, `polyhedron` (and the 2D
//!   leaves) tessellated exactly as OpenSCAD does.
//! - [`manifold_geom`]: 3D solids on `manifold-rust`, the pure-Rust port of
//!   the Manifold kernel OpenSCAD uses by default, with OpenSCAD's colour
//!   bookkeeping.
//! - [`evaluate`]: the tree walk, with a cache keyed by
//!   [`eval::dump::Keys`] and optional parallelism (feature `parallel`).
//! - [`export`]: STL (ASCII and binary), OFF and OBJ in OpenSCAD's formats,
//!   and the render summary.
//!
//! 2D booleans, extrusions, hull, minkowski, resize, surface, import and
//! text belong to later phases; the evaluator reports them as
//! [`evaluate::Unsupported`].

pub mod color;
pub mod evaluate;
pub mod export;
pub mod fragments;
pub mod manifold_geom;
pub mod polygon2d;
pub mod polyset;
pub mod primitives;

use std::sync::Arc;

pub use eval::node::{IDENTITY, Matrix};
pub use evaluate::{Msg, MsgLoc, RenderOptions, Rendered, Renderer, Unsupported};

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
