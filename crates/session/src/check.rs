//! `check`: printability checks on a model's rendered solid, for FDM
//! printing (`neoscad check`, the server's `check` method, and the marks of
//! `snapshot --issues`). The JSON is documented in `docs/cli-json.md`.
//!
//! Each check works on the rendered solid's triangles ([`Mesh`]); with
//! named parts, findings name the part their faces came from, and the
//! parts' own solids are checked too.
//!
//! - **closed and manifold:** Manifold's status of the solid (and of each
//!   part's); a mesh result that was never through a boolean has its open
//!   and over-shared edges counted; and a valid solid is welded by vertex
//!   position, as an STL reader sees it, to find edges pinched where two
//!   pieces touch ([`crate::mesh::bad_edges`]); and by `f32` position,
//!   as a slicer reads an STL, to find faces that collapse and edges that
//!   break only there ([`crate::mesh::weld`], the `stl-precision` finding).
//! - **components:** pieces whose triangles share no vertex. A piece
//!   whose lowest point is above the model's lowest point (by more than
//!   [`CheckSettings::bed_tolerance`]) is an unsupported island.
//! - **wall thickness:** a sampled estimate. From points on every face
//!   (the centroid, or a grid of points on large faces) a ray goes inward
//!   along the face's normal to where it leaves the solid; that distance
//!   is the wall's thickness there. A thin reading is measured again in
//!   the layer plane, as a slicer sees the wall (`in_layer`), and the
//!   larger reading stands: a sliver's tilted normal no longer turns a
//!   twisted extrusion's end caps into walls. A wall is measured exactly
//!   where its two sides are parallel and overestimated where they are
//!   not, so the faces that could hold the thinnest reading are sampled
//!   again near their corners, which finds a tapered rim's edge; the
//!   model's `min_wall` is still the thinnest *sample* (`sampled`), and a
//!   feature narrower than the sample spacing on a large face can be
//!   missed.
//! - **overhangs:** downward faces steeper than the limit from vertical,
//!   excluding faces on the bed, grouped into connected regions; a
//!   finding points at its steepest faces and names their heights, and
//!   the area steeper than the limit plus 15° apart.
//! - **bed fit**, **tiny features** (pieces smaller than two extrusion
//!   widths) and **intersecting parts** (overlap volume by a boolean
//!   intersection of the two parts' solids).
//! - **input meshes:** each `polyhedron()` and imported mesh that is
//!   inside out, partly flipped, open or not manifold
//!   ([`crate::orient`]), listed first: they are what the other findings
//!   of a broken boolean come from.

use std::collections::HashMap;

use geom::Geometry;
use geom::manifold_geom::{ManifoldGeometry, OpType};
use lang::diag::DiagCode;
use serde_json::{Value, json};

use crate::mesh::{Aabb, Bvh, Mesh, V3, add, dot, scale};
use crate::parts::Part;
use crate::{Cancelled, Log, Run, Session, stats};

/// What counts as a problem. The defaults are common FDM values: a 0.4 mm
/// nozzle, walls of at least two perimeters (0.8 mm), overhangs up to 45°
/// from vertical without support, and no bed unless one is given.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CheckSettings {
    /// Build volume `[width, depth, height]` in mm; `None` skips the check.
    pub bed: Option<[f64; 3]>,
    /// Nozzle diameter (mm): walls thinner than this cannot be printed at
    /// all (an error), and features smaller than twice it are tiny.
    pub nozzle: f64,
    /// Walls thinner than this are a warning (mm).
    pub min_wall: f64,
    /// The steepest printable overhang, degrees from vertical.
    pub max_overhang: f64,
    /// How far above the lowest point a piece may start and still count
    /// as on the bed, and a face as bed contact (mm).
    pub bed_tolerance: f64,
    /// Findings reported per code; the rest are counted in `truncated`.
    pub max_findings: usize,
}

impl Default for CheckSettings {
    fn default() -> Self {
        CheckSettings {
            bed: None,
            nozzle: 0.4,
            min_wall: 0.8,
            max_overhang: 45.0,
            bed_tolerance: 0.05,
            max_findings: 10,
        }
    }
}

impl CheckSettings {
    pub fn json(&self) -> Value {
        json!({
            "bed": self.bed,
            "nozzle": self.nozzle,
            "min_wall": self.min_wall,
            "max_overhang": self.max_overhang,
            "bed_tolerance": self.bed_tolerance,
            "max_findings": self.max_findings,
        })
    }
}

/// The fix for a pinched edge or point (the `not-manifold` finding, and
/// `render`'s note).
pub const PINCH_FIX: &str = "two parts touch along an edge or at a point here; overlap them by at \
                             least 0.01 or separate them";

/// The fix for pinched edges of a result with no volume
/// ([`stats::touch_only`]): the parts only touch, so there is no overlap to
/// remove, and the overlap advice of [`PINCH_FIX`] is for joining them.
pub const TOUCH_FIX: &str = "the parts only touch (no overlap): this zero-volume result is the \
                             faces where they meet, so nothing interferes; overlap them by at \
                             least 0.01 only if they should be one solid";

/// How bad a finding is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    /// Will not print (or not as modelled).
    Error,
    /// Likely to print badly.
    Warning,
    Info,
}

impl Level {
    pub fn name(self) -> &'static str {
        match self {
            Level::Error => "error",
            Level::Warning => "warning",
            Level::Info => "info",
        }
    }
}

/// One problem found.
#[derive(Debug, Clone, PartialEq)]
pub struct Finding {
    pub level: Level,
    /// Stable: `not-3d`, `empty`, `not-closed`, `not-manifold`,
    /// `floating`, `cavity`, `thin-wall`, `overhang`, `bed-fit`, `tiny-feature`,
    /// `parts-intersect`, `part-not-manifold`, `off-bed`, and for input
    /// meshes `polyhedron-inside-out`, `polyhedron-flipped-faces`,
    /// `polyhedron-open`, `polyhedron-not-manifold`; `stl-precision`.
    pub code: &'static str,
    pub message: String,
    /// Where: the worst point, and the box of the whole problem.
    pub point: V3,
    pub bbox: Aabb,
    pub part: Option<String>,
    pub fix: String,
    /// The measured value (thickness, area, volume, ...) and the limit it
    /// broke, in mm, mm² or mm³.
    pub value: Option<f64>,
    pub limit: Option<f64>,
}

/// Rounded to 1e-4 (a tenth of a micron is below any printer's
/// resolution), so the JSON stays short.
fn r4(x: f64) -> f64 {
    let y = (x * 1e4).round() / 1e4;
    if y == 0.0 { 0.0 } else { y }
}

fn v4(p: V3) -> [f64; 3] {
    p.map(r4)
}

fn bbox4(b: &Aabb) -> Value {
    if b.is_empty() {
        return Value::Null;
    }
    stats::bbox_json(&v4(b.lo), &v4(b.hi))
        .as_object()
        .map_or(Value::Null, |o| {
            let mut o = o.clone();
            if let Some(Value::Array(s)) = o.get_mut("size") {
                for x in s.iter_mut() {
                    *x = json!(r4(x.as_f64().unwrap_or(0.0)));
                }
            }
            Value::Object(o)
        })
}

impl Finding {
    pub fn json(&self, id: usize) -> Value {
        json!({
            "id": id,
            "severity": self.level.name(),
            "code": self.code,
            "message": self.message,
            "part": self.part,
            "location": {"point": v4(self.point), "bbox": bbox4(&self.bbox)},
            "fix": self.fix,
            "value": self.value.map(r4),
            "limit": self.limit,
        })
    }
}

fn mm(x: f64) -> String {
    render::snapshot::number(x)
}

/// The `stl-precision` finding: what saving the solid as STL does to it.
///
/// A warning when rounding to `f32` leaves edges with other than two
/// faces: slicers then see an open or non-manifold mesh (the CAD pilot's
/// grader rejected a twisted thread this way, while the exact weld said
/// manifold). Faces that merely collapse, with every edge still paired,
/// are info: a slicer drops a zero-area facet and the rest is still a
/// closed manifold, which is also what any mesh with a Clipper-snapped
/// sliver gives, so a warning there would be noise.
fn stl_precision(p: &crate::mesh::StlPrecision, bbox: &Aabb) -> Finding {
    let ulp = crate::mesh::f32_spacing(bbox);
    let n = p.collapsed_faces;
    let tri = if n == 1 { "triangle" } else { "triangles" };
    // In scientific notation: `mm` rounds to a tenth of a micron, and the
    // spacing is a few thousandths of one on a part of centimetres.
    let stored = format!("32-bit floats hold this part's coordinates to about {ulp:.1e} mm");
    let broken = p.nonmanifold_edges > 0;
    let message = if broken {
        let e = p.nonmanifold_edges;
        // Two faces a hair apart merge without any triangle collapsing.
        let what = if n == 0 {
            "vertices a hair apart merge".to_string()
        } else {
            format!("{n} {tri} collapse")
        };
        format!(
            "{what} when saved as STL (32-bit floats, as slicers read it), leaving {e} \
             edge{} shared by other than two faces: the file is not manifold for a slicer, \
             though the solid is ({stored})",
            if e == 1 { "" } else { "s" }
        )
    } else {
        format!(
            "{n} {tri} collapse to zero area when saved as STL (32-bit floats); slicers drop \
             them and the mesh stays closed, so no action is needed ({stored})"
        )
    };
    Finding {
        level: if broken { Level::Warning } else { Level::Info },
        code: "stl-precision",
        message,
        point: p.at,
        bbox: p.bbox,
        part: None,
        fix: stl_precision_fix(ulp),
        value: Some(if broken {
            p.nonmanifold_edges as f64
        } else {
            n as f64
        }),
        limit: None,
    }
}

/// Whether a finding's fix is shown in a short report (the MCP tools'
/// terse results, `neoscad check`'s text). The info-level `stl-precision`
/// finding needs no action, and its fix ("overlap or separate coincident
/// surfaces ...") read as an instruction: an agent in the T2 transcript
/// audit spent turns chasing it. The fix stays in the full JSON.
pub fn fix_shown(finding: &Value) -> bool {
    !(finding["code"] == "stl-precision" && finding["severity"] == "info")
}

/// What to do about triangles that collapse in an STL (the
/// `stl-precision` finding, and `render`'s note), given the `f32` spacing
/// at the part's coordinates.
///
/// Surfaces that lie on each other come first: in the CAD pilot's
/// twisted thread the 738 broken edges came from a core cylinder at
/// exactly the thread's root radius, not from the thread's 360-point
/// section. Moving the core 0.05 inward left none; coarsening the section
/// to 90 points did too, and coarsening only the slices did not.
pub fn stl_precision_fix(ulp: f64) -> String {
    format!(
        "slivers this thin come from surfaces lying on or grazing each other (such as a core \
         cylinder at exactly a thread's root radius) or from very fine tessellation: overlap \
         or separate coincident surfaces by 0.01 or more, or coarsen the tessellation there \
         (lower `$fn`, fewer points per section); keep neighbouring vertices more than about \
         {:.1e} mm apart",
        100.0 * ulp
    )
}

/// Everything a check found, before it is JSON.
#[derive(Debug, Clone, Default)]
pub struct Analysis {
    /// Sorted: input mesh problems (`polyhedron-*`) first, then errors,
    /// then by code order of the checks.
    pub findings: Vec<Finding>,
    /// Per code, findings left out past [`CheckSettings::max_findings`].
    pub truncated: Vec<(&'static str, usize)>,
    /// Findings by level (errors, warnings, info), before truncation.
    pub counts: [usize; 3],
    pub model: Value,
    pub parts: Vec<Value>,
    /// Milliseconds per stage.
    pub timings: Vec<(&'static str, f64)>,
    /// The mesh the checks ran on, with the triangles each marked (for
    /// `snapshot --issues`).
    pub mesh: Mesh,
    pub thin: Vec<u32>,
    pub overhang: Vec<u32>,
    /// Triangles of floating pieces.
    pub floating: Vec<u32>,
}

/// Run every check on a rendered model and its parts. `now` is a clock in
/// milliseconds for the stage timings.
pub fn analyze(
    geometry: Option<&Geometry>,
    parts: &[Part],
    s: &CheckSettings,
    now: &dyn Fn() -> f64,
) -> Analysis {
    analyze_with(geometry, parts, &[], s, now)
}

/// [`analyze`], with the problems of the model's input meshes
/// ([`Rendered::inputs`](crate::Rendered::inputs)) as findings too. When a
/// winding problem is among them, a not-manifold finding points to it: an
/// inside-out mesh is what made the booleans pinch, and the pinch's own
/// advice (overlap the parts) would send the reader the wrong way, as it
/// did an agent in the CAD pilot.
pub fn analyze_with(
    geometry: Option<&Geometry>,
    parts: &[Part],
    inputs: &[crate::orient::InputIssue],
    s: &CheckSettings,
    now: &dyn Fn() -> f64,
) -> Analysis {
    let mut a = analyze_solid(geometry, parts, inputs, s, now);
    link_to_winding(&mut a.findings);
    a
}

/// The fix of a not-manifold finding when a winding problem of an input
/// mesh (finding `id`) is the likely cause.
pub fn pinch_from_winding(id: &str) -> String {
    format!(
        "fix {id} first: an inside-out or partly flipped polyhedron is the likely cause, since \
         booleans with it go wrong; if these edges remain after that, overlap the parts that \
         touch by at least 0.01 or separate them"
    )
}

fn link_to_winding(findings: &mut [Finding]) {
    let Some(i) = findings.iter().position(|f| {
        f.code == DiagCode::PolyhedronInsideOut.as_str()
            || f.code == DiagCode::PolyhedronFlippedFaces.as_str()
    }) else {
        return;
    };
    let fix = pinch_from_winding(&format!("#{}", i + 1));
    for f in findings.iter_mut().filter(|f| f.code == "not-manifold") {
        f.fix = fix.clone();
    }
}

fn analyze_solid(
    geometry: Option<&Geometry>,
    parts: &[Part],
    inputs: &[crate::orient::InputIssue],
    s: &CheckSettings,
    now: &dyn Fn() -> f64,
) -> Analysis {
    let mut a = Analysis::default();
    let mut out: Vec<Finding> = Vec::new();
    let mut t = now();
    let mut lap = |a: &mut Analysis, name: &'static str| {
        let n = now();
        a.timings.push((name, n - t));
        t = n;
    };
    let Some(g) = geometry.filter(|g| !g.is_empty()) else {
        out.push(Finding {
            level: Level::Error,
            code: "empty",
            message: "the model is empty: nothing to print".into(),
            point: [0.0; 3],
            bbox: Aabb::EMPTY,
            part: None,
            fix: "check that the top level creates geometry (a `%` or `*` modifier, or an \
                  intersection or difference that removes everything, leaves nothing)"
                .into(),
            value: None,
            limit: None,
        });
        a.counts[0] = 1;
        a.findings = out;
        a.model = Value::Null;
        return a;
    };
    if let Geometry::Polygon2d(p) = g {
        let (lo, hi) = p.bounds().unwrap_or(([0.0; 2], [0.0; 2]));
        out.push(Finding {
            level: Level::Error,
            code: "not-3d",
            message: "the model is 2D: printability checks need a 3D solid".into(),
            point: [(lo[0] + hi[0]) / 2.0, (lo[1] + hi[1]) / 2.0, 0.0],
            bbox: Aabb {
                lo: [lo[0], lo[1], 0.0],
                hi: [hi[0], hi[1], 0.0],
            },
            part: None,
            fix: "extrude it (`linear_extrude(height) ...`) to check it as a print".into(),
            value: None,
            limit: None,
        });
        a.counts[0] = 1;
        a.findings = out;
        a.model = json!({"dimensions": 2});
        return a;
    }

    // Closed and manifold. A mesh that was never through a boolean (a lone
    // polyhedron or extrusion) is looked at directly, since converting it
    // is exactly what can fail.
    let mut open_edges = 0usize;
    let mut shared_edges = 0usize;
    if let Geometry::PolySet(ps) = g {
        let tri = ps.tessellate(&mut Vec::new());
        let mut edges: HashMap<(u32, u32), i32> = HashMap::new();
        for f in &tri.faces {
            for k in 0..f.len() {
                let (u, v) = (f[k], f[(k + 1) % f.len()]);
                *edges.entry((u.min(v), u.max(v))).or_insert(0) += 1;
            }
        }
        open_edges = edges.values().filter(|&&n| n == 1).count();
        shared_edges = edges.values().filter(|&&n| n > 2).count();
    }
    let solid = stats::solid(g);
    let mesh = Mesh::of_solid(&solid);
    let bbox = mesh.bbox();
    // What a file of the solid would show: Manifold keeps a vertex per
    // piece where two pieces touch, which its own status cannot see.
    // Slicers read an STL's corners as `f32`, which can merge vertices
    // that are distinct here: `stl` is what that adds.
    let weld = if solid.is_valid() {
        mesh.weld()
    } else {
        crate::mesh::Weld::default()
    };
    let (pinched, stl) = (weld.exact, weld.f32);
    let manifold = solid.is_valid() && open_edges == 0 && shared_edges == 0 && pinched.is_none();
    lap(&mut a, "manifold");
    for i in inputs {
        out.push(Finding {
            level: Level::Warning,
            code: i.code.as_str(),
            message: match &i.call {
                Some(c) => format!("{} ({c})", i.message),
                None => i.message.clone(),
            },
            point: i.point,
            bbox: Aabb::point(i.point),
            part: None,
            fix: i.fix.clone(),
            value: None,
            limit: None,
        });
    }
    if let Some(p) = pinched {
        let (volume, area, _) = mesh.mass();
        out.push(Finding {
            level: Level::Error,
            code: "not-manifold",
            message: format!(
                "the solid is not manifold as a file: {} edge{} {} shared by more than two faces once corners at the same position are merged, as an STL reader does",
                p.edges,
                if p.edges == 1 { "" } else { "s" },
                if p.edges == 1 { "is" } else { "are" },
            ),
            point: p.at,
            bbox: p.bbox,
            part: None,
            fix: if stats::touch_only(volume, area) {
                TOUCH_FIX
            } else {
                PINCH_FIX
            }
            .into(),
            value: Some(p.edges as f64),
            limit: None,
        });
    } else if !manifold {
        let (code, message, fix) = if open_edges > 0 {
            (
                "not-closed",
                format!("the surface is not closed: {open_edges} edges belong to only one face"),
                "close the mesh: every edge of a polyhedron must be shared by exactly two faces \
                 (check the face lists for missing or duplicated faces)",
            )
        } else {
            (
                "not-manifold",
                if shared_edges > 0 {
                    format!(
                        "the solid is not manifold: {shared_edges} edges belong to more than two faces"
                    )
                } else {
                    "the solid is not manifold (Manifold could not build a valid solid)".to_string()
                },
                "make objects that should be one overlap a little instead of touching at an edge \
                 or a point, and check polyhedron faces for consistent winding",
            )
        };
        out.push(Finding {
            level: Level::Error,
            code,
            message,
            point: bbox.center(),
            bbox,
            part: None,
            fix: fix.into(),
            value: None,
            limit: None,
        });
    }
    if mesh.tris.is_empty() {
        a.counts[0] = out.len();
        a.findings = out;
        a.model = json!({"dimensions": 3, "manifold": manifold, "components": 0});
        a.mesh = mesh;
        return a;
    }
    let bed_z = bbox.lo[2];

    // Components: floating islands, tiny pieces and sealed cavities.
    let (comp_of, ncomp) = mesh.components();
    let mut comp_box = vec![Aabb::EMPTY; ncomp];
    let mut comp_part: Vec<HashMap<u32, f64>> = vec![HashMap::new(); ncomp];
    let mut comp_volume = vec![0.0; ncomp];
    for (t, &c) in comp_of.iter().enumerate() {
        let c = c as usize;
        comp_box[c] = comp_box[c].union(&mesh.tri_box(t));
        if let Some(p) = mesh.part[t] {
            *comp_part[c].entry(p).or_insert(0.0) += mesh.area(t);
        }
        comp_volume[c] += mesh.signed_volume(t);
    }
    let cavity = cavities(&comp_volume, &comp_box, solid.is_valid());
    let owner = |m: &HashMap<u32, f64>| -> Option<String> {
        m.iter()
            .max_by(|a, b| a.1.total_cmp(b.1).then(b.0.cmp(a.0)))
            .map(|(&p, _)| mesh.part_names[p as usize].to_string())
    };
    let mut floating = 0;
    let mut floating_comps = Vec::new();
    let tiny = 2.0 * s.nozzle;
    let lifted: Vec<u32> = (0..ncomp as u32)
        .filter(|&c| !cavity[c as usize] && comp_box[c as usize].lo[2] - bed_z > s.bed_tolerance)
        .collect();
    let under = gaps_below(&mesh, &comp_of, &comp_box, &lifted, s.bed_tolerance);
    let mut cavities_found = 0;
    for c in 0..ncomp {
        let b = comp_box[c];
        if cavity[c] {
            // The inside surface of a hollow: a shell of its own (it
            // shares no vertex with the outside), so it was counted, and
            // reported, as a floating piece. It prints as part of the
            // solid around it; what matters is that it is sealed.
            cavities_found += 1;
            let size = b.size();
            let volume = -comp_volume[c];
            out.push(Finding {
                level: Level::Info,
                code: "cavity",
                message: format!(
                    "a sealed internal void of {} mm³ ({} x {} x {} mm), closed on every side",
                    mm(volume),
                    mm(size[0]),
                    mm(size[1]),
                    mm(size[2])
                ),
                point: b.center(),
                bbox: b,
                part: owner(&comp_part[c]),
                fix: "nothing is needed for FDM if the hollow is intended (its ceiling prints as \
                      a bridge or an overhang inside); for resin or powder printing add a drain \
                      hole, since the void traps what it is printed from; fill it if the part \
                      should be solid"
                    .into(),
                value: Some(r4(volume)),
                limit: None,
            });
            continue;
        }
        let lift = b.lo[2] - bed_z;
        if lift > s.bed_tolerance {
            floating += 1;
            floating_comps.push(c as u32);
            // What is under the piece: "nothing under it" was said of a
            // lid resting on its box too (the agent-eval pilot).
            let message = match under.get(&(c as u32)).copied().flatten() {
                Some(g) if g <= s.bed_tolerance => format!(
                    "a separate piece starts {} mm above the bed, resting on another piece (touching it, not joined to it)",
                    mm(lift)
                ),
                Some(g) => format!(
                    "a separate piece starts {} mm above the bed, {} mm above the piece under it",
                    mm(lift),
                    mm(g)
                ),
                None => format!(
                    "a piece starts {} mm above the bed with nothing under it",
                    mm(lift)
                ),
            };
            out.push(Finding {
                level: Level::Error,
                code: "floating",
                message,
                // The piece's centre (its bottom face is where the
                // overhang finding for it points).
                point: b.center(),
                bbox: b,
                part: owner(&comp_part[c]),
                fix: "connect it to the rest of the model or lower it onto the bed; otherwise it \
                      needs support, or print it as a separate object"
                    .into(),
                value: Some(lift),
                limit: Some(s.bed_tolerance),
            });
        }
        let size = b.size();
        let extent = size[0].max(size[1]).max(size[2]);
        if extent < tiny {
            out.push(Finding {
                level: Level::Warning,
                code: "tiny-feature",
                message: format!(
                    "a piece {} x {} x {} mm is smaller than two extrusion widths ({} mm)",
                    mm(size[0]),
                    mm(size[1]),
                    mm(size[2]),
                    mm(tiny)
                ),
                point: b.center(),
                bbox: b,
                part: owner(&comp_part[c]),
                fix: "enlarge it, merge it into a bigger piece, or remove it; the slicer will \
                      likely drop or blob it"
                    .into(),
                value: Some(extent),
                limit: Some(tiny),
            });
        }
    }
    if bed_z.abs() > s.bed_tolerance {
        out.push(Finding {
            level: Level::Info,
            code: "off-bed",
            message: format!(
                "the model's lowest point is at z = {} mm, not on the bed (z = 0)",
                mm(bed_z)
            ),
            point: [bbox.center()[0], bbox.center()[1], bed_z],
            bbox,
            part: None,
            fix: "slicers usually drop a model onto the bed; translate it to z = 0 to print it \
                  where it is modelled"
                .into(),
            value: Some(bed_z),
            limit: Some(0.0),
        });
    }
    a.floating = (0..mesh.tris.len() as u32)
        .filter(|&t| floating_comps.contains(&comp_of[t as usize]))
        .collect();
    lap(&mut a, "components");

    // Wall thickness.
    let bvh = Bvh::new(&mesh);
    let diag = crate::mesh::norm(bbox.size()).max(1e-9);
    let (walls, thin_tris, min_wall) = walls(&mesh, &bvh, s, diag);
    out.extend(walls);
    a.thin = thin_tris;
    lap(&mut a, "walls");

    // Overhangs.
    let (overhangs, over_tris, over_area) = overhangs(&mesh, s, bed_z);
    out.extend(overhangs);
    a.overhang = over_tris;
    lap(&mut a, "overhangs");

    // Bed fit.
    if let Some(bed) = s.bed {
        let size = bbox.size();
        let fits = |x: f64, y: f64| x <= bed[0] + 1e-9 && y <= bed[1] + 1e-9;
        let tall = size[2] > bed[2] + 1e-9;
        if tall || !fits(size[0], size[1]) {
            let rotated = !tall && fits(size[1], size[0]);
            out.push(Finding {
                level: if rotated {
                    Level::Warning
                } else {
                    Level::Error
                },
                code: "bed-fit",
                message: format!(
                    "the model ({} x {} x {} mm) does not fit the {} x {} x {} mm bed{}",
                    mm(size[0]),
                    mm(size[1]),
                    mm(size[2]),
                    mm(bed[0]),
                    mm(bed[1]),
                    mm(bed[2]),
                    if rotated { " as placed" } else { "" }
                ),
                point: bbox.center(),
                bbox,
                part: None,
                fix: if rotated {
                    "rotate it 90° about z (`rotate([0, 0, 90])`) and it fits".into()
                } else {
                    "scale it down, split it into parts that fit, or lay it on another side".into()
                },
                value: Some(size[0].max(size[1]).max(size[2])),
                limit: None,
            });
        }
    }

    // Parts: their own solids.
    let mut part_json = Vec::new();
    for p in parts {
        let Some(sol) = &p.solid else {
            part_json.push(json!({"name": p.name, "instances": p.instances,
                "context": p.context, "dimensions": 2}));
            continue;
        };
        let pm = Mesh::of_solid(sol);
        let (vol, area, _) = pm.mass();
        let (_, pc) = pm.components();
        let pb = pm.bbox();
        let valid = sol.is_valid() && pm.bad_edges().is_none();
        if !valid {
            out.push(Finding {
                level: Level::Error,
                code: "part-not-manifold",
                message: format!("part '{}' is not a valid solid on its own", p.name),
                point: pb.center(),
                bbox: pb,
                part: Some(p.name.clone()),
                fix: "make its pieces overlap a little instead of touching at an edge or a \
                      point, and check polyhedron faces"
                    .into(),
                value: None,
                limit: None,
            });
        }
        part_json.push(json!({
            "name": p.name,
            "instances": p.instances,
            "context": p.context,
            "dimensions": 3,
            "manifold": valid,
            "components": pc,
            "volume": r4(vol),
            "area": r4(area),
            "bbox": bbox4(&pb),
        }));
    }
    out.extend(intersections(parts));
    lap(&mut a, "parts");

    if let Some(p) = stl {
        out.push(stl_precision(&p, &bbox));
    }

    let (vol, area, centroid) = mesh.mass();
    a.model = json!({
        "dimensions": 3,
        "manifold": manifold,
        "components": ncomp,
        "floating": floating,
        "cavities": cavities_found,
        "volume": r4(vol),
        "area": r4(area),
        "centroid": v4(centroid),
        "bbox": bbox4(&bbox),
        "triangles": mesh.tris.len(),
        // `sampled`: the thinnest of the readings taken, which the true
        // thinnest can be a little under (see `walls`).
        "min_wall": min_wall.map(|(d, p, part)| json!({"thickness": r4(d), "point": v4(p), "part": part, "sampled": true})),
        "overhang_area": r4(over_area),
    });
    a.parts = part_json;

    // Problems of the input meshes first (the cause comes before the
    // effects it has on the result), then errors, then in check order;
    // truncate per code.
    out.sort_by_key(|f| (!f.code.starts_with("polyhedron-"), f.level));
    for f in &out {
        a.counts[f.level as usize] += 1;
    }
    let mut per: Vec<(&'static str, usize)> = Vec::new();
    let mut kept = Vec::new();
    for f in out {
        let n = match per.iter_mut().find(|(c, _)| *c == f.code) {
            Some(e) => {
                e.1 += 1;
                e.1
            }
            None => {
                per.push((f.code, 1));
                1
            }
        };
        if n <= s.max_findings {
            kept.push(f);
        }
    }
    a.truncated = per
        .into_iter()
        .filter(|(_, n)| *n > s.max_findings)
        .map(|(c, n)| (c, n - s.max_findings))
        .collect();
    a.findings = kept;
    a.mesh = mesh;
    a
}

/// How far below each piece's lowest points the nearest other piece is,
/// straight down (`None`: nothing under them), for the pieces in `which`.
/// Casts from up to 64 of each piece's vertices within `tol` of its
/// bottom, found in one pass over the triangles.
/// Which components are the inside surface of a sealed void: a closed
/// shell wound inward (negative signed volume; Manifold winds every shell
/// it outputs so that its normals point out of the material) whose box is
/// inside another component's, wound outward. The box test keeps an
/// inside-out lone shell (a mis-wound polyhedron that never went through a
/// boolean) from passing as a void. Without a valid solid the windings
/// mean nothing, so nothing is a cavity.
fn cavities(volume: &[f64], boxes: &[Aabb], valid: bool) -> Vec<bool> {
    let inside = |a: &Aabb, b: &Aabb| (0..3).all(|k| a.lo[k] >= b.lo[k] && a.hi[k] <= b.hi[k]);
    (0..volume.len())
        .map(|c| {
            valid
                && volume[c] < 0.0
                && (0..volume.len())
                    .any(|o| o != c && volume[o] > 0.0 && inside(&boxes[c], &boxes[o]))
        })
        .collect()
}

fn gaps_below(
    mesh: &Mesh,
    comp_of: &[u32],
    comp_box: &[Aabb],
    which: &[u32],
    tol: f64,
) -> HashMap<u32, Option<f64>> {
    const PER_PIECE: usize = 64;
    let mut out = HashMap::new();
    if which.is_empty() || comp_box.len() < 2 {
        return out;
    }
    let mut feet: HashMap<u32, Vec<V3>> = which.iter().map(|&c| (c, Vec::new())).collect();
    for (t, tri) in mesh.tris.iter().enumerate() {
        let c = comp_of[t];
        let Some(f) = feet.get_mut(&c) else { continue };
        if f.len() >= PER_PIECE {
            continue;
        }
        let z = comp_box[c as usize].lo[2];
        for &v in tri {
            let p = mesh.verts[v as usize];
            if p[2] <= z + tol && f.len() < PER_PIECE {
                f.push(p);
            }
        }
    }
    let bvh = Bvh::new(mesh);
    let down = [0.0, 0.0, -1.0];
    for (&c, f) in &feet {
        let mut best: Option<f64> = None;
        for p in f {
            let o = [p[0], p[1], p[2] + 1e-9];
            if let Some((h, _)) = bvh.ray(mesh, o, down, -1e-6, f64::INFINITY, |x| {
                comp_of[x as usize] == c
            }) {
                let g = (h - 1e-9).max(0.0);
                best = Some(best.map_or(g, |b: f64| b.min(g)));
            }
        }
        out.insert(c, best);
    }
    out
}

/// Points on triangle `t` to measure from: the centroid, or the centroids
/// of a 4- or 16-way midpoint subdivision on larger faces, so a thin spot
/// in the middle of a big face is not missed.
fn samples(mesh: &Mesh, t: usize, cell: f64) -> Vec<V3> {
    let [a, b, c] = mesh.corners(t);
    let area = mesh.area(t);
    let level = if area <= cell * cell {
        0
    } else if area <= 16.0 * cell * cell {
        1
    } else {
        2
    };
    let mut tris = vec![[a, b, c]];
    for _ in 0..level {
        let mut next = Vec::with_capacity(tris.len() * 4);
        for [p, q, r] in tris {
            let m = |x: V3, y: V3| scale(add(x, y), 0.5);
            let (pq, qr, rp) = (m(p, q), m(q, r), m(r, p));
            next.extend([[p, pq, rp], [pq, q, qr], [rp, qr, r], [pq, qr, rp]]);
        }
        tris = next;
    }
    tris.iter()
        .map(|[p, q, r]| scale(add(add(*p, *q), *r), 1.0 / 3.0))
        .collect()
}

/// A thin reading measured again in the layer plane.
enum Layer {
    /// The face is flat (a floor, a roof, a plate): no in-layer direction.
    Flat,
    /// The width in the layer: the thickness, the face the ray left
    /// through and the ray's direction.
    Across(f64, u32, V3),
    /// The ray in the layer found no far side facing away.
    Nothing,
}

/// A normal's direction in the layer (XY) plane, `None` for a flat face.
fn in_plane(n: V3) -> Option<V3> {
    let l = (n[0] * n[0] + n[1] * n[1]).sqrt();
    (l > 1e-6).then(|| [n[0] / l, n[1] / l, 0.0])
}

/// Measure a thin reading from `p`, on a face with unit `normal`, again
/// in the layer plane.
///
/// FDM prints a wall as perimeters in each layer, so what has to be at
/// least two extrusion widths is the wall's width *in the layer*, as a
/// slicer sees it. That is measured here, along the face's normal
/// projected onto the layer plane: for any face that is not flat this is
/// exactly the in-layer normal of the outline the face cuts from its
/// layer, however the face is tilted, which the face's own normal is not
/// a proxy for. The caller keeps the larger reading:
///
/// - A twisted `linear_extrude` is tessellated into slivers a slice high
///   and a profile edge long. Their normals tilt steeply (76° on a 20 mm
///   square twisted 360° over 12 mm in 100 slices), and near the corners
///   of a fast twist the surface itself is a shallow helical ramp. A ray
///   along such a normal runs down or up into the end cap: "8 walls 0.17
///   mm thick" on a solid 20 mm square, every layer of which is a full
///   square. An agent that believes these readings rewrites a sound
///   thread to escape them and breaks its major diameter.
/// - A dome's top faces are nearly flat and a layer near the top is a
///   small disc; there the reading along the normal, through the dome, is
///   the larger and stands.
/// - A leaning plate is as wide in each layer as its slope makes it, and
///   is judged by that width.
///
/// A flat face has no in-layer direction; its reading along the normal
/// is a floor's or a roof's thickness in layers. The caller keeps it only
/// when the far side is flat too, a plate: through a sloped face it is a
/// wedge where a slope meets a floor or an end cap (the twisted square's
/// first layer at each corner, which the layer above no longer covers),
/// and the sloped face's own samples measure that in the layer.
fn in_layer(
    mesh: &Mesh,
    bvh: &Bvh,
    p: V3,
    normal: V3,
    tmin: f64,
    diag: f64,
    near: &dyn Fn(u32) -> bool,
) -> Layer {
    let Some(d) = in_plane(normal).map(|n| scale(n, -1.0)) else {
        return Layer::Flat;
    };
    // Any face the ray leaves through will do, however oblique: this
    // reading can only replace a thinner one, and a ray that runs far
    // through material in the layer has shown the layer is not thin
    // there. (Held to the 45° of a wall's two sides, a ray from a twisted
    // tube's outside that left through its bore at a slant said nothing,
    // and the sliver's reading stood.)
    match bvh.ray(mesh, p, d, tmin, 2.0 * diag, near) {
        Some((h, hit)) if dot(mesh.normal(hit as usize), d) > 0.0 => Layer::Across(h, hit, d),
        _ => Layer::Nothing,
    }
}

/// A thin spot: where, how thin, and on which triangle.
struct Thin {
    mid: V3,
    thickness: f64,
    tri: u32,
    /// The face the ray left through.
    hit: u32,
}

/// How far inside a corner (towards the centroid) a corner sample is, as
/// a fraction of the nozzle, and at most a tenth of the way. A distance,
/// not only a fraction: a bore's faces after a difference run the part's
/// whole height, and a tenth of the way from the rim is millimetres from
/// it.
const CORNER_INSET: f64 = 0.1;

/// Faces sampled again near their corners, at least (see `walls`).
const MAX_CORNER_FACES: usize = 128;

/// How parallel a wall's two sides must be: the cosine of 45°.
const WALL_COS: f64 = std::f64::consts::FRAC_1_SQRT_2;

/// Rays at most this many, spread evenly over the faces; beyond it faces
/// are strided (a mesh this dense samples finely anyway).
const MAX_RAYS: usize = 400_000;

/// How far under a limit (the minimum wall, the nozzle) a wall must
/// measure before it counts as under it, in mm. A wall modelled at exactly
/// the minimum measures a hair either side of it (a ray's hit is computed
/// in floating point, and `20 - 18.8` is not `1.2`): compared strictly, a
/// 1.2 mm floor read "1.2 mm thick, under the 1.2 mm minimum" and an agent
/// in the T2 transcript audit spent turns thickening walls that met the
/// spec. A micron is far below what any FDM printer resolves (layers and
/// perimeters are tenths of a millimetre), so no printable difference
/// hides in it, while a faceted cylinder's wall (1.1986 mm for 1.2 mm at
/// `$fn = 64`) is still under.
const WALL_TOLERANCE: f64 = 1e-3;

/// Thin places whose faces add up to less than this (mm²) are slivers, an
/// info finding rather than a warning or an error. They are where two
/// surfaces meet at a sharp edge: every crest of a V thread measured "walls
/// at 22 places, the thinnest 0.62 mm thick (0 mm² of surface ...)" in an
/// agent's check of a hose adapter, and the agent thickened a thread that
/// printed fine. A real wall's faces measure square millimetres (the
/// thinnest wall a 0.4 mm nozzle prints, 0.4 mm by one 0.2 mm layer, is
/// 0.08 mm²), so no wall hides under it.
const SLIVER_AREA: f64 = 0.05;

/// Whether a wall `h` thick is under `limit` ([`WALL_TOLERANCE`]).
fn under(h: f64, limit: f64) -> bool {
    h < limit - WALL_TOLERANCE
}

/// A thickness as a thin-wall message gives it: to the hundredth, or to
/// the ten-thousandth when the hundredth would read as the limit itself
/// ("1.2 mm thick, under the 1.2 mm minimum" for 1.1986).
fn thickness(h: f64, limit: f64) -> String {
    let short = mm(h);
    if short != mm(limit) {
        return short;
    }
    let s = format!("{h:.4}");
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

type Walls = (Vec<Finding>, Vec<u32>, Option<(f64, V3, Option<String>)>);

fn walls(mesh: &Mesh, bvh: &Bvh, s: &CheckSettings, diag: f64) -> Walls {
    let n = mesh.tris.len();
    let stride = n.div_ceil(MAX_RAYS / 4).max(1);
    // Sample spacing: fine enough to catch a wall a few widths across.
    let cell = (4.0 * s.min_wall).max(diag / 200.0);
    let tmin = diag * 1e-9;
    // Closer than this, an exit is checked for a contact seam: Manifold
    // keeps touching surfaces within its tolerance (0.0002 mm on the
    // spring_handle example), and no printable wall is this thin.
    let seam_eps = (diag * 1e-4).min(0.01);
    let mut seams: Vec<(V3, u32)> = Vec::new();
    let mut thin: Vec<Thin> = Vec::new();
    let mut thinnest: Option<(f64, V3, u32)> = None;
    // One reading from `p` on face `t`: the thickness, the face the ray
    // left through, the ray's direction, and a contact seam it passed.
    let read = |t: usize,
                p: V3,
                thinnest: Option<f64>,
                corner: bool|
     -> Option<(f64, u32, V3, Option<V3>)> {
        let normal = mesh.normal(t);
        let d = scale(normal, -1.0);
        // Faces sharing a corner with this one are not across a wall
        // from it: at a sharp edge the neighbour is hit at once, and
        // every knife edge and sliver would measure zero.
        let corners = mesh.tris[t];
        let near = |h: u32| mesh.tris[h as usize].iter().any(|v| corners.contains(v));
        let (h, hit) = bvh.ray(mesh, p, d, tmin, 2.0 * diag, near)?;
        // A wall's two sides face away from each other. The far face
        // must look within 45° of the ray's way (it is where the ray
        // leaves the solid, roughly parallel to this face); anything
        // else is a corner or a slope, not a wall, and an entering hit
        // means the mesh is inconsistent here. Either way the sample
        // says nothing.
        if dot(mesh.normal(hit as usize), d) < WALL_COS {
            return None;
        }
        let (mut h, mut hit) = (h, hit);
        let mut seam = None;
        if h < seam_eps {
            // An exit this close is usually not the far side of a
            // wall but a contact seam: two pieces that touch (coils
            // of a spring, a lid on a box) keep both their surfaces,
            // and the ray leaves the neighbour's copy at once. Look
            // past it: if the ray next leaves through another face
            // facing its way, the material goes on and the wall is
            // that far; the seam itself is not a wall (it measured
            // "0 mm" with the fix "thicken it", which an agent would
            // obey). Nothing further, or an entering face, and it is
            // a thin sliver after all.
            let first = hit;
            if let Some((h2, hit2)) = bvh.ray(mesh, p, d, h + tmin.max(1e-9), 2.0 * diag, |x| {
                near(x) || x == first
            }) && dot(mesh.normal(hit2 as usize), d) >= WALL_COS
            {
                seam = Some(add(p, scale(d, h / 2.0)));
                (h, hit) = (h2, hit2);
            }
        }
        // A thin reading is measured again in the layer plane (see
        // `in_layer`), and so is one that would be the thinnest yet,
        // so the model's `min_wall` is always a measured one: a
        // sliver's 0.8 mm under a 20 mm twisted square read as a
        // wall below a 1.2 mm spec.
        if under(h, s.min_wall) || thinnest.is_none_or(|x| h < x) {
            match in_layer(mesh, bvh, p, normal, tmin, diag, &near) {
                Layer::Across(h2, hit2, d2) if h2 > h => return Some((h2, hit2, d2, seam)),
                // Near a corner, a layer's ray that leaves through a face
                // not across from it, or through one it may not hit (a
                // neighbour sharing the corner), has run out through the
                // wall's end (a leaning plate's bottom), and the reading
                // along the normal is the plate's thickness, not its
                // width in the layer: the sample says nothing the
                // middle's did not.
                Layer::Across(_, hit2, d2)
                    if corner && dot(mesh.normal(hit2 as usize), d2) < WALL_COS =>
                {
                    return None;
                }
                Layer::Nothing if corner => return None,
                Layer::Flat if in_plane(mesh.normal(hit as usize)).is_some() => return None,
                _ => {}
            }
        }
        Some((h, hit, d, seam))
    };
    let mut keep = |t: usize,
                    p: V3,
                    (h, hit, d, seam): (f64, u32, V3, Option<V3>),
                    thinnest: &mut Option<(f64, V3, u32)>| {
        if let Some(q) = seam {
            seams.push((q, t as u32));
        }
        let mid = add(p, scale(d, h / 2.0));
        if thinnest.is_none_or(|(x, _, _)| h < x) {
            *thinnest = Some((h, mid, t as u32));
        }
        if under(h, s.min_wall) {
            thin.push(Thin {
                mid,
                thickness: h,
                tri: t as u32,
                hit,
            });
        }
    };
    // Each face's thinnest reading, for the second pass.
    let mut least: Vec<(u32, f64)> = Vec::new();
    for t in (0..n).step_by(stride) {
        if mesh.normal(t) == [0.0; 3] {
            continue;
        }
        let mut lo = f64::INFINITY;
        for p in samples(mesh, t, cell) {
            if let Some(r) = read(t, p, thinnest.map(|x| x.0), false) {
                lo = lo.min(r.0);
                keep(t, p, r, &mut thinnest);
            }
        }
        if lo.is_finite() {
            least.push((t as u32, lo));
        }
    }
    // Second pass, near the corners. A reading from a face's middle is
    // exact where the wall's sides are parallel and too thick where the
    // wall tapers: the barb tip of run cad-20260929T031249Z's adapter,
    // 1.2 mm at its rim, read 1.39 from its faces' centroids, and the
    // agent reported 1.39 as the part's thinnest wall. A wall's sides are within 45° of each other,
    // so across a face the wall thins by at most about the distance from
    // its middle to its corners: the faces that could hold a reading
    // under the thinnest yet are sampled again, a little inside each
    // corner (on the corner itself, the neighbours that share it would be
    // in the way), most promising first, and at most `MAX_CORNER_FACES`
    // of them or one in 128, which keeps the pass a few percent of the
    // check's time. This sharpens `min_wall`; the thin-wall findings are
    // the first pass's, plus any corner reading under the minimum.
    let reach = |t: usize| {
        let [a, b, c] = mesh.corners(t);
        let mid = scale(add(add(a, b), c), 1.0 / 3.0);
        [a, b, c]
            .iter()
            .map(|&v| crate::mesh::dist(v, mid))
            .fold(0.0, f64::max)
    };
    let mut maybe: Vec<(f64, u32)> = least
        .iter()
        .map(|&(t, lo)| (lo - reach(t as usize), t))
        .filter(|&(x, _)| thinnest.is_some_and(|(h, _, _)| x < h))
        .collect();
    maybe.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
    maybe.truncate(MAX_CORNER_FACES.max(n / 128));
    for (bound, t) in maybe {
        if thinnest.is_some_and(|(h, _, _)| bound >= h) {
            break;
        }
        let t = t as usize;
        let [a, b, c] = mesh.corners(t);
        let mid = scale(add(add(a, b), c), 1.0 / 3.0);
        for v in [a, b, c] {
            let to_mid = add(mid, scale(v, -1.0));
            let len = crate::mesh::norm(to_mid);
            let inset = (0.1 * len).min(CORNER_INSET * s.nozzle);
            let p = add(v, scale(to_mid, inset / len.max(1e-300)));
            if let Some(r) = read(t, p, thinnest.map(|x| x.0), true) {
                keep(t, p, r, &mut thinnest);
            }
        }
    }
    let mut tris: Vec<u32> = thin.iter().map(|x| x.tri).collect();
    tris.sort_unstable();
    tris.dedup();
    // One finding per wall: thin faces that share an edge are one place,
    // and so are the two sides of a wall (a ray's start and the face it
    // left through), as long as they are in the same part.
    let mut parent: Vec<u32> = (0..n as u32).collect();
    fn find(p: &mut [u32], mut x: u32) -> u32 {
        while p[x as usize] != x {
            p[x as usize] = p[p[x as usize] as usize];
            x = p[x as usize];
        }
        x
    }
    let unite = |p: &mut Vec<u32>, a: u32, b: u32| {
        if mesh.part[a as usize] != mesh.part[b as usize] {
            return;
        }
        let (ra, rb) = (find(p, a), find(p, b));
        if ra != rb {
            p[ra.max(rb) as usize] = ra.min(rb);
        }
    };
    for x in &thin {
        unite(&mut parent, x.tri, x.hit);
    }
    let mut edges: HashMap<(u32, u32), u32> = HashMap::new();
    for &t in &tris {
        let v = mesh.tris[t as usize];
        for k in 0..3 {
            let (a, b) = (v[k], v[(k + 1) % 3]);
            match edges.entry((a.min(b), a.max(b))) {
                std::collections::hash_map::Entry::Occupied(e) => {
                    let o = *e.get();
                    unite(&mut parent, t, o);
                }
                std::collections::hash_map::Entry::Vacant(e) => {
                    e.insert(t);
                }
            }
        }
    }
    struct Cluster {
        root: u32,
        /// The thinnest sample.
        first: usize,
        b: Aabb,
        part: Option<u32>,
        area: f64,
    }
    let mut clusters: Vec<Cluster> = Vec::new();
    // Thinnest first, so each cluster's `first` is its worst sample.
    thin.sort_by(|a, b| {
        a.thickness
            .total_cmp(&b.thickness)
            .then(a.tri.cmp(&b.tri))
            .then(a.mid[0].total_cmp(&b.mid[0]))
            .then(a.mid[1].total_cmp(&b.mid[1]))
            .then(a.mid[2].total_cmp(&b.mid[2]))
    });
    for (i, x) in thin.iter().enumerate() {
        let root = find(&mut parent, x.tri);
        if !clusters.iter().any(|c| c.root == root) {
            clusters.push(Cluster {
                root,
                first: i,
                b: Aabb::EMPTY,
                part: mesh.part[x.tri as usize],
                area: 0.0,
            });
        }
    }
    for &t in &tris {
        let root = find(&mut parent, t);
        if let Some(c) = clusters.iter_mut().find(|c| c.root == root) {
            c.b = c.b.union(&mesh.tri_box(t as usize));
            c.area += mesh.area(t as usize);
        }
    }
    // Nearby places of one part and one severity are one finding: gear
    // teeth or a perforated plate would otherwise list every tooth and
    // hole. Clusters are in thinnest-first order, so each merged finding
    // keeps its thinnest place.
    let reach = (4.0 * s.min_wall).max(0.05 * diag);
    let level = |c: &Cluster| under(thin[c.first].thickness, s.nozzle);
    let mut merged: Vec<(Cluster, usize)> = Vec::new();
    for c in clusters {
        let at = merged
            .iter()
            .position(|(m, _)| m.part == c.part && level(m) == level(&c) && m.b.gap(&c.b) <= reach);
        match at {
            Some(i) => {
                let (m, n) = &mut merged[i];
                m.b = m.b.union(&c.b);
                m.area += c.area;
                *n += 1;
            }
            None => merged.push((c, 1)),
        }
    }
    let mut findings: Vec<Finding> = merged
        .iter()
        .map(|(c, places)| {
            let x = &thin[c.first];
            let error = under(x.thickness, s.nozzle);
            let limit = if error { s.nozzle } else { s.min_wall };
            let sliver = c.area < SLIVER_AREA;
            Finding {
                level: if sliver {
                    Level::Info
                } else if error {
                    Level::Error
                } else {
                    Level::Warning
                },
                code: "thin-wall",
                message: format!(
                    "{}{} {} mm thick{} ({} mm² of surface measures under the minimum)",
                    if sliver { "a sliver, not a wall: " } else { "" },
                    if *places > 1 {
                        format!("walls at {places} places, the thinnest")
                    } else {
                        "a wall".to_string()
                    },
                    thickness(x.thickness, limit),
                    if error {
                        format!(", thinner than the {} mm nozzle", mm(s.nozzle))
                    } else {
                        format!(", under the {} mm minimum", mm(s.min_wall))
                    },
                    mm(c.area)
                ),
                point: x.mid,
                bbox: c.b,
                part: c.part.map(|p| mesh.part_names[p as usize].to_string()),
                fix: format!(
                    "{}thicken it to at least {} mm ({} perimeters of a {} mm nozzle), or remove it",
                    if sliver {
                        "nothing to do where sharp edges meet (a thread's crest, a chamfer's tip): \
                         the slicer prints it as part of what is beside it; if it is meant to be \
                         a wall, "
                    } else {
                        ""
                    },
                    mm(s.min_wall),
                    mm((s.min_wall / s.nozzle).round()),
                    mm(s.nozzle)
                ),
                value: Some(x.thickness),
                limit: Some(limit),
            }
        })
        .collect();
    if let Some(&(point, _)) = seams.first() {
        let mut b = Aabb::EMPTY;
        let mut faces: Vec<u32> = seams.iter().map(|&(_, t)| t).collect();
        faces.sort_unstable();
        faces.dedup();
        for &t in &faces {
            b = b.union(&mesh.tri_box(t as usize));
        }
        findings.push(Finding {
            level: Level::Info,
            code: "touching-surfaces",
            message: format!(
                "surfaces touch with no gap ({} mm² of faces): pieces that meet here keep both their surfaces; this is a contact, not a thin wall",
                mm(faces.iter().map(|&t| mesh.area(t as usize)).sum())
            ),
            point,
            bbox: b,
            part: None,
            fix: format!(
                "they print fused together: if they should be separate, leave a gap of at least the {} mm nozzle; if they should be one piece, overlap them a little so they union into one solid",
                mm(s.nozzle)
            ),
            value: Some(0.0),
            limit: None,
        });
    }
    let min = thinnest.map(|(d, p, t)| (d, p, mesh.part_name(t as usize).map(|n| n.to_string())));
    (findings, tris, min)
}

/// How far past `max_overhang` (degrees) a face must lean before it is an
/// overhang. A face modelled at exactly the limit comes out of the
/// geometry a hair either side of it, and with a 1e-12 margin such faces
/// were flagged: an agent's snap-fit lid read "6.19 mm² faces down at up
/// to 45° from vertical (limit 45°)" four times over. Half a degree is
/// far below what a printer can tell apart (0.2 mm layers step out 0.2035
/// mm at 45.5° against 0.2 at 45°).
const OVERHANG_TOLERANCE: f64 = 0.5;

/// A bridge: a flat downward region (every face within this many degrees
/// of horizontal) held up by walls on two opposite sides, at most
/// [`BRIDGE_SPAN`] apart. Slicers print such a span as a bridge, in
/// straight strands from wall to wall, without support; the print rules
/// agents work to allow "short bridges", and the top of a USB port cut
/// through a 2 mm wall read as a 90° overhang that needed support.
const BRIDGE_FLAT: f64 = 1.0;
/// The longest span (mm) a bridge is reported as info. Common FDM printers
/// bridge 20 mm cleanly; longer spans sag and stay a warning.
const BRIDGE_SPAN: f64 = 20.0;

/// Thread flanks: a band of downward faces around a vertical axis at most
/// this deep (mm, radially) that winds all the way round and climbs more
/// than it is deep. An ISO metric thread's flanks lean 60° from its axis,
/// past the usual 45° limit, and FDM prints them as they are (each layer
/// steps out 0.35 mm, under a 0.4 mm line): every check of an M24 thread
/// warned about them, and agents raised `max_overhang` until the warning
/// went away, which would hide a real ledge too. The rules keep real
/// overhangs warnings: a flat ring or a chamfer around a boss climbs less
/// than it is deep (at any angle past 45°), a ledge is deeper than this,
/// and faces leaning more than [`THREAD_STEEP`] past the limit over more
/// than a speck (two extrusion widths squared, as for regions) disqualify
/// it: a square thread, or a ledge the flanks run into (a 41 mm² ledge on
/// a 554 mm² band of flanks stays a warning).
const THREAD_DEPTH: f64 = 2.5;
/// Degrees past `max_overhang` a thread flank may lean (75° by default).
const THREAD_STEEP: f64 = 30.0;

/// What an overhanging region is, which decides how loud its finding is.
#[derive(Debug, Clone, Copy, PartialEq)]
enum OverhangKind {
    /// Needs support (or a chamfer, or another orientation): a warning.
    Plain,
    /// A short bridge ([`BRIDGE_SPAN`]) of this span: info.
    Bridge(f64),
    /// A thread's lower flanks ([`THREAD_DEPTH`]), this deep: info.
    Thread(f64),
}

impl OverhangKind {
    fn same(self, o: OverhangKind) -> bool {
        std::mem::discriminant(&self) == std::mem::discriminant(&o)
    }
}

/// Whether the flat region `faces` is a short bridge: the faces across its
/// boundary edges go down from them (walls holding the span up) on both
/// ends of its box along x, or along y. `below` gives, for a boundary edge,
/// whether a face outside the region shares it and runs below it. The span
/// between the two supported ends is returned when it is short.
fn bridge_span(
    mesh: &Mesh,
    faces: &[usize],
    b: &Aabb,
    below: &HashMap<(u32, u32), bool>,
) -> Option<f64> {
    let tol = 0.01;
    let mut best: Option<f64> = None;
    for axis in 0..2 {
        let (lo, hi) = (b.lo[axis], b.hi[axis]);
        let (mut at_lo, mut at_hi) = (false, false);
        for &t in faces {
            let v = mesh.tris[t];
            for k in 0..3 {
                let (a, c) = (v[k], v[(k + 1) % 3]);
                if below.get(&(a.min(c), a.max(c))) != Some(&true) {
                    continue;
                }
                let (pa, pc) = (mesh.verts[a as usize][axis], mesh.verts[c as usize][axis]);
                at_lo |= pa <= lo + tol && pc <= lo + tol;
                at_hi |= pa >= hi - tol && pc >= hi - tol;
            }
        }
        let span = hi - lo;
        if at_lo && at_hi && span <= BRIDGE_SPAN {
            best = Some(best.map_or(span, |x: f64| x.min(span)));
        }
    }
    best
}

/// Whether the region `faces` (angles from vertical, areas, indices) is a
/// band of thread flanks ([`THREAD_DEPTH`]), and how deep it is.
fn thread_depth(
    mesh: &Mesh,
    faces: &[(f64, f64, usize)],
    b: &Aabb,
    s: &CheckSettings,
) -> Option<f64> {
    let c = b.center();
    let (mut rmin, mut rmax) = (f64::INFINITY, 0.0f64);
    let mut sectors = [false; 12];
    let mut steep = 0.0;
    for &(angle, a, t) in faces {
        for p in mesh.corners(t) {
            let r = (p[0] - c[0]).hypot(p[1] - c[1]);
            rmin = rmin.min(r);
            rmax = rmax.max(r);
        }
        let m = mesh.centroid(t);
        let turn = (m[1] - c[1]).atan2(m[0] - c[0]) + std::f64::consts::PI;
        sectors[((turn / std::f64::consts::TAU * 12.0) as usize).min(11)] = true;
        if angle > s.max_overhang + THREAD_STEEP {
            steep += a;
        }
    }
    let depth = rmax - rmin;
    let climbs = b.hi[2] - b.lo[2] > depth;
    (depth <= THREAD_DEPTH
        && rmin > depth
        && climbs
        && sectors.iter().all(|&x| x)
        && steep < (2.0 * s.nozzle).powi(2))
    .then_some(depth)
}

fn overhangs(mesh: &Mesh, s: &CheckSettings, bed_z: f64) -> (Vec<Finding>, Vec<u32>, f64) {
    let limit = (s.max_overhang + OVERHANG_TOLERANCE)
        .min(90.0)
        .to_radians()
        .sin();
    let over: Vec<usize> = (0..mesh.tris.len())
        .filter(|&t| {
            let n = mesh.normal(t);
            if -n[2] <= limit {
                return false;
            }
            // Faces on the bed are held up by it.
            !mesh
                .corners(t)
                .iter()
                .all(|v| v[2] <= bed_z + s.bed_tolerance)
        })
        .collect();
    // Regions: overhanging faces sharing an edge.
    let mut parent: Vec<usize> = (0..over.len()).collect();
    fn find(p: &mut [usize], mut x: usize) -> usize {
        while p[x] != x {
            p[x] = p[p[x]];
            x = p[x];
        }
        x
    }
    let mut edges: HashMap<(u32, u32), usize> = HashMap::new();
    for (i, &t) in over.iter().enumerate() {
        let v = mesh.tris[t];
        for k in 0..3 {
            let (a, b) = (v[k], v[(k + 1) % 3]);
            let key = (a.min(b), a.max(b));
            match edges.get(&key) {
                Some(&j) => {
                    let (ri, rj) = (find(&mut parent, i), find(&mut parent, j));
                    if ri != rj {
                        parent[ri.max(rj)] = ri.min(rj);
                    }
                }
                None => {
                    edges.insert(key, i);
                }
            }
        }
    }
    // Faces steeper than this are reported apart from the rest: an agent
    // given "550 mm² at up to 90°" could not tell a 41 mm² ledge from the
    // 60° thread flanks around it, and swept `max_overhang` to find it.
    let steep = (s.max_overhang + 15.0).min(89.0);
    // With the same tolerance as the limit: 60° flanks read "263 mm² of
    // it steeper than 60°" when their faces leaned a hair past it.
    let steep_sin = (steep + OVERHANG_TOLERANCE).min(90.0).to_radians().sin();
    struct Region {
        area: f64,
        b: Aabb,
        /// Each face's angle from vertical (degrees), area and index.
        faces: Vec<(f64, f64, usize)>,
        steep_area: f64,
        steep_b: Aabb,
        parts: HashMap<u32, f64>,
    }
    let mut regions: Vec<Region> = Vec::new();
    let mut slot: HashMap<usize, usize> = HashMap::new();
    let mut total = 0.0;
    for (i, &t) in over.iter().enumerate() {
        let r = find(&mut parent, i);
        let k = *slot.entry(r).or_insert_with(|| {
            regions.push(Region {
                area: 0.0,
                b: Aabb::EMPTY,
                faces: Vec::new(),
                steep_area: 0.0,
                steep_b: Aabb::EMPTY,
                parts: HashMap::new(),
            });
            regions.len() - 1
        });
        let reg = &mut regions[k];
        let a = mesh.area(t);
        total += a;
        reg.area += a;
        reg.b = reg.b.union(&mesh.tri_box(t));
        let down = -mesh.normal(t)[2];
        reg.faces
            .push((down.clamp(-1.0, 1.0).asin().to_degrees(), a, t));
        if down > steep_sin + 1e-9 {
            reg.steep_area += a;
            reg.steep_b = reg.steep_b.union(&mesh.tri_box(t));
        }
        if let Some(p) = mesh.part[t] {
            *reg.parts.entry(p).or_insert(0.0) += a;
        }
    }
    // Specks below two extrusion widths squared are noise (a chamfer's
    // sliver, a tessellation artefact), not something to support.
    let min_area = (2.0 * s.nozzle).powi(2);
    regions.retain(|r| r.area >= min_area);
    regions.sort_by(|a, b| b.area.total_cmp(&a.area));
    // Which regions are short bridges or thread flanks. A bridge is flat;
    // for the flat regions, find which of their boundary edges a wall runs
    // down from (the face across the edge has a corner below it).
    let flat = |r: &Region| r.faces.iter().all(|f| f.0 >= 90.0 - BRIDGE_FLAT);
    let mut below: HashMap<(u32, u32), bool> = HashMap::new();
    for r in regions.iter().filter(|r| flat(r)) {
        let mut seen: HashMap<(u32, u32), u32> = HashMap::new();
        for &(_, _, t) in &r.faces {
            let v = mesh.tris[t];
            for k in 0..3 {
                let (a, b) = (v[k], v[(k + 1) % 3]);
                *seen.entry((a.min(b), a.max(b))).or_insert(0) += 1;
            }
        }
        below.extend(seen.into_iter().filter(|e| e.1 == 1).map(|e| (e.0, false)));
    }
    if !below.is_empty() {
        for v in &mesh.tris {
            for k in 0..3 {
                let (a, b) = (v[k], v[(k + 1) % 3]);
                if let Some(e) = below.get_mut(&(a.min(b), a.max(b))) {
                    let z = mesh.verts[a as usize][2].min(mesh.verts[b as usize][2]);
                    *e |= mesh.verts[v[(k + 2) % 3] as usize][2] < z - 1e-6;
                }
            }
        }
    }
    let kinds: Vec<OverhangKind> = regions
        .iter()
        .map(|r| {
            let faces: Vec<usize> = r.faces.iter().map(|f| f.2).collect();
            if flat(r)
                && let Some(span) = bridge_span(mesh, &faces, &r.b, &below)
            {
                OverhangKind::Bridge(span)
            } else if let Some(depth) = thread_depth(mesh, &r.faces, &r.b, s) {
                OverhangKind::Thread(depth)
            } else {
                OverhangKind::Plain
            }
        })
        .collect();
    let owner = |r: &Region| -> Option<u32> {
        r.parts
            .iter()
            .max_by(|a, b| a.1.total_cmp(b.1).then(b.0.cmp(a.0)))
            .map(|(&p, _)| p)
    };
    // Nearby regions of one part are one finding, as thin walls are: the
    // undersides of a gear's teeth or a thread's flanks listed one by one
    // were most of a check's text. Largest first.
    let diag = crate::mesh::norm(mesh.bbox().size());
    let reach = (4.0 * s.min_wall).max(0.05 * diag);
    // Only regions of one kind merge: a bridge beside a ledge must not
    // quieten the ledge, nor the ledge make the bridge a warning.
    struct Merged {
        regions: Vec<usize>,
        b: Aabb,
        area: f64,
        steep_area: f64,
        steep_b: Aabb,
        kind: OverhangKind,
    }
    let mut merged: Vec<Merged> = Vec::new();
    for (i, r) in regions.iter().enumerate() {
        let kind = kinds[i];
        match merged.iter_mut().find(|m| {
            owner(&regions[m.regions[0]]) == owner(r) && m.kind.same(kind) && m.b.gap(&r.b) <= reach
        }) {
            Some(m) => {
                m.regions.push(i);
                m.b = m.b.union(&r.b);
                m.area += r.area;
                m.steep_area += r.steep_area;
                m.steep_b = m.steep_b.union(&r.steep_b);
                m.kind = match (m.kind, kind) {
                    (OverhangKind::Bridge(a), OverhangKind::Bridge(b)) => {
                        OverhangKind::Bridge(a.max(b))
                    }
                    (OverhangKind::Thread(a), OverhangKind::Thread(b)) => {
                        OverhangKind::Thread(a.max(b))
                    }
                    (k, _) => k,
                };
            }
            None => merged.push(Merged {
                regions: vec![i],
                b: r.b,
                area: r.area,
                steep_area: r.steep_area,
                steep_b: r.steep_b,
                kind,
            }),
        }
    }
    let findings = merged
        .iter()
        .map(|m| {
            let part =
                owner(&regions[m.regions[0]]).map(|p| mesh.part_names[p as usize].to_string());
            let (worst, at) = steepest(
                m.regions
                    .iter()
                    .flat_map(|&i| regions[i].faces.iter().copied()),
                min_area,
            );
            // The steep part is named apart only when it is more than a
            // speck and not the whole finding.
            let steep_note = if m.steep_area >= min_area && m.steep_area < m.area - 1e-9 {
                format!(
                    "; {} mm² of it steeper than {}° ({})",
                    mm(m.steep_area),
                    mm(steep),
                    z_range(&m.steep_b)
                )
            } else {
                String::new()
            };
            let (level, what) = match m.kind {
                OverhangKind::Plain => (Level::Warning, String::new()),
                OverhangKind::Bridge(span) => (
                    Level::Info,
                    format!("a {} mm bridge between walls: ", mm(span)),
                ),
                OverhangKind::Thread(depth) => (
                    Level::Info,
                    format!(
                        "thread flanks (a band {} mm deep winding round a vertical axis): ",
                        mm(depth)
                    ),
                ),
            };
            let fix = match m.kind {
                OverhangKind::Plain => format!(
                    "add support, chamfer it to {}° or less, or reorient the model; a short \
                     flat span between two walls may bridge instead",
                    mm(s.max_overhang)
                ),
                OverhangKind::Bridge(_) => format!(
                    "none needed: slicers bridge a flat span up to {} mm between walls; \
                     shorten it if the printer sags",
                    mm(BRIDGE_SPAN)
                ),
                OverhangKind::Thread(_) => format!(
                    "none needed for a thread (60° flanks print as they are); if it is not \
                     one, add support or chamfer it to {}° or less",
                    mm(s.max_overhang)
                ),
            };
            Finding {
                level,
                code: "overhang",
                message: format!(
                    "{what}{} mm² faces down{} at up to {}° from vertical (limit {}°), {}{}",
                    mm(m.area),
                    if m.regions.len() > 1 {
                        format!(" in {} places", m.regions.len())
                    } else {
                        String::new()
                    },
                    mm(worst.round()),
                    mm(s.max_overhang),
                    z_range(&m.b),
                    steep_note,
                ),
                // On the steepest faces, which the "up to" angle is about:
                // the largest region's middle put a 90° ledge's finding on
                // a 60° flank 5 mm below it, and the agent swept
                // `max_overhang` to find the ledge. A face's centroid is on
                // the surface, unlike a curved region's.
                point: mesh.centroid(at),
                bbox: m.b,
                part,
                fix,
                value: Some(m.area),
                limit: Some(s.max_overhang),
            }
        })
        .collect();
    let tris = over.iter().map(|&t| t as u32).collect();
    (findings, tris, total)
}

/// An overhang's "up to" angle and the face to point at: the steepest
/// faces that together cover `min_area` (the speck size below which a
/// region is not reported at all), the angle the last of them reaches,
/// and the largest of them. The single steepest face is often a sliver
/// where a thread meets its chamfer (0.06 mm² at 88° on the CAD pilot's
/// adapter, whose flanks are 60°), which is no place to send a reader.
fn steepest(faces: impl Iterator<Item = (f64, f64, usize)>, min_area: f64) -> (f64, usize) {
    let mut faces: Vec<(f64, f64, usize)> = faces.collect();
    faces.sort_by(|a, b| {
        b.0.total_cmp(&a.0)
            .then(b.1.total_cmp(&a.1))
            .then(a.2.cmp(&b.2))
    });
    let (mut area, mut angle, mut at) = (0.0, 0.0, (0.0, 0));
    for &(g, a, t) in &faces {
        if a > at.0 {
            at = (a, t);
        }
        angle = g;
        area += a;
        if area >= min_area {
            break;
        }
    }
    (angle, at.1)
}

/// "z 6.2 to 11.9", or "z 11.9" for a flat box. The heights go in an
/// overhang's text because an agent reads the text, not the bbox that
/// only `verbose` returns.
fn z_range(b: &Aabb) -> String {
    let (lo, hi) = (mm(b.lo[2]), mm(b.hi[2]));
    if lo == hi {
        format!("z {lo}")
    } else {
        format!("z {lo} to {hi}")
    }
}

/// Pairs of parts whose solids overlap (neither nested in the other, both
/// reaching the model as themselves).
fn intersections(parts: &[Part]) -> Vec<Finding> {
    let mut out = Vec::new();
    fn solid(p: &Part) -> Option<&ManifoldGeometry> {
        p.solid.as_ref().filter(|_| p.context.is_none())
    }
    for (i, a) in parts.iter().enumerate() {
        for b in &parts[i + 1..] {
            if a.contains(&b.name) || b.contains(&a.name) {
                continue;
            }
            let (Some(sa), Some(sb)) = (solid(a), solid(b)) else {
                continue;
            };
            let (Some(ba), Some(bb)) = (sa.bounds(), sb.bounds()) else {
                continue;
            };
            let (ba, bb) = (Aabb { lo: ba.0, hi: ba.1 }, Aabb { lo: bb.0, hi: bb.1 });
            if !ba.overlaps(&bb, 0.0) {
                continue;
            }
            let both: ManifoldGeometry = sa.boolean(sb, OpType::Intersect);
            let vol = both.manifold.volume();
            let floor = 1e-6_f64.max(1e-9 * sa.manifold.volume().min(sb.manifold.volume()));
            if vol <= floor {
                continue;
            }
            let bx = both
                .bounds()
                .map_or(Aabb::EMPTY, |(lo, hi)| Aabb { lo, hi });
            out.push(Finding {
                level: Level::Warning,
                code: "parts-intersect",
                message: format!(
                    "parts '{}' and '{}' overlap by {} mm³",
                    a.name,
                    b.name,
                    mm(vol)
                ),
                point: bx.center(),
                bbox: bx,
                part: Some(a.name.clone()),
                fix: "if they are separate pieces, move them apart or subtract one from the \
                     other (with clearance, e.g. 0.2 mm, for a fit); if they are one piece, \
                     put them in one part"
                    .into(),
                value: Some(vol),
                limit: None,
            });
        }
    }
    out
}

/// A `check` request.
#[derive(Debug, Clone)]
pub struct CheckRequest {
    pub run: Run,
    pub settings: CheckSettings,
}

/// A check's result.
#[derive(Debug)]
pub struct Checked {
    /// 0 when nothing is an error; 1 for errors, or a model that failed to
    /// load, evaluate or render (then `summary.failed` is true).
    pub exit_code: u8,
    /// The JSON summary (`docs/cli-json.md`, "check").
    pub summary: Value,
    pub analysis: Analysis,
    pub log: Log,
}

fn round(ms: f64) -> f64 {
    (ms * 10.0).round() / 10.0
}

impl Session {
    /// Check a model's printability.
    pub fn check(&self, req: &CheckRequest) -> Result<Checked, Cancelled> {
        let started = self.now();
        let scheme = render::ColorScheme::cornfield();
        let (model, parts) = self.render_parts(&req.run, &scheme)?;
        if model.exit_code != 0 {
            return Ok(Checked {
                exit_code: model.exit_code,
                summary: json!({
                    "schema": 1,
                    "input": req.run.input,
                    "failed": true,
                    "exit_code": model.exit_code,
                    "diagnostics": crate::diag::summary_json(&model.log.lines, &model.log.names),
                }),
                analysis: Analysis::default(),
                log: model.log,
            });
        }
        let t = self.now();
        let clock = || self.now();
        let analysis = analyze_with(
            model.geometry.as_ref(),
            &parts,
            &model.inputs,
            &req.settings,
            &clock,
        );
        let check_ms = self.now() - t;
        let count = |l: Level| analysis.counts[l as usize];
        let errors = count(Level::Error);
        let exit_code = if errors > 0 { 1 } else { 0 };
        let mut timings = serde_json::Map::new();
        timings.insert(
            "evaluate".into(),
            json!(round(model.timings.parse + model.timings.evaluate)),
        );
        timings.insert("geometry".into(), json!(round(model.timings.geometry)));
        let mut stages = serde_json::Map::new();
        for (k, v) in &analysis.timings {
            stages.insert((*k).into(), json!(round(*v)));
        }
        stages.insert("total".into(), json!(round(check_ms)));
        timings.insert("check".into(), Value::Object(stages));
        timings.insert("total".into(), json!(round(self.now() - started)));
        let summary = json!({
            "schema": 1,
            "input": req.run.input,
            "ok": errors == 0,
            "exit_code": exit_code,
            "settings": req.settings.json(),
            "model": analysis.model,
            "parts": analysis.parts,
            "counts": {
                "errors": errors,
                "warnings": count(Level::Warning),
                "info": count(Level::Info),
            },
            "findings": analysis.findings.iter().enumerate().map(|(i, f)| f.json(i + 1)).collect::<Vec<_>>(),
            "truncated": analysis.truncated.iter().map(|(c, n)| (c.to_string(), json!(n))).collect::<serde_json::Map<_, _>>(),
            "timings_ms": Value::Object(timings),
            "diagnostics": crate::diag::summary_json(&model.log.lines, &model.log.names),
        });
        Ok(Checked {
            exit_code,
            summary,
            analysis,
            log: model.log,
        })
    }
}

/// A check's counts as words: "1 error, 2 warnings, 0 info". The app's
/// check panel (`client::check_summary`) says the same, so the two read
/// alike; "info" is a mass noun and stays as it is.
pub fn counts(errors: u64, warnings: u64, info: u64) -> String {
    let plural = |n: u64, one: &str| {
        if n == 1 {
            format!("1 {one}")
        } else {
            format!("{n} {one}s")
        }
    };
    format!(
        "{}, {}, {info} info",
        plural(errors, "error"),
        plural(warnings, "warning")
    )
}

/// The human-readable report of a check: one line per finding and its
/// fix, after a summary line.
pub fn text(summary: &Value) -> String {
    let mut out = String::new();
    let input = summary["input"].as_str().unwrap_or("");
    if summary["failed"] == json!(true) {
        out.push_str(&format!("check {input}: the model did not render\n"));
        return out;
    }
    let c = &summary["counts"];
    let m = &summary["model"];
    let n = |k: &str| c[k].as_u64().unwrap_or(0);
    out.push_str(&format!(
        "check {input}: {}",
        counts(n("errors"), n("warnings"), n("info"))
    ));
    if m["dimensions"] == json!(3) {
        out.push_str(&format!(
            " ({}, {} component{}",
            if m["manifold"] == json!(true) {
                "manifold"
            } else {
                "not manifold"
            },
            m["components"],
            if m["components"] == json!(1) { "" } else { "s" }
        ));
        // A hollow box is two components (its inside is a shell of its
        // own); saying which of them are voids keeps "2 components" from
        // reading as two pieces.
        match m["cavities"].as_u64() {
            Some(1) => out.push_str(", 1 of them a sealed cavity"),
            Some(n) if n > 1 => out.push_str(&format!(", {n} of them sealed cavities")),
            _ => {}
        }
        if let Some(w) = m["min_wall"]["thickness"].as_f64() {
            out.push_str(&format!(", thinnest wall about {} mm (sampled)", mm(w)));
        }
        out.push(')');
    }
    out.push('\n');
    for f in summary["findings"].as_array().into_iter().flatten() {
        let p = &f["location"]["point"];
        let at: Vec<String> = p
            .as_array()
            .into_iter()
            .flatten()
            .map(|x| mm(x.as_f64().unwrap_or(0.0)))
            .collect();
        out.push_str(&format!(
            "{:>2}. {} {}: {} at [{}]",
            f["id"],
            f["severity"].as_str().unwrap_or(""),
            f["code"].as_str().unwrap_or(""),
            f["message"].as_str().unwrap_or(""),
            at.join(", ")
        ));
        if let Some(part) = f["part"].as_str() {
            out.push_str(&format!(" in part '{part}'"));
        }
        out.push('\n');
        if fix_shown(f) {
            out.push_str(&format!("    fix: {}\n", f["fix"].as_str().unwrap_or("")));
        }
    }
    for (code, n) in summary["truncated"].as_object().into_iter().flatten() {
        out.push_str(&format!("    ... and {n} more {code}\n"));
    }
    out
}
