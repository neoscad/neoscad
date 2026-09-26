//! File formats for NeoSCAD, in OpenSCAD's layouts and with its quirks.
//!
//! Readers (what `import()` and `surface()` load):
//! - [`stl`]: ASCII and binary STL (`src/io/import_stl.cc`);
//! - [`off`]: OFF with per-face colours (`import_off.cc`);
//! - [`obj`]: Wavefront OBJ vertices and faces (`import_obj.cc`);
//! - [`threemf`]: 3MF meshes, components, build items and colours, as
//!   OpenSCAD's lib3mf v2 path reads them (`import_3mf_v2.cc`);
//! - [`dxf`]: DXF entities joined into paths (`DxfData.cc`), which also
//!   serves `dxf_dim()`/`dxf_cross()` in the evaluator;
//! - [`svg`]: a port of OpenSCAD's own `libsvg` plus `import_svg.cc`;
//! - [`surface`]: `.dat` and PNG heightmaps (`SurfaceNode.cc`).
//!
//! Writers: STL, OFF, OBJ, SVG, DXF and 3MF (`export_*.cc`).
//!
//! This crate knows file formats and nothing about geometry kernels, so it
//! sits below both the evaluator (which needs the DXF reader) and `geom`
//! (which turns readers' output into its geometry and hands meshes to the
//! writers). Readers return plain data ([`Mesh`], [`Outline`] lists) and
//! the messages OpenSCAD prints while reading, word for word; the caller
//! decides where the messages point. Files are read through
//! [`lang::loader::FileSystem`], so the WASM build can supply its own.

pub mod color;
pub mod dxf;
pub mod mesh;
pub mod obj;
pub mod off;
pub mod stl;
pub mod surface;
pub mod svg;
pub mod text;
pub mod threemf;
pub mod trig;

pub use color::Color;
pub use lang::diag::Severity;
pub use mesh::{Mesh, MeshBuilder, MeshRef};

/// One closed 2D outline (OpenSCAD's `Outline2d`).
#[derive(Debug, Clone, PartialEq)]
pub struct Outline {
    pub vertices: Vec<[f64; 2]>,
    /// `positive`: an outer outline rather than a hole. Clipper results set
    /// it from the winding; `polygon()` sets it from path order (only the
    /// first path is positive), which matters for extrusion diagonals.
    pub positive: bool,
}

impl Outline {
    pub fn new(vertices: Vec<[f64; 2]>) -> Outline {
        Outline {
            vertices,
            positive: true,
        }
    }
}

/// A message a reader or writer produced, with OpenSCAD's text.
#[derive(Debug, Clone, PartialEq)]
pub struct Message {
    /// `None` for OpenSCAD's plain `LOG(...)` lines, printed without a
    /// `WARNING:`-style prefix.
    pub severity: Option<Severity>,
    pub text: String,
    /// OpenSCAD logs it with the `import()` call's location, so it ends in
    /// "in file F, line N". The others carry no location (some name the
    /// line in their own text instead).
    pub located: bool,
}

impl Message {
    pub fn warning(text: impl Into<String>) -> Message {
        Message {
            severity: Some(Severity::Warning),
            text: text.into(),
            located: false,
        }
    }

    pub fn error(text: impl Into<String>) -> Message {
        Message {
            severity: Some(Severity::Error),
            text: text.into(),
            located: false,
        }
    }

    /// The same message, logged with the caller's location.
    pub fn at_call(mut self) -> Message {
        self.located = true;
        self
    }
}

/// Segment counts for curves, as OpenSCAD's `CurveDiscretizer` gives them
/// from `$fn`, `$fa` and `$fs`. The evaluator and `geom` own the counting
/// rule; readers only ask.
pub trait Curves {
    /// `getCircularSegmentCount(r, angle_degrees)`; `None` where OpenSCAD's
    /// returns no value (a radius below the grid, a non-finite `$fn` or
    /// angle), and each caller then picks its own fallback.
    fn circular_segments(&self, r: f64, angle_degrees: f64) -> Option<i32>;
    /// `getPathSegmentCount()`: `max($fn, 3)`, for Bézier curves.
    fn path_segments(&self) -> i32;
}
