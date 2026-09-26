//! 2D geometry, as far as phase 5a needs it.
//!
//! The 2D kernel (Clipper2: sanitising outlines, 2D booleans, offsets) is
//! phase 5b. Until then 2D leaves are built exactly as OpenSCAD builds them
//! and transformed, which is enough for the 3D evaluator to tell 2D from 3D
//! and emit OpenSCAD's mixing warnings. Anything that would need Clipper
//! marks the result `approximate`, and nothing exports 2D yet.

use crate::Matrix;

/// Outlines of a 2D shape (`Polygon2d`), unsanitised.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Polygon2d {
    pub outlines: Vec<Vec<[f64; 2]>>,
    /// Set when a 2D operation was skipped for lack of a 2D kernel, so the
    /// outlines are not the operation's result.
    pub approximate: bool,
}

impl Polygon2d {
    pub fn from_outlines(outlines: Vec<Vec<[f64; 2]>>) -> Polygon2d {
        Polygon2d { outlines, approximate: false }
    }

    pub fn is_empty(&self) -> bool {
        self.outlines.is_empty()
    }

    /// The 2D part of a 3D transform (`GeometryEvaluator.cc:754-758`: rows
    /// and columns 0, 1 and 3).
    pub fn transform(&mut self, m: &Matrix) {
        for o in &mut self.outlines {
            for p in o.iter_mut() {
                let (x, y) = (p[0], p[1]);
                *p = [m[0][0] * x + m[0][1] * y + m[0][3], m[1][0] * x + m[1][1] * y + m[1][3]];
            }
        }
    }
}
