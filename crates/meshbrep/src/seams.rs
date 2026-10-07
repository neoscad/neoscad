//! Parametrising faces: frames, seams and parameter-space curves.
//!
//! A STEP reader trims a face in its surface's (u, v) space. Without the
//! 2D curves a reader rebuilds them itself, and on spheres trimmed by
//! circles that are not parallels OCCT ends at tolerances up to 6.7e-3
//! (the audit's F3). So every edge on a curved face gets a parameter-space
//! curve here, and every face that wraps around its axis gets a seam: the
//! edge along `u = 0` (= 2π) where the face meets itself, used once in each
//! direction.
//!
//! Each curved face gets its own frame, chosen so that its loops are
//! simple in (u, v): a sphere's axis is the normal of the circles that
//! bound it (so they are parallels), and the seam is placed along a
//! meridian that misses every hole, at `u = 0`.

use crate::Error;
use crate::bspline;
use crate::curve::{self, CurveEval};
use crate::math::*;
use crate::model::{BSpline, Curve};
use crate::surf::{Param, Surf};
use crate::topo::{TEdge, Topo};

/// One sample of a loop in parameter space.
#[derive(Clone, Copy, Debug)]
struct S {
    u: f64,
    v: f64,
    /// Curve parameter of the sample.
    t: f64,
}

/// Samples of a coedge in loop order, `n + 1` of them, with the ends at
/// the exact vertex positions.
fn coedge_points(topo: &Topo, e: usize, fwd: bool, n: usize) -> Vec<(V, f64)> {
    let ed = &topo.edges[e];
    let [t0, t1] = ed.range;
    let mut out: Vec<(V, f64)> = (0..=n)
        .map(|i| {
            let t = t0 + (t1 - t0) * i as f64 / n as f64;
            (curve::eval(&ed.curve, t), t)
        })
        .collect();
    out[0].0 = topo.verts[ed.v0];
    out[n].0 = topo.verts[ed.v1];
    if !fwd {
        out.reverse();
    }
    out
}

fn wrap_near(raw: f64, prev: f64) -> f64 {
    let mut d = raw - prev;
    d -= TAU * (d / TAU).round();
    prev + d
}

/// Lifts points to (u, v) with `u` continuous, starting near `start_u`
/// (or at the first point's own angle). Points on the axis take the angle
/// of their neighbours.
fn lift(param: &Param, pts: &[(V, f64)], start_u: Option<f64>, tol: f64) -> Vec<S> {
    let raw: Vec<(Option<f64>, f64)> = pts
        .iter()
        .map(|&(p, _)| {
            let (u, w) = param.uv(p);
            ((!param.near_axis(p, tol)).then_some(u), w)
        })
        .collect();
    let mut us: Vec<Option<f64>> = raw.iter().map(|r| r.0).collect();
    // Fill axis points from the next known angle, then the previous.
    let mut next: Option<f64> = None;
    for u in us.iter_mut().rev() {
        match u {
            Some(x) => next = Some(*x),
            None => *u = next,
        }
    }
    let mut prev_known: Option<f64> = None;
    for u in us.iter_mut() {
        match u {
            Some(x) => prev_known = Some(*x),
            None => *u = prev_known,
        }
    }
    let mut out = Vec::with_capacity(pts.len());
    let mut prev = start_u;
    for (i, u) in us.iter().enumerate() {
        let raw_u = u.unwrap_or(prev.unwrap_or(0.0));
        // Only an angle wraps. On a plane `u` is a length, and wrapping it
        // by 2π folded any loop whose samples are more than π apart (a
        // 30 mm cube's 5-sample edges) into the wrong area, so a hole
        // read as a second outer loop.
        let lu = match prev {
            Some(p) if param.periodic() => wrap_near(raw_u, p),
            _ => raw_u,
        };
        out.push(S {
            u: lu,
            v: raw[i].1,
            t: pts[i].1,
        });
        prev = Some(lu);
    }
    out
}

/// A loop lifted into parameter space: per coedge, its samples in loop
/// order.
fn lift_loop(
    topo: &Topo,
    param: &Param,
    lp: &[(usize, bool)],
    start_u: Option<f64>,
    tol: f64,
) -> Vec<Vec<S>> {
    let mut out: Vec<Vec<S>> = Vec::with_capacity(lp.len());
    let mut cur = start_u;
    for &(e, fwd) in lp {
        let ed = &topo.edges[e];
        let n = curve::sample_count(&ed.curve, ed.range);
        let pts = coedge_points(topo, e, fwd, n);
        let s = lift(param, &pts, cur, tol);
        cur = s.last().map(|x| x.u);
        out.push(s);
    }
    out
}

fn winding(l: &[Vec<S>]) -> i64 {
    let (a, b) = (
        l[0][0].u,
        l.last().expect("loop").last().expect("samples").u,
    );
    ((b - a) / TAU).round() as i64
}

/// Twice the signed area of a lifted loop (positive counter-clockwise).
fn area(l: &[Vec<S>]) -> f64 {
    let mut s = 0.0;
    for c in l {
        for w in c.windows(2) {
            s += w[0].u * w[1].v - w[1].u * w[0].v;
        }
    }
    s
}

/// Where a loop crosses the meridian `u ≡ u0`.
#[derive(Clone, Copy, Debug)]
struct Crossing {
    /// Index of the coedge in the loop.
    ci: usize,
    /// Curve parameter of the crossing.
    t: f64,
    /// At the coedge's start vertex.
    at_start: bool,
}

fn crossings(
    topo: &Topo,
    param: &Param,
    lp: &[(usize, bool)],
    l: &[Vec<S>],
    u0: f64,
) -> Vec<Crossing> {
    const EPS: f64 = 1e-11;
    let g = |u: f64| {
        let d = u - u0;
        d - TAU * (d / TAU).round()
    };
    let mut out = Vec::new();
    for (ci, c) in l.iter().enumerate() {
        for i in 0..c.len() - 1 {
            let (a, b) = (c[i], c[i + 1]);
            // Signed offsets in one sheet: continue a's sheet to b.
            let ga = g(a.u);
            let gb = ga + (b.u - a.u);
            if ga.abs() < EPS {
                out.push(Crossing {
                    ci,
                    t: a.t,
                    at_start: i == 0,
                });
                continue;
            }
            // Between samples u moves by less than π, so b lies in the
            // same sheet as a, and a crossing is a change of sign. A
            // crossing at b itself is counted as the next segment's start.
            if gb.abs() < EPS || (ga < 0.0) == (gb < 0.0) {
                continue;
            }
            // Bisection on the curve parameter.
            let ed = &topo.edges[lp[ci].0];
            let ev = CurveEval::new(&ed.curve);
            let (mut lo, mut hi) = (a.t, b.t);
            let f = |t: f64| ga + (wrap_near(param.angle(ev.at(t)), a.u) - a.u);
            for _ in 0..80 {
                let mid = 0.5 * (lo + hi);
                if (f(mid) < 0.0) == (ga < 0.0) {
                    lo = mid;
                } else {
                    hi = mid;
                }
            }
            let t = 0.5 * (lo + hi);
            out.push(Crossing {
                ci,
                t,
                at_start: false,
            });
        }
    }
    out
}

/// A loop that is one closed circle or ellipse whose vertex nothing else
/// uses: its vertex can move anywhere along it.
fn movable(topo: &Topo, lp: &[(usize, bool)]) -> bool {
    if lp.len() != 1 {
        return false;
    }
    let e = &topo.edges[lp[0].0];
    e.closed()
        && !topo.pinned[e.v0]
        && matches!(e.curve, Curve::Circle { .. } | Curve::Ellipse { .. })
        && topo
            .edges
            .iter()
            .filter(|x| x.v0 == e.v0 || x.v1 == e.v0)
            .count()
            == 1
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum End {
    Loop(usize),
    Pole(f64),
}

/// A provisional frame for a curved face (x is fixed later).
fn base_param(topo: &Topo, f: usize) -> Param {
    let face = &topo.faces[f];
    let verts: Vec<V> = face
        .loops
        .iter()
        .flatten()
        .map(|&c| topo.verts[topo.start(c)])
        .collect();
    match face.surf {
        Surf::Plane { o, n } => {
            let p0 = verts.first().copied().unwrap_or(o);
            let origin = p0 - n * (p0 - o).dot(n);
            Param::new(face.surf, origin, n, n.perp(), 0.0)
        }
        Surf::Cyl { o, a, .. } => {
            let p0 = verts.first().copied().unwrap_or(o);
            Param::new(face.surf, o + a * (p0 - o).dot(a), a, a.perp(), 0.0)
        }
        Surf::Cone { apex, a, k } => {
            let t = verts
                .iter()
                .map(|&p| (p - apex).dot(a))
                .fold(0.0f64, f64::max);
            let t = if t > 0.0 { t } else { 1.0 };
            Param::new(face.surf, apex + a * t, a, a.perp(), k * t)
        }
        Surf::Sphere { c, r } => {
            // The normal shared by most of the bounding circles, so that
            // they are parallels (lines of constant v).
            let mut counts: Vec<(V, usize)> = Vec::new();
            for &(e, _) in face.loops.iter().flatten() {
                if let Curve::Circle { normal, .. } = topo.edges[e].curve {
                    let mut n = V::from(normal);
                    let arr = n.arr();
                    let big = (0..3)
                        .max_by(|&i, &j| arr[i].abs().total_cmp(&arr[j].abs()))
                        .expect("axis");
                    if arr[big] < 0.0 {
                        n = -n;
                    }
                    match counts.iter_mut().find(|(m, _)| m.dot(n) > 1.0 - 1e-9) {
                        Some(x) => x.1 += 1,
                        None => counts.push((n, 1)),
                    }
                }
            }
            // Most shared first; equal counts keep their order of first
            // use, so the choice is deterministic.
            let mut cands: Vec<V> = Vec::new();
            for k in (1..=counts.iter().map(|x| x.1).max().unwrap_or(0)).rev() {
                cands.extend(counts.iter().filter(|x| x.1 == k).map(|x| x.0));
            }
            cands.push(v(0.0, 0.0, 1.0));
            // Then axes no model aligns with by accident.
            for d in [
                [1.0, 2.0, 3.0],
                [3.0, -1.0, 2.0],
                [-2.0, 3.0, 1.0],
                [1.0, -3.0, -2.0],
            ] {
                cands.push(V::from(d).norm());
            }
            let z = cands
                .iter()
                .copied()
                .find(|&z| clear_of_poles(topo, face, c, r, z))
                .unwrap_or(cands[0]);
            Param::new(face.surf, c, z, z.perp(), 0.0)
        }
        Surf::Torus { .. } => torus_param(topo, f).0,
    }
}

/// Whether no boundary of `face` (on the sphere `c`, `r`) comes near the
/// poles of axis `z`, except along parallels. At a pole `u` is undefined:
/// an edge through it jumps by π in parameter space, and a seam from that
/// pole runs along the boundary. Three quarter circles bounding a sphere
/// cut by an octant corner (`rotate-parameters.scad`) pass through the
/// poles of every one of their own normals.
fn clear_of_poles(topo: &Topo, face: &crate::topo::TFace, c: V, r: f64, z: V) -> bool {
    // About 0.6 degrees.
    let min_sin = 0.01;
    face.loops.iter().flatten().all(|&(e, _)| {
        let ed = &topo.edges[e];
        if let Curve::Circle { normal, .. } = ed.curve
            && V::from(normal).cross(z).len() < 1e-9
        {
            // A parallel: constant v, whatever its latitude.
            return true;
        }
        // Samples 0.005 r apart, so that a curve through a pole has one
        // within the limit.
        let len: f64 = curve::sample(&ed.curve, ed.range, 16)
            .windows(2)
            .map(|w| (w[1] - w[0]).len())
            .sum();
        let n = ((1.1 * len / (0.005 * r)).ceil() as usize).clamp(8, 4096);
        curve::sample(&ed.curve, ed.range, n)
            .iter()
            .all(|&p| ((p - c) * (1.0 / r)).cross(z).len() > min_sin)
    })
}

/// Rotates the frame's x axis by `u0` about z (a swapped torus's tube
/// angle instead; see [`Param::rotated`]).
fn rotated(p: &Param, u0: f64) -> Param {
    p.rotated(u0)
}

/// The middle of the widest gap that angle intervals leave on the circle,
/// or `None` when they cover all of it. Each interval is shorter than π.
fn widest_gap(intervals: &[(f64, f64)]) -> Option<f64> {
    let mut iv: Vec<(f64, f64)> = Vec::with_capacity(intervals.len() + 4);
    for &(lo, hi) in intervals {
        let l = lo - TAU * (lo / TAU).floor();
        let h = l + (hi - lo);
        iv.push((l, h));
        if h > TAU {
            iv.push((l - TAU, h - TAU));
        }
    }
    if iv.is_empty() {
        return None;
    }
    iv.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.total_cmp(&b.1)));
    let first = iv[0].0;
    let mut end = iv[0].1;
    let mut best = (0.0f64, 0.0f64);
    for &(l, h) in &iv[1..] {
        if l > end && l - end > best.0 {
            best = (l - end, 0.5 * (l + end));
        }
        end = end.max(h);
    }
    let wrap = first + TAU - end;
    if wrap > best.0 {
        best = (wrap, end + 0.5 * wrap);
    }
    (best.0 > 1e-9).then_some(best.1)
}

/// A torus face's frame. Both of a torus's coordinates are angles, so
/// each needs its cut (where the angle jumps by 2π) outside the face, or
/// a seam where it cannot be: which one is decided from the angles the
/// face's mesh triangles cover. Returns the frame and whether the face
/// wraps both ways (a whole torus, perhaps with holes), which needs two
/// seams.
fn torus_param(topo: &Topo, f: usize) -> (Param, bool) {
    let face = &topo.faces[f];
    let Surf::Torus { c, a, big, .. } = face.surf else {
        unreachable!("a torus face")
    };
    let xr = a.perp();
    let reference = Param::new(face.surf, c, a, xr, 0.0);
    let angles = |p: V| {
        let d = p - c;
        let h = d.dot(reference.z);
        let w = d.reject(reference.z);
        (
            atan2(w.dot(reference.y), w.dot(reference.x)),
            atan2(h, w.len() - big),
        )
    };
    let mut phi = Vec::with_capacity(face.tris.len());
    let mut theta = Vec::with_capacity(face.tris.len());
    for t in &face.tris {
        let q = t.map(angles);
        let span = |k: usize| {
            let pick = |p: (f64, f64)| if k == 0 { p.0 } else { p.1 };
            let a0 = pick(q[0]);
            let (mut lo, mut hi) = (a0, a0);
            for &p in &q[1..] {
                let x = wrap_near(pick(p), a0);
                lo = lo.min(x);
                hi = hi.max(x);
            }
            (lo, hi)
        };
        phi.push(span(0));
        theta.push(span(1));
    }
    let gp = widest_gap(&phi);
    let gt = widest_gap(&theta);
    let x = match gp {
        // The axis-angle cut (±π) in the middle of the gap.
        Some(g) => reference.x * cos(g + PI) + reference.y * sin(g + PI),
        None => xr,
    };
    let mut p = Param::new(face.surf, c, a, x, 0.0);
    p.t0 = gt.map_or(0.0, |g| g + PI);
    p.swap = gp.is_some() && gt.is_none();
    (p, gp.is_none() && gt.is_none())
}

/// Seams a torus face that wraps both ways: a meridian and a parallel
/// through one new vertex, both clear of the face's loops (which must
/// then be holes), joined into one loop around the parameter square.
fn double_seam(topo: &mut Topo, f: usize, param: Param) -> Result<(), Error> {
    let Surf::Torus { c, big, r, .. } = param.s else {
        unreachable!("a torus face")
    };
    let loops = topo.faces[f].loops.clone();
    // Each loop's continuous (axis, tube) angle ranges.
    let mut ranges: Vec<((f64, f64), (f64, f64))> = Vec::with_capacity(loops.len());
    for lp in &loops {
        let mut prev: Option<(f64, f64)> = None;
        let (mut first, mut last) = ((0.0, 0.0), (0.0, 0.0));
        let mut lo = (f64::INFINITY, f64::INFINITY);
        let mut hi = (f64::NEG_INFINITY, f64::NEG_INFINITY);
        for &(e, fwd) in lp {
            let ed = &topo.edges[e];
            let n = curve::sample_count(&ed.curve, ed.range);
            for (p, _) in coedge_points(topo, e, fwd, n) {
                let (u, w) = param.uv(p);
                let q = match prev {
                    None => {
                        first = (u, w);
                        (u, w)
                    }
                    Some((pu, pw)) => (wrap_near(u, pu), wrap_near(w, pw)),
                };
                lo = (lo.0.min(q.0), lo.1.min(q.1));
                hi = (hi.0.max(q.0), hi.1.max(q.1));
                last = q;
                prev = Some(q);
            }
        }
        if ((last.0 - first.0) / TAU).round() != 0.0 || ((last.1 - first.1) / TAU).round() != 0.0 {
            return Err(Error::Reconstruction(format!(
                "face {f} on a torus: a loop winds about the axis or the tube of a face that wraps both ways"
            )));
        }
        ranges.push((lo, hi));
    }
    let clear = |x: f64, k: usize| {
        ranges.iter().all(|r| {
            let (lo, hi) = if k == 0 {
                (r.0.0, r.1.0)
            } else {
                (r.0.1, r.1.1)
            };
            let m = 1e-6;
            // The first copy of x at or above lo must lie beyond hi.
            let j = ((lo - m - x) / TAU).ceil();
            x + j * TAU > hi + m
        })
    };
    let pick = |k: usize| {
        (0..256)
            .map(|i| TAU * (i as f64 + 0.5) / 256.0 - PI)
            .find(|&x| clear(x, k))
    };
    let (Some(u0), Some(v0)) = (pick(0), pick(1)) else {
        return Err(Error::Reconstruction(format!(
            "face {f} on a torus: no meridian or parallel for a seam misses the face's holes"
        )));
    };
    let mut fr = rotated(&param, u0);
    // Tube angles of the face then lie in (v0, v0 + 2π].
    fr.t0 = v0 + PI;
    let x = fr.x;
    let p0 = fr.eval(0.0, v0);
    let vx = topo.add_vertex(p0);
    topo.pinned[vx] = true;
    let meridian = topo.edges.len();
    topo.edges.push(TEdge {
        v0: vx,
        v1: vx,
        curve: Curve::Circle {
            center: (c + x * big).arr(),
            normal: x.cross(fr.z).arr(),
            x_axis: x.arr(),
            radius: r,
        },
        range: [v0, v0 + TAU],
        faces: [f, f],
        chain: Vec::new(),
        seam: true,
        dev: 0.0,
    });
    let parallel = topo.edges.len();
    topo.edges.push(TEdge {
        v0: vx,
        v1: vx,
        curve: Curve::Circle {
            center: (c + fr.z * (r * sin(v0))).arr(),
            normal: fr.z.arr(),
            x_axis: x.arr(),
            radius: big + r * cos(v0),
        },
        range: [0.0, TAU],
        faces: [f, f],
        chain: Vec::new(),
        seam: true,
        dev: 0.0,
    });
    // Counter-clockwise around [0, 2π] x [v0, v0 + 2π] for a face whose
    // normal is the torus's own, clockwise otherwise.
    let outer = if topo.faces[f].same_sense {
        vec![
            (parallel, true),
            (meridian, true),
            (parallel, false),
            (meridian, false),
        ]
    } else {
        vec![
            (meridian, true),
            (parallel, true),
            (meridian, false),
            (parallel, false),
        ]
    };
    let mut new_loops = vec![outer];
    new_loops.extend(loops);
    topo.faces[f].loops = new_loops;
    topo.faces[f].param = Some(fr);
    Ok(())
}

/// The parameter-space curves of the loop [`double_seam`] made, in edge
/// direction: the parallel along `v0` or `v0 + 2π`, the meridian along
/// `u = 0` or `2π`.
fn double_seam_pcurves(
    topo: &Topo,
    param: &Param,
    lp: &[(usize, bool)],
    same: bool,
) -> Vec<BSpline<2>> {
    let v0 = param.t0 - PI;
    lp.iter()
        .map(|&(e, fwd)| {
            let ed = &topo.edges[e];
            let is_parallel = matches!(ed.curve, Curve::Circle { normal, .. }
                if V::from(normal).cross(param.z).len() < 1e-9);
            if is_parallel {
                // A same-sense loop runs the parallel forward along the
                // bottom (v0) and back along the top; the other sense the
                // opposite way round.
                let w = if fwd == same { v0 } else { v0 + TAU };
                bspline::interpolate_at(&[[0.0, w], [TAU, w]], &ed.range)
            } else {
                let u = if fwd == same { TAU } else { 0.0 };
                bspline::interpolate_at(&[[u, v0], [u, v0 + TAU]], &ed.range)
            }
        })
        .collect()
}

/// Whether a loop is the one [`double_seam`] made.
fn is_double_seam_loop(topo: &Topo, param: &Param, lp: &[(usize, bool)]) -> bool {
    matches!(param.s, Surf::Torus { .. })
        && !param.swap
        && lp.len() == 4
        && lp.iter().all(|&(e, _)| topo.edges[e].seam)
}

/// Chooses each face's frame and inserts seams (pass 1).
fn frames_and_seams(topo: &mut Topo, scale: f64) -> Result<(), Error> {
    let tol = 1e-9 * scale;
    for f in 0..topo.faces.len() {
        let (param, double) = match topo.faces[f].surf {
            Surf::Torus { .. } => torus_param(topo, f),
            _ => (base_param(topo, f), false),
        };
        if !param.periodic() {
            topo.faces[f].param = Some(param);
            continue;
        }
        if double {
            double_seam(topo, f, param)?;
            continue;
        }
        let s = if param.sense(topo.faces[f].same_sense) {
            1
        } else {
            -1
        };
        let loops = topo.faces[f].loops.clone();
        let lifts: Vec<Vec<Vec<S>>> = loops
            .iter()
            .map(|lp| lift_loop(topo, &param, lp, None, tol))
            .collect();
        let mut bottoms = Vec::new();
        let mut tops = Vec::new();
        for (i, l) in lifts.iter().enumerate() {
            match s * winding(l) {
                0 => {}
                1 => bottoms.push(i),
                -1 => tops.push(i),
                w => {
                    return Err(Error::Reconstruction(format!(
                        "face {f}: a loop winds {w} times about the axis"
                    )));
                }
            }
        }
        let (south, north) = param.poles();
        let any_outer = lifts.iter().any(|l| s as f64 * area(l) > 0.0);
        let is_sphere = matches!(param.s, Surf::Sphere { .. });
        let ends: Option<(End, End)> = match (bottoms.len(), tops.len()) {
            (1, 1) => Some((End::Loop(bottoms[0]), End::Loop(tops[0]))),
            (1, 0) if is_sphere => Some((End::Loop(bottoms[0]), End::Pole(north.expect("pole")))),
            (0, 1) if south.is_some() => {
                Some((End::Pole(south.expect("pole")), End::Loop(tops[0])))
            }
            (0, 0) if !any_outer && is_sphere => Some((
                End::Pole(south.expect("pole")),
                End::Pole(north.expect("pole")),
            )),
            (0, 0) if any_outer => None,
            (b, t) => {
                return Err(Error::Reconstruction(format!(
                    "face {f} on a {}: unsupported topology ({b} loops wind up, {t} down)",
                    param.s.to_public().kind()
                )));
            }
        };
        let Some((bot, top)) = ends else {
            // No seam: start u at the outer loop's smallest angle.
            let outer = lifts
                .iter()
                .find(|l| s as f64 * area(l) > 0.0)
                .expect("outer loop");
            let umin = outer
                .iter()
                .flatten()
                .map(|x| x.u)
                .fold(f64::INFINITY, f64::min);
            topo.faces[f].param = Some(rotated(&param, umin));
            continue;
        };
        // Candidate seam angles: the vertices of the wrapping loops, then
        // a fixed sweep.
        let mut cands: Vec<f64> = Vec::new();
        for end in [bot, top] {
            if let End::Loop(i) = end {
                for c in &lifts[i] {
                    cands.push(c[0].u);
                }
            }
        }
        for k in 0..64 {
            cands.push(TAU * (k as f64 + 0.5) / 64.0);
        }
        let mut best: Option<(usize, f64, Vec<Option<Crossing>>)> = None;
        'cand: for &u0 in &cands {
            let mut chosen: Vec<Option<Crossing>> = vec![None; loops.len()];
            let mut cost = 0;
            for (i, lp) in loops.iter().enumerate() {
                let xs = crossings(topo, &param, lp, &lifts[i], u0);
                let wraps = End::Loop(i) == bot || End::Loop(i) == top;
                if wraps {
                    if xs.len() != 1 {
                        continue 'cand;
                    }
                    if !xs[0].at_start && !movable(topo, lp) {
                        cost += 1;
                    }
                    chosen[i] = Some(xs[0]);
                } else if !xs.is_empty() {
                    continue 'cand;
                }
            }
            if best.as_ref().is_none_or(|b| cost < b.0) {
                best = Some((cost, u0, chosen));
                if cost == 0 {
                    break;
                }
            }
        }
        let Some((_, u0, chosen)) = best else {
            return Err(Error::Reconstruction(format!(
                "face {f}: no meridian for a seam misses the face's holes"
            )));
        };
        // Put a vertex where each wrapping loop crosses the seam.
        let end_vertex = |end: End, topo: &mut Topo| -> usize {
            match end {
                End::Pole(w) => {
                    let p = param.eval(0.0, w);
                    let i = topo.add_vertex(p);
                    topo.pinned[i] = true;
                    i
                }
                End::Loop(i) => {
                    let x = chosen[i].expect("crossing");
                    let (e, fwd) = loops[i][x.ci];
                    let vi = if x.at_start {
                        topo.start((e, fwd))
                    } else if movable(topo, &loops[i]) {
                        topo.reseat_closed(e, x.t);
                        topo.edges[e].v0
                    } else {
                        let p = curve::eval(&topo.edges[e].curve, x.t);
                        topo.split_edge(e, x.t, p)
                    };
                    topo.pinned[vi] = true;
                    vi
                }
            }
        };
        let vb = end_vertex(bot, topo);
        let vt = end_vertex(top, topo);
        let fr = rotated(&param, u0);
        let (pb, pt) = (topo.verts[vb], topo.verts[vt]);
        let (curve, range) = match fr.s {
            Surf::Sphere { c, r } => {
                let d = fr.x;
                let n = d.cross(fr.z);
                let (wb, wt) = (fr.uv(pb).1, fr.uv(pt).1);
                (
                    Curve::Circle {
                        center: c.arr(),
                        normal: n.arr(),
                        x_axis: d.arr(),
                        radius: r,
                    },
                    [wb, wt],
                )
            }
            // A torus: the meridian at u = 0 (the circle's parameter is
            // the tube angle), or for a swapped one the parallel at the
            // internal u = 0 (its parameter the axis angle).
            Surf::Torus { c, big, r, .. } if !fr.swap => {
                let d = fr.x;
                let (wb, wt) = (fr.uv(pb).1, fr.uv(pt).1);
                (
                    Curve::Circle {
                        center: (c + d * big).arr(),
                        normal: d.cross(fr.z).arr(),
                        x_axis: d.arr(),
                        radius: r,
                    },
                    [wb, wt],
                )
            }
            Surf::Torus { c, big, r, .. } => {
                let (wb, wt) = (fr.uv(pb).1, fr.uv(pt).1);
                (
                    Curve::Circle {
                        center: (c + fr.z * (r * sin(fr.t0))).arr(),
                        normal: fr.z.arr(),
                        x_axis: fr.x.arr(),
                        radius: big + r * cos(fr.t0),
                    },
                    [wb, wt],
                )
            }
            _ => {
                let l = (pt - pb).len();
                (
                    Curve::Line {
                        origin: pb.arr(),
                        direction: (pt - pb).norm().arr(),
                    },
                    [0.0, l],
                )
            }
        };
        let seam = topo.edges.len();
        topo.edges.push(TEdge {
            v0: vb,
            v1: vt,
            curve,
            range,
            faces: [f, f],
            chain: Vec::new(),
            seam: true,
            dev: 0.0,
        });
        // The loops after splitting; rotate each end loop to start at its
        // seam vertex, then join them with the seam into one loop.
        let cur = topo.faces[f].loops.clone();
        let rotate_to = |lp: &[(usize, bool)], vx: usize, topo: &Topo| -> Vec<(usize, bool)> {
            let k = lp
                .iter()
                .position(|&c| topo.start(c) == vx)
                .expect("seam vertex on its loop");
            lp[k..].iter().chain(&lp[..k]).copied().collect()
        };
        let mut merged = Vec::new();
        if let End::Loop(i) = bot {
            merged.extend(rotate_to(&cur[i], vb, topo));
        }
        merged.push((seam, true));
        if let End::Loop(i) = top {
            merged.extend(rotate_to(&cur[i], vt, topo));
        }
        merged.push((seam, false));
        let mut new_loops = vec![merged];
        for (i, lp) in cur.into_iter().enumerate() {
            if End::Loop(i) != bot && End::Loop(i) != top {
                new_loops.push(lp);
            }
        }
        topo.faces[f].loops = new_loops;
        topo.faces[f].param = Some(fr);
    }
    Ok(())
}

/// Builds the parameter-space curve of one coedge (pass 2): a segment
/// when the edge is an iso-line of the frame, else a cubic refined until
/// its image lies within `fit_tol` of the edge's other surface.
#[allow(clippy::too_many_arguments)]
fn pcurve(
    topo: &Topo,
    param: &Param,
    other: Option<Surf>,
    e: usize,
    fwd: bool,
    start_u: f64,
    tol: f64,
    fit_tol: f64,
) -> (BSpline<2>, f64, f64) {
    let ed = &topo.edges[e];
    // Start coarse and double: the samples become the fit's control
    // points, so starting from the edge's own (dense) sampling would write
    // far more of them than the tolerance needs.
    let mut n = match ed.curve {
        Curve::Line { .. } => 4,
        _ => 16,
    };
    loop {
        let pts = coedge_points(topo, e, fwd, n);
        let mut s = lift(param, &pts, Some(start_u), tol);
        let end_u = s.last().expect("samples").u;
        if !fwd {
            s.reverse();
        }
        let uv: Vec<[f64; 2]> = s.iter().map(|x| [x.u, x.v]).collect();
        // Parametrised like the edge's 3D curve (see
        // `bspline::interpolate_at`).
        let ts: Vec<f64> = s.iter().map(|x| x.t).collect();
        let (a, b) = (uv[0], uv[uv.len() - 1]);
        let iso_u = uv.iter().all(|q| (q[0] - a[0]).abs() < 1e-12);
        let iso_v = uv
            .iter()
            .all(|q| (q[1] - a[1]).abs() < 1e-12 * (1.0 + a[1].abs()));
        let bs = if (iso_u || iso_v) && linear_in(&uv, &ts) {
            bspline::interpolate_at(&[a, b], &[ts[0], ts[ts.len() - 1]])
        } else {
            bspline::interpolate_at(&uv, &ts)
        };
        let dev = match other {
            Some(o) => {
                let m = (4 * uv.len()).min(16384);
                let [t0, t1] = [bs.knots[0], *bs.knots.last().expect("knots")];
                (0..=m)
                    .map(|i| {
                        let q = bspline::eval(&bs, t0 + (t1 - t0) * i as f64 / m as f64);
                        o.f(param.eval(q[0], q[1])).abs()
                    })
                    .fold(0.0, f64::max)
            }
            None => 0.0,
        };
        if dev < fit_tol || n >= 4096 {
            return (bs, end_u, dev);
        }
        n *= 2;
    }
}

/// Whether the points are the segment from the first to the last,
/// traversed linearly in the curve parameter `ts` (to rounding), so that a
/// degree-1 curve on `ts`' range is exact and shares the edge's parameter.
fn linear_in(uv: &[[f64; 2]], ts: &[f64]) -> bool {
    let (a, b) = (uv[0], uv[uv.len() - 1]);
    let (t0, t1) = (ts[0], ts[ts.len() - 1]);
    let l = ((b[0] - a[0]).powi(2) + (b[1] - a[1]).powi(2)).sqrt();
    if t1 <= t0 {
        return false;
    }
    uv.iter().zip(ts).all(|(q, &t)| {
        let f = (t - t0) / (t1 - t0);
        let e = [
            a[0] + (b[0] - a[0]) * f - q[0],
            a[1] + (b[1] - a[1]) * f - q[1],
        ];
        (e[0] * e[0] + e[1] * e[1]).sqrt() < 1e-12 * (1.0 + l)
    })
}

/// Pass 2: parameter-space curves on curved faces, and which loop is
/// outer. Returns the largest pcurve deviation.
fn pcurves(topo: &mut Topo, scale: f64, fit_tol: f64) -> f64 {
    let tol = 1e-9 * scale;
    let mut max_dev = 0.0f64;
    for f in 0..topo.faces.len() {
        let param = topo.faces[f].param.expect("param");
        let s = if param.sense(topo.faces[f].same_sense) {
            1.0
        } else {
            -1.0
        };
        let loops = topo.faces[f].loops.clone();
        let mut all_pc = Vec::with_capacity(loops.len());
        let mut outer = Vec::with_capacity(loops.len());
        for lp in &loops {
            if !param.periodic() {
                let l = lift_loop(topo, &param, lp, None, tol);
                outer.push(s * area(&l) > 0.0);
                all_pc.push(vec![None; lp.len()]);
                continue;
            }
            if is_double_seam_loop(topo, &param, lp) {
                let pcs = double_seam_pcurves(topo, &param, lp, topo.faces[f].same_sense);
                all_pc.push(pcs.into_iter().map(Some).collect());
                outer.push(true);
                continue;
            }
            let seamed = lp.iter().any(|&(e, _)| topo.edges[e].seam);
            // Where the loop starts in u, so that the whole face lies in
            // [0, 2π].
            let start_u = if seamed {
                let fwd_u = if s > 0.0 { TAU } else { 0.0 };
                match lp[0] {
                    (e, true) if topo.edges[e].seam => fwd_u,
                    _ => TAU - fwd_u,
                }
            } else {
                let l = lift_loop(topo, &param, lp, None, tol);
                let pts: Vec<f64> = l.iter().flatten().map(|x| x.u).collect();
                let mean = pts.iter().sum::<f64>() / pts.len() as f64;
                let lo = pts.iter().copied().fold(f64::INFINITY, f64::min);
                let hi = pts.iter().copied().fold(f64::NEG_INFINITY, f64::max);
                // Shift by whole turns so the loop sits inside [0, 2π]
                // (when it spans a full turn, from 0).
                let k = if hi - lo >= TAU - 1e-9 {
                    (lo / TAU).round()
                } else {
                    ((mean - PI) / TAU).round()
                };
                l[0][0].u - k * TAU
            };
            let mut cur_u = start_u;
            let mut pcs = Vec::with_capacity(lp.len());
            for &(e, fwd) in lp {
                let ed = &topo.edges[e];
                if ed.seam {
                    let u = if fwd == (s > 0.0) { TAU } else { 0.0 };
                    let (vb, vt) = (param.uv(topo.verts[ed.v0]).1, param.uv(topo.verts[ed.v1]).1);
                    let (vb, vt) = match param.s {
                        Surf::Sphere { .. } | Surf::Torus { .. } => (ed.range[0], ed.range[1]),
                        _ => (vb, vt),
                    };
                    // v is linear in the seam's own parameter (length
                    // along a line, latitude along a meridian).
                    pcs.push(Some(bspline::interpolate_at(
                        &[[u, vb], [u, vt]],
                        &ed.range,
                    )));
                    cur_u = u;
                    continue;
                }
                let other = {
                    let [a, b] = ed.faces;
                    let o = if a == f { b } else { a };
                    (o != f).then(|| topo.faces[o].surf)
                };
                let (bs, end_u, dev) = pcurve(topo, &param, other, e, fwd, cur_u, tol, fit_tol);
                max_dev = max_dev.max(dev);
                pcs.push(Some(bs));
                cur_u = end_u;
            }
            if seamed {
                outer.push(true);
            } else {
                let l = lift_loop(topo, &param, lp, Some(start_u), tol);
                outer.push(s * area(&l) > 0.0);
            }
            if param.swap {
                // Back to STEP's (axis angle, tube angle).
                for bs in pcs.iter_mut().flatten() {
                    for q in &mut bs.control {
                        *q = param.step_coords(*q);
                    }
                }
            }
            all_pc.push(pcs);
        }
        // The frame as written: STEP's coordinates, whatever the internal
        // ones were.
        let mut written = param;
        written.swap = false;
        topo.faces[f].param = Some(written);
        topo.faces[f].pcurves = all_pc;
        topo.faces[f].outer = outer;
    }
    max_dev
}

/// Frames, seams and parameter-space curves for every face. Returns the
/// largest pcurve deviation.
pub(crate) fn parametrise(topo: &mut Topo, scale: f64, fit_tol: f64) -> Result<f64, Error> {
    frames_and_seams(topo, scale)?;
    Ok(pcurves(topo, scale, fit_tol))
}
