//! `linear_extrude` and `rotate_extrude` for the Manifold backend
//! (`src/geometry/linear_extrude.cc`, `rotate_extrude.cc`), with the
//! slice and segment counts of `CurveDiscretizer`
//! (`src/core/CurveDiscretizer.cc:254-534`).
//!
//! Both build one ring of vertices per slice (every outline's vertices, in
//! order), stitch neighbouring rings with two triangles per edge, and close
//! the ends with the tessellation of the 2D shape, reusing the ring
//! vertices. The vertex order, the diagonal each side quad is split along
//! and the order of the triangles all follow OpenSCAD, because the images
//! show the triangles' shading and Manifold orders its output by them.

use eval::node::{Discretizer, LinearExtrude};
use eval::trig::{cos_degrees, sin_degrees};

use crate::fragments::{GRID_FINE, circular_segments_for_angle};
use crate::polygon2d::{Outline, Polygon2d};
use crate::polyset::PolySet;

/// `Eigen::Scaling(s) * Affine2d(rotate_degrees(-rot))` as a 2x2 matrix:
/// the scale applied to the rows of the rotation, as Eigen multiplies a
/// diagonal into an affine transform.
fn scale_rotate(sx: f64, sy: f64, rot: f64) -> [[f64; 2]; 2] {
    let s = sin_degrees(-rot);
    let c = cos_degrees(-rot);
    [[sx * c, sx * -s], [sy * s, sy * c]]
}

/// A matrix-vector product as the nightly computes it: Eigen sums each row
/// left to right and Apple clang fuses every multiply-add after the first
/// into an FMA (`-ffp-contract=on`), so `m00*x + m01*y` is
/// `fma(m01, y, m00*x)`. Plain arithmetic differs in the last bit, which
/// shows as `2.22045e-16` against `0` in exported coordinates.
pub(crate) fn apply(m: &[[f64; 2]; 2], v: [f64; 2]) -> [f64; 2] {
    [
        m[0][1].mul_add(v[1], m[0][0] * v[0]),
        m[1][1].mul_add(v[1], m[1][0] * v[0]),
    ]
}

/// Eigen's `norm()`, with the same fused sum as [`apply`].
fn norm(v: [f64; 2]) -> f64 {
    v[1].mul_add(v[1], v[0] * v[0]).sqrt()
}

fn sub(a: [f64; 2], b: [f64; 2]) -> [f64; 2] {
    [a[0] - b[0], a[1] - b[1]]
}

/// `Calc::lerp`.
fn lerp(a: f64, b: f64, t: f64) -> f64 {
    (1.0 - t) * a + t * b
}

// ---------------------------------------------------------------------------
// Slice counts (CurveDiscretizer)

fn min_twist_slices(twist: f64) -> i32 {
    ((twist / 120.0).ceil() as i32).max(1)
}

/// `getHelixSlices`, without the experimental `$fe` branch.
fn helix_slices(d: &Discretizer, r_sqr: f64, height: f64, twist: f64) -> Option<i32> {
    let twist = twist.abs();
    let min_slices = min_twist_slices(twist);
    if r_sqr.sqrt() < GRID_FINE
        || d.fn_.is_infinite()
        || d.fn_.is_nan()
        || height.is_nan()
        || twist.is_nan()
    {
        return None;
    }
    if d.fn_ > 0.0 {
        return Some(((twist / 360.0 * d.fn_).ceil() as i32).max(min_slices));
    }
    let fa_slices = (twist / d.fa).ceil() as i32;
    // `helix_arc_length`: the length of the helix a vertex follows.
    let t = twist * std::f64::consts::PI / 180.0;
    let c = height / t;
    let len = t * (r_sqr + c * c).sqrt();
    let fs_slices = (len / d.fs).ceil() as i32;
    Some(fa_slices.min(fs_slices).max(min_slices))
}

/// `archimedes_length`.
fn archimedes_length(a: f64, theta: f64) -> f64 {
    0.5 * a * (theta * (1.0 + theta * theta).sqrt() + theta.asinh())
}

/// `getConicalHelixSlices`: twist with a uniform scale other than 1.
fn conical_helix_slices(
    d: &Discretizer,
    r_sqr: f64,
    height: f64,
    twist: f64,
    scale: f64,
) -> Option<i32> {
    let twist = twist.abs();
    let r = r_sqr.sqrt();
    let min_slices = min_twist_slices(twist);
    if r < GRID_FINE || d.fn_.is_infinite() || d.fn_.is_nan() {
        return None;
    }
    if d.fn_ > 0.0 {
        return Some(((twist * d.fn_ / 360.0).ceil() as i32).max(min_slices));
    }
    let rads = twist * std::f64::consts::PI / 180.0;
    let angle_end = if scale > 1.0 {
        rads * scale / (scale - 1.0)
    } else {
        rads / (1.0 - scale)
    };
    let angle_start = angle_end - rads;
    let a = r / angle_end;
    let spiral = archimedes_length(a, angle_end) - archimedes_length(a, angle_start);
    let total = (spiral * spiral + height * height).sqrt();
    let fs_slices = (total / d.fs).ceil() as i32;
    let fa_slices = (twist / d.fa).ceil() as i32;
    Some(fa_slices.min(fs_slices).max(min_slices))
}

/// `getDiagonalSlices`: a non-uniform scale bends the side faces.
fn diagonal_slices(d: &Discretizer, delta_sqr: f64, height: f64) -> Option<i32> {
    if delta_sqr.sqrt() < GRID_FINE || d.fn_.is_infinite() || d.fn_.is_nan() {
        return None;
    }
    if d.fn_ > 0.0 {
        return Some((d.fn_ as i32).max(1));
    }
    Some((((delta_sqr + height * height).sqrt() / d.fs).ceil() as i32).max(1))
}

/// `calc_max_delta_sqr`: how far the scale moves any vertex, squared.
fn max_delta_sqr(poly: &Polygon2d, sx: f64, sy: f64) -> f64 {
    let mut m = 0.0f64;
    for o in &poly.outlines {
        for v in &o.vertices {
            let d = [v[0] - v[0] * sx, v[1] - v[1] * sy];
            m = m.max(d[0] * d[0] + d[1] * d[1]);
        }
    }
    m
}

/// `calc_num_slices`.
fn num_slices(e: &LinearExtrude, poly: &Polygon2d) -> u32 {
    if e.has_slices {
        return e.slices;
    }
    let (sx, sy) = (e.scale[0], e.scale[1]);
    let h = e.height[2];
    let d = &e.disc;
    let fallback = || min_twist_slices(e.twist);
    if e.has_twist {
        let mut r1 = 0.0f64;
        for o in &poly.outlines {
            for v in &o.vertices {
                r1 = r1.max(v[0] * v[0] + v[1] * v[1]);
            }
        }
        if sx == 1.0 && sy == 1.0 {
            helix_slices(d, r1, h, e.twist).unwrap_or_else(fallback) as u32
        } else if sx != sy {
            let by_scale = diagonal_slices(d, max_delta_sqr(poly, sx, sy), h).unwrap_or(1) as u32;
            let by_twist = helix_slices(d, r1, h, e.twist).unwrap_or_else(fallback) as u32;
            by_scale.max(by_twist)
        } else {
            conical_helix_slices(d, r1, h, e.twist, sx).unwrap_or_else(fallback) as u32
        }
    } else if sx != sy {
        diagonal_slices(d, max_delta_sqr(poly, sx, sy), h).unwrap_or(1) as u32
    } else {
        1
    }
}

// ---------------------------------------------------------------------------
// Outline segmentation (CurveDiscretizer::splitOutline)

/// `add_segmented_edge`: `n` points from `v0` towards `v1`, excluding `v1`.
fn add_segmented_edge(out: &mut Vec<[f64; 2]>, v0: [f64; 2], v1: [f64; 2], n: u32) {
    for j in 0..n {
        let t = f64::from(j) / f64::from(n);
        out.push([(1.0 - t) * v0[0] + t * v1[0], (1.0 - t) * v0[1] + t * v1[1]]);
    }
}

/// The longest an edge gets over all slices of the extrusion.
fn max_edge_lengths(o: &Outline, twist: f64, sx: f64, sy: f64, slices: u32) -> Vec<f64> {
    let n = o.vertices.len();
    let mut out = Vec::with_capacity(n);
    let mut v0 = o.vertices[0];
    for i in 1..=n {
        let v1 = o.vertices[i % n];
        if sx != sy {
            let mut m = 0.0f64;
            for j in 0..=slices {
                let t = f64::from(j) / f64::from(slices);
                let tr = scale_rotate(lerp(1.0, sx, t), lerp(1.0, sy, t), twist * t);
                m = m.max(norm(sub(apply(&tr, v1), apply(&tr, v0))));
            }
            out.push(m);
        } else {
            out.push(norm(sub(v1, v0)) * sx.max(1.0));
        }
        v0 = v1;
    }
    out
}

#[derive(Debug, Clone, Copy)]
struct Tracker {
    edge: usize,
    max_len: f64,
    count: u32,
}

impl Tracker {
    fn metric(&self) -> f64 {
        self.max_len / (f64::from(self.count) + 0.5)
    }
    fn close_match(&self, other: &Tracker) -> bool {
        let (a, b) = (self.metric(), other.metric());
        a.min(b) / a.max(b) >= 0.999
    }
}

/// `std::priority_queue<segment_tracker>` with libc++'s heap algorithms
/// (`__sift_up`, and `__floyd_sift_down` in `pop_heap`), so edges whose
/// metrics tie come out in the same order as on the nightly (a macOS
/// build).
#[derive(Debug, Default)]
struct Heap(Vec<Tracker>);

impl Heap {
    fn less(a: &Tracker, b: &Tracker) -> bool {
        a.metric() < b.metric()
    }

    /// `__sift_up` over `v[..end]`.
    fn sift_up(v: &mut [Tracker], end: usize) {
        if end > 1 {
            let mut len = (end - 2) / 2;
            let mut last = end - 1;
            if Self::less(&v[len], &v[last]) {
                let t = v[last];
                loop {
                    v[last] = v[len];
                    last = len;
                    if len == 0 {
                        break;
                    }
                    len = (len - 1) / 2;
                    if !Self::less(&v[len], &t) {
                        break;
                    }
                }
                v[last] = t;
            }
        }
    }

    fn push(&mut self, t: Tracker) {
        self.0.push(t);
        let end = self.0.len();
        Self::sift_up(&mut self.0, end);
    }

    fn top(&self) -> Option<Tracker> {
        self.0.first().copied()
    }

    fn pop(&mut self) {
        let v = &mut self.0;
        let len = v.len();
        if len > 1 {
            let top = v[0];
            let mut hole = 0;
            let mut child = 0;
            loop {
                child = 2 * child + 1;
                if child + 1 < len && Self::less(&v[child], &v[child + 1]) {
                    child += 1;
                }
                v[hole] = v[child];
                hole = child;
                if child > (len - 2) / 2 {
                    break;
                }
            }
            let last = len - 1;
            if hole == last {
                v[hole] = top;
            } else {
                v[hole] = v[last];
                v[last] = top;
                Self::sift_up(v, hole + 1);
            }
        }
        v.pop();
    }
}

/// `splitOutlineByFn`: add points to the longest edges until the outline
/// has `fn_` vertices, splitting edges of nearly equal length together so
/// the result stays symmetric.
fn split_by_fn(o: &Outline, twist: f64, sx: f64, sy: f64, fn_: f64, slices: u32) -> Outline {
    let n = o.vertices.len();
    let mut counts = vec![1u32; n];
    let mut q = Heap::default();
    for (i, len) in max_edge_lengths(o, twist, sx, sy, slices)
        .into_iter()
        .enumerate()
    {
        q.push(Tracker {
            edge: i,
            max_len: len,
            count: 1,
        });
    }
    let mut tmp: Vec<Tracker> = Vec::new();
    let mut total = n;
    while (total as f64) < fn_ {
        while let Some(top) = q.top() {
            if !(tmp.is_empty() || top.close_match(&tmp[0])) {
                break;
            }
            tmp.push(top);
            q.pop();
        }
        if ((total + tmp.len()) as f64) <= fn_ {
            while let Some(mut cur) = tmp.pop() {
                cur.count += 1;
                counts[cur.edge] += 1;
                total += 1;
                q.push(cur);
            }
        } else {
            while let Some(cur) = tmp.pop() {
                q.push(cur);
            }
            break;
        }
    }
    let mut out = Vec::with_capacity(total);
    let mut v0 = o.vertices[0];
    for i in 1..=n {
        let v1 = o.vertices[i % n];
        add_segmented_edge(&mut out, v0, v1, counts[i - 1]);
        v0 = v1;
    }
    Outline {
        vertices: out,
        positive: o.positive,
    }
}

/// `splitOutlineByFs`: every edge in pieces no longer than `$fs`.
fn split_by_fs(o: &Outline, twist: f64, sx: f64, sy: f64, fs: f64, slices: u32) -> Outline {
    let n = o.vertices.len();
    let lens = max_edge_lengths(o, twist, sx, sy, slices);
    let mut out = Vec::new();
    let mut v0 = o.vertices[0];
    for i in 1..=n {
        let v1 = o.vertices[i % n];
        add_segmented_edge(&mut out, v0, v1, (lens[i - 1] / fs).ceil() as u32);
        v0 = v1;
    }
    Outline {
        vertices: out,
        positive: o.positive,
    }
}

/// `CurveDiscretizer::splitOutline`.
fn split_outline(
    d: &Discretizer,
    o: &Outline,
    twist: f64,
    sx: f64,
    sy: f64,
    slices: u32,
    segments: u32,
) -> Outline {
    if segments > 0 || d.fn_ > 0.0 {
        let min_vertices = if segments > 0 {
            segments
        } else {
            d.fn_.max(3.0) as u32
        };
        if o.vertices.len() >= min_vertices as usize {
            return o.clone();
        }
        return split_by_fn(o, twist, sx, sy, f64::from(min_vertices), slices);
    }
    let fa_segs = (360.0 / d.fa).ceil() as u32;
    if o.vertices.len() >= fa_segs as usize {
        return o.clone();
    }
    let by_fs = split_by_fs(o, twist, sx, sy, d.fs, slices);
    if by_fs.vertices.len() >= fa_segs as usize {
        split_by_fn(o, twist, sx, sy, f64::from(fa_segs), slices)
    } else {
        by_fs
    }
}

// ---------------------------------------------------------------------------
// linear_extrude

/// `sgn_vdiff`: compare two lengths, treating them as equal within 1e-5 of
/// their size.
fn sgn_vdiff(a: [f64; 2], b: [f64; 2]) -> i32 {
    let (l1, l2) = (norm(a), norm(b));
    let scale = l1 + l2;
    let diff = 2.0 * (l1 - l2).abs() * 1e5;
    if diff > scale {
        if l1 < l2 { -1 } else { 1 }
    } else {
        0
    }
}

/// `add_slice_indices`: two triangles per edge between ring `slice - 1`
/// and ring `slice`, split along the shorter diagonal.
#[allow(clippy::too_many_arguments)]
fn add_slice_indices(
    faces: &mut Vec<Vec<u32>>,
    slice: u32,
    stride: u32,
    poly: &Polygon2d,
    rot_bot: f64,
    rot_top: f64,
    scale_bot: [f64; 2],
    scale_top: [f64; 2],
) {
    let bottom = (slice - 1) * stride;
    let top = slice * stride;
    let tb = scale_rotate(scale_bot[0], scale_bot[1], rot_bot);
    let tt = scale_rotate(scale_top[0], scale_top[1], rot_top);
    let any_zero = scale_top[0] == 0.0 || scale_top[1] == 0.0;
    // "setting back_twist true helps keep diagonals same as previous builds"
    let back_twist = rot_top <= rot_bot;
    let mut curr = 0u32;
    for o in &poly.outlines {
        let n = o.vertices.len();
        let mut prev_bot = apply(&tb, o.vertices[0]);
        let mut prev_top = apply(&tt, o.vertices[0]);
        // Equal diagonals go the way their neighbours do, which depends on
        // the twist direction and on whether this outline is a hole.
        let flip = (!o.positive) ^ back_twist;
        for i in 1..=n {
            let k = i % n;
            let bot = apply(&tb, o.vertices[k]);
            let topv = apply(&tt, o.vertices[k]);
            let idx = curr + k as u32;
            let prev = curr + i as u32 - 1;
            let sign = sgn_vdiff(sub(prev_bot, topv), sub(bot, prev_top));
            let split_first = sign == -1 || (sign == 0 && !flip);
            // Split along the shorter diagonal, except under a top scaled to
            // zero on an axis, where that would leave zero-thickness ears.
            if split_first ^ any_zero {
                faces.push(vec![bottom + idx, top + idx, bottom + prev]);
                faces.push(vec![top + prev, bottom + prev, top + idx]);
            } else {
                faces.push(vec![bottom + idx, top + prev, bottom + prev]);
                faces.push(vec![bottom + idx, top + idx, top + prev]);
            }
            prev_bot = bot;
            prev_top = topv;
        }
        curr += n as u32;
    }
}

/// `extrudePolygon` (`linear_extrude.cc:362-419`), Manifold branch. `poly`
/// must be sanitized.
pub fn linear_extrude(e: &LinearExtrude, poly: &Polygon2d) -> PolySet {
    let empty = PolySet {
        triangular: true,
        ..Default::default()
    };
    if e.height[2] <= 0.0 {
        return empty;
    }
    let (sx, sy) = (e.scale[0], e.scale[1]);
    let non_linear = e.twist != 0.0 || sx != sy;
    // "Twist makes convex polygons into unknown polyhedrons"
    let convex = if poly.is_convex() {
        if non_linear { None } else { Some(true) }
    } else {
        Some(false)
    };
    let slices = num_slices(e, poly);

    let mut seg = Polygon2d::default();
    if !(e.has_segments && e.segments == 0) && (e.segments > 0 || non_linear) {
        for o in &poly.outlines {
            seg.outlines.push(split_outline(
                &e.disc, o, e.twist, sx, sy, slices, e.segments,
            ));
        }
    }
    let polyref = if seg.is_empty() { poly } else { &seg };

    let (mut h1, mut h2) = ([0.0; 3], e.height);
    if e.center {
        for k in 0..3 {
            h1[k] -= e.height[k] / 2.0;
            h2[k] -= e.height[k] / 2.0;
        }
    }

    // `prepareVerticesAndIndices`.
    let stride: u32 = polyref
        .outlines
        .iter()
        .map(|o| o.vertices.len() as u32)
        .sum();
    let mut vertices = Vec::with_capacity((stride * (slices + 1)) as usize);
    let full_scale = [1.0 - sx, 1.0 - sy];
    let full_rot = -e.twist;
    let full_height = [h2[0] - h1[0], h2[1] - h1[1], h2[2] - h1[2]];
    let n = f64::from(slices);
    for j in 0..=slices {
        let jf = f64::from(j);
        let s = [1.0 - full_scale[0] * jf / n, 1.0 - full_scale[1] * jf / n];
        // `rotate_degrees(full_rot * j / n)`, which is the same matrix as
        // `scale_rotate` with the angle negated.
        let m = scale_rotate(s[0], s[1], -(full_rot * jf / n));
        for o in &polyref.outlines {
            for v in &o.vertices {
                let t = apply(&m, *v);
                vertices.push(std::array::from_fn(|k| {
                    let base = if k < 2 { t[k] } else { 0.0 };
                    (base + h1[k]) + full_height[k] * jf / n
                }));
            }
        }
    }
    let mut faces = Vec::with_capacity((stride * (slices + 1) * 2) as usize);
    for j in 1..=slices {
        let (jb, jt) = (f64::from(j - 1), f64::from(j));
        let rot_bot = e.twist * jb / n;
        let rot_top = e.twist * jt / n;
        let scale_bot = [1.0 - (1.0 - sx) * jb / n, 1.0 - (1.0 - sy) * jb / n];
        let scale_top = [1.0 - (1.0 - sx) * jt / n, 1.0 - (1.0 - sy) * jt / n];
        add_slice_indices(
            &mut faces, j, stride, polyref, rot_bot, rot_top, scale_bot, scale_top,
        );
    }

    // `assemblePolySetForManifold`: the caps reuse the ring vertices, top
    // first, then the bottom with its triangles reversed.
    let caps = polyref.tessellate();
    let top = stride * slices;
    for t in &caps.faces {
        faces.push(t.iter().map(|&i| i + top).collect());
    }
    for t in &caps.faces {
        faces.push(t.iter().rev().copied().collect());
    }
    PolySet {
        vertices,
        faces,
        convex,
        triangular: true,
        ..Default::default()
    }
}

// ---------------------------------------------------------------------------
// rotate_extrude

/// What `rotate_extrude` produced: `Ok(None)` is no geometry (a zero
/// angle); `Err` carries OpenSCAD's error for a shape across the Y axis.
pub type Rotated = Result<Option<PolySet>, String>;

/// `rotatePolygon` (`rotate_extrude.cc:84-177`).
pub fn rotate_extrude(angle: f64, start: f64, disc: &Discretizer, poly: &Polygon2d) -> Rotated {
    if angle == 0.0 {
        return Ok(None);
    }
    let (mut min_x, mut max_x) = (0.0f64, 0.0f64);
    for o in &poly.outlines {
        for v in &o.vertices {
            min_x = min_x.min(v[0]);
            max_x = max_x.max(v[0]);
        }
    }
    if max_x > 0.0 && min_x < 0.0 {
        return Err(format!(
            "Children of rotate_extrude() may not lie across the Y axis (Range of X coords for all children [{min_x:.2} : {max_x:.2}])"
        ));
    }
    let sections = circular_segments_for_angle(disc, max_x - min_x, angle)
        .unwrap_or_else(|| ((angle.abs() / 360.0 * 3.0) as i32).max(1));
    let closed = angle == 360.0;
    let rings = sections as u32 + u32::from(!closed);
    let flip = (min_x >= 0.0 && angle > 0.0) || (min_x < 0.0 && angle < 0.0);
    let stride: u32 = poly.outlines.iter().map(|o| o.vertices.len() as u32).sum();
    let nv = stride * rings;
    let mut vertices = Vec::with_capacity(nv as usize);
    for j in 0..rings {
        let a = start + f64::from(j) * angle / f64::from(sections);
        let (c, s) = (cos_degrees(a), sin_degrees(a));
        for o in &poly.outlines {
            for v in &o.vertices {
                vertices.push([v[0] * c, v[0] * s, v[1]]);
            }
        }
    }
    let mut faces: Vec<Vec<u32>> = Vec::with_capacity((stride * rings * 2) as usize);
    for slice in 1..=sections as u32 {
        let prev_slice = (slice - 1) * stride;
        let curr_slice = slice * stride;
        let mut curr = 0u32;
        for o in &poly.outlines {
            let n = o.vertices.len() as u32;
            for i in 1..=n {
                let ci = curr + i % n;
                let pi = curr + i - 1;
                if flip {
                    faces.push(vec![
                        (prev_slice + pi) % nv,
                        (curr_slice + ci) % nv,
                        (prev_slice + ci) % nv,
                    ]);
                    faces.push(vec![
                        (curr_slice + ci) % nv,
                        (prev_slice + pi) % nv,
                        (curr_slice + pi) % nv,
                    ]);
                } else {
                    faces.push(vec![
                        (prev_slice + ci) % nv,
                        (curr_slice + ci) % nv,
                        (prev_slice + pi) % nv,
                    ]);
                    faces.push(vec![
                        (curr_slice + pi) % nv,
                        (prev_slice + pi) % nv,
                        (curr_slice + ci) % nv,
                    ]);
                }
            }
            curr += n;
        }
    }
    if !closed {
        // Caps: the start ring's tessellation, reversed unless the faces
        // are flipped, then the end ring's facing the other way.
        let caps = poly.tessellate();
        let offset = stride * sections as u32;
        for t in &caps.faces {
            faces.push(if flip {
                t.clone()
            } else {
                t.iter().rev().copied().collect()
            });
        }
        for t in &caps.faces {
            let t: Vec<u32> = if flip {
                t.iter().rev().copied().collect()
            } else {
                t.clone()
            };
            faces.push(t.iter().map(|&i| i + offset).collect());
        }
    }
    Ok(Some(PolySet {
        vertices,
        faces,
        convex: Some(false),
        triangular: true,
        ..Default::default()
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn disc(fn_: f64) -> Discretizer {
        Discretizer {
            fn_,
            fa: 12.0,
            fs: 2.0,
        }
    }

    fn ext(h: f64) -> LinearExtrude {
        LinearExtrude {
            height: [0.0, 0.0, h],
            center: false,
            convexity: 1,
            twist: 0.0,
            has_twist: false,
            slices: 1,
            has_slices: false,
            segments: 0,
            has_segments: false,
            scale: [1.0, 1.0],
            disc: disc(0.0),
        }
    }

    fn square(s: f64) -> Polygon2d {
        Polygon2d::from_outline(vec![[0.0, 0.0], [s, 0.0], [s, s], [0.0, s]])
    }

    fn circle(r: f64, n: i32) -> Polygon2d {
        Polygon2d::from_outline(
            (0..n)
                .map(|i| {
                    let phi = 360.0 * f64::from(i) / f64::from(n);
                    [r * cos_degrees(phi), r * sin_degrees(phi)]
                })
                .collect(),
        )
    }

    /// Vertex and triangle counts of `openscad -o x.off` on the 2026.09.23
    /// nightly (the second line of the OFF file).
    #[test]
    fn linear_counts_match_the_nightly() {
        let twist = |mut e: LinearExtrude, t: f64| {
            e.twist = t;
            e.has_twist = true;
            e
        };
        let scale = |mut e: LinearExtrude, s: [f64; 2]| {
            e.scale = s;
            e
        };
        let centered =
            Polygon2d::from_outline(vec![[-2.0, -2.0], [2.0, -2.0], [2.0, 2.0], [-2.0, 2.0]]);
        let cases: Vec<(&str, PolySet, usize, usize)> = vec![
            (
                "linear_extrude(10) square(10)",
                linear_extrude(&ext(10.0), &square(10.0)),
                8,
                12,
            ),
            (
                "linear_extrude(10, twist=90) square(10)",
                linear_extrude(&twist(ext(10.0), 90.0), &square(10.0)),
                180,
                356,
            ),
            (
                "linear_extrude(10, scale=[2,0.5]) square(10)",
                linear_extrude(&scale(ext(10.0), [2.0, 0.5]), &square(10.0)),
                270,
                536,
            ),
            // circle(5) has 16 fragments by default.
            (
                "linear_extrude(10, twist=90, scale=0.5) circle(5)",
                linear_extrude(&scale(twist(ext(10.0), 90.0), [0.5, 0.5]), &circle(5.0, 16)),
                112,
                220,
            ),
            (
                "linear_extrude(10, twist=-45, scale=[1,0]) square(4, center=true)",
                linear_extrude(&scale(twist(ext(10.0), -45.0), [1.0, 0.0]), &centered),
                56,
                108,
            ),
        ];
        for (name, ps, v, f) in cases {
            assert_eq!((ps.vertices.len(), ps.faces.len()), (v, f), "{name}");
        }
    }

    #[test]
    fn rotate_counts() {
        // rotate_extrude($fn=8) translate([5,0]) square(1);
        let sq = Polygon2d::from_outline(vec![[5.0, 0.0], [6.0, 0.0], [6.0, 1.0], [5.0, 1.0]]);
        let ps = rotate_extrude(360.0, 0.0, &disc(8.0), &sq)
            .unwrap()
            .unwrap();
        assert_eq!((ps.vertices.len(), ps.faces.len()), (32, 64));
        // rotate_extrude(angle=90) translate([5,0]) circle(1): 5 sections
        // of a 5-gon, plus caps.
        let c: Vec<[f64; 2]> = circle(1.0, 5).outlines[0]
            .vertices
            .iter()
            .map(|v| [v[0] + 5.0, v[1]])
            .collect();
        let ps = rotate_extrude(90.0, 0.0, &disc(0.0), &Polygon2d::from_outline(c))
            .unwrap()
            .unwrap();
        assert_eq!((ps.vertices.len(), ps.faces.len()), (30, 56));
        let err = rotate_extrude(
            360.0,
            0.0,
            &disc(8.0),
            &Polygon2d::from_outline(vec![[-1.0, 0.0], [1.0, 0.0], [1.0, 1.0]]),
        );
        assert_eq!(
            err.unwrap_err(),
            "Children of rotate_extrude() may not lie across the Y axis (Range of X coords for all children [-1.00 : 1.00])"
        );
    }

    #[test]
    fn heap_pops_in_libcxx_order() {
        let mut h = Heap::default();
        for (i, l) in [3.0, 1.0, 3.0, 2.0, 3.0].into_iter().enumerate() {
            h.push(Tracker {
                edge: i,
                max_len: l,
                count: 1,
            });
        }
        let mut order = Vec::new();
        while let Some(t) = h.top() {
            order.push(t.edge);
            h.pop();
        }
        assert_eq!(order.len(), 5);
        assert_eq!(&order[3..], &[3, 1]);
    }
}
