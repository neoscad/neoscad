//! `measure`: the numbers of a model and its parts (`neoscad measure`, the
//! server's `measure` method; JSON in `docs/cli-json.md`).
//!
//! - volume, surface area, bounding box and centre of mass (the centroid
//!   of the enclosed volume, assuming uniform density) of the model and of
//!   each part's own solid;
//! - the smallest distance between two parts, and whether they touch or
//!   overlap (overlap by a boolean intersection, distance by an exact
//!   triangle-to-triangle search over bounding volume hierarchies);
//! - a cross-section at an axis plane: area, perimeter, contours and
//!   bounding box, optionally as an SVG outline, and per contour its area,
//!   box, whether it is a hole, and its nearest and farthest distance from
//!   an axis (a thread's minor and major radius, a barb's root and crest);
//! - a radius profile along an axis: those radii every `step` over a range,
//!   and the crests along one side (a thread's pitch).
//!
//! Where two parts overlap, the overlap's separate pieces are listed.

use geom::Geometry;
use geom::manifold_geom::{ManifoldGeometry, OpType};
use geom::polygon2d::Polygon2d;
use serde_json::{Value, json};

use crate::mesh::{Aabb, Bvh, Mesh, V3};
use crate::parts::{Part, is_within};
use crate::{Cancelled, Log, Run, Session, stats};

/// An axis-aligned cutting plane.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Plane {
    X(f64),
    Y(f64),
    Z(f64),
}

impl Plane {
    /// `z=10`, `x=-2.5`, `y=0`.
    pub fn parse(s: &str) -> Option<Plane> {
        let (axis, v) = s.split_once('=')?;
        let v: f64 = v.trim().parse().ok()?;
        if !v.is_finite() {
            return None;
        }
        match axis.trim() {
            "x" | "X" => Some(Plane::X(v)),
            "y" | "Y" => Some(Plane::Y(v)),
            "z" | "Z" => Some(Plane::Z(v)),
            _ => None,
        }
    }

    pub fn name(&self) -> String {
        let (a, v) = match self {
            Plane::X(v) => ("x", v),
            Plane::Y(v) => ("y", v),
            Plane::Z(v) => ("z", v),
        };
        format!("{a}={}", render::snapshot::number(*v))
    }

    /// A rotation (and shift) taking the plane to z = 0 with the section's
    /// 2D axes as x and y: (x, y) for z, (y, z) for x, (x, z) for y. Each
    /// is a proper rotation, so the solid stays inside out as it was.
    fn to_z0(self) -> eval::node::Matrix {
        match self {
            Plane::Z(h) => [
                [1.0, 0.0, 0.0, 0.0],
                [0.0, 1.0, 0.0, 0.0],
                [0.0, 0.0, 1.0, -h],
                [0.0, 0.0, 0.0, 1.0],
            ],
            Plane::X(h) => [
                [0.0, 1.0, 0.0, 0.0],
                [0.0, 0.0, 1.0, 0.0],
                [1.0, 0.0, 0.0, -h],
                [0.0, 0.0, 0.0, 1.0],
            ],
            Plane::Y(h) => [
                [1.0, 0.0, 0.0, 0.0],
                [0.0, 0.0, 1.0, 0.0],
                [0.0, -1.0, 0.0, h],
                [0.0, 0.0, 0.0, 1.0],
            ],
        }
    }

    /// A section point back in model coordinates.
    pub fn to_model(self, p: [f64; 2]) -> [f64; 3] {
        match self {
            Plane::Z(h) => [p[0], p[1], h],
            Plane::X(h) => [h, p[0], p[1]],
            Plane::Y(h) => [p[0], h, p[1]],
        }
    }

    /// The names of the section's 2D axes.
    pub fn axes(self) -> (&'static str, &'static str) {
        match self {
            Plane::Z(_) => ("x", "y"),
            Plane::X(_) => ("y", "z"),
            Plane::Y(_) => ("x", "z"),
        }
    }
}

/// The line radii are measured from: parallel to an axis, through
/// `center` (its position in the other two coordinates, in the order of
/// [`Plane::axes`]: x, y for z; y, z for x; x, z for y).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Axis {
    /// 0, 1 or 2: x, y or z.
    pub along: usize,
    pub center: [f64; 2],
}

impl Default for Axis {
    /// The z axis: where a part turned on a lathe, a thread or a barb is
    /// usually modelled.
    fn default() -> Axis {
        Axis {
            along: 2,
            center: [0.0; 2],
        }
    }
}

impl Axis {
    /// `x`, `y` or `z`, and the centre (default the origin).
    pub fn parse(letter: &str, center: Option<[f64; 2]>) -> Option<Axis> {
        let along = match letter.trim() {
            "x" | "X" => 0,
            "y" | "Y" => 1,
            "z" | "Z" => 2,
            _ => return None,
        };
        let center = center.unwrap_or([0.0; 2]);
        center
            .iter()
            .all(|c| c.is_finite())
            .then_some(Axis { along, center })
    }

    pub fn name(&self) -> &'static str {
        ["x", "y", "z"][self.along]
    }

    /// The plane across the axis at `h`.
    pub fn plane(&self, h: f64) -> Plane {
        match self.along {
            0 => Plane::X(h),
            1 => Plane::Y(h),
            _ => Plane::Z(h),
        }
    }

    /// A model point relative to the axis, in the other two coordinates.
    fn flat(&self, p: V3) -> [f64; 2] {
        let (i, j) = match self.along {
            0 => (1, 2),
            1 => (0, 2),
            _ => (0, 1),
        };
        [p[i] - self.center[0], p[j] - self.center[1]]
    }

    /// The nearest and farthest distance of a closed outline (model
    /// points) from the axis: the farthest is at a corner, the nearest on
    /// an edge.
    fn radii(&self, pts: &[V3]) -> (f64, f64) {
        let (mut lo, mut hi) = (f64::INFINITY, 0.0f64);
        for k in 0..pts.len() {
            let (a, b) = (self.flat(pts[k]), self.flat(pts[(k + 1) % pts.len()]));
            hi = hi.max(a[0].hypot(a[1]));
            lo = lo.min(segment_to_origin(a, b));
        }
        (lo, hi)
    }
}

/// The distance from the origin to the segment `ab`.
fn segment_to_origin(a: [f64; 2], b: [f64; 2]) -> f64 {
    let d = [b[0] - a[0], b[1] - a[1]];
    let l = d[0] * d[0] + d[1] * d[1];
    let t = if l > 0.0 {
        (-(a[0] * d[0] + a[1] * d[1]) / l).clamp(0.0, 1.0)
    } else {
        0.0
    };
    (a[0] + t * d[0]).hypot(a[1] + t * d[1])
}

/// A radius profile: from, to and step along the axis.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Profile {
    pub from: f64,
    pub to: f64,
    pub step: f64,
}

/// At most this many samples in a profile: enough for a 50 mm part every
/// 0.1 mm, and a bound on the slices one request can ask for.
pub const MAX_PROFILE: usize = 1000;

impl Profile {
    /// `[from, to, step]`, checked.
    pub fn new(from: f64, to: f64, step: f64) -> Result<Profile, String> {
        if !(from.is_finite() && to.is_finite() && step.is_finite()) || step <= 0.0 || to < from {
            return Err(format!(
                "profile must be [from, to, step] with from <= to and step > 0 (got [{from}, {to}, {step}])"
            ));
        }
        let p = Profile { from, to, step };
        let n = p.samples();
        if n > MAX_PROFILE {
            return Err(format!(
                "profile would take {n} samples; at most {MAX_PROFILE}: use a larger step or a shorter range"
            ));
        }
        Ok(p)
    }

    pub fn samples(&self) -> usize {
        ((self.to - self.from) / self.step + 1e-9).floor() as usize + 1
    }
}

/// A `measure` request.
#[derive(Debug, Clone)]
pub struct MeasureRequest {
    pub run: Run,
    /// Report only this part (and the parts nested in it); a section is
    /// taken through its solid rather than the model's.
    pub part: Option<String>,
    /// Two parts to measure the distance between.
    pub between: Option<(String, String)>,
    pub section: Option<Plane>,
    /// Also draw the section as SVG ([`Measured::svg`]).
    pub svg: bool,
    /// The axis of a section's and a profile's radii.
    pub axis: Axis,
    pub profile: Option<Profile>,
    /// A constrained sketch's solved values, by its `name`
    /// (`--enable sketch`; `docs/language-extensions.md`, section 4.8).
    pub sketch: Option<String>,
}

impl MeasureRequest {
    pub fn new(run: Run) -> MeasureRequest {
        MeasureRequest {
            run,
            part: None,
            between: None,
            section: None,
            svg: false,
            axis: Axis::default(),
            profile: None,
            sketch: None,
        }
    }
}

/// A measurement.
#[derive(Debug)]
pub struct Measured {
    /// 0; 1 when the model failed or a named part does not exist.
    pub exit_code: u8,
    pub summary: Value,
    /// The section's outline as an SVG document, when asked for.
    pub svg: Option<String>,
    pub log: Log,
}

fn r6(x: f64) -> f64 {
    let y = (x * 1e6).round() / 1e6;
    if y == 0.0 { 0.0 } else { y }
}

fn p6(p: [f64; 3]) -> [f64; 3] {
    p.map(r6)
}

fn bbox(lo: [f64; 3], hi: [f64; 3]) -> Value {
    stats::bbox_json(&p6(lo), &p6(hi))
        .as_object()
        .map_or(Value::Null, |o| {
            let mut o = o.clone();
            if let Some(Value::Array(s)) = o.get_mut("size") {
                for x in s.iter_mut() {
                    *x = json!(r6(x.as_f64().unwrap_or(0.0)));
                }
            }
            Value::Object(o)
        })
}

/// Volume, area, box and centre of mass of a solid.
pub fn solid_json(m: &ManifoldGeometry) -> Value {
    let mesh = Mesh::of_solid(m);
    let (vol, area, c) = mesh.mass();
    let b = mesh.bbox();
    json!({
        "volume": r6(vol),
        "area": r6(area),
        "bbox": if b.is_empty() { Value::Null } else { bbox(b.lo, b.hi) },
        "centroid": p6(c),
        "triangles": mesh.tris.len(),
    })
}

/// At most this many contours are described one by one (largest first);
/// `contours` counts them all.
const MAX_OUTLINES: usize = 20;

/// A cross-section's numbers, and its outlines in the section's 2D axes,
/// with radii about the z axis.
pub fn section(m: &ManifoldGeometry, plane: Plane) -> (Value, Polygon2d) {
    section_about(m, plane, Axis::default())
}

/// A cross-section's numbers, and its outlines in the section's 2D axes;
/// each contour's radii are about `axis`.
pub fn section_about(m: &ManifoldGeometry, plane: Plane, axis: Axis) -> (Value, Polygon2d) {
    let mut moved = m.clone();
    moved.transform(&plane.to_z0());
    let poly = moved.slice();
    let mut area = 0.0;
    let mut perimeter = 0.0;
    let mut lo = [f64::INFINITY; 3];
    let mut hi = [f64::NEG_INFINITY; 3];
    for o in &poly.outlines {
        let v = &o.vertices;
        for i in 0..v.len() {
            let (a, b) = (v[i], v[(i + 1) % v.len()]);
            area += (a[0] * b[1] - b[0] * a[1]) / 2.0;
            perimeter += ((b[0] - a[0]).powi(2) + (b[1] - a[1]).powi(2)).sqrt();
            let p = plane.to_model(a);
            for k in 0..3 {
                lo[k] = lo[k].min(p[k]);
                hi[k] = hi[k].max(p[k]);
            }
        }
    }
    let (u, v) = plane.axes();
    let empty = poly.outlines.is_empty();
    // Each contour on its own. The outlines are sanitised (outer ones
    // counter-clockwise, holes clockwise), so the sign of the area says
    // which a contour is.
    let mut each: Vec<(f64, Value)> = poly
        .outlines
        .iter()
        .map(|o| {
            let pts: Vec<V3> = o.vertices.iter().map(|&p| plane.to_model(p)).collect();
            let signed: f64 = (0..o.vertices.len())
                .map(|i| {
                    let (a, b) = (o.vertices[i], o.vertices[(i + 1) % o.vertices.len()]);
                    a[0] * b[1] - b[0] * a[1]
                })
                .sum::<f64>()
                / 2.0;
            let mut b = Aabb::EMPTY;
            for &p in &pts {
                b.grow(p);
            }
            let (rmin, rmax) = axis.radii(&pts);
            (
                signed.abs(),
                json!({
                    "area": r6(signed.abs()),
                    "hole": signed < 0.0,
                    "bbox": bbox(b.lo, b.hi),
                    "radius": [r6(rmin), r6(rmax)],
                }),
            )
        })
        .collect();
    each.sort_by(|a, b| b.0.total_cmp(&a.0));
    (
        json!({
            "plane": plane.name(),
            "axes": [u, v],
            "area": r6(area.abs()),
            "perimeter": r6(perimeter),
            "contours": poly.outlines.len(),
            "bbox": if empty { Value::Null } else { bbox(lo, hi) },
            "axis": axis.name(),
            "center": axis.center.map(r6),
            "outlines": each.into_iter().take(MAX_OUTLINES).map(|(_, v)| v).collect::<Vec<_>>(),
        }),
        poly,
    )
}

/// The radius profile of a solid along an axis: at each sample the
/// nearest and farthest distance of the outer contours from the axis (a
/// thread's minor and major radius, a barb's root or crest), and the
/// crests met along one side (the half-plane from the axis towards the
/// first of the section's axes: +x for the z axis), whose spacing is a
/// thread's pitch. A helical thread's section is the same at every height
/// but turned, so its radii stay constant along the thread while the side
/// sees one crest per pitch.
pub fn profile(m: &ManifoldGeometry, axis: Axis, p: Profile) -> Value {
    // One transform, then a slice per sample: the axis becomes z.
    let mut moved = m.clone();
    moved.transform(&axis.plane(0.0).to_z0());
    let c = axis.center;
    let mut bands: Vec<Value> = Vec::with_capacity(p.samples());
    let mut side: Vec<(f64, f64)> = Vec::new();
    let (mut all_lo, mut all_hi) = (f64::INFINITY, f64::NEG_INFINITY);
    for k in 0..p.samples() {
        let h = p.from + k as f64 * p.step;
        let (lo, hi, far) = slice_radii(&moved, c, h);
        if lo.is_finite() {
            all_lo = all_lo.min(lo);
            all_hi = all_hi.max(hi);
            bands.push(json!([r6(h), r6(lo), r6(hi)]));
        } else {
            bands.push(json!([r6(h), Value::Null, Value::Null]));
        }
        if let Some(f) = far {
            side.push((h, f));
        }
    }
    let found = crests(&side, p.step);
    let far = |h: f64| slice_radii(&moved, c, h).2;
    let refined: Vec<Crest> = if found.len() <= MAX_REFINED {
        found.iter().map(|x| refine(&far, &side, x)).collect()
    } else {
        found.iter().map(|x| x.sampled(&side)).collect()
    };
    let at: Vec<f64> = refined.iter().map(|x| x.at).collect();
    let pitch = pitch(&refined, p.step);
    json!({
        "axis": axis.name(),
        "center": c.map(r6),
        "from": r6(p.from),
        "to": r6(p.to),
        "step": r6(p.step),
        "radius": if all_lo.is_finite() { json!([r6(all_lo), r6(all_hi)]) } else { Value::Null },
        "crests": at.iter().take(100).map(|&z| r6(z)).collect::<Vec<_>>(),
        "pitch": pitch.map(|(x, _, _)| r6(x)),
        "pitch_span": pitch.map(|(_, a, b)| [r6(a), r6(b)]),
        "bands": bands,
    })
}

/// At height `h` of a solid whose axis is z (at `c`): the nearest and
/// farthest distance of the outer contours from the axis (infinite when
/// there are none), and the outermost crossing of the boundary with the
/// +u half-line (the surface's radius on that side).
fn slice_radii(moved: &ManifoldGeometry, c: [f64; 2], h: f64) -> (f64, f64, Option<f64>) {
    let loops: Vec<Vec<[f64; 2]>> = if moved.is_empty() {
        Vec::new()
    } else {
        moved
            .manifold
            .slice(h)
            .to_polygons()
            .iter()
            .map(|l| l.iter().map(|v| [v.x - c[0], v.y - c[1]]).collect())
            .collect()
    };
    let (mut lo, mut hi) = (f64::INFINITY, f64::NEG_INFINITY);
    let mut far: Option<f64> = None;
    for l in &loops {
        let n = l.len();
        if n < 3 {
            continue;
        }
        let signed: f64 = (0..n)
            .map(|i| l[i][0] * l[(i + 1) % n][1] - l[(i + 1) % n][0] * l[i][1])
            .sum();
        for i in 0..n {
            let (a, b) = (l[i], l[(i + 1) % n]);
            // Where the boundary crosses the +u half-line: the
            // outermost crossing is the surface's radius on that side.
            if (a[1] > 0.0) != (b[1] > 0.0) {
                let x = a[0] + (b[0] - a[0]) * (0.0 - a[1]) / (b[1] - a[1]);
                if x >= 0.0 {
                    far = Some(far.map_or(x, |f: f64| f.max(x)));
                }
            }
            if signed > 0.0 {
                hi = hi.max(a[0].hypot(a[1]));
                lo = lo.min(segment_to_origin(a, b));
            }
        }
    }
    (lo, hi, far)
}

/// Crests refined between the samples, at most this many (each takes
/// `GOLDEN_STEPS + 4 * BISECT_STEPS`, 72, slices); past it they stay at
/// their samples.
const MAX_REFINED: usize = 100;

/// Steps of the search for a crest's top: its bracket, two samples wide,
/// shrinks to 0.618^16, about a two-thousandth.
const GOLDEN_STEPS: usize = 16;

/// Halvings of a flank's bracket (up to half a pitch): to 1/16384 of it,
/// a ten-thousandth of a millimetre on a 3 mm flank.
const BISECT_STEPS: usize = 14;

/// A crest as the samples found it: its level run `i..=j` of `side`, and
/// the lowest samples between it and its neighbours on each side.
#[derive(Debug, Clone, Copy)]
struct Found {
    i: usize,
    j: usize,
    valley: [usize; 2],
}

impl Found {
    fn sampled(&self, side: &[(f64, f64)]) -> Crest {
        Crest {
            at: (side[self.i].0 + side[self.j].0) / 2.0,
            radius: side[self.i].1,
            rise: side[self.i].1 - side[self.valley[0]].1.max(side[self.valley[1]].1),
            width: side[self.j].0 - side[self.i].0,
            cut: false,
        }
    }
}

/// A crest: its height along the axis, radius, how far it rises above
/// the higher of its valleys, the width of its top, and whether a flank
/// ran into the end of the range.
#[derive(Debug, Clone, Copy)]
struct Crest {
    at: f64,
    radius: f64,
    rise: f64,
    width: f64,
    cut: bool,
}

/// Refine a crest found at the samples to its top's middle, between the
/// samples: the barb's middle crest, flat from 34.81 to 35.18, was "35.2"
/// at a 0.4 step, and a thread's crests moved by a step with the phase of
/// the range.
///
/// The top's height is the maximum of the side's radius near the crest
/// (a golden-section search between the crest's neighbouring samples).
/// Each flank is then crossed at two levels, a tenth and a fifth of the
/// crest's height above its higher valley below the top, and extended in
/// a line to the top's height: that is where the flank meets the top,
/// whatever the facets do on the top itself (a helical thread's crest,
/// cut by the half-line between its facets, ripples by a hundredth of a
/// millimetre, and so the top's own edges would wander), and exact for
/// straight flanks, which a barb's steep and shallow ones are.
fn refine(far: &dyn Fn(f64) -> Option<f64>, side: &[(f64, f64)], x: &Found) -> Crest {
    let r = |h: f64| far(h).unwrap_or(f64::NEG_INFINITY);
    let sampled = x.sampled(side);
    let (a, b) = (
        side[x.i.saturating_sub(1)].0,
        side[(x.j + 1).min(side.len() - 1)].0,
    );
    // Golden-section search for the top.
    let g = (5f64.sqrt() - 1.0) / 2.0;
    let (mut lo, mut hi) = (a, b);
    let (mut p, mut q) = (hi - g * (hi - lo), lo + g * (hi - lo));
    let (mut rp, mut rq) = (r(p), r(q));
    for _ in 0..GOLDEN_STEPS {
        if rp >= rq {
            hi = q;
            (q, rq) = (p, rp);
            p = hi - g * (hi - lo);
            rp = r(p);
        } else {
            lo = p;
            (p, rp) = (q, rq);
            q = lo + g * (hi - lo);
            rq = r(q);
        }
    }
    let (top_h, top) = if rp.max(rq) > sampled.radius {
        if rp >= rq { (p, rp) } else { (q, rq) }
    } else {
        (sampled.at, sampled.radius)
    };
    let low = side[x.valley[0]].1.max(side[x.valley[1]].1);
    let rise = top - low;
    if rise.is_nan() || rise <= 0.0 {
        return sampled;
    }
    // Where the flank from `top_h` towards `end` crosses `level`, or
    // `None` when it stays above it all the way (the range ends first).
    let cross = |end: f64, level: f64| -> Option<f64> {
        if r(end) >= level {
            return None;
        }
        let (mut inside, mut out) = (top_h, end);
        for _ in 0..BISECT_STEPS {
            let m = (inside + out) / 2.0;
            if r(m) >= level {
                inside = m;
            } else {
                out = m;
            }
        }
        Some((inside + out) / 2.0)
    };
    let (l1, l2) = (top - 0.1 * rise, top - 0.2 * rise);
    let edge = |end: f64| -> Option<f64> {
        let (x1, x2) = (cross(end, l1)?, cross(end, l2)?);
        // The line through the two crossings, at the top's height.
        Some(x1 + (x1 - x2))
    };
    let (left, right) = (edge(side[x.valley[0]].0), edge(side[x.valley[1]].0));
    match (left, right) {
        // At a sharp crest the two edges meet at its tip, a little
        // crossed by the halvings' precision.
        (Some(l), Some(rr)) if l <= rr + 0.01 * (b - a) => Crest {
            at: (l + rr) / 2.0,
            radius: top,
            rise,
            width: (rr - l).max(0.0),
            cut: false,
        },
        _ => Crest {
            cut: true,
            ..sampled
        },
    }
}

/// Whether an end crest of a run is not like the rest: cut short by the
/// end of the range, lower than the others (a chamfer's), or narrower or
/// wider on top (a thread's last crest, half as wide where the flange
/// starts). `run` has at least three crests.
fn odd_end(run: &[Crest], x: &Crest, step: f64) -> bool {
    let median = |mut v: Vec<f64>| {
        v.sort_by(f64::total_cmp);
        v[v.len() / 2]
    };
    let radius = median(run.iter().map(|x| x.radius).collect());
    let rise = median(run.iter().map(|x| x.rise).collect());
    let width = median(run.iter().map(|x| x.width).collect());
    // A tenth of the crests' height: well over the hundredth of a
    // millimetre a helical crest's radius ripples by between facets.
    x.cut
        || x.radius < radius - 0.1 * rise
        || (x.width - width).abs() > (0.25 * width).max(0.1 * step)
}

/// A thread's pitch from its crests: the mean spacing of the longest run
/// of evenly spaced crests (each gap within a tenth of the run's first, or
/// a step), with the first and last crest of the run. A median over every
/// gap mixes a thread with the barbs above it (4.8 for the pilot's M24x2
/// adapter, whose thread crests are 2 apart). An end crest of the run
/// that is not like the others is left out: the M24x2 adapter of run
/// cad-20260929T031249Z, whose last thread crest is half as wide where
/// the flange starts and 0.06 closer to the one before, gave 1.98.
fn pitch(crests: &[Crest], step: f64) -> Option<(f64, f64, f64)> {
    let gaps: Vec<f64> = crests.windows(2).map(|w| w[1].at - w[0].at).collect();
    let mut best: Option<(usize, usize)> = None;
    let mut i = 0;
    while i < gaps.len() {
        let tol = (0.1 * gaps[i]).max(step);
        let mut j = i;
        while j + 1 < gaps.len() && (gaps[j + 1] - gaps[i]).abs() <= tol {
            j += 1;
        }
        if best.is_none_or(|(a, b)| j - i > b - a) {
            best = Some((i, j));
        }
        i = j + 1;
    }
    let (i, j) = best?;
    // The run's crests are `i..=j + 1`.
    let run = &crests[i..=j + 1];
    let (mut a, mut b) = (0, run.len() - 1);
    if run.len() >= 3 {
        if odd_end(run, &run[0], step) {
            a += 1;
        }
        if odd_end(run, &run[b], step) {
            b -= 1;
        }
    }
    if b <= a {
        return None;
    }
    let (first, last) = (run[a].at, run[b].at);
    Some(((last - first) / (b - a) as f64, first, last))
}

/// The local maxima of a radius sampled along an axis. A crest must rise
/// above the lowest sample between it and the neighbouring maximum on
/// each side by more than a hundredth of the radius's range (and a
/// micron), so facet noise on a smooth wall is not a crest. A gap in the
/// samples (the side is empty there) ends a run: a maximum next to one is
/// not a crest.
fn crests(side: &[(f64, f64)], step: f64) -> Vec<Found> {
    let (lo, hi) = side
        .iter()
        .fold((f64::INFINITY, f64::NEG_INFINITY), |(l, h), &(_, r)| {
            (l.min(r), h.max(r))
        });
    let tol = (0.01 * (hi - lo)).max(1e-3);
    let joined = |i: usize| side[i + 1].0 - side[i].0 <= 1.5 * step;
    // Local maxima: runs of level samples with lower neighbours.
    let mut peaks: Vec<(usize, usize)> = Vec::new();
    let mut i = 0;
    while i < side.len() {
        let mut j = i;
        while j + 1 < side.len() && joined(j) && (side[j + 1].1 - side[i].1).abs() <= 1e-6 {
            j += 1;
        }
        let r = side[i].1;
        let lower_before = i > 0 && joined(i - 1) && side[i - 1].1 < r;
        let lower_after = j + 1 < side.len() && joined(j) && side[j + 1].1 < r;
        if lower_before && lower_after {
            peaks.push((i, j));
        }
        i = j + 1;
    }
    // Prominence against the valleys between neighbouring maxima (or the
    // ends of the run of joined samples).
    let valley = |from: usize, to: usize| -> usize {
        (from..=to)
            .min_by(|&a, &b| side[a].1.total_cmp(&side[b].1))
            .unwrap_or(from)
    };
    let mut out = Vec::new();
    for (k, &(i, j)) in peaks.iter().enumerate() {
        let mut start = i;
        while start > 0 && joined(start - 1) {
            start -= 1;
            if k > 0 && start == peaks[k - 1].1 {
                break;
            }
        }
        let mut end = j;
        while end + 1 < side.len() && joined(end) {
            end += 1;
            if k + 1 < peaks.len() && end == peaks[k + 1].0 {
                break;
            }
        }
        let r = side[i].1;
        let (vl, vr) = (valley(start, i), valley(j, end));
        if r - side[vl].1 > tol && r - side[vr].1 > tol {
            out.push(Found {
                i,
                j,
                valley: [vl, vr],
            });
        }
    }
    out
}

/// The section as an SVG document in mm, the second axis pointing up.
pub fn section_svg(poly: &Polygon2d, plane: Plane) -> String {
    let (mut lo, mut hi) = ([f64::INFINITY; 2], [f64::NEG_INFINITY; 2]);
    for o in &poly.outlines {
        for p in &o.vertices {
            for k in 0..2 {
                lo[k] = lo[k].min(p[k]);
                hi[k] = hi[k].max(p[k]);
            }
        }
    }
    if poly.outlines.is_empty() {
        lo = [0.0; 2];
        hi = [1.0; 2];
    }
    let pad = ((hi[0] - lo[0]).max(hi[1] - lo[1]) * 0.05).max(0.5);
    let (w, h) = (hi[0] - lo[0] + 2.0 * pad, hi[1] - lo[1] + 2.0 * pad);
    let n = |x: f64| {
        let s = format!("{:.4}", x);
        let s = s.trim_end_matches('0').trim_end_matches('.').to_string();
        if s == "-0" { "0".into() } else { s }
    };
    let mut d = String::new();
    for o in &poly.outlines {
        for (i, p) in o.vertices.iter().enumerate() {
            d.push_str(if i == 0 { "M" } else { "L" });
            d.push_str(&format!(
                "{},{} ",
                n(p[0] - lo[0] + pad),
                n(hi[1] - p[1] + pad)
            ));
        }
        d.push_str("Z ");
    }
    let (u, v) = plane.axes();
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{w}mm\" height=\"{h}mm\" viewBox=\"0 0 {w} {h}\">\n\
         <title>section {plane} ({u} right, {v} up; origin at {u}={x0}, {v}={y0})</title>\n\
         <path d=\"{d}\" fill=\"#d8d4c8\" fill-rule=\"evenodd\" stroke=\"#202020\" stroke-width=\"{sw}\"/>\n\
         </svg>\n",
        w = n(w),
        h = n(h),
        plane = plane.name(),
        x0 = n(lo[0] - pad),
        y0 = n(hi[1] + pad),
        d = d.trim_end(),
        sw = n((w.max(h) / 400.0).max(0.01)),
    )
}

/// At most this many overlap pieces are listed (largest first).
const MAX_PIECES: usize = 10;

/// The separate pieces of an overlap, largest first, each with its volume
/// and box, and how many there are above `floor`. One box around
/// everything (a pin through a plate that also grazes a boss) says
/// neither where nor how much each overlap is.
fn pieces(both: &ManifoldGeometry, floor: f64) -> (Vec<Value>, usize) {
    let mesh = Mesh::of_solid(both);
    let (comp, n) = mesh.components();
    let mut vol = vec![0.0; n];
    let mut boxes = vec![Aabb::EMPTY; n];
    for (t, &c) in comp.iter().enumerate() {
        let [a, b, d] = mesh.corners(t);
        vol[c as usize] += crate::mesh::dot(a, crate::mesh::cross(b, d)) / 6.0;
        boxes[c as usize] = boxes[c as usize].union(&mesh.tri_box(t));
    }
    let mut order: Vec<usize> = (0..n).filter(|&c| vol[c] > floor).collect();
    order.sort_by(|&a, &b| vol[b].total_cmp(&vol[a]).then(a.cmp(&b)));
    let count = order.len();
    let list = order
        .into_iter()
        .take(MAX_PIECES)
        .map(|c| json!({"volume": r6(vol[c]), "bbox": bbox(boxes[c].lo, boxes[c].hi)}))
        .collect();
    (list, count)
}

/// Distance, touch and overlap of two solids. Touching is within a
/// micrometre (below that, the kernels' own rounding decides).
pub fn between(a: &ManifoldGeometry, b: &ManifoldGeometry) -> Value {
    const TOUCH: f64 = 1e-3;
    let both = a.boolean(b, OpType::Intersect);
    let overlap = both.manifold.volume();
    let floor = 1e-9_f64.max(1e-9 * a.manifold.volume().min(b.manifold.volume()));
    let (ma, mb) = (Mesh::of_solid(a), Mesh::of_solid(b));
    let closest = Bvh::new(&ma).closest(&ma, &Bvh::new(&mb), &mb);
    if overlap > floor {
        let bx = both.bounds();
        let (pieces, count) = pieces(&both, floor);
        return json!({
            "distance": 0.0,
            "touching": true,
            "overlapping": true,
            "overlap_volume": r6(overlap),
            "overlap_bbox": bx.map(|(lo, hi)| bbox(lo, hi)),
            "overlap_pieces": count,
            "pieces": pieces,
            "points": Value::Null,
        });
    }
    match closest {
        Some((d, p, q)) => json!({
            "distance": r6(d),
            "touching": d <= TOUCH,
            "overlapping": false,
            "overlap_volume": 0.0,
            "overlap_bbox": Value::Null,
            "points": [p6(p), p6(q)],
        }),
        None => Value::Null,
    }
}

impl Session {
    /// Measure a model (see [`MeasureRequest`]).
    pub fn measure(&self, req: &MeasureRequest) -> Result<Measured, Cancelled> {
        let started = self.now();
        let scheme = render::ColorScheme::cornfield();
        let (model, parts) = self.render_parts(&req.run, &scheme)?;
        let diagnostics = crate::diag::summary_json(&model.log.lines, &model.log.names);
        let fail = |log: Log, code: u8, error: Option<String>| Measured {
            exit_code: code,
            summary: json!({
                "schema": 1,
                "input": req.run.input,
                "failed": true,
                "exit_code": code,
                "error": error,
                "diagnostics": diagnostics,
            }),
            svg: None,
            log,
        };
        if model.exit_code != 0 {
            return Ok(fail(model.log, model.exit_code, None));
        }
        let names: Vec<&str> = parts.iter().map(|p| p.name.as_str()).collect();
        let find = |name: &str| -> Result<&Part, String> {
            parts.iter().find(|p| p.name == name).ok_or_else(|| {
                if names.is_empty() {
                    format!("no part '{name}': the model has no parts (they need `--enable part`)")
                } else {
                    format!("no part '{name}' (parts: {})", names.join(", "))
                }
            })
        };
        let t = self.now();
        let mut out = serde_json::Map::new();
        out.insert("schema".into(), json!(1));
        out.insert("input".into(), json!(req.run.input));
        out.insert("exit_code".into(), json!(0));
        let model_json = match &model.geometry {
            None => Value::Null,
            Some(g @ Geometry::Polygon2d(_)) => stats::geometry(g, &scheme.geometry_scheme()),
            Some(g) => {
                let solid = stats::solid(g);
                let mut v = solid_json(&solid);
                let mesh = Mesh::of_solid(&solid);
                v["dimensions"] = json!(3);
                v["components"] = json!(mesh.components().1);
                let pinched = if solid.is_valid() {
                    mesh.bad_edges()
                } else {
                    None
                };
                v["manifold"] = json!(solid.is_valid() && pinched.is_none());
                if let Some(p) = pinched {
                    let (vol, area, _) = mesh.mass();
                    v["pinched"] = stats::pinched_json(&p, vol, area);
                }
                v
            }
        };
        out.insert("model".into(), model_json);
        // Parts: all of them, or the one asked for and its nested ones.
        let chosen = match &req.part {
            Some(p) => match find(p) {
                Ok(_) => Some(p.as_str()),
                Err(e) => return Ok(fail(model.log, 1, Some(e))),
            },
            None => None,
        };
        if let Some(name) = &req.sketch {
            let found = match crate::sketches::find(&model.log.sketches.0, name) {
                Ok(f) => f,
                Err(e) => return Ok(fail(model.log, 1, Some(e))),
            };
            out.insert("sketch".into(), sketch_json(&found));
        }
        let part_json: Vec<Value> = parts
            .iter()
            .filter(|p| chosen.is_none_or(|c| is_within(&p.name, c)))
            .map(|p| {
                let mut v = p
                    .solid
                    .as_ref()
                    .map_or(json!({"dimensions": 2}), solid_json);
                v["name"] = json!(p.name);
                v["instances"] = json!(p.instances);
                v["context"] = json!(p.context);
                v
            })
            .collect();
        out.insert("parts".into(), json!(part_json));
        if let Some((a, b)) = &req.between {
            let (pa, pb) = match (find(a), find(b)) {
                (Ok(x), Ok(y)) => (x, y),
                (Err(e), _) | (_, Err(e)) => return Ok(fail(model.log, 1, Some(e))),
            };
            let v = match (&pa.solid, &pb.solid) {
                (Some(sa), Some(sb)) => {
                    let mut v = between(sa, sb);
                    v["a"] = json!(a);
                    v["b"] = json!(b);
                    v
                }
                _ => json!({"a": a, "b": b, "distance": Value::Null}),
            };
            out.insert("between".into(), v);
        }
        let mut svg = None;
        if let Some(plane) = req.section {
            let solid = match chosen {
                Some(c) => find(c).ok().and_then(|p| p.solid.clone()),
                None => match &model.geometry {
                    Some(Geometry::Polygon2d(_)) | None => None,
                    Some(g) => Some(stats::solid(g)),
                },
            };
            match solid {
                Some(s) => {
                    let (mut v, poly) = section_about(&s, plane, req.axis);
                    if let Some(c) = chosen {
                        v["part"] = json!(c);
                    }
                    if req.svg {
                        svg = Some(section_svg(&poly, plane));
                    }
                    out.insert("section".into(), v);
                }
                None => {
                    out.insert("section".into(), Value::Null);
                }
            }
        }
        if let Some(p) = req.profile {
            let solid = match chosen {
                Some(c) => find(c).ok().and_then(|p| p.solid.clone()),
                None => match &model.geometry {
                    Some(Geometry::Polygon2d(_)) | None => None,
                    Some(g) => Some(stats::solid(g)),
                },
            };
            let mut v = solid.map_or(Value::Null, |s| profile(&s, req.axis, p));
            if let (Some(c), Some(o)) = (chosen, v.as_object_mut()) {
                o.insert("part".into(), json!(c));
            }
            out.insert("profile".into(), v);
        }
        let round = |ms: f64| (ms * 10.0).round() / 10.0;
        out.insert(
            "timings_ms".into(),
            json!({
                "evaluate": round(model.timings.parse + model.timings.evaluate),
                "geometry": round(model.timings.geometry),
                "measure": round(self.now() - t),
                "total": round(self.now() - started),
            }),
        );
        out.insert("diagnostics".into(), diagnostics);
        Ok(Measured {
            exit_code: 0,
            summary: Value::Object(out),
            svg,
            log: model.log,
        })
    }
}

/// What `measure --sketch` reports: the first sketch of that name, with
/// its entities' solved values, and how many sketches have the name (a
/// module called twice makes two, each solved on its own).
fn sketch_json(found: &[&Value]) -> Value {
    let mut v = found[0].clone();
    if let Some(o) = v.as_object_mut() {
        o.remove("pin");
        o.remove("profile");
        o.remove("profile_omitted");
        o.insert("instances".into(), json!(found.len()));
    }
    v
}

/// The text of `measure --sketch`: its state, then a line per entity.
pub fn sketch_text(s: &Value) -> String {
    let mut out = crate::sketches::line_text(s);
    if let Some(n) = s["instances"].as_u64().filter(|n| *n > 1) {
        out.push_str(&format!(" (the first of {n} with this name)"));
    }
    out.push('\n');
    for e in s["entities"].as_array().into_iter().flatten() {
        out.push_str("  ");
        out.push_str(&crate::sketches::entity_text(e));
        out.push('\n');
    }
    out
}

/// The human-readable report of a measurement.
pub fn text(summary: &Value) -> String {
    let n = |v: &Value| render::snapshot::number(v.as_f64().unwrap_or(0.0));
    let vec = |v: &Value| -> String {
        v.as_array()
            .into_iter()
            .flatten()
            .map(n)
            .collect::<Vec<_>>()
            .join(", ")
    };
    let mut out = String::new();
    let input = summary["input"].as_str().unwrap_or("");
    if summary["failed"] == json!(true) {
        match summary["error"].as_str() {
            Some(e) => out.push_str(&format!("measure {input}: {e}\n")),
            None => out.push_str(&format!("measure {input}: the model did not render\n")),
        }
        return out;
    }
    let solid = |out: &mut String, label: &str, v: &Value| {
        if v["dimensions"] == json!(2) && v.get("volume").is_none() {
            out.push_str(&format!("{label}: 2D"));
            if let Some(a) = v.get("area") {
                out.push_str(&format!(", area {} mm²", n(a)));
            }
            out.push('\n');
            return;
        }
        out.push_str(&format!(
            "{label}: volume {} mm³, area {} mm², size [{}] mm, centre of mass [{}]\n",
            n(&v["volume"]),
            n(&v["area"]),
            vec(&v["bbox"]["size"]),
            vec(&v["centroid"]),
        ));
    };
    if let Some(s) = summary.get("sketch") {
        out.push_str(&sketch_text(s));
    }
    if summary["model"].is_null() {
        out.push_str(&format!("{input}: empty\n"));
    } else {
        solid(&mut out, input, &summary["model"]);
    }
    for p in summary["parts"].as_array().into_iter().flatten() {
        let label = format!("  part '{}'", p["name"].as_str().unwrap_or(""));
        solid(&mut out, &label, p);
    }
    if let Some(b) = summary.get("between").filter(|b| !b.is_null()) {
        let (a, c) = (b["a"].as_str().unwrap_or(""), b["b"].as_str().unwrap_or(""));
        if b["overlapping"] == json!(true) {
            out.push_str(&format!(
                "'{a}' and '{c}' overlap by {} mm³\n",
                n(&b["overlap_volume"])
            ));
            let count = b["overlap_pieces"].as_u64().unwrap_or(0);
            if count > 1 {
                out.push_str(&format!("  in {count} pieces:\n"));
                for p in b["pieces"].as_array().into_iter().flatten() {
                    out.push_str(&format!(
                        "    {} mm³ at [{}]..[{}]\n",
                        n(&p["volume"]),
                        vec(&p["bbox"]["min"]),
                        vec(&p["bbox"]["max"])
                    ));
                }
            }
        } else {
            out.push_str(&format!(
                "'{a}' to '{c}': {} mm{}\n",
                n(&b["distance"]),
                if b["touching"] == json!(true) {
                    " (touching)"
                } else {
                    ""
                }
            ));
        }
    }
    if let Some(s) = summary.get("section") {
        if s.is_null() {
            out.push_str("section: nothing to cut\n");
        } else {
            out.push_str(&format!(
                "section {}: area {} mm², perimeter {} mm, {} contour{}",
                s["plane"].as_str().unwrap_or(""),
                n(&s["area"]),
                n(&s["perimeter"]),
                s["contours"],
                if s["contours"] == json!(1) { "" } else { "s" }
            ));
            if !s["bbox"].is_null() {
                out.push_str(&format!(", size [{}] mm", vec(&s["bbox"]["size"])));
            }
            out.push('\n');
            for o in s["outlines"].as_array().into_iter().flatten() {
                out.push_str(&format!(
                    "  {} {} mm², [{}]..[{}], radius {}..{} mm about the {} axis\n",
                    if o["hole"] == json!(true) {
                        "hole"
                    } else {
                        "outline"
                    },
                    n(&o["area"]),
                    vec(&o["bbox"]["min"]),
                    vec(&o["bbox"]["max"]),
                    n(&o["radius"][0]),
                    n(&o["radius"][1]),
                    s["axis"].as_str().unwrap_or("z"),
                ));
            }
        }
    }
    if let Some(p) = summary.get("profile") {
        if p.is_null() {
            out.push_str("profile: nothing to measure\n");
        } else {
            let ax = p["axis"].as_str().unwrap_or("z");
            out.push_str(&format!(
                "profile along {ax} from {} to {} every {} mm: radius {}..{} mm",
                n(&p["from"]),
                n(&p["to"]),
                n(&p["step"]),
                n(&p["radius"][0]),
                n(&p["radius"][1]),
            ));
            if !p["pitch"].is_null() {
                out.push_str(&format!(
                    ", pitch {} mm (crests {}..{})",
                    n(&p["pitch"]),
                    n(&p["pitch_span"][0]),
                    n(&p["pitch_span"][1])
                ));
            }
            out.push('\n');
            let crests = vec(&p["crests"]);
            if !crests.is_empty() {
                out.push_str(&format!("  crests at {ax} = {crests}\n"));
            }
            for b in p["bands"].as_array().into_iter().flatten() {
                if b[1].is_null() {
                    out.push_str(&format!("  {ax}={}: empty\n", n(&b[0])));
                } else {
                    out.push_str(&format!(
                        "  {ax}={}: radius {}..{}\n",
                        n(&b[0]),
                        n(&b[1]),
                        n(&b[2])
                    ));
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn planes_parse() {
        assert_eq!(Plane::parse("z=10"), Some(Plane::Z(10.0)));
        assert_eq!(Plane::parse("x = -2.5"), Some(Plane::X(-2.5)));
        assert_eq!(Plane::parse("w=1"), None);
        assert_eq!(Plane::parse("z=nan"), None);
    }
}
