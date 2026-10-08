//! Calls built in two passes (`docs/fillets.md`, section 15.6, stage
//! F5a). Where convex and concave selected edges meet at a vertex, no
//! one pass can round them: the blends of one sense must run on across
//! the blends of the other. The nested rewrite v1 offered (round the
//! concave edges in an inner call, then the convex edges of its result in
//! an outer one) is what one call now does itself:
//!
//! - **The first pass** rounds the selected edges of one sense, concave
//!   first, exactly as a call selecting only those would.
//! - **The second pass** rounds the selected edges of the other sense in
//!   the solid the first made, selected on that solid's own B-rep (an
//!   export render of the child with the first pass's tools applied:
//!   `export_render_traced` with its first pass). Which of its edges are
//!   the call's is decided by provenance, not by the selector again: an
//!   edge counts when it lies on one of the call's selected edges of the
//!   second sense (what the first pass left of it), or when it continues
//!   such an edge smoothly across a blend the first pass made (an
//!   L-bracket's end face: its outline's two lines now meet the inner
//!   blend's arc, which the outline's rounding must follow). Edges the
//!   first pass made that continue nothing (the ellipses where two
//!   mitred blends meet) are not the call's, so they are not rounded and
//!   not reported, where the nested rewrite under `"all"` named them.
//! - **Which order**: concave first; when that cannot be built because
//!   of a vertex (not a size, which is the call's problem with its hint,
//!   so that the shape does not flip with the size), convex first. When
//!   neither order builds, the call is an error naming the pass and the
//!   problem of the concave-first order.
//!
//! A size hint for a two-pass call is planned again at the size it
//! offers ([`verified`]), and offered only when that builds: a smaller
//! size changes both passes, so the second pass's own largest size is
//! only a starting point.

use std::collections::BTreeSet;
use std::sync::Arc;

use eval::dump::Keys;
use eval::fillet::FilletNode;
use eval::node::{Node, NodeKind};
use lang::diag::{DiagCode, Severity};
use meshbrep::Curve;

use super::build::{self, Built};
use super::curve::{V, dot, norm, sub};
use super::{
    Facts, Fix, Names, Plan, PlanDiag, SecondPass, Sense, Status, Unavailable, edge_text, facts,
    plan_at, problem_diag, profile_of, select,
};
use crate::evaluate::{RenderOptions, Renderer};

/// The first pass of a pending plan `p` (one left [`Plan::pending`]) that
/// rounds its selected edges of `sense`, checked: `None` when it cannot
/// be built (the plan then says why).
pub(crate) fn first_alone(f: &FilletNode, p: &Plan, facts: &Facts, sense: Sense) -> Option<Built> {
    let list: Vec<usize> = p
        .pending
        .as_ref()?
        .iter()
        .copied()
        .filter(|&i| facts.edges[i].sense == sense)
        .collect();
    if list.is_empty() {
        return None;
    }
    build::prepare(facts, &list, profile_of(f), f.size).ok()
}

fn other(s: Sense) -> Sense {
    if s == Sense::Convex {
        Sense::Concave
    } else {
        Sense::Convex
    }
}

/// Plans a pending call's two passes: the first order that builds, or
/// the concave-first order's problem.
pub(crate) fn two(
    renderer: &Renderer,
    node: &Node,
    keys: &Keys,
    opts: &RenderOptions,
    f: &FilletNode,
    p: &mut Plan,
    verify: bool,
) {
    let Some(list) = p.pending.clone() else {
        return;
    };
    let Some(f1) = p.facts.clone() else {
        return;
    };
    let mut failure: Option<(Status, PlanDiag)> = None;
    for first in [Sense::Concave, Sense::Convex] {
        // A size the concave-first order cannot take is the call's
        // problem, with its hint: the other order is for vertices only,
        // so that the shape a size gives does not flip with the size.
        if failure
            .as_ref()
            .is_some_and(|(s, _)| matches!(s, Status::TooLarge | Status::Overlap))
        {
            break;
        }
        let second = other(first);
        let l1: Vec<usize> = list
            .iter()
            .copied()
            .filter(|&i| f1.edges[i].sense == first)
            .collect();
        let l2: Vec<usize> = list
            .iter()
            .copied()
            .filter(|&i| f1.edges[i].sense == second)
            .collect();
        let label = |which: &str, sense: Sense| {
            format!(
                "in its {which} pass (the {} edges{})",
                sense.name(),
                if which == "first" {
                    String::new()
                } else {
                    format!(", after the {} ones are rounded", other(sense).name())
                }
            )
        };
        let b1 = match build::prepare(&f1, &l1, profile_of(f), f.size) {
            Ok(b) => b,
            Err(problem) => {
                if failure.is_none() {
                    let names = Names::first(p, &f1, &l1);
                    let pass = label("first", first);
                    failure = Some(problem_diag(f, &f1, &l1, &names, problem, Some(&pass)));
                }
                continue;
            }
        };
        let f2 = match facts(renderer, node, keys, opts, Some(first)) {
            Ok(x) => x,
            Err(Unavailable::Interrupted) => {
                p.status = Status::Interrupted;
                p.pending = None;
                return;
            }
            Err(why) => {
                if failure.is_none() {
                    let why = match why {
                        Unavailable::NoBrep(s) => s,
                        _ => "it is empty".to_string(),
                    };
                    failure = Some((
                        Status::Failed,
                        PlanDiag {
                            severity: Severity::Error,
                            code: DiagCode::FilletFailed,
                            message: format!(
                                "{}() {}: what the first pass made could not be reconstructed as a B-rep to select on: {why}",
                                f.kind.module(),
                                label("second", second)
                            ),
                            hints: vec![
                                "this is a limitation or a bug of NeoSCAD; selecting fewer edges may avoid it"
                                    .into(),
                            ],
                            fix: None,
                        },
                    ));
                }
                continue;
            }
        };
        let (sel, origin) = continued(&f1, &l2, &f2);
        if sel.is_empty() {
            // The first pass left nothing of the other sense to round
            // (its blends took the whole of those edges): one pass.
            p.build = Some(Arc::new(b1));
            p.status = Status::Built;
            p.pending = None;
            return;
        }
        match build::prepare(&f2, &sel, profile_of(f), f.size) {
            Ok(b2) => {
                p.build = Some(Arc::new(b1));
                p.second = Some(SecondPass {
                    first,
                    sense: second,
                    facts: f2,
                    selected: sel,
                    origin,
                    build: Arc::new(b2),
                });
                p.status = Status::Built;
                p.pending = None;
                return;
            }
            Err(problem) => {
                if failure.is_none() {
                    let names = Names {
                        numbers: origin
                            .iter()
                            .map(|o| o.and_then(|i| p.selected.iter().position(|&s| s == i)))
                            .map(|x| x.map(|x| x + 1))
                            .collect(),
                        words: sel.iter().map(|&i| edge_text(&f2.edges[i])).collect(),
                        centers: sel.iter().map(|&i| f2.edges[i].center).collect(),
                    };
                    let pass = label("second", second);
                    failure = Some(problem_diag(f, &f2, &sel, &names, problem, Some(&pass)));
                }
            }
        }
    }
    p.pending = None;
    let Some((status, mut d)) = failure else {
        return;
    };
    // A size that fixes one pass changes the other too: offer it only
    // when the whole call builds at that size. (A plan made to check a
    // size keeps its pass's own offer, the next size to try.)
    if verify && let Some(Fix::Size(x)) = d.fix {
        match verified(renderer, node, keys, opts, x) {
            Some(y) => {
                d.fix = Some(Fix::Size(y));
                d.hints = vec![format!(
                    "use {} = {}: the largest that fits both passes is a little over it",
                    f.kind.size_name(),
                    super::number_text(y)
                )];
            }
            None => {
                d.fix = None;
                d.hints = vec![format!(
                    "use a smaller {}, or select fewer edges",
                    f.kind.size_name()
                )];
            }
        }
    }
    p.status = status;
    p.diags.push(d);
}

/// The size, `x` or less, at which the call `node` builds in full (both
/// passes), tried by planning it again at each: at a size one pass
/// cannot take, the next try is what that pass offers (its own largest
/// size, less 5%), else 10% less, up to [`TRIES`] plans.
fn verified(
    renderer: &Renderer,
    node: &Node,
    keys: &Keys,
    opts: &RenderOptions,
    x: f64,
) -> Option<f64> {
    let mut size = x;
    for _ in 0..TRIES {
        let mut n = node.clone();
        if let NodeKind::Fillet(f) = &mut n.kind {
            f.size = size;
        }
        let p = plan_at(renderer, &n, keys, opts, false)?;
        if p.status == Status::Built {
            return Some(size);
        }
        if p.status == Status::Interrupted {
            return None;
        }
        let offered = p.diags.iter().find_map(|d| match d.fix {
            Some(Fix::Size(y)) if y > 0.0 && y < size => Some(y),
            _ => None,
        });
        size = match offered {
            Some(y) => y,
            None => {
                // Three significant digits, as the hints write them.
                let next = 0.9 * size;
                let e = libm::floor(libm::log10(next)) as i32 - 2;
                let q = libm::pow(10.0, f64::from(e));
                format!("{:.*}", (-e).max(0) as usize, (next / q).floor() * q)
                    .parse::<f64>()
                    .unwrap_or(next)
            }
        };
        if size <= 0.0 {
            return None;
        }
    }
    None
}

/// Plans [`verified`] makes at most: each is a reconstruction of what
/// the first pass makes at that size.
const TRIES: usize = 6;

/// Whether `p` lies on B-rep edge `e` of `f` (its curve, within its
/// extent), within `tol`.
fn on_edge(f: &Facts, e: &super::EdgeFact, p: V, tol: f64) -> bool {
    let be = &f.brep.edges[e.brep_edge as usize];
    match &be.curve {
        Curve::Line { .. } => {
            let (a, b) = (e.from, e.to);
            let d = sub(b, a);
            let dd = dot(d, d);
            if dd <= 0.0 {
                return false;
            }
            let t = dot(sub(p, a), d) / dd;
            let l = dd.sqrt();
            if t * l < -tol || (t - 1.0) * l > tol {
                return false;
            }
            let q = [a[0] + d[0] * t, a[1] + d[1] * t, a[2] + d[2] * t];
            norm(sub(p, q)) <= tol
        }
        Curve::Circle { .. } => {
            let Some((c, axis, sweep, radius)) = build::arc_of(be) else {
                return false;
            };
            let v = sub(p, c);
            let h = dot(v, axis);
            let r = sub(v, super::curve::mul(axis, h));
            let rho = norm(r);
            if h.abs() > tol || (rho - radius).abs() > tol {
                return false;
            }
            if sweep >= std::f64::consts::TAU * (1.0 - 1e-12) || e.closed {
                return true;
            }
            // Within the arc: its angle from the start, about the axis it
            // turns counter-clockwise about.
            let s = sub(e.from, c);
            let s = sub(s, super::curve::mul(axis, dot(s, axis)));
            let x = dot(super::curve::cross(s, r), axis);
            let mut th = libm::atan2(x, dot(s, r));
            if th < 0.0 {
                th += std::f64::consts::TAU;
            }
            let slack = tol / radius.max(tol);
            th <= sweep + slack || th >= std::f64::consts::TAU - slack
        }
        _ => false,
    }
}

/// The second pass's edges in `f2` (the child with the first pass's
/// blends): those of the second sense that lie on one of the call's
/// edges `l2` of `f1` (indices into its edges), and, repeatedly, those of
/// the second sense beside a first-pass blend that continue a chosen edge
/// smoothly at a vertex, across a face they share. With each, the edge
/// of `l2` it lies on, if any. Both lists in `f2`'s canonical order.
fn continued(f1: &Facts, l2: &[usize], f2: &Facts) -> (Vec<usize>, Vec<Option<usize>>) {
    let Some(&first) = l2.first() else {
        return (Vec::new(), Vec::new());
    };
    let sense = f1.edges[first].sense;
    // Positions in the two B-reps agree to the first one's tolerance (the
    // same exact surfaces, reconstructed twice).
    let tol = 10.0 * f1.tolerance.max(f2.tolerance);
    let mut origin: Vec<Option<Option<usize>>> = vec![None; f2.edges.len()];
    for (j, e2) in f2.edges.iter().enumerate() {
        if e2.skip.is_some() || e2.sense != sense || !select::supported(e2) {
            continue;
        }
        if let Some(&i) = l2.iter().find(|&&i| {
            let e1 = &f1.edges[i];
            e1.curve == e2.curve && e2.path.iter().all(|&q| on_edge(f1, e1, q, tol))
        }) {
            origin[j] = Some(Some(i));
        }
    }
    let b = &*f2.brep;
    loop {
        let mut grew = false;
        for (j, e2) in f2.edges.iter().enumerate() {
            if origin[j].is_some()
                || e2.skip.is_some()
                || e2.sense != sense
                || !select::supported(e2)
                || !(e2.own[0] || e2.own[1])
            {
                continue;
            }
            let faces: BTreeSet<u32> = e2.brep_faces.iter().copied().collect();
            let joins = e2.vertices.iter().any(|&v| {
                f2.edges.iter().enumerate().any(|(k, x)| {
                    origin[k].is_some()
                        && x.vertices.contains(&v)
                        && x.brep_faces.iter().any(|fa| faces.contains(fa))
                        && dot(
                            build::leaving(b, e2.brep_edge, v),
                            build::leaving(b, x.brep_edge, v),
                        ) < -1.0 + 1e-6
                })
            });
            if joins {
                origin[j] = Some(None);
                grew = true;
            }
        }
        if !grew {
            break;
        }
    }
    let mut sel = Vec::new();
    let mut from = Vec::new();
    for (j, o) in origin.into_iter().enumerate() {
        if let Some(o) = o {
            sel.push(j);
            from.push(o);
        }
    }
    (sel, from)
}
