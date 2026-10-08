//! 2D attribution for the export render: which exact curve each edge of a
//! 2D shape lies on, through 2D booleans, transforms and offsets, so that
//! `linear_extrude` and `rotate_extrude` know the exact surface each side
//! face sweeps (the exact-geometry audit's F6).
//!
//! The audit suggested carrying the curve in Clipper2's Z channel. That
//! was not done: `clipper2-rust`'s `using_z` is a Cargo feature that adds
//! a `z` field to every `Point64` in the build (features unify, so the
//! normal render's and Manifold's own Clipper would carry it too, and the
//! pinned output would have to be re-proved), and an intersection vertex
//! gets one Z from a callback that sees both crossing edges but cannot
//! know which of them the vertex's outgoing edge follows. Instead each
//! output edge is matched geometrically against what can have made it:
//!
//! - after a boolean, an output edge lies on an input edge (Clipper only
//!   cuts edges, and its simplification only merges collinear ones), so
//!   it takes that input edge's curve;
//! - after an offset, it lies on the offset of an input line, on the
//!   concentric offset of an input circle, or (round joins) on a circle of
//!   radius `|delta|` about an input vertex.
//!
//! Clipper's output is unchanged: the export render calls the same
//! [`crate::clipper`] functions as the normal render.

use crate::polygon2d::Polygon2d;

/// A 2D curve that profile edges lie on.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Curve2 {
    /// The line through `p` along unit `d`.
    Line { p: [f64; 2], d: [f64; 2] },
    /// The circle about `c` of radius `r`. Its edges are chords spanning
    /// at most `step` radians of a circle of radius `chord_r` (the circle
    /// they were tessellated on, before any offset). `sag_normal` is how
    /// far the normal render's polygon stands inside the exact circle.
    Circle {
        c: [f64; 2],
        r: f64,
        step: f64,
        chord_r: f64,
        sag_normal: f64,
    },
    /// No exact curve: an edge of a shape only the normal render builds
    /// (text, imports, hulls), kept as a facet.
    Faceted,
}

/// The table index of [`Curve2::Faceted`], always entry 0.
pub const FACETED: u32 = 0;

/// A 2D shape with each edge's curve: `tags[o][i]` is the curve of the
/// edge from vertex `i` to vertex `i + 1` of outline `o`.
#[derive(Clone, Debug, Default)]
pub struct Tagged2 {
    pub poly: Polygon2d,
    pub tags: Vec<Vec<u32>>,
}

impl Tagged2 {
    /// Every outline's edges with their curve: (start, end, tag).
    pub fn edges(&self) -> impl Iterator<Item = ([f64; 2], [f64; 2], u32)> + '_ {
        self.poly
            .outlines
            .iter()
            .zip(&self.tags)
            .flat_map(|(o, t)| {
                let n = o.vertices.len();
                (0..n).map(move |i| (o.vertices[i], o.vertices[(i + 1) % n], t[i]))
            })
    }

    /// Every edge on one curve.
    pub fn uniform(poly: Polygon2d, tag: u32) -> Tagged2 {
        let tags = poly
            .outlines
            .iter()
            .map(|o| vec![tag; o.vertices.len()])
            .collect();
        Tagged2 { poly, tags }
    }
}

fn sub(a: [f64; 2], b: [f64; 2]) -> [f64; 2] {
    [a[0] - b[0], a[1] - b[1]]
}
fn dot(a: [f64; 2], b: [f64; 2]) -> f64 {
    a[0] * b[0] + a[1] * b[1]
}
fn cross(a: [f64; 2], b: [f64; 2]) -> f64 {
    a[0] * b[1] - a[1] * b[0]
}
fn len(a: [f64; 2]) -> f64 {
    dot(a, a).sqrt()
}

/// The line through `a` and `b` (`None` when they coincide).
pub fn line_through(a: [f64; 2], b: [f64; 2]) -> Option<Curve2> {
    let e = sub(b, a);
    let l = len(e);
    (l > 0.0).then(|| Curve2::Line {
        p: a,
        d: [e[0] / l, e[1] / l],
    })
}

/// The tolerance edges are matched within: Clipper rounds to 2^-27 units
/// and its simplification drops vertices within about 8.5e-9 of a line,
/// so a few grid steps, plus what transform rounding leaves on large
/// coordinates.
pub fn tolerance(poly: &Polygon2d) -> f64 {
    let reach = poly
        .outlines
        .iter()
        .flat_map(|o| o.vertices.iter())
        .map(|v| v[0].abs().max(v[1].abs()))
        .fold(0.0, f64::max);
    5e-8 + 1e-12 * reach
}

/// A uniform grid over axis-aligned boxes, for finding the candidates near
/// an edge without scanning them all (text and BOSL2 profiles have
/// thousands of edges).
struct Grid {
    lo: [f64; 2],
    cell: f64,
    n: [usize; 2],
    cells: Vec<Vec<u32>>,
    /// Items spanning too many cells, returned for every query.
    big: Vec<u32>,
}

impl Grid {
    fn new(boxes: &[([f64; 2], [f64; 2])]) -> Grid {
        let mut lo = [f64::INFINITY; 2];
        let mut hi = [f64::NEG_INFINITY; 2];
        for (a, b) in boxes {
            for k in 0..2 {
                lo[k] = lo[k].min(a[k]);
                hi[k] = hi[k].max(b[k]);
            }
        }
        if !lo[0].is_finite() || !hi[0].is_finite() || !lo[1].is_finite() || !hi[1].is_finite() {
            return Grid {
                lo: [0.0; 2],
                cell: 1.0,
                n: [1, 1],
                cells: vec![Vec::new()],
                big: (0..boxes.len() as u32).collect(),
            };
        }
        let ext = (hi[0] - lo[0]).max(hi[1] - lo[1]).max(1e-9);
        let side = ((boxes.len() as f64).sqrt().ceil() as usize).clamp(1, 512);
        let cell = ext / side as f64;
        let n = [
            (((hi[0] - lo[0]) / cell) as usize + 1).min(side + 1),
            (((hi[1] - lo[1]) / cell) as usize + 1).min(side + 1),
        ];
        let mut g = Grid {
            lo,
            cell,
            n,
            cells: vec![Vec::new(); n[0] * n[1]],
            big: Vec::new(),
        };
        for (i, (a, b)) in boxes.iter().enumerate() {
            let (x0, y0) = g.at(*a);
            let (x1, y1) = g.at(*b);
            if (x1 - x0 + 1) * (y1 - y0 + 1) > 64 {
                g.big.push(i as u32);
                continue;
            }
            for x in x0..=x1 {
                for y in y0..=y1 {
                    g.cells[x * g.n[1] + y].push(i as u32);
                }
            }
        }
        g
    }

    fn at(&self, p: [f64; 2]) -> (usize, usize) {
        let f = |k: usize| {
            let v = ((p[k] - self.lo[k]) / self.cell).floor();
            if v.is_nan() || v < 0.0 {
                0
            } else {
                (v as usize).min(self.n[k] - 1)
            }
        };
        (f(0), f(1))
    }

    /// The items whose cells meet the box, in increasing order.
    fn query(&self, a: [f64; 2], b: [f64; 2]) -> Vec<u32> {
        let (x0, y0) = self.at(a);
        let (x1, y1) = self.at(b);
        let mut out = self.big.clone();
        for x in x0..=x1 {
            for y in y0..=y1 {
                out.extend_from_slice(&self.cells[x * self.n[1] + y]);
            }
        }
        out.sort_unstable();
        out.dedup();
        out
    }
}

fn bbox(a: [f64; 2], b: [f64; 2], pad: f64) -> ([f64; 2], [f64; 2]) {
    (
        [a[0].min(b[0]) - pad, a[1].min(b[1]) - pad],
        [a[0].max(b[0]) + pad, a[1].max(b[1]) + pad],
    )
}

/// Which kind of curve wins when an edge lies on several: an exact line,
/// then a circle, then a facet.
fn rank(c: &Curve2) -> u8 {
    match c {
        Curve2::Line { .. } => 0,
        Curve2::Circle { .. } => 1,
        Curve2::Faceted => 2,
    }
}

/// The curves of `out`'s edges, `out` being a Clipper boolean (or
/// sanitization) of `sources`: each output edge lies on an input edge.
/// An edge that lies on none (it should not happen) is a facet.
pub fn attribute_boolean(
    out: &Polygon2d,
    sources: &[&Tagged2],
    curves: &[Curve2],
) -> Vec<Vec<u32>> {
    let segs: Vec<([f64; 2], [f64; 2], u32)> = sources.iter().flat_map(|s| s.edges()).collect();
    let tol = tolerance(out).max(
        sources
            .iter()
            .map(|s| tolerance(&s.poly))
            .fold(0.0, f64::max),
    );
    let grid = Grid::new(
        &segs
            .iter()
            .map(|&(a, b, _)| bbox(a, b, 2.0 * tol))
            .collect::<Vec<_>>(),
    );
    out.outlines
        .iter()
        .map(|o| {
            let n = o.vertices.len();
            (0..n)
                .map(|i| {
                    let (a, b) = (o.vertices[i], o.vertices[(i + 1) % n]);
                    let mid = [0.5 * (a[0] + b[0]), 0.5 * (a[1] + b[1])];
                    let (qa, qb) = bbox(mid, mid, tol);
                    let mut best: Option<(u8, u32)> = None;
                    for k in grid.query(qa, qb) {
                        let (s0, s1, tag) = segs[k as usize];
                        let e = sub(s1, s0);
                        let l = len(e);
                        if l == 0.0 {
                            continue;
                        }
                        let u = [e[0] / l, e[1] / l];
                        let off = |p: [f64; 2]| cross(u, sub(p, s0)).abs();
                        let t = dot(u, sub(mid, s0));
                        if off(a) > tol || off(b) > tol || t < -tol || t > l + tol {
                            continue;
                        }
                        let r = rank(&curves[tag as usize]);
                        if best.is_none_or(|(br, _)| r < br) {
                            best = Some((r, tag));
                        }
                    }
                    best.map_or(FACETED, |b| b.1)
                })
                .collect()
        })
        .collect()
}

/// What an offset's output edge can lie on.
#[derive(Clone, Copy, Debug)]
enum Cand {
    /// The offset of an input line (or facet), with the tag to give.
    Line { p: [f64; 2], d: [f64; 2], tag: u32 },
    /// A circle about `c` of radius `r` whose edges are chords of at most
    /// `step` radians on a circle of radius `chord_r`; `band` is how far
    /// inside the circle a chord's corners may stand.
    Circle {
        c: [f64; 2],
        r: f64,
        step: f64,
        chord_r: f64,
        band: f64,
        tag: u32,
    },
}

/// How an offset joins its edges, for [`attribute_offset`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Joins {
    /// Round joins whose arcs become exact circles, stepping by at most
    /// `step` radians.
    Arcs { step: f64, sag_normal: f64 },
    /// Round joins kept as their polygon (`$fn` set): new edges are lines.
    Polygon,
    /// Miter or square joins: new edges (chamfers) are lines.
    Straight,
}

/// The curves of `out`'s edges, `out` being the offset of `input` by
/// `delta`. New curves are appended to `curves`.
pub fn attribute_offset(
    out: &Polygon2d,
    input: &Tagged2,
    delta: f64,
    joins: Joins,
    curves: &mut Vec<Curve2>,
) -> Vec<Vec<u32>> {
    let tol = 4.0 * tolerance(out).max(tolerance(&input.poly));
    let mut cands: Vec<Cand> = Vec::new();
    // New curves per input curve (and side, for circles), so that every
    // edge on one offset curve shares one record.
    let mut made: std::collections::BTreeMap<(u32, i8), u32> = std::collections::BTreeMap::new();
    let mut new_curve = |curves: &mut Vec<Curve2>, key: (u32, i8), c: Curve2| -> u32 {
        *made.entry(key).or_insert_with(|| {
            curves.push(c);
            (curves.len() - 1) as u32
        })
    };
    for (o, tags) in input.poly.outlines.iter().zip(&input.tags) {
        let n = o.vertices.len();
        for (i, &tag) in tags.iter().enumerate() {
            let (a, b) = (o.vertices[i], o.vertices[(i + 1) % n]);
            let e = sub(b, a);
            let l = len(e);
            if l == 0.0 {
                continue;
            }
            let u = [e[0] / l, e[1] / l];
            // Clipper's results keep the material on the left of every
            // edge, so the outward normal is on the right.
            let nrm = [u[1], -u[0]];
            match curves[tag as usize] {
                Curve2::Line { p, d } => {
                    let q = [p[0] + nrm[0] * delta, p[1] + nrm[1] * delta];
                    // The normal's side of this edge: a line used in both
                    // directions (two shapes meeting along it) offsets
                    // both ways.
                    let side = if dot(nrm, [d[1], -d[0]]) > 0.0 { 1 } else { -1 };
                    let t = new_curve(curves, (tag, side), Curve2::Line { p: q, d });
                    cands.push(Cand::Line { p: q, d: u, tag: t });
                }
                Curve2::Faceted => {
                    let q = [a[0] + nrm[0] * delta, a[1] + nrm[1] * delta];
                    cands.push(Cand::Line {
                        p: q,
                        d: u,
                        tag: FACETED,
                    });
                }
                Curve2::Circle {
                    c,
                    r,
                    step,
                    chord_r,
                    sag_normal,
                } => {
                    // Counter-clockwise about the centre: the material is
                    // inside the circle, which grows by delta.
                    let ccw = cross(sub(a, c), e) > 0.0;
                    let rr = if ccw { r + delta } else { r - delta };
                    if rr <= tol {
                        continue;
                    }
                    let t = new_curve(
                        curves,
                        (tag, if ccw { 2 } else { -2 }),
                        Curve2::Circle {
                            c,
                            r: rr,
                            step,
                            chord_r,
                            sag_normal,
                        },
                    );
                    cands.push(Cand::Circle {
                        c,
                        r: rr,
                        step,
                        chord_r: chord_r.max(rr),
                        band: chord_r * (1.0 - (0.5 * step).cos()),
                        tag: t,
                    });
                }
            }
        }
        if let Joins::Arcs { step, sag_normal } = joins {
            for i in 0..n {
                let prev = tags[(i + n - 1) % n];
                let next = tags[i];
                let (pc, nc) = (curves[prev as usize], curves[next as usize]);
                // Between chords of one circle the join belongs to the
                // circle's offset, which is a candidate already.
                if prev == next && matches!(pc, Curve2::Circle { .. }) {
                    continue;
                }
                // The join's centre is the corner of the two curves where
                // both are lines: their records are exact, where the
                // polygon's vertex was rounded onto Clipper's grid. A
                // centre off the corner (by 3e-9 at 15.3) makes the arc
                // miss the offset lines it should touch, and anything
                // built on that tangency (a fillet's end cut across it)
                // does not reconstruct.
                let v = match (pc, nc) {
                    (Curve2::Line { p: p1, d: d1 }, Curve2::Line { p: p2, d: d2 }) => {
                        let den = cross(d1, d2);
                        let at = o.vertices[i];
                        if den.abs() > 1e-9 * len(d1) * len(d2) {
                            let t = cross(sub(p2, p1), d2) / den;
                            let c = [p1[0] + d1[0] * t, p1[1] + d1[1] * t];
                            if len(sub(c, at)) <= tol { c } else { at }
                        } else {
                            at
                        }
                    }
                    _ => o.vertices[i],
                };
                let faceted = matches!(pc, Curve2::Faceted) || matches!(nc, Curve2::Faceted);
                let tag = if faceted {
                    FACETED
                } else {
                    curves.push(Curve2::Circle {
                        c: v,
                        r: delta.abs(),
                        step,
                        chord_r: delta.abs(),
                        sag_normal,
                    });
                    (curves.len() - 1) as u32
                };
                cands.push(Cand::Circle {
                    c: v,
                    r: delta.abs(),
                    step,
                    chord_r: delta.abs(),
                    band: 0.0,
                    tag,
                });
            }
        }
    }
    let reach = 2.0 * delta.abs() + tol;
    let boxes: Vec<([f64; 2], [f64; 2])> = cands
        .iter()
        .map(|c| match *c {
            // A line candidate is near its source edge: the box of the
            // edge itself would do, but the edge is not kept, so the
            // offset point with the reach of a long edge is too small.
            // Lines are rare enough to be returned for every query.
            Cand::Line { .. } => ([f64::NEG_INFINITY; 2], [f64::INFINITY; 2]),
            Cand::Circle { c, r, .. } => bbox(c, c, r + reach),
        })
        .collect();
    let (lines, circles): (Vec<usize>, Vec<usize>) =
        (0..cands.len()).partition(|&i| matches!(cands[i], Cand::Line { .. }));
    let circle_boxes: Vec<_> = circles.iter().map(|&i| boxes[i]).collect();
    let grid = Grid::new(&circle_boxes);
    // Lines indexed by direction angle and offset would be faster; a
    // sorted scan by the line's distance from the origin keeps it simple.
    let mut line_key: Vec<(f64, usize)> = lines
        .iter()
        .map(|&i| match cands[i] {
            Cand::Line { p, d, .. } => (cross(d, p).abs(), i),
            Cand::Circle { .. } => unreachable!(),
        })
        .collect();
    line_key.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
    out.outlines
        .iter()
        .map(|o| {
            let n = o.vertices.len();
            (0..n)
                .map(|i| {
                    let (a, b) = (o.vertices[i], o.vertices[(i + 1) % n]);
                    let e = sub(b, a);
                    let l = len(e);
                    // Lines: both ends on the line, along it.
                    if l > 0.0 {
                        let u = [e[0] / l, e[1] / l];
                        let k = cross(u, a).abs();
                        let lo = line_key.partition_point(|x| x.0 < k - 2.0 * tol);
                        let mut hit: Option<usize> = None;
                        for &(key, ci) in &line_key[lo..] {
                            if key > k + 2.0 * tol {
                                break;
                            }
                            let Cand::Line { p, d, .. } = cands[ci] else {
                                continue;
                            };
                            if cross(d, u).abs() * l > tol {
                                continue;
                            }
                            if cross(d, sub(a, p)).abs() <= tol && cross(d, sub(b, p)).abs() <= tol
                            {
                                hit = Some(hit.map_or(ci, |h| h.min(ci)));
                            }
                        }
                        if let Some(ci) = hit
                            && let Cand::Line { tag, .. } = cands[ci]
                        {
                            return tag;
                        }
                    }
                    // Circles: both ends in the band the chords keep, and
                    // no longer than a chord.
                    let mid = [0.5 * (a[0] + b[0]), 0.5 * (a[1] + b[1])];
                    let mut best: Option<(bool, usize)> = None;
                    for k in grid.query(mid, mid) {
                        let ci = circles[k as usize];
                        let Cand::Circle {
                            c,
                            r,
                            step,
                            chord_r,
                            band,
                            ..
                        } = cands[ci]
                        else {
                            continue;
                        };
                        let ok = |p: [f64; 2]| {
                            let d = len(sub(p, c));
                            d <= r + tol && d >= r - band - tol
                        };
                        let max_chord = 2.0 * chord_r * (0.5 * step).sin() * 1.05 + 2.0 * tol;
                        if ok(a) && ok(b) && l <= max_chord {
                            // Offsets of input circles before join arcs.
                            let key = (band == 0.0, ci);
                            if best.is_none_or(|bk| key < bk) {
                                best = Some(key);
                            }
                        }
                    }
                    if let Some((_, ci)) = best
                        && let Cand::Circle { tag, .. } = cands[ci]
                    {
                        return tag;
                    }
                    match joins {
                        Joins::Arcs { .. } => FACETED,
                        Joins::Polygon | Joins::Straight => match line_through(a, b) {
                            Some(c) => {
                                curves.push(c);
                                (curves.len() - 1) as u32
                            }
                            None => FACETED,
                        },
                    }
                })
                .collect()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clipper::{self, Join, Op2};

    fn square_tagged(curves: &mut Vec<Curve2>, x: f64, y: f64, s: f64) -> Tagged2 {
        let v = vec![[x, y], [x + s, y], [x + s, y + s], [x, y + s]];
        let tags = (0..4)
            .map(|i| {
                curves.push(line_through(v[i], v[(i + 1) % 4]).unwrap());
                (curves.len() - 1) as u32
            })
            .collect();
        Tagged2 {
            poly: Polygon2d::from_outline(v),
            tags: vec![tags],
        }
    }

    fn circle_tagged(curves: &mut Vec<Curve2>, c: [f64; 2], r: f64, n: usize) -> Tagged2 {
        let step = std::f64::consts::TAU / n as f64;
        curves.push(Curve2::Circle {
            c,
            r,
            step,
            chord_r: r,
            sag_normal: 0.0,
        });
        let id = (curves.len() - 1) as u32;
        let v: Vec<[f64; 2]> = (0..n)
            .map(|i| {
                let a = step * i as f64;
                [c[0] + r * a.cos(), c[1] + r * a.sin()]
            })
            .collect();
        Tagged2::uniform(Polygon2d::from_outline(v), id)
    }

    #[test]
    fn boolean_edges_keep_their_curves() {
        let mut curves = vec![Curve2::Faceted];
        let sq = square_tagged(&mut curves, -10.0, -10.0, 20.0);
        let ci = circle_tagged(&mut curves, [0.0, 0.0], 5.0, 32);
        let out = clipper::apply(&[Some(&sq.poly), Some(&ci.poly)], Op2::Difference);
        let tags = attribute_boolean(&out, &[&sq, &ci], &curves);
        let circle_id = ci.tags[0][0];
        assert_eq!(out.outlines.len(), 2);
        for (o, t) in out.outlines.iter().zip(&tags) {
            let want_circle = !o.positive;
            for &k in t {
                assert_eq!(k == circle_id, want_circle, "{tags:?}");
                assert_ne!(k, FACETED);
            }
        }
    }

    #[test]
    fn round_offset_makes_corner_arcs() {
        let mut curves = vec![Curve2::Faceted];
        let sq = square_tagged(&mut curves, 0.0, 0.0, 10.0);
        let step = std::f64::consts::TAU / 32.0;
        let tol = 2.0 * (1.0 - (0.5 * step).cos());
        let out = clipper::offset(&sq.poly, 2.0, Join::Round, 2.0, tol);
        let n0 = curves.len();
        let tags = attribute_offset(
            &out,
            &sq,
            2.0,
            Joins::Arcs {
                step,
                sag_normal: 0.0,
            },
            &mut curves,
        );
        let mut kinds = std::collections::BTreeMap::new();
        for &k in &tags[0] {
            assert_ne!(k, FACETED, "{tags:?}");
            let kind = match curves[k as usize] {
                Curve2::Line { .. } => "line",
                Curve2::Circle { .. } => "circle",
                Curve2::Faceted => "facet",
            };
            kinds
                .entry(kind)
                .or_insert_with(std::collections::BTreeSet::new)
                .insert(k);
        }
        // Four offset sides and four corner circles, all new records.
        assert_eq!(kinds["line"].len(), 4);
        assert_eq!(kinds["circle"].len(), 4);
        assert!(tags[0].iter().all(|&k| k as usize >= n0));
    }

    #[test]
    fn offset_circle_stays_one_circle() {
        let mut curves = vec![Curve2::Faceted];
        let ci = circle_tagged(&mut curves, [1.0, 2.0], 5.0, 32);
        let step = std::f64::consts::TAU / 32.0;
        for delta in [1.5f64, -1.5] {
            let tol = delta.abs() * (1.0 - (0.5 * step).cos());
            let out = clipper::offset(&ci.poly, delta, Join::Round, 2.0, tol);
            let tags = attribute_offset(
                &out,
                &ci,
                delta,
                Joins::Arcs {
                    step,
                    sag_normal: 0.0,
                },
                &mut curves,
            );
            let first = tags[0][0];
            assert!(tags[0].iter().all(|&k| k == first), "{delta}: {tags:?}");
            let Curve2::Circle { r, .. } = curves[first as usize] else {
                panic!("not a circle")
            };
            assert!((r - (5.0 + delta)).abs() < 1e-12);
        }
    }
}
