//! The geometry-agnostic node tree evaluation produces.
//!
//! Each builtin module becomes a node carrying its evaluated parameters,
//! in the shape OpenSCAD's `AbstractNode` subclasses hold them (and the
//! `.csg` export prints them): tessellation settings are resolved from
//! `$fn`/`$fa`/`$fs` at instantiation, transforms are 4x4 matrices, and
//! user modules become named groups. Nothing here computes geometry.

use lang::source::Span;

/// `$fn`, `$fa`, `$fs` as a node captured them (`CurveDiscretizer`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Discretizer {
    pub fn_: f64,
    pub fa: f64,
    pub fs: f64,
}

/// A 4x4 affine matrix, row major.
pub type Matrix = [[f64; 4]; 4];

pub const IDENTITY: Matrix = [[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0], [0.0, 0.0, 1.0, 0.0], [0.0, 0.0, 0.0, 1.0]];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CsgOp {
    Union,
    Difference,
    Intersection,
}

#[derive(Debug, Clone, PartialEq)]
pub enum NodeKind {
    /// The top of the tree (`RootNode`).
    Root,
    /// `group()`, a control module's children, or a user module call
    /// (`name` is then `"module <name>"`).
    Group { name: Option<String> },
    /// The children of `intersection_for`.
    IntersectionFor,
    Csg(CsgOp),
    Transform { matrix: Matrix, verb: &'static str },
    Color { rgba: [f32; 4] },
    Render { convexity: i32 },
    Projection { cut: bool, convexity: i32 },
    Minkowski { convexity: i32 },
    Hull,
    Fill,
    Resize { newsize: [f64; 3], autosize: [bool; 3], convexity: i32 },
    Offset { delta: f64, chamfer: bool, join: OffsetJoin, disc: Discretizer },
    LinearExtrude(LinearExtrude),
    RotateExtrude { angle: f64, start: f64, convexity: i32, disc: Discretizer },
    Cube { size: [f64; 3], center: bool },
    Sphere { r: f64, disc: Discretizer },
    Cylinder { h: f64, r1: f64, r2: f64, center: bool, disc: Discretizer },
    Polyhedron { points: Vec<[f64; 3]>, faces: Vec<Vec<usize>>, convexity: i32 },
    Square { size: [f64; 2], center: bool },
    Circle { r: f64, disc: Discretizer },
    Polygon { points: Vec<[f64; 2]>, paths: Vec<Vec<usize>>, convexity: i32 },
    Surface { file: String, center: bool, invert: bool, convexity: i32 },
    Import(Import),
    Text(Text),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OffsetJoin {
    Round,
    Miter,
    Square,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LinearExtrude {
    pub height: [f64; 3],
    pub center: bool,
    pub convexity: u32,
    pub twist: f64,
    pub has_twist: bool,
    pub slices: u32,
    pub has_slices: bool,
    pub segments: u32,
    pub has_segments: bool,
    pub scale: [f64; 2],
    pub disc: Discretizer,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Import {
    /// The file type from the extension (`stl`, `off`, `dxf`, ...), empty
    /// when unknown.
    pub kind: String,
    pub file: String,
    pub layer: Option<String>,
    pub id: Option<String>,
    pub convexity: i32,
    pub origin: [f64; 2],
    pub scale: f64,
    pub center: bool,
    pub dpi: f64,
    pub width: f64,
    pub height: f64,
    pub disc: Discretizer,
}

/// `text()` parameters as given; shaping (and script detection) belongs to
/// the geometry phase.
#[derive(Debug, Clone, PartialEq)]
pub struct Text {
    pub text: String,
    pub size: f64,
    pub spacing: f64,
    pub font: String,
    pub direction: String,
    pub language: String,
    pub script: String,
    pub halign: String,
    pub valign: String,
    pub disc: Discretizer,
}

/// The instantiation that produced a node.
#[derive(Debug, Clone, PartialEq)]
pub struct Origin {
    pub name: String,
    /// Which parsed program the span belongs to: 0 for the main program,
    /// `1 + i` for the i-th library passed to the evaluator.
    pub unit: u32,
    pub span: Span,
    pub line: u32,
    pub tag_root: bool,
    pub tag_highlight: bool,
    pub tag_background: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Node {
    pub kind: NodeKind,
    pub children: Vec<Node>,
    /// `None` only for the root.
    pub origin: Option<Box<Origin>>,
    /// Creation order, as OpenSCAD's node index counter assigns it.
    pub index: usize,
}

impl Node {
    /// OpenSCAD's `find_root_tag`: the first node instantiated with `!`,
    /// and the origin of a second, different one if there is.
    pub fn find_root_tag(&self) -> (Option<&Node>, Option<&Origin>) {
        let mut found: Option<&Node> = None;
        let mut next: Option<&Origin> = None;
        fn walk<'n>(n: &'n Node, found: &mut Option<&'n Node>, next: &mut Option<&'n Origin>) {
            for c in &n.children {
                if next.is_some() {
                    return;
                }
                if let Some(o) = &c.origin
                    && o.tag_root
                {
                    match found {
                        None => *found = Some(c),
                        Some(f) => {
                            let same = f.origin.as_ref().is_some_and(|fo| fo.span == o.span && fo.unit == o.unit);
                            if !same {
                                *next = Some(o);
                                return;
                            }
                        }
                    }
                }
                walk(c, found, next);
            }
        }
        walk(self, &mut found, &mut next);
        (found, next)
    }
}
