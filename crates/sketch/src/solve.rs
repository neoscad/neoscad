//! The solve: placing unguessed points, cleaning the drawing,
//! Levenberg–Marquardt, continuation when the direct solve fails or flips,
//! exactify, and the rank diagnosis.

use std::collections::BTreeMap;

use crate::equations::{
    Dim, Eq, Layout, Row, Source, coincident_classes, cross, dot, lower, norm, sub,
};
use crate::linalg::{Basis, Mat, damped_step, rank_analysis};
use crate::model::{Constraint, Entity, EntityId, Sketch};
use crate::report::{Dependency, FreeCoordinate, Orientation, Slot, Solution, SolveError, Status};
use crate::trig;

/// Settings for [`Sketch::solve_with`].
#[derive(Clone, Copy)]
pub struct SolveOptions<'a> {
    /// Iterations per Levenberg–Marquardt run (each phase, and each
    /// continuation step, is a run). Default 100.
    pub max_iterations: u32,
    /// Refuse sketches with more unknowns than this, before any work: the
    /// factorisations are O(n³). Default: no limit.
    pub max_unknowns: usize,
    /// Polled between iterations; returning `true` stops the solve with
    /// [`SolveError::Interrupted`].
    pub interrupt: Option<&'a dyn Fn() -> bool>,
}

impl Default for SolveOptions<'_> {
    fn default() -> Self {
        SolveOptions {
            max_iterations: 100,
            max_unknowns: usize::MAX,
            interrupt: None,
        }
    }
}

impl std::fmt::Debug for SolveOptions<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SolveOptions")
            .field("max_iterations", &self.max_iterations)
            .field("max_unknowns", &self.max_unknowns)
            .field("interrupt", &self.interrupt.is_some())
            .finish()
    }
}

/// Relative residual tolerance: a sketch is solved when every residual is
/// within this times its size.
const TOLERANCE: f64 = 1e-10;
/// Rank tolerance on unit-normalised Jacobian rows.
const RANK_TOLERANCE: f64 = 1e-8;
/// A dependent equation whose residual is not the same combination of the
/// others' (within this times the size) contradicts them.
const CONFLICT_TOLERANCE: f64 = 1e-7;
/// Signs drawn closer to zero than this (sine of the angle) are not
/// recorded: a nearly straight corner or a half-circle arc can legitimately
/// come out either way.
const SIGN_THRESHOLD: f64 = 1e-3;
/// Continuation gives up when its step falls below 1/2^10 of the way.
const MIN_STEP: f64 = 1.0 / 1024.0;
/// Continuation steps of at least this fraction must converge without
/// backtracking to be taken.
const SMOOTH_STEP: f64 = 1.0 / 16.0;
/// Polishing steps after convergence (see `Solver::polish`).
const POLISH_STEPS: u32 = 4;
/// Continuation's iteration budget, all steps together.
const CONTINUATION_ITERATIONS: u32 = 500;

impl Sketch {
    /// Solve with the default [`SolveOptions`].
    pub fn solve(&self) -> Solution {
        self.solve_with(&SolveOptions::default())
            .expect("no limit or interrupt was set")
    }

    /// Solve the sketch. The sketch is not changed, and the result depends
    /// only on the sketch: never on an earlier solve, the platform or the
    /// thread.
    pub fn solve_with(&self, opt: &SolveOptions<'_>) -> Result<Solution, SolveError> {
        Solver::new(self, opt)?.run()
    }

    /// Which of `candidates` would complete the constraints at `sol` (a
    /// solution of this sketch): taken in order, each candidate whose
    /// equations are all independent of the sketch's and of the
    /// candidates already taken, until no degree of freedom is left. The
    /// indices of the candidates taken come back in order.
    ///
    /// This is how a host turns "these coordinates can still move" into
    /// constraints to suggest: it proposes candidates in the order it
    /// prefers (a dimension before a fixed coordinate, say), measured on
    /// `sol` so that each holds there, and this keeps the ones that each
    /// remove a degree of freedom. Each taken candidate on its own is also
    /// independent of the sketch's equations, so a host may offer them one
    /// by one. Only the equations' gradients at `sol` count, not their
    /// targets, so a candidate's value only needs to be close to what it
    /// measures. A candidate the model refuses (a wrong kind of entity) is
    /// not taken.
    ///
    /// The cost is a rank-one update per equation, O(equations × n²) for n
    /// unknowns. `opt.interrupt` is polled per equation, and
    /// `opt.max_unknowns` applies as in [`Sketch::solve_with`].
    pub fn completion(
        &self,
        sol: &Solution,
        candidates: &[Constraint],
        opt: &SolveOptions<'_>,
    ) -> Result<Vec<usize>, SolveError> {
        let lay = Layout::new(self);
        let n = lay.len();
        if n > opt.max_unknowns {
            return Err(SolveError::TooManyUnknowns {
                unknowns: n,
                limit: opt.max_unknowns,
            });
        }
        if sol.values.len() != n {
            return Ok(Vec::new());
        }
        let mut all = self.clone();
        let base = self.constraints.len();
        // Each candidate's constraint index in `all`, if the model took it.
        let mut index = Vec::with_capacity(candidates.len());
        for c in candidates {
            index.push(all.add(c.clone()).ok().map(|id| id.index()));
        }
        let rows = lower(&all, &lay, &sol.values);
        let interrupted = || opt.interrupt.is_some_and(|f| f());
        let mut basis = Basis::new(n, RANK_TOLERANCE);
        let mut g = Vec::new();
        let mut dense = vec![0.0; n];
        let mut unit = |row: &Row, out: &mut Vec<f64>| -> bool {
            g.clear();
            row.eval(&sol.values, sol.size, &mut g);
            out.iter_mut().for_each(|e| *e = 0.0);
            for &(v, d) in &g {
                out[v] += d;
            }
            let len = sum_squares(out).sqrt();
            if !(len > 0.0 && len.is_finite()) {
                return false;
            }
            out.iter_mut().for_each(|e| *e /= len);
            true
        };
        let mut by_constraint: Vec<Vec<usize>> = vec![Vec::new(); all.constraints.len()];
        for (i, r) in rows.iter().enumerate() {
            match r.source {
                Source::Constraint(c) if c.index() >= base => by_constraint[c.index()].push(i),
                _ => {
                    if interrupted() {
                        return Err(SolveError::Interrupted);
                    }
                    if unit(r, &mut dense) {
                        basis.push(&dense);
                    }
                }
            }
        }
        let mut taken = Vec::new();
        for (k, at) in index.iter().enumerate() {
            if basis.rank() == n {
                break;
            }
            let Some(at) = *at else { continue };
            let before = basis.rank();
            let mut all_in = !by_constraint[at].is_empty();
            for &ri in &by_constraint[at] {
                if interrupted() {
                    return Err(SolveError::Interrupted);
                }
                if !(unit(&rows[ri], &mut dense) && basis.push(&dense)) {
                    all_in = false;
                    break;
                }
            }
            if all_in {
                taken.push(k);
            } else {
                basis.truncate(before);
            }
        }
        Ok(taken)
    }
}

/// A connected group of unknowns and the equations between them.
#[derive(Debug)]
struct Component {
    rows: Vec<usize>,
    vars: Vec<usize>,
}

/// A signature item: something whose sign the solution should share with
/// the drawing.
#[derive(Clone, Copy, Debug)]
struct Signed {
    what: Orientation,
    /// The drawing's sign (+1 for items that must simply be positive).
    sign: f64,
    probe: Probe,
}

/// What to measure for a signature item. Points are first unknowns.
#[derive(Clone, Copy, Debug)]
enum Probe {
    /// cross(p1 − p0, q1 − q0).
    Cross([usize; 4]),
    /// dot(p1 − p0, q1 − q0).
    Dot([usize; 4]),
    /// Which side of the line a→b the point p is on: [p, a, b].
    Side([usize; 3]),
    /// An unknown's value.
    Var(usize),
    /// An angle row's alignment (see `Row::angle_alignment`).
    Angle(usize),
}

impl Probe {
    /// An unknown it reads, to find its component.
    fn var(&self, rows: &[Row]) -> usize {
        match *self {
            Probe::Cross([p, ..]) | Probe::Dot([p, ..]) | Probe::Side([p, ..]) | Probe::Var(p) => p,
            Probe::Angle(ri) => {
                let mut v = Vec::new();
                rows[ri].vars(&mut v);
                v[0]
            }
        }
    }
}

struct Solver<'s, 'o> {
    sketch: &'s Sketch,
    opt: &'s SolveOptions<'o>,
    lay: Layout,
    rows: Vec<Row>,
    x: Vec<f64>,
    size: f64,
    tol: f64,
    placed: Vec<EntityId>,
    signature: Vec<Signed>,
    iterations: u32,
    /// Scratch: global unknown → local column (per component).
    local: Vec<usize>,
}

struct Run {
    converged: bool,
    /// Steps that raised the cost and were taken back: the linear model
    /// was poor somewhere on the way, which is where a solve can jump to
    /// another branch without any recorded sign changing.
    rejected: u32,
}

impl<'s, 'o> Solver<'s, 'o> {
    fn new(sketch: &'s Sketch, opt: &'s SolveOptions<'o>) -> Result<Self, SolveError> {
        let lay = Layout::new(sketch);
        if lay.len() > opt.max_unknowns {
            return Err(SolveError::TooManyUnknowns {
                unknowns: lay.len(),
                limit: opt.max_unknowns,
            });
        }
        let (x, size, placed) = initial(sketch, &lay);
        let rows = lower(sketch, &lay, &x);
        let n = lay.len();
        let mut s = Solver {
            sketch,
            opt,
            lay,
            rows,
            x,
            size,
            tol: TOLERANCE * size,
            placed,
            signature: Vec::new(),
            iterations: 0,
            local: vec![usize::MAX; n],
        };
        s.signature = s.signature_of_drawing();
        Ok(s)
    }

    fn interrupted(&self) -> bool {
        self.opt.interrupt.is_some_and(|f| f())
    }

    /// Connected components of the unknown–equation graph, ordered by
    /// their lowest unknown (so by lowest entity id).
    fn components(&self) -> Vec<Component> {
        let n = self.lay.len();
        let mut parent: Vec<usize> = (0..n).collect();
        fn find(p: &mut [usize], mut i: usize) -> usize {
            while p[i] != i {
                p[i] = p[p[i]];
                i = p[i];
            }
            i
        }
        let mut vars = Vec::new();
        for r in &self.rows {
            vars.clear();
            r.vars(&mut vars);
            for w in vars.windows(2) {
                let (a, b) = (find(&mut parent, w[0]), find(&mut parent, w[1]));
                if a != b {
                    // The lower index becomes the root, so roots are the
                    // components' lowest unknowns.
                    let (lo, hi) = if a < b { (a, b) } else { (b, a) };
                    parent[hi] = lo;
                }
            }
        }
        let mut by_root: BTreeMap<usize, Component> = BTreeMap::new();
        for (i, r) in self.rows.iter().enumerate() {
            vars.clear();
            r.vars(&mut vars);
            let root = find(&mut parent, vars[0]);
            by_root
                .entry(root)
                .or_insert_with(|| Component {
                    rows: Vec::new(),
                    vars: Vec::new(),
                })
                .rows
                .push(i);
        }
        for v in 0..n {
            let root = find(&mut parent, v);
            if let Some(c) = by_root.get_mut(&root) {
                c.vars.push(v);
            }
        }
        by_root.into_values().collect()
    }

    fn run(mut self) -> Result<Solution, SolveError> {
        let comps = self.components();
        let mut converged = vec![false; comps.len()];
        let mut continuation = false;
        let mut flipped = Vec::new();
        for (ci, comp) in comps.iter().enumerate() {
            for (k, &v) in comp.vars.iter().enumerate() {
                self.local[v] = k;
            }
            let (ok, cont, flips) = self.solve_component(comp)?;
            converged[ci] = ok;
            continuation |= cont;
            flipped.extend(flips);
        }
        flipped.sort();
        flipped.dedup();
        let all = converged.iter().all(|c| *c);
        self.exactify(&comps, &converged);
        for v in &mut self.x {
            // −0 prints as "-0" and hashes differently; nothing here means
            // a signed zero.
            if *v == 0.0 {
                *v = 0.0;
            }
        }
        Ok(self.diagnose(&comps, all, continuation, flipped))
    }

    /// The residuals of a component's rows at the current `x`.
    fn residuals(&self, comp: &Component, out: &mut Vec<f64>) {
        out.clear();
        let mut g = Vec::new();
        for &r in &comp.rows {
            g.clear();
            out.push(self.rows[r].eval(&self.x, self.size, &mut g));
        }
    }

    fn jacobian(&self, comp: &Component) -> (Mat, Vec<f64>) {
        let mut j = Mat::zeros(comp.rows.len(), comp.vars.len());
        let mut r = Vec::with_capacity(comp.rows.len());
        let mut g = Vec::new();
        for (i, &ri) in comp.rows.iter().enumerate() {
            g.clear();
            r.push(self.rows[ri].eval(&self.x, self.size, &mut g));
            for &(v, d) in &g {
                j.add(i, self.local[v], d);
            }
        }
        (j, r)
    }

    /// Levenberg–Marquardt on one component from the current `x`, with a
    /// fixed damping schedule (÷10 on success, ×10 on failure), so the
    /// iterations, like everything else, depend only on the input.
    fn levenberg_marquardt(&mut self, comp: &Component) -> Result<Run, SolveError> {
        let mut r = Vec::new();
        self.residuals(comp, &mut r);
        let mut cost = sum_squares(&r);
        let mut mu = 1e-3;
        let mut trial = Vec::new();
        let mut it = 0;
        let mut rejected = 0;
        loop {
            if max_abs(&r) <= self.tol {
                self.polish(comp, &mut r, &mut cost);
                return Ok(Run {
                    converged: true,
                    rejected,
                });
            }
            if it >= self.opt.max_iterations {
                break;
            }
            if self.interrupted() {
                return Err(SolveError::Interrupted);
            }
            it += 1;
            self.iterations += 1;
            let (j, _) = self.jacobian(comp);
            let step = damped_step(&j, &r, mu);
            let saved: Vec<f64> = comp.vars.iter().map(|&v| self.x[v]).collect();
            for (k, &v) in comp.vars.iter().enumerate() {
                self.x[v] += step[k];
            }
            self.residuals(comp, &mut trial);
            let new_cost = sum_squares(&trial);
            if new_cost < cost {
                std::mem::swap(&mut r, &mut trial);
                let shrink = cost - new_cost;
                cost = new_cost;
                mu = (mu * 0.1).max(1e-15);
                // Stalled: a tiny step that gained nothing measurable is a
                // least-squares minimum that is not a solution.
                if shrink <= 1e-30 * self.size * self.size && max_abs(&step) <= 1e-15 * self.size {
                    break;
                }
            } else {
                for (k, &v) in comp.vars.iter().enumerate() {
                    self.x[v] = saved[k];
                }
                rejected += 1;
                mu *= 10.0;
                if mu > 1e12 {
                    break;
                }
            }
        }
        Ok(Run {
            converged: max_abs(&r) <= self.tol,
            rejected,
        })
    }

    /// A few more nearly undamped steps once within tolerance, while each
    /// at least halves the cost. Gauss–Newton converges quadratically
    /// there, so this takes a solution from 1e-10 of the size to rounding
    /// level in a step or two; without it, a dimension of 30 would come out
    /// as 30.000000000329.
    fn polish(&mut self, comp: &Component, r: &mut Vec<f64>, cost: &mut f64) {
        let mut trial = Vec::new();
        for _ in 0..POLISH_STEPS {
            if *cost == 0.0 {
                return;
            }
            self.iterations += 1;
            let (j, _) = self.jacobian(comp);
            let step = damped_step(&j, r, 1e-15);
            let saved: Vec<f64> = comp.vars.iter().map(|&v| self.x[v]).collect();
            for (k, &v) in comp.vars.iter().enumerate() {
                self.x[v] += step[k];
            }
            self.residuals(comp, &mut trial);
            let new_cost = sum_squares(&trial);
            if new_cost <= 0.5 * *cost && max_abs(&trial) <= self.tol {
                std::mem::swap(r, &mut trial);
                *cost = new_cost;
            } else {
                for (k, &v) in comp.vars.iter().enumerate() {
                    self.x[v] = saved[k];
                }
                return;
            }
        }
    }

    fn set_targets(&mut self, comp: &Component, measured: &[Option<f64>], lambda: f64) {
        for (k, &ri) in comp.rows.iter().enumerate() {
            let row = &mut self.rows[ri];
            let Some(m) = measured[k] else { continue };
            let goal = row.goal;
            let t = if lambda >= 1.0 {
                goal
            } else if lambda <= 0.0 {
                m
            } else {
                let mut delta = goal - m;
                if matches!(row.dim, Dim::Angle | Dim::Sweep) {
                    // The short way round.
                    delta %= 360.0;
                    if delta > 180.0 {
                        delta -= 360.0;
                    } else if delta <= -180.0 {
                        delta += 360.0;
                    }
                }
                m + lambda * delta
            };
            row.set_target(t);
        }
    }

    /// Solve one component; returns (converged, continuation ran, flips).
    fn solve_component(
        &mut self,
        comp: &Component,
    ) -> Result<(bool, bool, Vec<Orientation>), SolveError> {
        let measured: Vec<Option<f64>> = comp
            .rows
            .iter()
            .map(|&r| self.rows[r].measure(&self.x, self.size))
            .collect();
        // 1. Clean the drawing: every dimension at its drawn value, so only
        //    the geometric constraints move points, and minimal-norm steps
        //    leave everything else where it was drawn.
        let drawing: Vec<f64> = comp.vars.iter().map(|&v| self.x[v]).collect();
        self.set_targets(comp, &measured, 0.0);
        let clean = self.levenberg_marquardt(comp)?;
        if !clean.converged || !self.flips(comp).is_empty() {
            // The drawn dimensions need not be consistent with the geometric
            // constraints when the drawing does not meet them (a point drawn
            // further from a line than a circle it must lie on reaches). The
            // least-squares compromise is then a poor start, often on another
            // branch, as is a cleaned drawing that already flipped; the
            // drawing itself is a better one.
            for (k, &v) in comp.vars.iter().enumerate() {
                self.x[v] = drawing[k];
            }
        }
        let cleaned: Vec<f64> = comp.vars.iter().map(|&v| self.x[v]).collect();
        // 2. Solve for the real values from the cleaned drawing.
        self.set_targets(comp, &measured, 1.0);
        let direct = self.levenberg_marquardt(comp)?;
        let flips = self.flips(comp);
        // A direct solve that never had to take a step back went smoothly
        // downhill from the drawing. One that did may have jumped to
        // another solution in a way no recorded sign shows (a point on a
        // circle crossing to the far side of its centre), so it is checked
        // against continuation too.
        if direct.converged && flips.is_empty() && direct.rejected == 0 {
            return Ok((true, false, flips));
        }
        let direct_x: Vec<f64> = comp.vars.iter().map(|&v| self.x[v]).collect();
        // 3. Continuation: walk the targets from drawn to wanted, each step
        //    from the last, which follows the drawing's branch.
        for (k, &v) in comp.vars.iter().enumerate() {
            self.x[v] = cleaned[k];
        }
        let (mut lambda, mut h) = (0.0f64, 0.5f64);
        let start = self.iterations;
        let mut done = false;
        while h >= MIN_STEP && self.iterations - start < CONTINUATION_ITERATIONS {
            let next = (lambda + h).min(1.0);
            let before: Vec<f64> = comp.vars.iter().map(|&v| self.x[v]).collect();
            self.set_targets(comp, &measured, next);
            let run = self.levenberg_marquardt(comp)?;
            // Steps short enough are taken even if their solve backtracked:
            // some sketches backtrack at any step size.
            let smooth = run.rejected == 0 || h < SMOOTH_STEP;
            if run.converged && smooth && self.flips(comp).is_empty() {
                lambda = next;
                if lambda >= 1.0 {
                    done = true;
                    break;
                }
                h = (h * 2.0).min(1.0);
            } else {
                for (k, &v) in comp.vars.iter().enumerate() {
                    self.x[v] = before[k];
                }
                h *= 0.5;
            }
        }
        self.set_targets(comp, &measured, 1.0);
        if done {
            return Ok((true, true, Vec::new()));
        }
        // Continuation failed: keep the direct result, flips and all.
        for (k, &v) in comp.vars.iter().enumerate() {
            self.x[v] = direct_x[k];
        }
        Ok((direct.converged, true, flips))
    }

    /// The orientation items, measured on the drawing (the initial `x`).
    fn signature_of_drawing(&self) -> Vec<Signed> {
        let s = self.sketch;
        let pt = |p: EntityId| self.lay.pt(p);
        let mut items: Vec<(Orientation, Probe, bool)> = Vec::new();
        // Arcs with a sweep constraint may legitimately cross 180°, and
        // lines in an angle constraint together may legitimately turn the
        // other way; neither is recorded.
        let mut swept = Vec::new();
        let mut angled = Vec::new();
        for c in &s.constraints {
            match c {
                Constraint::Sweep { arc, .. } => swept.push(*arc),
                Constraint::Angle { from, to, .. } => {
                    angled.push((*from, *to));
                    angled.push((*to, *from));
                }
                _ => {}
            }
        }
        for (i, e) in s.entities.iter().enumerate() {
            let id = EntityId::from_index(i);
            match *e {
                Entity::Arc {
                    center, start, end, ..
                } if !swept.contains(&id) => items.push((
                    Orientation::ArcSweep(id),
                    Probe::Cross([pt(center), pt(start), pt(center), pt(end)]),
                    true,
                )),
                Entity::Circle { .. } => items.push((
                    Orientation::RadiusSign(id),
                    Probe::Var(self.lay.var[i]),
                    false,
                )),
                _ => {}
            }
        }
        for (ri, r) in self.rows.iter().enumerate() {
            let Source::Constraint(c) = r.source else {
                continue;
            };
            match r.eq {
                Eq::Angle { .. } => {
                    items.push((Orientation::AngleBranch(c), Probe::Angle(ri), false))
                }
                // Endpoint tangency: which side of the line the arc's
                // centre is on, and whether two arcs' centres are on the
                // same side of their shared point (inside) or not.
                Eq::Dot { a1, b1, a2, .. }
                    if matches!(s.constraints[c.index()], Constraint::Tangent(..)) =>
                {
                    items.push((Orientation::TangentSide(c), Probe::Side([a2, a1, b1]), true))
                }
                Eq::Cross { a1, b1, a2, b2 }
                    if matches!(s.constraints[c.index()], Constraint::Tangent(..)) =>
                {
                    items.push((
                        Orientation::TangentSide(c),
                        Probe::Dot([a1, b1, a2, b2]),
                        true,
                    ))
                }
                _ => {}
            }
        }
        // Corners: two lines with an end at the same point (the same
        // entity, or points made coincident).
        let class = coincident_classes(s);
        // (corner, line, the line's point there, its other end)
        let mut line_ends: Vec<(usize, EntityId, EntityId, EntityId)> = Vec::new();
        for (i, e) in s.entities.iter().enumerate() {
            if let Entity::Line { start, end } = *e {
                let id = EntityId::from_index(i);
                line_ends.push((class[start.index()], id, start, end));
                line_ends.push((class[end.index()], id, end, start));
            }
        }
        line_ends.sort_by_key(|e| (e.0, e.1));
        for (i, a) in line_ends.iter().enumerate() {
            for b in &line_ends[i + 1..] {
                if b.0 != a.0 {
                    break;
                }
                if a.1 == b.1 || angled.contains(&(a.1, b.1)) {
                    continue;
                }
                items.push((
                    Orientation::Corner {
                        point: a.2,
                        lines: [a.1, b.1],
                    },
                    Probe::Cross([pt(a.2), pt(a.3), pt(b.2), pt(b.3)]),
                    true,
                ));
            }
        }
        let mut out = Vec::new();
        for (what, probe, from_drawing) in items {
            let sign = if from_drawing {
                // Only clear signs: a straight corner or a half-circle arc
                // can legitimately come out either way.
                let (v, scale) = self.probe(probe);
                if v.abs() < SIGN_THRESHOLD * scale {
                    continue;
                }
                v.signum()
            } else {
                1.0
            };
            out.push(Signed { what, sign, probe });
        }
        out
    }

    /// A probe's value at the current `x`, and the scale to compare it
    /// with (the product of the vectors' lengths, for cross and dot).
    fn probe(&self, probe: Probe) -> (f64, f64) {
        let x = &self.x;
        let at = |p: usize| [x[p], x[p + 1]];
        match probe {
            Probe::Cross([p0, p1, q0, q1]) | Probe::Dot([p0, p1, q0, q1]) => {
                let u = sub(at(p1), at(p0));
                let v = sub(at(q1), at(q0));
                let value = if matches!(probe, Probe::Cross(_)) {
                    cross(u, v)
                } else {
                    dot(u, v)
                };
                (value, norm(u) * norm(v))
            }
            Probe::Side([p, a, b]) => {
                let d = sub(at(b), at(a));
                let w = sub(at(p), at(a));
                (cross(d, w), norm(d) * norm(w))
            }
            Probe::Var(i) => (x[i], x[i].abs()),
            Probe::Angle(ri) => (self.rows[ri].angle_alignment(x).unwrap_or(1.0), 1.0),
        }
    }

    /// The signature items of `comp` whose sign is clearly the opposite of
    /// the drawing's at the current `x`. Reaching zero is not a flip: a
    /// drawn 160° arc that solves to exactly 180° (tangent to two parallel
    /// lines, as in a slot) has not crossed over.
    fn flips(&self, comp: &Component) -> Vec<Orientation> {
        let mut out = Vec::new();
        for item in &self.signature {
            if comp
                .vars
                .binary_search(&item.probe.var(&self.rows))
                .is_err()
            {
                continue;
            }
            let (v, scale) = self.probe(item.probe);
            if v * item.sign < -SIGN_THRESHOLD * scale || (scale == 0.0 && v == 0.0) {
                out.push(item.what);
            }
        }
        out
    }

    /// Snap what has a closed form: coordinates that equations say are
    /// equal (coincident, horizontal, vertical) get the same bits, and
    /// fixed coordinates and circle radii with a radius constraint get
    /// their exact values. Reverted if it would leave a residual over the
    /// tolerance. Without it, profiles carry 1e-17 noise into exports.
    fn exactify(&mut self, comps: &[Component], converged: &[bool]) {
        let n = self.lay.len();
        let mut ok = vec![false; n];
        for (c, conv) in comps.iter().zip(converged) {
            if *conv {
                for &v in &c.vars {
                    ok[v] = true;
                }
            }
        }
        let mut parent: Vec<usize> = (0..n).collect();
        fn find(p: &mut [usize], mut i: usize) -> usize {
            while p[i] != i {
                p[i] = p[p[i]];
                i = p[i];
            }
            i
        }
        let mut exact: Vec<Option<f64>> = vec![None; n];
        let mut joins = Vec::new();
        for r in &self.rows {
            match &r.eq {
                Eq::Lin(t)
                    if t.len() == 2
                        && t[0].1 == 1.0
                        && t[1].1 == -1.0
                        && (r.dim == Dim::None || r.goal == 0.0) =>
                {
                    joins.push((t[0].0, t[1].0));
                }
                Eq::Lin(t) if t.len() == 1 && t[0].1 == 1.0 && r.dim == Dim::Linear => {
                    let v = t[0].0;
                    exact[v].get_or_insert(r.goal);
                }
                Eq::RadiusValue {
                    r: crate::equations::Radius::Var(v),
                } => {
                    exact[*v].get_or_insert(r.goal * r.tcoef);
                }
                _ => {}
            }
        }
        for (a, b) in joins {
            if ok[a] && ok[b] {
                let (ra, rb) = (find(&mut parent, a), find(&mut parent, b));
                let (lo, hi) = if ra < rb { (ra, rb) } else { (rb, ra) };
                parent[hi] = lo;
            }
        }
        let mut value: Vec<Option<f64>> = vec![None; n];
        for v in 0..n {
            if !ok[v] {
                continue;
            }
            let root = find(&mut parent, v);
            if let Some(e) = exact[v] {
                // The first fixed value in the class, by unknown order.
                value[root].get_or_insert(e);
            }
        }
        let before = self.x.clone();
        for (v, &snap) in ok.iter().enumerate() {
            if snap {
                let root = find(&mut parent, v);
                self.x[v] = value[root].unwrap_or(before[root]);
            }
        }
        let mut g = Vec::new();
        let worst = self.rows.iter().fold(0.0f64, |m, r| {
            g.clear();
            worse(m, r.eval(&self.x, self.size, &mut g))
        });
        let worst_before = self.rows.iter().fold(0.0f64, |m, r| {
            g.clear();
            worse(m, r.eval(&before, self.size, &mut g))
        });
        if worst > self.tol && worst > worst_before {
            self.x = before;
        }
    }

    fn diagnose(
        &self,
        comps: &[Component],
        all: bool,
        continuation: bool,
        flipped: Vec<Orientation>,
    ) -> Solution {
        let n = self.lay.len();
        let mut rank = 0;
        let mut free = Vec::new();
        let mut redundant: BTreeMap<Source, Vec<Source>> = BTreeMap::new();
        let mut conflicts: BTreeMap<Source, Vec<Source>> = BTreeMap::new();
        let mut covered = vec![false; n];
        let mut residual = 0.0f64;
        let mut unmet: BTreeMap<Source, f64> = BTreeMap::new();
        let mut local = vec![usize::MAX; n];
        let mut g = Vec::new();
        for comp in comps {
            for (k, &v) in comp.vars.iter().enumerate() {
                local[v] = k;
                covered[v] = true;
            }
            let nc = comp.vars.len();
            let mut rows = Vec::with_capacity(comp.rows.len());
            let mut res = Vec::with_capacity(comp.rows.len());
            for &ri in &comp.rows {
                g.clear();
                let f = self.rows[ri].eval(&self.x, self.size, &mut g);
                residual = worse(residual, f);
                if worse(0.0, f) > self.tol {
                    let e = unmet.entry(self.rows[ri].source).or_insert(0.0);
                    *e = worse(*e, f);
                }
                let mut row = vec![0.0; nc];
                for &(v, d) in &g {
                    row[local[v]] += d;
                }
                let len = sum_squares(&row).sqrt();
                if len > 0.0 {
                    row.iter_mut().for_each(|e| *e /= len);
                    res.push(f / len);
                } else {
                    // No gradient (a degenerate configuration, such as a
                    // zero-length line): dependent on nothing, and in
                    // conflict if unmet.
                    res.push(f);
                }
                rows.push(row);
            }
            let a = rank_analysis(&rows, nc, RANK_TOLERANCE);
            rank += a.rank;
            for (k, &v) in comp.vars.iter().enumerate() {
                if a.mobility[k] > 1e-6 {
                    let (entity, coordinate) = self.lay.owner[v];
                    free.push(FreeCoordinate {
                        entity,
                        coordinate,
                        mobility: a.mobility[k],
                    });
                }
            }
            for (k, coef) in &a.dependent {
                let mut combo = res[*k];
                for &(j, c) in coef {
                    combo -= c * res[a.independent[j]];
                }
                let source = self.rows[comp.rows[*k]].source;
                let mut with: Vec<Source> = coef
                    .iter()
                    .map(|&(j, _)| self.rows[comp.rows[a.independent[j]]].source)
                    .filter(|s| *s != source)
                    .collect();
                let target = if combo.abs() > CONFLICT_TOLERANCE * self.size {
                    &mut conflicts
                } else {
                    &mut redundant
                };
                let entry = target.entry(source).or_default();
                entry.append(&mut with);
                entry.sort();
                entry.dedup();
            }
        }
        for (v, &seen) in covered.iter().enumerate() {
            if !seen {
                let (entity, coordinate) = self.lay.owner[v];
                free.push(FreeCoordinate {
                    entity,
                    coordinate,
                    mobility: 1.0,
                });
            }
        }
        free.sort_by_key(|f| {
            self.lay.var[f.entity.index()] + usize::from(f.coordinate == crate::Coordinate::Y)
        });
        // A conflicting equation is not also redundant.
        redundant.retain(|s, _| !conflicts.contains_key(s));
        let deps = |m: BTreeMap<Source, Vec<Source>>| {
            m.into_iter()
                .map(|(source, with)| Dependency { source, with })
                .collect()
        };
        let slots = self
            .sketch
            .entities
            .iter()
            .enumerate()
            .map(|(i, e)| match *e {
                Entity::Point { .. } => Slot::Point(self.lay.var[i]),
                Entity::Circle { .. } => Slot::Circle(self.lay.var[i]),
                Entity::Arc { center, start, .. } => Slot::Arc {
                    c: self.lay.pt(center),
                    s: self.lay.pt(start),
                },
                Entity::Line { .. } => Slot::Line,
            })
            .collect();
        let mut unmet: Vec<(Source, f64)> = unmet.into_iter().collect();
        // Worst first; equal residuals in source order (the sort is
        // stable), so the order is the same everywhere.
        unmet.sort_by(|a, b| b.1.total_cmp(&a.1));
        Solution {
            status: if all && residual <= self.tol {
                Status::Solved
            } else {
                Status::NotConverged
            },
            unknowns: n,
            equations: self.rows.len(),
            rank,
            dof: n - rank,
            free,
            redundant: deps(redundant),
            conflicts: deps(conflicts),
            flipped,
            unmet,
            placed: self.placed.clone(),
            iterations: self.iterations,
            residual,
            tolerance: self.tol,
            size: self.size,
            continuation,
            values: self.x.clone(),
            slots,
        }
    }
}

fn sum_squares(v: &[f64]) -> f64 {
    let mut s = 0.0;
    for x in v {
        s += x * x;
    }
    s
}

fn max_abs(v: &[f64]) -> f64 {
    v.iter().fold(0.0f64, |m, x| worse(m, *x))
}

/// The larger of `m` and |r|, where a NaN residual counts as infinitely
/// large. `f64::max` returns the other operand for NaN, which would let a
/// sketch whose arithmetic broke down (an overflow from absurd values)
/// count as solved.
fn worse(m: f64, r: f64) -> f64 {
    if r.is_nan() {
        f64::INFINITY
    } else {
        m.max(r.abs())
    }
}

/// The starting values: guesses, and deterministic places for points and
/// circles without one. Also the sketch's size.
fn initial(s: &Sketch, lay: &Layout) -> (Vec<f64>, f64, Vec<EntityId>) {
    let mut x = vec![0.0; lay.len()];
    let (mut lo, mut hi) = ([f64::INFINITY; 2], [f64::NEG_INFINITY; 2]);
    let mut sum = [0.0, 0.0];
    let mut count = 0usize;
    for e in &s.entities {
        if let Entity::Point { guess: Some(g) } = e {
            for k in 0..2 {
                lo[k] = lo[k].min(g[k]);
                hi[k] = hi[k].max(g[k]);
                sum[k] += g[k];
            }
            count += 1;
        }
    }
    let diag = if count > 0 { norm(sub(hi, lo)) } else { 0.0 };
    let mut largest = 0.0f64;
    for c in &s.constraints {
        let v = match c {
            Constraint::Distance { value, .. }
            | Constraint::Length { value, .. }
            | Constraint::Radius { value, .. }
            | Constraint::Diameter { value, .. } => value.abs(),
            _ => 0.0,
        };
        largest = largest.max(v);
    }
    // A drawing too large to measure (the diagonal overflows) still gets a
    // finite size, so its tolerance is finite and an overflowing residual
    // cannot pass it.
    let size = match diag.max(largest) {
        s if s > 0.0 && s.is_finite() => s,
        s if s > 0.0 => f64::MAX,
        _ => 1.0,
    };
    let centre = if count > 0 {
        [sum[0] / count as f64, sum[1] / count as f64]
    } else {
        [0.0, 0.0]
    };
    let unguessed = s
        .entities
        .iter()
        .filter(|e| matches!(e, Entity::Point { guess: None }))
        .count();
    let mut placed = Vec::new();
    let mut k = 0usize;
    for (i, e) in s.entities.iter().enumerate() {
        let id = EntityId::from_index(i);
        match e {
            Entity::Point { guess: Some(g) } => {
                x[lay.var[i]] = g[0];
                x[lay.var[i] + 1] = g[1];
            }
            Entity::Point { guess: None } => {
                // A sunflower spiral around the drawing's centre: no two
                // placed points coincide or line up with the centre, which
                // would make lines of no length or arcs of no radius.
                const GOLDEN_ANGLE: f64 = 2.399_963_229_728_653;
                let (c, sn) = trig::cos_sin(GOLDEN_ANGLE * (k as f64 + 1.0));
                let rho = 0.5 * size * ((k as f64 + 1.0) / unguessed as f64).sqrt();
                x[lay.var[i]] = centre[0] + rho * c;
                x[lay.var[i] + 1] = centre[1] + rho * sn;
                placed.push(id);
                k += 1;
            }
            Entity::Circle { radius_guess, .. } => {
                x[lay.var[i]] = match radius_guess {
                    Some(r) => *r,
                    None => {
                        placed.push(id);
                        s.constraints
                            .iter()
                            .find_map(|c| match c {
                                Constraint::Radius { curve, value } if *curve == id => Some(*value),
                                Constraint::Diameter { curve, value } if *curve == id => {
                                    Some(0.5 * value)
                                }
                                _ => None,
                            })
                            .unwrap_or(0.25 * size)
                    }
                };
            }
            _ => {}
        }
    }
    placed.sort();
    (x, size, placed)
}
