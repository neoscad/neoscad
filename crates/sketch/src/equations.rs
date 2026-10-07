//! The sketch as equations: the unknowns, one residual per equation, and
//! each residual's analytic gradient.
//!
//! Every residual is measured in lengths, so one tolerance fits them all:
//! direction equations (parallel, perpendicular, angle) are sines and
//! cosines of unit vectors, multiplied by the sketch's size. Only `+ - * /`
//! and `sqrt` are used here, all of them correctly rounded in IEEE 754, so
//! a residual has the same bits on every platform. (Rust never fuses a
//! multiply and an add on its own, and nothing here asks it to.)

use crate::model::{Along, Constraint, ConstraintId, Entity, EntityId, EntityKind, Pair, Sketch};
use crate::trig;

/// What an equation comes from: a constraint, or an arc's own equation
/// (its start and end are the same distance from its centre).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Source {
    Constraint(ConstraintId),
    Arc(EntityId),
}

/// Which coordinate of an entity an unknown is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Coordinate {
    X,
    Y,
    /// A circle's radius.
    Radius,
}

/// Where the unknowns are: two per point, one per circle.
#[derive(Clone, Debug)]
pub(crate) struct Layout {
    /// For each entity, its first unknown (points and circles), or
    /// `usize::MAX`.
    pub var: Vec<usize>,
    /// For each unknown, its entity and coordinate.
    pub owner: Vec<(EntityId, Coordinate)>,
}

impl Layout {
    pub fn new(s: &Sketch) -> Layout {
        let mut var = Vec::with_capacity(s.entities.len());
        let mut owner = Vec::new();
        for (i, e) in s.entities.iter().enumerate() {
            let id = EntityId::from_index(i);
            match e {
                Entity::Point { .. } => {
                    var.push(owner.len());
                    owner.push((id, Coordinate::X));
                    owner.push((id, Coordinate::Y));
                }
                Entity::Circle { .. } => {
                    var.push(owner.len());
                    owner.push((id, Coordinate::Radius));
                }
                _ => var.push(usize::MAX),
            }
        }
        Layout { var, owner }
    }

    pub fn len(&self) -> usize {
        self.owner.len()
    }

    /// The first unknown of point `p` (x; y is the next).
    pub fn pt(&self, p: EntityId) -> usize {
        self.var[p.index()]
    }
}

/// An arc's or circle's radius: an unknown, or an arc's |start − centre|.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Radius {
    Var(usize),
    Arc { c: usize, s: usize },
}

/// One equation, by shape. Point fields are first unknowns (x; y follows).
#[derive(Clone, Debug)]
pub(crate) enum Eq {
    /// Σ k·x − t·tcoef.
    Lin(Vec<(usize, f64)>),
    /// |q − p| − t.
    PointDist { p: usize, q: usize },
    /// Signed distance of `p` from the line a→b (positive on the left),
    /// − t·tcoef.
    LineDist { p: usize, a: usize, b: usize },
    /// |p − c| − r.
    OnCircle { p: usize, c: usize, r: Radius },
    /// Signed distance of `c` from the line a→b, − sign·r.
    TangentLine {
        c: usize,
        a: usize,
        b: usize,
        r: Radius,
        sign: f64,
    },
    /// |c2 − c1| − (r1 + r2), or − sign·(r1 − r2) when internal.
    TangentCircles {
        c1: usize,
        c2: usize,
        r1: Radius,
        r2: Radius,
        internal: Option<f64>,
    },
    /// r1 − r2.
    RadiusDiff { r1: Radius, r2: Radius },
    /// r − t·tcoef.
    RadiusValue { r: Radius },
    /// |s − c| − |e − c|.
    ArcEqual { c: usize, s: usize, e: usize },
    /// size · cross(u1, u2), u the unit directions a→b.
    Cross {
        a1: usize,
        b1: usize,
        a2: usize,
        b2: usize,
    },
    /// size · dot(u1, u2).
    Dot {
        a1: usize,
        b1: usize,
        a2: usize,
        b2: usize,
    },
    /// size · cross(R(dir·θ) u1, u2), θ the target in degrees.
    Angle {
        a1: usize,
        b1: usize,
        a2: usize,
        b2: usize,
        dir: f64,
    },
    /// |b1 − a1| − |b2 − a2|.
    LenDiff {
        a1: usize,
        b1: usize,
        a2: usize,
        b2: usize,
    },
    /// Signed distance of (p + q)/2 from the line a→b.
    SymMid {
        p: usize,
        q: usize,
        a: usize,
        b: usize,
    },
    /// (q − p)·(b − a)/|b − a|.
    SymPerp {
        p: usize,
        q: usize,
        a: usize,
        b: usize,
    },
}

/// What a dimension measures, which decides how the solver moves its
/// target from the drawn value to the wanted one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Dim {
    /// A geometric equation: no target.
    None,
    /// A length, offset or position.
    Linear,
    /// An angle in degrees.
    Angle,
    /// An arc's sweep in degrees, in (0, 360].
    Sweep,
}

/// One equation with its target.
#[derive(Clone, Debug)]
pub(crate) struct Row {
    pub eq: Eq,
    pub source: Source,
    pub dim: Dim,
    /// The value the user asked for.
    pub goal: f64,
    /// The value the residual currently uses (the goal, or the drawn value
    /// while cleaning the drawing, or a value in between).
    pub target: f64,
    pub tcoef: f64,
    cos: f64,
    sin: f64,
}

impl Row {
    fn new(eq: Eq, source: Source) -> Row {
        Row {
            eq,
            source,
            dim: Dim::None,
            goal: 0.0,
            target: 0.0,
            tcoef: 1.0,
            cos: 1.0,
            sin: 0.0,
        }
    }

    fn dim(mut self, dim: Dim, goal: f64, tcoef: f64) -> Row {
        self.dim = dim;
        self.goal = goal;
        self.tcoef = tcoef;
        self.set_target(goal);
        self
    }

    /// Use `t` as the target. An angle's cosine and sine are computed here,
    /// once, not in the iteration (the only trigonometry in a solve).
    pub fn set_target(&mut self, t: f64) {
        self.target = t;
        if let Eq::Angle { dir, .. } = self.eq {
            let (c, s) = trig::cos_sin_degrees(dir * t);
            self.cos = c;
            self.sin = s;
        }
    }

    /// The unknowns this equation reads, possibly repeated.
    pub fn vars(&self, out: &mut Vec<usize>) {
        let pt = |out: &mut Vec<usize>, p: usize| {
            out.push(p);
            out.push(p + 1);
        };
        let rad = |out: &mut Vec<usize>, r: Radius| match r {
            Radius::Var(i) => out.push(i),
            Radius::Arc { c, s } => {
                pt(out, c);
                pt(out, s);
            }
        };
        match &self.eq {
            Eq::Lin(t) => out.extend(t.iter().map(|&(i, _)| i)),
            Eq::PointDist { p, q } => {
                pt(out, *p);
                pt(out, *q);
            }
            Eq::LineDist { p, a, b } => {
                pt(out, *p);
                pt(out, *a);
                pt(out, *b);
            }
            Eq::OnCircle { p, c, r } => {
                pt(out, *p);
                pt(out, *c);
                rad(out, *r);
            }
            Eq::TangentLine { c, a, b, r, .. } => {
                pt(out, *c);
                pt(out, *a);
                pt(out, *b);
                rad(out, *r);
            }
            Eq::TangentCircles { c1, c2, r1, r2, .. } => {
                pt(out, *c1);
                pt(out, *c2);
                rad(out, *r1);
                rad(out, *r2);
            }
            Eq::RadiusDiff { r1, r2 } => {
                rad(out, *r1);
                rad(out, *r2);
            }
            Eq::RadiusValue { r } => rad(out, *r),
            Eq::ArcEqual { c, s, e } => {
                pt(out, *c);
                pt(out, *s);
                pt(out, *e);
            }
            Eq::Cross { a1, b1, a2, b2 }
            | Eq::Dot { a1, b1, a2, b2 }
            | Eq::Angle { a1, b1, a2, b2, .. }
            | Eq::LenDiff { a1, b1, a2, b2 } => {
                for p in [*a1, *b1, *a2, *b2] {
                    pt(out, p);
                }
            }
            Eq::SymMid { p, q, a, b } | Eq::SymPerp { p, q, a, b } => {
                for v in [*p, *q, *a, *b] {
                    pt(out, v);
                }
            }
        }
    }

    /// The residual at `x` for a sketch of size `size`, with its gradient
    /// appended to `grad` as (unknown, ∂) pairs (an unknown may repeat; the
    /// pairs add up).
    pub fn eval(&self, x: &[f64], size: f64, grad: &mut Vec<(usize, f64)>) -> f64 {
        let f = self.base(x, size, grad);
        match self.dim {
            Dim::Angle | Dim::Sweep => f,
            _ => f - self.target * self.tcoef,
        }
    }

    /// The residual without the target term (the whole residual for an
    /// angle, whose target is inside the rotation).
    fn base(&self, x: &[f64], size: f64, g: &mut Vec<(usize, f64)>) -> f64 {
        match &self.eq {
            Eq::Lin(terms) => {
                let mut f = 0.0;
                for &(i, k) in terms {
                    f += k * x[i];
                    g.push((i, k));
                }
                f
            }
            Eq::PointDist { p, q } => {
                let v = sub(at(x, *q), at(x, *p));
                let (l, dl) = length(v);
                push(g, *q, dl, 1.0);
                push(g, *p, dl, -1.0);
                l
            }
            Eq::LineDist { p, a, b } => line_dist(x, *p, *a, *b, 1.0, g),
            Eq::OnCircle { p, c, r } => {
                let v = sub(at(x, *p), at(x, *c));
                let (l, dl) = length(v);
                push(g, *p, dl, 1.0);
                push(g, *c, dl, -1.0);
                l - radius(x, *r, -1.0, g)
            }
            Eq::TangentLine { c, a, b, r, sign } => {
                line_dist(x, *c, *a, *b, 1.0, g) - sign * radius(x, *r, -sign, g)
            }
            Eq::TangentCircles {
                c1,
                c2,
                r1,
                r2,
                internal,
            } => {
                let v = sub(at(x, *c2), at(x, *c1));
                let (d, dd) = length(v);
                push(g, *c2, dd, 1.0);
                push(g, *c1, dd, -1.0);
                match internal {
                    None => d - radius(x, *r1, -1.0, g) - radius(x, *r2, -1.0, g),
                    Some(s) => d - s * (radius(x, *r1, -s, g) - radius(x, *r2, *s, g)),
                }
            }
            Eq::RadiusDiff { r1, r2 } => radius(x, *r1, 1.0, g) - radius(x, *r2, -1.0, g),
            Eq::RadiusValue { r } => radius(x, *r, 1.0, g),
            Eq::ArcEqual { c, s, e } => {
                let vs = sub(at(x, *s), at(x, *c));
                let ve = sub(at(x, *e), at(x, *c));
                let (ls, dls) = length(vs);
                let (le, dle) = length(ve);
                push(g, *s, dls, 1.0);
                push(g, *c, dls, -1.0);
                push(g, *e, dle, -1.0);
                push(g, *c, dle, 1.0);
                ls - le
            }
            Eq::Cross { a1, b1, a2, b2 } => {
                directions(x, [*a1, *b1, *a2, *b2], (1.0, 0.0), true, size, g)
            }
            Eq::Dot { a1, b1, a2, b2 } => {
                directions(x, [*a1, *b1, *a2, *b2], (1.0, 0.0), false, size, g)
            }
            Eq::Angle { a1, b1, a2, b2, .. } => {
                directions(x, [*a1, *b1, *a2, *b2], (self.cos, self.sin), true, size, g)
            }
            Eq::LenDiff { a1, b1, a2, b2 } => {
                let (l1, d1) = length(sub(at(x, *b1), at(x, *a1)));
                let (l2, d2) = length(sub(at(x, *b2), at(x, *a2)));
                push(g, *b1, d1, 1.0);
                push(g, *a1, d1, -1.0);
                push(g, *b2, d2, -1.0);
                push(g, *a2, d2, 1.0);
                l1 - l2
            }
            Eq::SymMid { p, q, a, b } => {
                let m = scale(add(at(x, *p), at(x, *q)), 0.5);
                let (f, gm, ga, gb) = signed_distance(m, at(x, *a), at(x, *b));
                push(g, *p, gm, 0.5);
                push(g, *q, gm, 0.5);
                push(g, *a, ga, 1.0);
                push(g, *b, gb, 1.0);
                f
            }
            Eq::SymPerp { p, q, a, b } => {
                let w = sub(at(x, *q), at(x, *p));
                let d = sub(at(x, *b), at(x, *a));
                let l = norm(d);
                if l == 0.0 {
                    return 0.0;
                }
                let f = dot(w, d) / l;
                push(g, *q, d, 1.0 / l);
                push(g, *p, d, -1.0 / l);
                // ∂f/∂d = w/l − f·d/l².
                let gd = sub(scale(w, 1.0 / l), scale(d, f / (l * l)));
                push(g, *b, gd, 1.0);
                push(g, *a, gd, -1.0);
                f
            }
        }
    }

    /// The value of this dimension in the configuration `x` (what the
    /// drawing shows), or `None` for a geometric equation.
    pub fn measure(&self, x: &[f64], size: f64) -> Option<f64> {
        let mut scratch = Vec::new();
        match self.dim {
            Dim::None => None,
            Dim::Linear => Some(self.base(x, size, &mut scratch) / self.tcoef),
            Dim::Angle | Dim::Sweep => {
                let Eq::Angle {
                    a1,
                    b1,
                    a2,
                    b2,
                    dir,
                } = self.eq
                else {
                    unreachable!("angle dimensions are angle equations")
                };
                let u = sub(at(x, b1), at(x, a1));
                let v = sub(at(x, b2), at(x, a2));
                let mut deg = dir * trig::atan2_degrees(cross(u, v), dot(u, v));
                if self.dim == Dim::Sweep && deg <= 0.0 {
                    deg += 360.0;
                }
                Some(deg)
            }
        }
    }

    /// For an angle equation, the cosine between the rotated first
    /// direction and the second: positive on the wanted branch (the
    /// residual alone is also zero 180° away).
    pub fn angle_alignment(&self, x: &[f64]) -> Option<f64> {
        let Eq::Angle { a1, b1, a2, b2, .. } = self.eq else {
            return None;
        };
        let u = sub(at(x, b1), at(x, a1));
        let v = sub(at(x, b2), at(x, a2));
        let ru = [
            self.cos * u[0] - self.sin * u[1],
            self.sin * u[0] + self.cos * u[1],
        ];
        let n = norm(u) * norm(v);
        Some(if n > 0.0 { dot(ru, v) / n } else { 0.0 })
    }
}

fn at(x: &[f64], p: usize) -> [f64; 2] {
    [x[p], x[p + 1]]
}

pub(crate) fn sub(a: [f64; 2], b: [f64; 2]) -> [f64; 2] {
    [a[0] - b[0], a[1] - b[1]]
}

fn add(a: [f64; 2], b: [f64; 2]) -> [f64; 2] {
    [a[0] + b[0], a[1] + b[1]]
}

fn scale(a: [f64; 2], k: f64) -> [f64; 2] {
    [a[0] * k, a[1] * k]
}

pub(crate) fn dot(a: [f64; 2], b: [f64; 2]) -> f64 {
    a[0] * b[0] + a[1] * b[1]
}

pub(crate) fn cross(a: [f64; 2], b: [f64; 2]) -> f64 {
    a[0] * b[1] - a[1] * b[0]
}

pub(crate) fn norm(a: [f64; 2]) -> f64 {
    (a[0] * a[0] + a[1] * a[1]).sqrt()
}

/// |v| and its gradient v/|v| (zero at the origin, where it has none).
fn length(v: [f64; 2]) -> (f64, [f64; 2]) {
    let l = norm(v);
    if l == 0.0 {
        (0.0, [0.0, 0.0])
    } else {
        (l, [v[0] / l, v[1] / l])
    }
}

fn push(g: &mut Vec<(usize, f64)>, p: usize, d: [f64; 2], k: f64) {
    g.push((p, d[0] * k));
    g.push((p + 1, d[1] * k));
}

/// The radius `r` at `x`; its gradient is appended times `k`.
fn radius(x: &[f64], r: Radius, k: f64, g: &mut Vec<(usize, f64)>) -> f64 {
    match r {
        Radius::Var(i) => {
            g.push((i, k));
            x[i]
        }
        Radius::Arc { c, s } => {
            let (l, dl) = length(sub(at(x, s), at(x, c)));
            push(g, s, dl, k);
            push(g, c, dl, -k);
            l
        }
    }
}

/// The signed distance of `p` from the line through a and b (positive to
/// the left of a→b), and its gradients with respect to p, a and b.
fn signed_distance(p: [f64; 2], a: [f64; 2], b: [f64; 2]) -> (f64, [f64; 2], [f64; 2], [f64; 2]) {
    let d = sub(b, a);
    let w = sub(p, a);
    let l = norm(d);
    if l == 0.0 {
        // A line of no length has no direction; the distance is taken as
        // the distance to the point, without gradient, so a degenerate
        // drawing shows up as a dependent equation rather than a NaN.
        return (0.0, [0.0; 2], [0.0; 2], [0.0; 2]);
    }
    let c = cross(d, w);
    let f = c / l;
    // ∂c/∂d = (w.y, −w.x), ∂c/∂w = (−d.y, d.x); ∂l/∂d = d/l.
    let gw = [-d[1] / l, d[0] / l];
    let gd = [
        w[1] / l - f * d[0] / (l * l),
        -w[0] / l - f * d[1] / (l * l),
    ];
    let gp = gw;
    let gb = gd;
    let ga = [-gd[0] - gw[0], -gd[1] - gw[1]];
    (f, gp, ga, gb)
}

fn line_dist(x: &[f64], p: usize, a: usize, b: usize, k: f64, g: &mut Vec<(usize, f64)>) -> f64 {
    let (f, gp, ga, gb) = signed_distance(at(x, p), at(x, a), at(x, b));
    push(g, p, gp, k);
    push(g, a, ga, k);
    push(g, b, gb, k);
    f
}

/// size·cross(R u1, u2) (or size·dot(u1, u2) when `!crossed`), where u
/// are the unit directions of a1→b1 and a2→b2 and R rotates by the angle
/// with cosine and sine `rot`.
fn directions(
    x: &[f64],
    [a1, b1, a2, b2]: [usize; 4],
    (c, s): (f64, f64),
    crossed: bool,
    size: f64,
    g: &mut Vec<(usize, f64)>,
) -> f64 {
    let d1 = sub(at(x, b1), at(x, a1));
    let d2 = sub(at(x, b2), at(x, a2));
    let l1 = norm(d1);
    let l2 = norm(d2);
    if l1 == 0.0 || l2 == 0.0 {
        return 0.0;
    }
    let ll = l1 * l2;
    let (h, g1, g2) = if crossed {
        let e = [c * d1[0] - s * d1[1], s * d1[0] + c * d1[1]];
        let h = cross(e, d2) / ll;
        // ∂cross/∂e = (d2.y, −d2.x), carried back through R by Rᵀ;
        // ∂cross/∂d2 = (−e.y, e.x).
        let ge = [d2[1], -d2[0]];
        let gd1 = [c * ge[0] + s * ge[1], -s * ge[0] + c * ge[1]];
        let gd2 = [-e[1], e[0]];
        (h, gd1, gd2)
    } else {
        (dot(d1, d2) / ll, d2, d1)
    };
    // ∂h/∂d1 = ∂raw/∂d1 /(l1 l2) − h d1/l1², and the same for d2.
    let gd1 = [
        g1[0] / ll - h * d1[0] / (l1 * l1),
        g1[1] / ll - h * d1[1] / (l1 * l1),
    ];
    let gd2 = [
        g2[0] / ll - h * d2[0] / (l2 * l2),
        g2[1] / ll - h * d2[1] / (l2 * l2),
    ];
    push(g, b1, gd1, size);
    push(g, a1, gd1, -size);
    push(g, b2, gd2, size);
    push(g, a2, gd2, -size);
    size * h
}

/// For each entity, a representative of the points it is the same as:
/// itself, or the lowest point it is made coincident with.
pub(crate) fn coincident_classes(s: &Sketch) -> Vec<usize> {
    let mut class: Vec<usize> = (0..s.entities.len()).collect();
    fn find(p: &mut [usize], mut i: usize) -> usize {
        while p[i] != i {
            p[i] = p[p[i]];
            i = p[i];
        }
        i
    }
    for c in &s.constraints {
        if let Constraint::Coincident(a, b) = c {
            let (ra, rb) = (find(&mut class, a.index()), find(&mut class, b.index()));
            let (lo, hi) = if ra < rb { (ra, rb) } else { (rb, ra) };
            class[hi] = lo;
        }
    }
    for i in 0..class.len() {
        class[i] = find(&mut class, i);
    }
    class
}

/// The ends of a line or arc (a circle has none).
pub(crate) fn ends(s: &Sketch, e: EntityId) -> Option<[EntityId; 2]> {
    match s.entities[e.index()] {
        Entity::Line { start, end } | Entity::Arc { start, end, .. } => Some([start, end]),
        _ => None,
    }
}

/// The first pair of ends, one of `x` and one of `y`, that are the same
/// point (or coincident ones).
fn shared_end(
    s: &Sketch,
    class: &[usize],
    x: EntityId,
    y: EntityId,
) -> Option<(EntityId, EntityId)> {
    let (ex, ey) = (ends(s, x)?, ends(s, y)?);
    for p in ex {
        for q in ey {
            if class[p.index()] == class[q.index()] {
                return Some((p, q));
            }
        }
    }
    None
}

/// The sign of `v`, with 0 counted as positive.
fn side(v: f64) -> f64 {
    if v >= 0.0 { 1.0 } else { -1.0 }
}

/// Lower the sketch to rows, in constraint order, then the arcs' own
/// equations in entity order. Sides and inside/outside choices are read
/// from `x0`, the drawing.
pub(crate) fn lower(s: &Sketch, lay: &Layout, x0: &[f64]) -> Vec<Row> {
    let mut rows = Vec::new();
    let pts = |l: EntityId| -> (usize, usize) {
        match s.entities[l.index()] {
            Entity::Line { start, end } => (lay.pt(start), lay.pt(end)),
            _ => unreachable!("checked to be a line"),
        }
    };
    let round = |e: EntityId| -> (usize, Radius) {
        match s.entities[e.index()] {
            Entity::Arc { center, start, .. } => (
                lay.pt(center),
                Radius::Arc {
                    c: lay.pt(center),
                    s: lay.pt(start),
                },
            ),
            Entity::Circle { center, .. } => (lay.pt(center), Radius::Var(lay.var[e.index()])),
            _ => unreachable!("checked to be an arc or circle"),
        }
    };
    let kind = |e: EntityId| s.entities[e.index()].kind();
    let classes = coincident_classes(s);
    let mut scratch = Vec::new();
    for (ci, c) in s.constraints.iter().enumerate() {
        let src = Source::Constraint(ConstraintId::from_index(ci));
        let row = |eq| Row::new(eq, src);
        match c {
            Constraint::Coincident(a, b) => {
                let (a, b) = (lay.pt(*a), lay.pt(*b));
                rows.push(row(Eq::Lin(vec![(b, 1.0), (a, -1.0)])));
                rows.push(row(Eq::Lin(vec![(b + 1, 1.0), (a + 1, -1.0)])));
            }
            Constraint::On { point, curve } => {
                let p = lay.pt(*point);
                rows.push(row(match kind(*curve) {
                    EntityKind::Line => {
                        let (a, b) = pts(*curve);
                        Eq::LineDist { p, a, b }
                    }
                    _ => {
                        let (c, r) = round(*curve);
                        Eq::OnCircle { p, c, r }
                    }
                }));
            }
            Constraint::Horizontal(pair) | Constraint::Vertical(pair) => {
                let (a, b) = match *pair {
                    Pair::Line(l) => pts(l),
                    Pair::Points(a, b) => (lay.pt(a), lay.pt(b)),
                };
                let o = usize::from(matches!(c, Constraint::Horizontal(_)));
                rows.push(row(Eq::Lin(vec![(b + o, 1.0), (a + o, -1.0)])));
            }
            Constraint::Parallel(l1, l2) | Constraint::Perpendicular(l1, l2) => {
                let ((a1, b1), (a2, b2)) = (pts(*l1), pts(*l2));
                rows.push(row(if matches!(c, Constraint::Parallel(..)) {
                    Eq::Cross { a1, b1, a2, b2 }
                } else {
                    Eq::Dot { a1, b1, a2, b2 }
                }));
            }
            Constraint::Tangent(x, y) => {
                let (line, other) = match (kind(*x), kind(*y)) {
                    (EntityKind::Line, _) => (Some(*x), *y),
                    (_, EntityKind::Line) => (Some(*y), *x),
                    _ => (None, *y),
                };
                // Tangency where the curves share an endpoint is written at
                // that point, as FreeCAD's endpoint tangency and SolveSpace's
                // ARC_LINE_TANGENT are: "the radius there is perpendicular
                // to the line" (or "the two radii there are parallel").
                // The distance form below is also true there, but only to
                // second order: sliding the shared point along the line
                // changes no residual at first order, so the Jacobian loses
                // a rank and a fully constrained slot would report a free
                // degree of freedom.
                let shared = shared_end(s, &classes, *x, *y);
                let eq = if let Some((p, q)) = shared {
                    match line {
                        Some(l) => {
                            let (a, b) = pts(l);
                            let (c, _) = round(other);
                            let arc_end = if kind(*x) == EntityKind::Line { q } else { p };
                            Eq::Dot {
                                a1: a,
                                b1: b,
                                a2: c,
                                b2: lay.pt(arc_end),
                            }
                        }
                        None => Eq::Cross {
                            a1: round(*x).0,
                            b1: lay.pt(p),
                            a2: round(*y).0,
                            b2: lay.pt(q),
                        },
                    }
                } else if let Some(l) = line {
                    let (a, b) = pts(l);
                    let (c, r) = round(other);
                    scratch.clear();
                    let drawn =
                        Row::new(Eq::LineDist { p: c, a, b }, src).base(x0, 1.0, &mut scratch);
                    Eq::TangentLine {
                        c,
                        a,
                        b,
                        r,
                        sign: side(drawn),
                    }
                } else {
                    let (c1, r1) = round(*x);
                    let (c2, r2) = round(*y);
                    scratch.clear();
                    let d = norm(sub(at(x0, c2), at(x0, c1)));
                    let rr1 = radius(x0, r1, 1.0, &mut scratch);
                    let rr2 = radius(x0, r2, 1.0, &mut scratch);
                    let outside = (d - (rr1 + rr2)).abs();
                    let inside = (d - (rr1 - rr2).abs()).abs();
                    Eq::TangentCircles {
                        c1,
                        c2,
                        r1,
                        r2,
                        internal: (inside < outside).then(|| side(rr1 - rr2)),
                    }
                };
                rows.push(row(eq));
            }
            Constraint::Distance { a, b, value, along } => {
                let r = match (kind(*a), kind(*b), along) {
                    (EntityKind::Point, EntityKind::Point, Along::Direct) => row(Eq::PointDist {
                        p: lay.pt(*a),
                        q: lay.pt(*b),
                    })
                    .dim(Dim::Linear, *value, 1.0),
                    (EntityKind::Point, EntityKind::Point, _) => {
                        let o = usize::from(*along == Along::Y);
                        row(Eq::Lin(vec![(lay.pt(*b) + o, 1.0), (lay.pt(*a) + o, -1.0)])).dim(
                            Dim::Linear,
                            *value,
                            1.0,
                        )
                    }
                    (EntityKind::Point, EntityKind::Line, _)
                    | (EntityKind::Line, EntityKind::Line, _) => {
                        let p = match kind(*a) {
                            EntityKind::Point => lay.pt(*a),
                            _ => pts(*b).0,
                        };
                        let line = if kind(*a) == EntityKind::Point {
                            *b
                        } else {
                            *a
                        };
                        let (la, lb) = pts(line);
                        let eq = Eq::LineDist { p, a: la, b: lb };
                        scratch.clear();
                        let drawn = Row::new(eq.clone(), src).base(x0, 1.0, &mut scratch);
                        row(eq).dim(Dim::Linear, *value, side(drawn))
                    }
                    _ => unreachable!("checked kinds"),
                };
                rows.push(r);
            }
            Constraint::Length { line, value } => {
                let (p, q) = pts(*line);
                rows.push(row(Eq::PointDist { p, q }).dim(Dim::Linear, *value, 1.0));
            }
            Constraint::Radius { curve, value } | Constraint::Diameter { curve, value } => {
                let (_, r) = round(*curve);
                let k = if matches!(c, Constraint::Diameter { .. }) {
                    0.5
                } else {
                    1.0
                };
                rows.push(row(Eq::RadiusValue { r }).dim(Dim::Linear, *value, k));
            }
            Constraint::Angle { from, to, degrees } => {
                let ((a1, b1), (a2, b2)) = (pts(*from), pts(*to));
                rows.push(
                    row(Eq::Angle {
                        a1,
                        b1,
                        a2,
                        b2,
                        dir: 1.0,
                    })
                    .dim(Dim::Angle, *degrees, 1.0),
                );
            }
            Constraint::Sweep { arc, degrees } => {
                let Entity::Arc {
                    center,
                    start,
                    end,
                    clockwise,
                } = s.entities[arc.index()]
                else {
                    unreachable!("checked to be an arc")
                };
                let c = lay.pt(center);
                rows.push(
                    row(Eq::Angle {
                        a1: c,
                        b1: lay.pt(start),
                        a2: c,
                        b2: lay.pt(end),
                        dir: if clockwise { -1.0 } else { 1.0 },
                    })
                    .dim(Dim::Sweep, *degrees, 1.0),
                );
            }
            Constraint::Equal(a, b) => {
                rows.push(row(if kind(*a) == EntityKind::Line {
                    let ((a1, b1), (a2, b2)) = (pts(*a), pts(*b));
                    Eq::LenDiff { a1, b1, a2, b2 }
                } else {
                    Eq::RadiusDiff {
                        r1: round(*a).1,
                        r2: round(*b).1,
                    }
                }));
            }
            Constraint::Midpoint { point, line } => {
                let p = lay.pt(*point);
                let (a, b) = pts(*line);
                for o in 0..2 {
                    rows.push(row(Eq::Lin(vec![
                        (p + o, 1.0),
                        (a + o, -0.5),
                        (b + o, -0.5),
                    ])));
                }
            }
            Constraint::Symmetric { a, b, about } => {
                let (p, q) = (lay.pt(*a), lay.pt(*b));
                if kind(*about) == EntityKind::Line {
                    let (la, lb) = pts(*about);
                    rows.push(row(Eq::SymMid { p, q, a: la, b: lb }));
                    rows.push(row(Eq::SymPerp { p, q, a: la, b: lb }));
                } else {
                    let m = lay.pt(*about);
                    for o in 0..2 {
                        rows.push(row(Eq::Lin(vec![
                            (p + o, 1.0),
                            (q + o, 1.0),
                            (m + o, -2.0),
                        ])));
                    }
                }
            }
            Constraint::Fix { entity, at: pos } => {
                let points: Vec<EntityId> = match s.entities[entity.index()] {
                    Entity::Line { start, end } => vec![start, end],
                    _ => vec![*entity],
                };
                for p in points {
                    let v = lay.pt(p);
                    let target = pos.unwrap_or_else(|| [x0[v], x0[v + 1]]);
                    for (o, t) in target.into_iter().enumerate() {
                        rows.push(row(Eq::Lin(vec![(v + o, 1.0)])).dim(Dim::Linear, t, 1.0));
                    }
                }
            }
        }
    }
    for (i, e) in s.entities.iter().enumerate() {
        if let Entity::Arc {
            center, start, end, ..
        } = *e
        {
            rows.push(Row::new(
                Eq::ArcEqual {
                    c: lay.pt(center),
                    s: lay.pt(start),
                    e: lay.pt(end),
                },
                Source::Arc(EntityId::from_index(i)),
            ));
        }
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Deterministic pseudo-random numbers for the checks (SplitMix64).
    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> f64 {
            self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
            let mut z = self.0;
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            z ^= z >> 31;
            (z >> 11) as f64 / (1u64 << 53) as f64
        }
    }

    /// Every equation's analytic gradient against central differences, at
    /// many random configurations.
    #[test]
    fn gradients_match_finite_differences() {
        let mut rng = Rng(7);
        let size = 3.0;
        let lay_n = 13; // 6 points (12 unknowns) and one circle radius.
        let r_var = Radius::Var(12);
        let r_arc = Radius::Arc { c: 8, s: 10 };
        let eqs: Vec<(&str, Eq, Dim)> = vec![
            (
                "lin",
                Eq::Lin(vec![(0, 1.0), (3, -0.5), (5, -0.5)]),
                Dim::Linear,
            ),
            ("pointdist", Eq::PointDist { p: 0, q: 2 }, Dim::Linear),
            ("linedist", Eq::LineDist { p: 0, a: 2, b: 4 }, Dim::Linear),
            (
                "oncircle var",
                Eq::OnCircle {
                    p: 0,
                    c: 2,
                    r: r_var,
                },
                Dim::None,
            ),
            (
                "oncircle arc",
                Eq::OnCircle {
                    p: 0,
                    c: 8,
                    r: r_arc,
                },
                Dim::None,
            ),
            (
                "tangent line",
                Eq::TangentLine {
                    c: 8,
                    a: 0,
                    b: 2,
                    r: r_arc,
                    sign: -1.0,
                },
                Dim::None,
            ),
            (
                "tangent circles ext",
                Eq::TangentCircles {
                    c1: 0,
                    c2: 8,
                    r1: r_var,
                    r2: r_arc,
                    internal: None,
                },
                Dim::None,
            ),
            (
                "tangent circles int",
                Eq::TangentCircles {
                    c1: 0,
                    c2: 8,
                    r1: r_var,
                    r2: r_arc,
                    internal: Some(-1.0),
                },
                Dim::None,
            ),
            (
                "radius diff",
                Eq::RadiusDiff {
                    r1: r_arc,
                    r2: r_var,
                },
                Dim::None,
            ),
            ("radius value", Eq::RadiusValue { r: r_arc }, Dim::Linear),
            ("arc equal", Eq::ArcEqual { c: 0, s: 2, e: 4 }, Dim::None),
            (
                "cross",
                Eq::Cross {
                    a1: 0,
                    b1: 2,
                    a2: 4,
                    b2: 6,
                },
                Dim::None,
            ),
            (
                "dot",
                Eq::Dot {
                    a1: 0,
                    b1: 2,
                    a2: 4,
                    b2: 6,
                },
                Dim::None,
            ),
            (
                "angle",
                Eq::Angle {
                    a1: 0,
                    b1: 2,
                    a2: 4,
                    b2: 6,
                    dir: 1.0,
                },
                Dim::Angle,
            ),
            (
                "sweep",
                Eq::Angle {
                    a1: 0,
                    b1: 2,
                    a2: 0,
                    b2: 6,
                    dir: -1.0,
                },
                Dim::Sweep,
            ),
            (
                "lendiff",
                Eq::LenDiff {
                    a1: 0,
                    b1: 2,
                    a2: 4,
                    b2: 0,
                },
                Dim::None,
            ),
            (
                "symmid",
                Eq::SymMid {
                    p: 0,
                    q: 2,
                    a: 4,
                    b: 6,
                },
                Dim::None,
            ),
            (
                "symperp",
                Eq::SymPerp {
                    p: 0,
                    q: 2,
                    a: 4,
                    b: 6,
                },
                Dim::None,
            ),
            // Shared points: the gradient pairs of one unknown must add up.
            (
                "shared",
                Eq::Cross {
                    a1: 0,
                    b1: 2,
                    a2: 2,
                    b2: 4,
                },
                Dim::None,
            ),
        ];
        for (name, eq, dim) in eqs {
            for trial in 0..50 {
                let x: Vec<f64> = (0..lay_n).map(|_| rng.next() * 10.0 - 5.0).collect();
                let mut row = Row::new(eq.clone(), Source::Arc(EntityId::from_index(0)));
                if dim != Dim::None {
                    row = row.dim(dim, 37.0 * rng.next(), 0.5);
                }
                let mut g = Vec::new();
                row.eval(&x, size, &mut g);
                let mut analytic = vec![0.0; lay_n];
                for (i, d) in g {
                    analytic[i] += d;
                }
                for i in 0..lay_n {
                    let h = 1e-6;
                    let mut xp = x.clone();
                    let mut xm = x.clone();
                    xp[i] += h;
                    xm[i] -= h;
                    let fd = (row.eval(&xp, size, &mut Vec::new())
                        - row.eval(&xm, size, &mut Vec::new()))
                        / (2.0 * h);
                    assert!(
                        (fd - analytic[i]).abs() <= 1e-6 * (1.0 + fd.abs()),
                        "{name} trial {trial}: ∂/∂x{i} analytic {} vs finite difference {fd}",
                        analytic[i]
                    );
                }
            }
        }
    }

    #[test]
    fn angle_measure_inverts_the_residual() {
        let x = [
            0.0,
            0.0,
            2.0,
            0.0,
            1.0,
            1.0,
            1.0 - 3.0,
            1.0 + 3.0 * 3f64.sqrt(),
        ];
        let mut row = Row::new(
            Eq::Angle {
                a1: 0,
                b1: 2,
                a2: 4,
                b2: 6,
                dir: 1.0,
            },
            Source::Arc(EntityId::from_index(0)),
        )
        .dim(Dim::Angle, 0.0, 1.0);
        let m = row.measure(&x, 1.0).unwrap();
        assert!((m - 120.0).abs() < 1e-12, "{m}");
        row.set_target(m);
        assert!(row.eval(&x, 1.0, &mut Vec::new()).abs() < 1e-15);
        assert!(row.angle_alignment(&x).unwrap() > 0.0);
    }
}
