//! Which edges a selector matches (`docs/fillets.md`, section 5.2), on the
//! facts of a fillet's child.
//!
//! Two universes, so that only an atom that names an edge can report it
//! skipped: `all` and `not` range over the *selectable* edges (never a
//! polygon seam, a tangent edge or an edge of a faceted region), so
//! `"all"` and `"not >z"` are quiet; every other atom tests every edge,
//! so `"|z"` on a `$fn = 24` cylinder names its 24 side seams, which the
//! plan then reports as skipped rather than silently ignoring the
//! request. Positional atoms (`>z`, `>>z[i]`) rank every edge, as
//! CadQuery ranks every object it is given.

use eval::fillet::FilletNode;
use eval::fillet::selector::{Atom, Curve as CurveKind, Dir, Expr, Item, Selector};

use super::curve::{cross, dot, norm, unit};
use super::{Class, EdgeFact, Facts, Sense};

/// `sin` of the largest angle between directions that are "parallel"
/// (section 5.2: within 1e-9 rad).
const PARALLEL: f64 = 1e-9;

/// The edges `sel` matches, as a flag per entry of `facts.edges`.
pub(crate) fn matches(facts: &Facts, sel: &Selector, node: &FilletNode) -> Vec<bool> {
    let n = facts.edges.len();
    let mut out = vec![false; n];
    for item in &sel.items {
        let m = match item {
            Item::Expr(e) => expr(facts, e, node),
            Item::Descriptor(d) => descriptor(facts, *d),
        };
        for (o, x) in out.iter_mut().zip(m) {
            *o |= x;
        }
    }
    out
}

fn expr(facts: &Facts, e: &Expr, node: &FilletNode) -> Vec<bool> {
    match e {
        Expr::Atom(a) => atom(facts, a, node),
        Expr::Not(x) => {
            let m = expr(facts, x, node);
            facts
                .edges
                .iter()
                .zip(m)
                .map(|(f, x)| f.skip.is_none() && !x)
                .collect()
        }
        Expr::And(a, b) => zip(expr(facts, a, node), expr(facts, b, node), |x, y| x && y),
        Expr::Or(a, b) => zip(expr(facts, a, node), expr(facts, b, node), |x, y| x || y),
        Expr::Exc(a, b) => zip(expr(facts, a, node), expr(facts, b, node), |x, y| x && !y),
    }
}

fn zip(a: Vec<bool>, b: Vec<bool>, f: impl Fn(bool, bool) -> bool) -> Vec<bool> {
    a.into_iter().zip(b).map(|(x, y)| f(x, y)).collect()
}

fn each(facts: &Facts, f: impl Fn(&EdgeFact) -> bool) -> Vec<bool> {
    facts.edges.iter().map(f).collect()
}

fn parallel(a: [f64; 3], b: [f64; 3]) -> bool {
    norm(cross(unit(a), unit(b))) <= PARALLEL
}

fn perpendicular(a: [f64; 3], b: [f64; 3]) -> bool {
    dot(unit(a), unit(b)).abs() <= PARALLEL
}

fn atom(facts: &Facts, a: &Atom, node: &FilletNode) -> Vec<bool> {
    let tol = facts.tolerance;
    match a {
        Atom::All => each(facts, |e| e.skip.is_none()),
        Atom::Convex => each(facts, |e| e.sense == Sense::Convex),
        Atom::Concave => each(facts, |e| e.sense == Sense::Concave),
        Atom::Curve(c) => each(facts, |e| e.curve == *c),
        Atom::Parallel(d) => {
            let d = d.vector();
            each(facts, |e| e.direction.is_some_and(|v| parallel(v, d)))
        }
        Atom::Perpendicular(d) => {
            let d = d.vector();
            each(facts, |e| match (e.curve, e.direction, e.axis) {
                (CurveKind::Line, Some(v), _) => perpendicular(v, d),
                (CurveKind::Circle, _, Some(n)) => parallel(n, d),
                _ => false,
            })
        }
        Atom::Farthest { max, dir } => nth(facts, *dir, *max, -1),
        Atom::Nth { max, dir, index } => nth(facts, *dir, *max, *index),
        Atom::New => each(facts, |e| {
            !e.leaves[0].iter().any(|l| e.leaves[1].contains(l))
        }),
        Atom::Child(i, None) => each(facts, |e| {
            e.children[0].contains(i) || e.children[1].contains(i)
        }),
        // Where child i meets child j: one face is i's and not j's, the
        // other j's and not i's. A face merged from coplanar faces of both
        // (the shared side of two flush blocks) has both, so the edges
        // around it, which either child has on its own, do not count:
        // with "a face of each" an L-bracket's two flush legs matched all
        // fifteen of its edges instead of the one inner corner.
        Atom::Child(i, Some(j)) if i == j => each(facts, |e| {
            e.children[0].contains(i) && e.children[1].contains(i)
        }),
        Atom::Child(i, Some(j)) => each(facts, |e| {
            let [a, b] = &e.children;
            let meets = |x: &u32, y: &u32| {
                a.contains(x) && !a.contains(y) && b.contains(y) && !b.contains(x)
            };
            meets(i, j) || meets(j, i)
        }),
        Atom::Part(name) => {
            let within = |p: &String| {
                p == name
                    || p.strip_prefix(name.as_str())
                        .is_some_and(|r| r.starts_with('.'))
            };
            each(facts, |e| e.parts.iter().flatten().any(within))
        }
        Atom::Anchor(name) => {
            // Resolved by the evaluator; a missing one was an error there,
            // and the node never reached geometry as a fillet.
            let Some(a) = node.anchor(name) else {
                return vec![false; facts.edges.len()];
            };
            let p = a.point;
            let dir = a.dir;
            each(facts, |e| {
                let through = distance_to(e, p) <= tol;
                let along = match dir {
                    None => true,
                    Some(d) => match (e.direction, e.axis) {
                        (Some(v), _) => parallel(v, d),
                        (None, Some(n)) => parallel(n, d),
                        _ => false,
                    },
                };
                through && along
            })
        }
        Atom::Box { min, max } => each(facts, |e| {
            e.path
                .iter()
                .all(|q| (0..3).all(|k| q[k] >= min[k] - tol && q[k] <= max[k] + tol))
        }),
    }
}

/// How far `p` is from the edge, measured on its drawn path (exact for
/// lines; for curves within the path's sagitta, far below any anchor's
/// use).
fn distance_to(e: &EdgeFact, p: [f64; 3]) -> f64 {
    e.path
        .windows(2)
        .map(|w| segment_distance(w[0], w[1], p))
        .fold(f64::INFINITY, f64::min)
}

fn segment_distance(a: [f64; 3], b: [f64; 3], p: [f64; 3]) -> f64 {
    let ab = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
    let ap = [p[0] - a[0], p[1] - a[1], p[2] - a[2]];
    let l2 = dot(ab, ab);
    let t = if l2 > 0.0 {
        (dot(ap, ab) / l2).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let q = [a[0] + ab[0] * t, a[1] + ab[1] * t, a[2] + ab[2] * t];
    norm([p[0] - q[0], p[1] - q[1], p[2] - q[2]])
}

/// CadQuery's `CenterNthSelector` (`cadquery/selectors.py`, `_NthSelector`,
/// `master`, retrieved 2026-10-08): every edge's centre projected on the
/// direction, sorted ascending and clustered (a cluster takes keys within
/// the tolerance of its first); `<` and `<<` reverse the clusters; then
/// cluster `index`, negative from the end. So `>z` (index -1) is the top
/// group, `>>z[0]` the bottom one and `<<z[0]` the top one, as in
/// CadQuery. An index past the end matches nothing (CadQuery raises).
fn nth(facts: &Facts, dir: Dir, max: bool, index: i64) -> Vec<bool> {
    let d = unit(dir.vector());
    let mut keyed: Vec<(f64, usize)> = facts
        .edges
        .iter()
        .enumerate()
        .map(|(i, e)| (dot(e.center, d), i))
        .collect();
    keyed.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
    let mut clusters: Vec<Vec<usize>> = Vec::new();
    let mut start = f64::NAN;
    for (k, i) in keyed {
        match clusters.last_mut() {
            Some(c) if (k - start).abs() <= facts.tolerance => c.push(i),
            _ => {
                clusters.push(vec![i]);
                start = k;
            }
        }
    }
    if !max {
        clusters.reverse();
    }
    let len = clusters.len() as i64;
    let at = if index < 0 { len + index } else { index };
    let mut out = vec![false; facts.edges.len()];
    if (0..len).contains(&at) {
        for &i in &clusters[at as usize] {
            out[i] = true;
        }
    }
    out
}

/// A BOSL2 edge descriptor on the child's bounding box (section 5.2): one
/// non-zero entry is the edges lying in that face of the box, two the
/// edges lying along that edge of it, three the edges touching that
/// corner.
fn descriptor(facts: &Facts, d: [i8; 3]) -> Vec<bool> {
    let Some((lo, hi)) = facts.bbox else {
        return vec![false; facts.edges.len()];
    };
    let tol = facts.tolerance;
    let bound = |k: usize| if d[k] > 0 { hi[k] } else { lo[k] };
    let axes: Vec<usize> = (0..3).filter(|&k| d[k] != 0).collect();
    let on = |q: &[f64; 3]| axes.iter().all(|&k| (q[k] - bound(k)).abs() <= tol);
    if axes.len() == 3 {
        return each(facts, |e| on(&e.from) || on(&e.to));
    }
    each(facts, |e| e.path.iter().all(on))
}

/// Whether the edge is of a class the blends cover (`docs/fillets.md`,
/// section 6.1) and a sense they handle: the rest is
/// `fillet-unsupported-edge`.
pub(crate) fn supported(e: &EdgeFact) -> bool {
    e.class != Class::Other && matches!(e.sense, Sense::Convex | Sense::Concave)
}
