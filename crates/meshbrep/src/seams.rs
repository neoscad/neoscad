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
        let lu = match prev {
            Some(p) => wrap_near(raw_u, p),
            None => raw_u,
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
        Surf::Sphere { c, .. } => {
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
            let mut z = v(0.0, 0.0, 1.0);
            let mut best = 0;
            for (n, k) in counts {
                if k > best {
                    best = k;
                    z = n;
                }
            }
            Param::new(face.surf, c, z, z.perp(), 0.0)
        }
    }
}

/// Rotates the frame's x axis by `u0` about z.
fn rotated(p: &Param, u0: f64) -> Param {
    let x = p.x * cos(u0) + p.y * sin(u0);
    Param::new(p.s, p.o, p.z, x, p.r0)
}

/// Chooses each face's frame and inserts seams (pass 1).
fn frames_and_seams(topo: &mut Topo, scale: f64) -> Result<(), Error> {
    let tol = 1e-9 * scale;
    for f in 0..topo.faces.len() {
        let param = base_param(topo, f);
        if !param.periodic() {
            topo.faces[f].param = Some(param);
            continue;
        }
        let s = if topo.faces[f].same_sense { 1 } else { -1 };
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
                    match param.s {
                        Surf::Cyl { .. } => "cylinder",
                        Surf::Cone { .. } => "cone",
                        _ => "sphere",
                    }
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
        let s = if topo.faces[f].same_sense { 1.0 } else { -1.0 };
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
                        Surf::Sphere { .. } => (ed.range[0], ed.range[1]),
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
            all_pc.push(pcs);
        }
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
