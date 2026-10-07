//! NeoSCAD's constrained sketches (`--enable sketch`;
//! `docs/language-extensions.md`, section 4): the language binding of the
//! `sketch-solver` crate.
//!
//! ```openscad
//! sketch(name = "slot") {
//!   c1 = point([0, 0]);          // entities: assignments
//!   axis = line(c1, [30, 0]);
//!   fix(c1); horizontal(axis);   // constraints: statements
//!   length(axis, 30);
//! }
//! ```
//!
//! How it runs:
//!
//! - `sketch()` is a builtin module (in the table only with the flag on,
//!   like `part`). While its body runs, the evaluator holds a [`Builder`]:
//!   the solver's model plus, on NeoSCAD's side, where each entity and
//!   constraint came from (spans and variable names; the solver knows only
//!   ids).
//! - The vocabulary (`point`, `line`, `arc`, `circle` and the constraint
//!   statements) is bound only inside sketch bodies, by the resolver
//!   (`resolve`, `Env::vocab`), so BOSL2's `arc()` or MCAD's `distance()`
//!   keep their meaning everywhere else.
//! - Entities are functions returning a handle ([`Entity`], a new kind of
//!   value). After the body's assignments ran, each entity made directly
//!   by an assignment takes the variable's name as its label, for
//!   messages and printing.
//! - Constraints are statements that add equations and make no node.
//! - A `sketch()` met while a sketch is being built (a helper module whose
//!   body is a `sketch()`, called from a sketch body) adds to the
//!   enclosing sketch instead of starting its own: that is how constraint
//!   patterns are reused.
//! - When the outermost body ends, the model is solved, the diagnosis is
//!   printed with the constraints' spans, fillets and chamfers are cut at
//!   their corners, and the profile's closed loops become a polygon
//!   (`NodeKind::Sketch`), tessellated by `circle()`'s rule.
//!
//! A sketch's errors leave an empty shape rather than stopping evaluation
//! (section 4.7), so the rest of the model still renders. Everything here
//! is single-threaded and deterministic: the solver uses only correctly
//! rounded arithmetic and never starts from an earlier solve, and the
//! tessellation uses `io::trig` as `circle()` does.

use std::cell::OnceCell;
use std::collections::HashMap;
use std::fmt;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use lang::diag::{DiagCode, Severity};
use lang::number::fmt_number;
use sketch_solver::{
    Along, Constraint, ConstraintId, Coordinate, EntityId, EntityKind, ModelError, Orientation,
    Pair, Sketch, Solution, SolveError, SolveOptions, Source, Status,
};

use crate::builtins::functions::Builtin;
use crate::builtins::modules::{BuiltinModule, Params};
use crate::call::ArgVal;
use crate::context::{Ctx, ScopeRef};
use crate::eval::Evaluator;
use crate::message::{Loc, R};
use crate::node::{Discretizer, Node, NodeKind, SketchNode, SketchReport};
use crate::sym::{FxBuild, Sym, Syms};
use crate::trig::{atan2_degrees, cos_degrees, sin_degrees};
use crate::value::Value;

/// Unknowns a sketch may have under a host's resource limits: the solver's
/// factorisations are O(n³) in time and O(n²) in memory, so a generated
/// sketch must be stopped before it starts (the design's
/// `Limits::sketch_unknowns`, which stage 3 makes a limit of its own).
const LIMITED_UNKNOWNS: usize = 5000;

/// A sketch statement: a constraint, a fillet or chamfer, or an entity
/// written as a statement by mistake.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Vocab {
    Coincident,
    On,
    Horizontal,
    Vertical,
    Parallel,
    Perpendicular,
    Tangent,
    Distance,
    Length,
    Radius,
    Diameter,
    Angle,
    Equal,
    Midpoint,
    Symmetric,
    Fix,
    Fillet,
    Chamfer,
    /// `circle(5);` (or `point`, `line`, `arc`) as a statement: an error
    /// with a hint, since in a sketch these are entities, assigned.
    Geometry,
}

/// The vocabulary's functions, by name.
const FUNCTIONS: [(&str, Builtin); 4] = [
    ("point", Builtin::SketchPoint),
    ("line", Builtin::SketchLine),
    ("arc", Builtin::SketchArc),
    ("circle", Builtin::SketchCircle),
];

/// The vocabulary's statements, by name, with their parameters.
const STATEMENTS: [(&str, Vocab, &[&str]); 22] = [
    ("coincident", Vocab::Coincident, &["a", "b"]),
    ("on", Vocab::On, &["p", "c"]),
    ("horizontal", Vocab::Horizontal, &["a", "b"]),
    ("vertical", Vocab::Vertical, &["a", "b"]),
    ("parallel", Vocab::Parallel, &["l1", "l2"]),
    ("perpendicular", Vocab::Perpendicular, &["l1", "l2"]),
    ("tangent", Vocab::Tangent, &["a", "b"]),
    ("distance", Vocab::Distance, &["a", "b", "d", "along"]),
    ("length", Vocab::Length, &["l", "d"]),
    ("radius", Vocab::Radius, &["c", "r"]),
    ("diameter", Vocab::Diameter, &["c", "d"]),
    ("angle", Vocab::Angle, &["l1", "l2", "deg"]),
    ("equal", Vocab::Equal, &["a", "b"]),
    ("midpoint", Vocab::Midpoint, &["p", "l"]),
    ("symmetric", Vocab::Symmetric, &["p", "q", "about"]),
    ("fix", Vocab::Fix, &["p", "at"]),
    ("fillet", Vocab::Fillet, &["corner", "r"]),
    ("chamfer", Vocab::Chamfer, &["corner", "d"]),
    ("point", Vocab::Geometry, &[]),
    ("line", Vocab::Geometry, &[]),
    ("arc", Vocab::Geometry, &[]),
    ("circle", Vocab::Geometry, &[]),
];

/// Every name the vocabulary binds inside a sketch body, functions first,
/// for the docs and tests.
pub fn vocabulary() -> impl Iterator<Item = (&'static str, bool)> {
    FUNCTIONS.iter().map(|(n, _)| (*n, true)).chain(
        STATEMENTS
            .iter()
            .filter(|s| s.1 != Vocab::Geometry)
            .map(|(n, _, _)| (*n, false)),
    )
}

impl Vocab {
    fn params(self) -> &'static [&'static str] {
        STATEMENTS.iter().find(|s| s.1 == self).map_or(&[], |s| s.2)
    }

    fn name(self) -> &'static str {
        STATEMENTS
            .iter()
            .find(|s| s.1 == self)
            .map_or("sketch", |s| s.0)
    }
}

/// The vocabulary's function and statement tables, which the resolver
/// binds inside sketch bodies only; empty with the extension off.
pub(crate) type VocabTables = (
    HashMap<Sym, Builtin, FxBuild>,
    HashMap<Sym, BuiltinModule, FxBuild>,
);

pub(crate) fn tables(syms: &mut Syms, extensions: crate::Extensions) -> VocabTables {
    let mut fns = HashMap::default();
    let mut mods = HashMap::default();
    if extensions.has(crate::Extension::Sketch) {
        for (n, b) in FUNCTIONS {
            fns.insert(syms.intern(n), b);
        }
        for (n, v, _) in STATEMENTS {
            mods.insert(syms.intern(n), BuiltinModule::SketchStatement(v));
        }
    }
    (fns, mods)
}

/// A sketch entity as a value: a handle naming one point, line, arc or
/// circle of one sketch. Two handles are equal when they name the same
/// entity of the same sketch.
pub struct Entity {
    /// Which sketch made it, numbered per evaluation (a merged helper's
    /// entities belong to the sketch it merged into).
    sketch: u32,
    id: EntityId,
    kind: EntityKind,
    /// The variable it was assigned to, set once the assignments of the
    /// body that made it have run; sub-points of a line, arc or circle made
    /// from coordinates are `line.start` and so on.
    label: OnceCell<String>,
    /// `.start`, `.end`, `.center`.
    parts: [Option<Rc<Entity>>; 3],
}

impl fmt::Debug for Entity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Entity")
            .field("sketch", &self.sketch)
            .field("id", &self.id.index())
            .field("kind", &self.kind_name())
            .field("label", &self.label.get())
            .finish()
    }
}

impl Entity {
    /// `point`, `line`, `arc` or `circle`.
    pub fn kind_name(&self) -> &'static str {
        kind_name(self.kind)
    }

    /// The variable name it was assigned to, if any.
    pub fn label(&self) -> Option<&str> {
        self.label.get().map(String::as_str)
    }

    /// Whether both name the same entity.
    pub fn same(&self, o: &Entity) -> bool {
        self.sketch == o.sketch && self.id == o.id
    }

    /// `.start`, `.end` and `.center`; anything else is `undef`, as a
    /// member a value does not have is. The body cannot read coordinates:
    /// before the solve an entity has none (section 4.1).
    pub(crate) fn member(&self, name: &str) -> Value {
        let i = match name {
            "start" => 0,
            "end" => 1,
            "center" => 2,
            _ => return Value::Undef,
        };
        self.parts[i].clone().map_or(Value::Undef, Value::Entity)
    }

    /// How `echo` and `str` print it: `<sketch line "base">`, or
    /// `<sketch point>` before it has a name.
    pub(crate) fn write(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(b"<sketch ");
        out.extend_from_slice(self.kind_name().as_bytes());
        if let Some(l) = self.label() {
            out.extend_from_slice(b" \"");
            out.extend_from_slice(l.as_bytes());
            out.push(b'"');
        }
        out.push(b'>');
    }
}

fn kind_name(k: EntityKind) -> &'static str {
    match k {
        EntityKind::Point => "point",
        EntityKind::Line => "line",
        EntityKind::Arc => "arc",
        EntityKind::Circle => "circle",
    }
}

/// A statement (or entity call) that added constraints: the source text
/// and span messages name it by.
struct Stmt {
    text: String,
    loc: Loc,
}

/// A `fillet` or `chamfer` to cut after the solve.
struct Corner {
    point: EntityId,
    size: f64,
    round: bool,
    stmt: usize,
}

/// The sketch being built: the solver's model and, per entity and
/// constraint, where it came from.
pub(crate) struct Builder {
    serial: u32,
    name: String,
    loc: Loc,
    strict: bool,
    convexity: i32,
    disc: Discretizer,
    model: Sketch,
    ents: Vec<Rc<Entity>>,
    ent_locs: Vec<Loc>,
    stmts: Vec<Stmt>,
    /// Per solver constraint, its statement in `stmts`.
    cons: Vec<usize>,
    corners: Vec<Corner>,
    /// Points made one by `coincident`, so the profile joins curves there.
    joins: Vec<(EntityId, EntityId)>,
    /// An error was printed: the sketch gives an empty shape.
    failed: bool,
}

impl Builder {
    /// "Sketch 'slot': " or "Sketch: ", the start of every message.
    fn prefix(&self) -> String {
        if self.name.is_empty() {
            "Sketch: ".to_string()
        } else {
            format!("Sketch '{}': ", self.name)
        }
    }

    /// An entity's name for messages: its label, or its kind and number.
    fn describe(&self, id: EntityId) -> String {
        let e = &self.ents[id.index()];
        match e.label() {
            Some(l) => format!("'{l}'"),
            None => format!("{} #{}", e.kind_name(), id.index() + 1),
        }
    }

    fn push(
        &mut self,
        id: EntityId,
        kind: EntityKind,
        parts: [Option<Rc<Entity>>; 3],
        loc: Loc,
    ) -> Rc<Entity> {
        let e = Rc::new(Entity {
            sketch: self.serial,
            id,
            kind,
            label: OnceCell::new(),
            parts,
        });
        debug_assert_eq!(self.ents.len(), id.index());
        self.ents.push(e.clone());
        self.ent_locs.push(loc);
        e
    }

    fn add(&mut self, c: Constraint, stmt: usize) -> Result<ConstraintId, ModelError> {
        let id = self.model.add(c)?;
        debug_assert_eq!(self.cons.len(), id.index());
        self.cons.push(stmt);
        Ok(id)
    }
}

/// A message about a sketch, with what is needed to print it.
struct Note {
    severity: Severity,
    code: DiagCode,
    loc: Loc,
    text: String,
    hint: Option<String>,
}

/// One curve of the profile, between two vertices.
#[derive(Clone, Copy)]
struct Curve {
    a: usize,
    b: usize,
    /// For an arc: its centre and whether it runs counter-clockwise from
    /// `a` to `b`.
    arc: Option<([f64; 2], bool)>,
    /// The entity it comes from (for messages).
    src: EntityId,
}

impl<'a> Evaluator<'a> {
    /// The start of a `sketch()` instantiation that begins a sketch (not
    /// one merging into the sketch being built): its arguments, and the
    /// `$fn`, `$fa`, `$fs` its arcs and circles are tessellated with.
    pub(crate) fn sketch_open(&mut self, p: &Params, loc: Loc) {
        let name = match self.get(p, "name") {
            Value::Str(s) => String::from_utf8_lossy(s.as_bytes()).into_owned(),
            Value::Undef => String::new(),
            v => {
                let mut t = b"sketch(name = ".to_vec();
                self.write_echo_nothrow(&v, &mut t);
                t.extend_from_slice(b"): the name must be a string");
                self.warn(loc, DiagCode::InvalidArgument, t);
                String::new()
            }
        };
        let strict = self.get(p, "strict").to_bool();
        let convexity = (self.get(p, "convexity").to_f64() as i32).max(1);
        let disc = self.discretizer(p);
        self.sketch_serial += 1;
        self.sketch = Some(Box::new(Builder {
            serial: self.sketch_serial,
            name,
            loc,
            strict,
            convexity,
            disc,
            model: Sketch::new(),
            ents: Vec::new(),
            ent_locs: Vec::new(),
            stmts: Vec::new(),
            cons: Vec::new(),
            corners: Vec::new(),
            joins: Vec::new(),
            failed: false,
        }));
    }

    /// How many entities the sketch being built has, so that
    /// [`Self::sketch_label`] can name the ones a body's assignments make.
    pub(crate) fn sketch_entities(&self) -> usize {
        self.sketch.as_ref().map_or(0, |b| b.ents.len())
    }

    /// Name the entities made since `from` by the assignments of body
    /// `sr` (`c1 = point([0, 0]);` names the point `c1`), and then the
    /// points made from coordinates for a named line, arc or circle
    /// (`top.start`).
    pub(crate) fn sketch_label(&mut self, from: usize, sr: ScopeRef) {
        let Some(b) = self.sketch.as_ref() else {
            return;
        };
        let unit = &self.units[sr.unit as usize];
        let scope = self.scope(sr);
        let made = &b.ents[from.min(b.ents.len())..];
        let locs = &b.ent_locs[from.min(b.ent_locs.len())..];
        for a in &scope.assignments {
            let span = unit.ast.expr(a.expr).span;
            if let Some((e, _)) = made
                .iter()
                .zip(locs)
                .rev()
                .find(|(_, l)| l.unit == sr.unit && l.span == span)
            {
                let _ = e.label.set(unit.ast.name(a.name).to_string());
            }
        }
        for e in made {
            let Some(l) = e.label() else { continue };
            for (k, part) in ["start", "end", "center"].iter().enumerate() {
                if let Some(p) = &e.parts[k]
                    && p.id.index() >= from
                {
                    let _ = p.label.set(format!("{l}.{part}"));
                }
            }
        }
    }

    /// `point`, `line`, `arc` and `circle`: a new entity of the sketch
    /// being built, as a handle.
    pub(crate) fn sketch_entity(&mut self, f: Builtin, loc: Loc, a: &mut Vec<ArgVal>) -> R<Value> {
        // Making an entity changes the sketch: a call or statement that
        // does cannot be replayed from a memo.
        self.untracked();
        let (name, params): (&str, &[&str]) = match f {
            Builtin::SketchPoint => ("point", &["at"]),
            Builtin::SketchLine => ("line", &["p", "q", "construction"]),
            Builtin::SketchArc => ("arc", &["center", "start", "end", "cw", "construction"]),
            _ => ("circle", &["center", "r", "d", "construction"]),
        };
        let syms: Vec<Sym> = params.iter().map(|n| self.syms.intern(n)).collect();
        let vars = self.bind_builtin(std::mem::take(a), loc, &[], &syms, true);
        let vals: Vec<Value> = syms
            .iter()
            .map(|s| vars.get(*s).cloned().unwrap_or_default())
            .collect();
        let Some(mut b) = self.sketch.take() else {
            // The resolver binds the vocabulary only inside sketch bodies,
            // which run only inside a sketch; this guards the invariant.
            let t = format!("{name}() is only valid inside a sketch body");
            self.error(Some(loc), DiagCode::SketchForeignEntity, t);
            return Ok(Value::Undef);
        };
        let mut notes = Vec::new();
        let r = sketch_entity_in(&mut b, f, name, &vals, loc, &mut notes, self);
        if !notes.is_empty() {
            b.failed = true;
        }
        let prefix = b.prefix();
        self.sketch = Some(b);
        self.print_notes(&prefix, notes);
        Ok(r.map_or(Value::Undef, Value::Entity))
    }

    /// A constraint, fillet or chamfer statement in a sketch body.
    pub(crate) fn sketch_statement(
        &mut self,
        v: Vocab,
        sr: ScopeRef,
        i: usize,
        ctx: &Rc<Ctx>,
    ) -> R<()> {
        self.untracked();
        let loc = self.inst_loc(sr, i);
        let args = self.inst_args(sr, i, ctx)?;
        self.no_children(sr, i);
        let params = v.params();
        // An entity written as a statement has its own error below; its
        // arguments are not bound, which would only add a warning about
        // their count.
        let vals: Vec<Value> = if v == Vocab::Geometry {
            Vec::new()
        } else {
            let p = self.params(args, loc, &[], params, v.name());
            let vals = params.iter().map(|n| self.get(&p, n)).collect();
            self.end(p);
            vals
        };
        let text = statement_text(self.units[sr.unit as usize].program.sources.text(loc.span));
        let Some(mut b) = self.sketch.take() else {
            let t = format!("{}() is only valid inside a sketch body", v.name());
            self.error(Some(loc), DiagCode::SketchForeignEntity, t);
            return Ok(());
        };
        let mut notes = Vec::new();
        if v == Vocab::Geometry {
            let name = text
                .split('(')
                .next()
                .unwrap_or("circle")
                .trim()
                .to_string();
            notes.push(Note {
                severity: Severity::Error,
                code: DiagCode::SketchGeometryInBody,
                loc,
                text: format!(
                    "{text}: in a sketch body, {name}() makes an entity and must be assigned"
                ),
                hint: Some(format!(
                    "inside a sketch, write `c = {name}(...);` with entities as its arguments"
                )),
            });
        } else {
            b.stmts.push(Stmt {
                text: text.clone(),
                loc,
            });
            let stmt = b.stmts.len() - 1;
            statement_in(&mut b, v, &text, stmt, &vals, loc, &mut notes, self);
        }
        if notes.iter().any(|n| n.severity == Severity::Error) {
            b.failed = true;
        }
        let prefix = b.prefix();
        self.sketch = Some(b);
        self.print_notes(&prefix, notes);
        Ok(())
    }

    fn print_notes(&mut self, prefix: &str, notes: Vec<Note>) {
        for n in notes {
            let text = format!("{prefix}{}", n.text);
            self.emit_hinted(n.severity, n.code, text.as_bytes(), Some(n.loc), n.hint);
        }
    }

    /// The end of a `sketch()` instantiation, its body done. A merging one
    /// gives no node; the outermost solves the sketch and becomes its
    /// polygon.
    pub(crate) fn sketch_close(&mut self, mut node: Node, top: bool) -> R<Option<Node>> {
        let kids = std::mem::take(&mut node.children);
        if let Some(at) = first_geometry(&kids)
            && let Some(b) = self.sketch.as_mut()
        {
            b.failed = true;
            let prefix = b.prefix();
            let loc = at.unwrap_or(b.loc);
            let t = format!(
                "{prefix}geometry in a sketch body is not part of the sketch; only entities and constraints are"
            );
            self.emit_hinted(
                Severity::Error,
                DiagCode::SketchGeometryInBody,
                t.as_bytes(),
                Some(loc),
                Some("move it out of the sketch body".to_string()),
            );
        }
        drop(kids);
        if !top {
            return Ok(None);
        }
        let b = self.sketch.take().expect("the sketch being built");
        let kind = self.sketch_solve(*b)?;
        node.kind = kind;
        Ok(Some(node))
    }

    /// Drop the sketch being built after an error unwound its body.
    pub(crate) fn sketch_abandon(&mut self) {
        self.sketch = None;
    }

    /// Solve, report, and turn the profile into a polygon.
    fn sketch_solve(&mut self, b: Builder) -> R<NodeKind> {
        let mut report = SketchReport {
            name: b.name.clone(),
            failed: true,
            ..SketchReport::default()
        };
        let empty = |report: SketchReport, convexity: i32| {
            NodeKind::Sketch(Box::new(SketchNode {
                points: Vec::new(),
                paths: Vec::new(),
                convexity,
                report: Arc::new(report),
            }))
        };
        if b.failed {
            return Ok(empty(report, b.convexity));
        }
        let flag = self.opts.interrupt.clone();
        let guard = self.opts.guard.clone();
        let stop = move || {
            flag.as_ref().is_some_and(|f| f.load(Ordering::Relaxed))
                || guard.as_ref().is_some_and(|g| g.over_time())
        };
        let opts = SolveOptions {
            max_unknowns: if self.opts.guard.is_some() {
                LIMITED_UNKNOWNS
            } else {
                usize::MAX
            },
            interrupt: Some(&stop),
            ..SolveOptions::default()
        };
        let prefix = b.prefix();
        let sol = match b.model.solve_with(&opts) {
            Ok(s) => s,
            Err(SolveError::Interrupted) => {
                self.check_interrupt()?;
                self.check_limits(Some(b.loc))?;
                return Ok(empty(report, b.convexity));
            }
            Err(e @ SolveError::TooManyUnknowns { .. }) => {
                let t = format!("{prefix}{e}");
                self.error(Some(b.loc), DiagCode::ResourceLimit, t);
                return Ok(empty(report, b.convexity));
            }
        };
        report.unknowns = sol.unknowns;
        report.equations = sol.equations;
        report.rank = sol.rank;
        report.dof = sol.dof;
        report.iterations = sol.iterations;
        report.residual = sol.residual;
        report.solved = sol.status == Status::Solved;
        report.continuation = sol.continuation;
        let units = &self.units;
        let line = |l: Loc| {
            units[l.unit as usize]
                .program
                .sources
                .get(l.span.file)
                .line_of(l.span.start)
        };
        let mut notes = diagnose(&b, &sol, &line);
        let mut ok = !notes.iter().any(|n| n.severity == Severity::Error);
        let mut loops = Vec::new();
        if ok {
            match profile(&b, &sol) {
                Ok(l) => loops = l,
                Err(n) => {
                    notes.extend(n);
                    ok = false;
                }
            }
        }
        self.print_notes(&prefix, notes);
        if !ok {
            return Ok(empty(report, b.convexity));
        }
        report.failed = false;
        let mut points = Vec::new();
        let mut paths = Vec::new();
        for l in &loops {
            paths.push((points.len()..points.len() + l.len()).collect::<Vec<usize>>());
            points.extend_from_slice(l);
        }
        // One loop is written as `polygon()` without `paths`: the same
        // outline, and the shorter `.csg`.
        if paths.len() == 1 {
            paths.clear();
        }
        Ok(NodeKind::Sketch(Box::new(SketchNode {
            points,
            paths,
            convexity: b.convexity,
            report: Arc::new(report),
        })))
    }
}

/// The source text of a statement, on one line and without its `;`: how
/// messages name a constraint (`length(axis, slot_len)`).
fn statement_text(src: &[u8]) -> String {
    let s = String::from_utf8_lossy(src);
    let s = s.trim().trim_end_matches(';').trim_end();
    let mut out = String::with_capacity(s.len());
    let mut space = false;
    for c in s.chars() {
        if c.is_whitespace() {
            space = true;
        } else {
            if space && !out.is_empty() {
                out.push(' ');
            }
            space = false;
            out.push(c);
        }
    }
    out
}

/// The first shape a sketch body made, and its location. Only a primitive
/// is one: an operation around nothing (`translate(...) fix(p);`, a helper
/// module's group, a `for` over constraints) makes no geometry.
fn first_geometry(kids: &[Node]) -> Option<Option<Loc>> {
    let mut stack: Vec<&Node> = kids.iter().rev().collect();
    while let Some(n) = stack.pop() {
        let leaf = matches!(
            n.kind,
            NodeKind::Cube { .. }
                | NodeKind::Sphere { .. }
                | NodeKind::Cylinder { .. }
                | NodeKind::Polyhedron { .. }
                | NodeKind::Square { .. }
                | NodeKind::Circle { .. }
                | NodeKind::Polygon { .. }
                | NodeKind::Surface { .. }
                | NodeKind::Import(_)
                | NodeKind::Text(_)
                | NodeKind::Sketch(_)
        );
        if leaf {
            return Some(n.origin.as_ref().map(|o| Loc {
                unit: o.unit,
                span: o.span,
            }));
        }
        stack.extend(n.children.iter().rev());
    }
    None
}

/// An argument that must be a handle of this sketch, or the note saying
/// why it is not.
fn handle(
    b: &Builder,
    v: &Value,
    what: &str,
    param: &str,
    loc: Loc,
    notes: &mut Vec<Note>,
    ev: &mut Evaluator<'_>,
) -> Option<Rc<Entity>> {
    match v {
        Value::Entity(e) if e.sketch == b.serial => Some(e.clone()),
        Value::Entity(e) => {
            notes.push(Note {
                severity: Severity::Error,
                code: DiagCode::SketchForeignEntity,
                loc,
                text: format!(
                    "{what}: {param} is {} {}, which belongs to another sketch",
                    article(e.kind_name()),
                    e.label().map_or(String::new(), |l| format!("'{l}'"))
                ),
                hint: None,
            });
            None
        }
        _ => {
            let mut found = Vec::new();
            ev.write_echo_nothrow(v, &mut found);
            notes.push(Note {
                severity: Severity::Error,
                code: DiagCode::SketchUnknownEntity,
                loc,
                text: format!(
                    "{what}: {param} must be a sketch entity, found {} ({})",
                    v.type_name(),
                    String::from_utf8_lossy(&found)
                ),
                hint: Some(
                    "pass a variable assigned from point(), line(), arc() or circle()".into(),
                ),
            });
            None
        }
    }
}

fn article(w: &str) -> &'static str {
    if w.starts_with(['a', 'e', 'i', 'o', 'u']) {
        "an"
    } else {
        "a"
    }
}

/// A number argument, or the note saying it is missing or not one.
fn number(v: &Value, what: &str, param: &str, loc: Loc, notes: &mut Vec<Note>) -> Option<f64> {
    match v {
        Value::Number(x) if x.is_finite() => Some(*x),
        _ => {
            notes.push(Note {
                severity: Severity::Error,
                code: DiagCode::InvalidArgument,
                loc,
                text: format!(
                    "{what}: {param} must be a finite number, found {}",
                    v.type_name()
                ),
                hint: None,
            });
            None
        }
    }
}

fn model_note(e: &ModelError, what: &str, loc: Loc) -> Note {
    Note {
        severity: Severity::Error,
        code: DiagCode::InvalidArgument,
        loc,
        text: format!("{what}: {e}"),
        hint: None,
    }
}

/// A point argument of `line`, `arc` or `circle`: a point handle, or
/// `[x, y]` for a new point drawn there.
#[allow(clippy::too_many_arguments)]
fn point_arg(
    b: &mut Builder,
    v: &Value,
    what: &str,
    param: &str,
    loc: Loc,
    notes: &mut Vec<Note>,
    ev: &mut Evaluator<'_>,
) -> Option<Rc<Entity>> {
    if let Some(xy) = v.as_vec2(true) {
        return match b.model.point(Some(xy)) {
            Ok(id) => Some(b.push(id, EntityKind::Point, [None, None, None], loc)),
            Err(e) => {
                notes.push(model_note(&e, what, loc));
                None
            }
        };
    }
    let e = handle(b, v, what, param, loc, notes, ev)?;
    if e.kind != EntityKind::Point {
        notes.push(Note {
            severity: Severity::Error,
            code: DiagCode::InvalidArgument,
            loc,
            text: format!(
                "{what}: {param} must be a point or [x, y], found {} {}",
                article(e.kind_name()),
                e.kind_name()
            ),
            hint: None,
        });
        return None;
    }
    Some(e)
}

/// [`Evaluator::sketch_entity`] on the builder taken out of the evaluator.
fn sketch_entity_in(
    b: &mut Builder,
    f: Builtin,
    name: &str,
    vals: &[Value],
    loc: Loc,
    notes: &mut Vec<Note>,
    ev: &mut Evaluator<'_>,
) -> Option<Rc<Entity>> {
    let what = format!("{name}()");
    let flag = |v: &Value| v.to_bool();
    match f {
        Builtin::SketchPoint => {
            let guess = match &vals[0] {
                Value::Undef => None,
                v => match v.as_vec2(true) {
                    Some(xy) => Some(xy),
                    None => {
                        notes.push(Note {
                            severity: Severity::Error,
                            code: DiagCode::InvalidArgument,
                            loc,
                            text: format!("{what}: at must be [x, y], found {}", v.type_name()),
                            hint: None,
                        });
                        return None;
                    }
                },
            };
            match b.model.point(guess) {
                Ok(id) => Some(b.push(id, EntityKind::Point, [None, None, None], loc)),
                Err(e) => {
                    notes.push(model_note(&e, &what, loc));
                    None
                }
            }
        }
        Builtin::SketchLine => {
            let p = point_arg(b, &vals[0], &what, "p", loc, notes, ev)?;
            let q = point_arg(b, &vals[1], &what, "q", loc, notes, ev)?;
            let id = match b.model.line(p.id, q.id) {
                Ok(id) => id,
                Err(e) => {
                    notes.push(model_note(&e, &what, loc));
                    return None;
                }
            };
            let _ = b.model.set_construction(id, flag(&vals[2]));
            Some(b.push(id, EntityKind::Line, [Some(p), Some(q), None], loc))
        }
        Builtin::SketchArc => {
            let c = point_arg(b, &vals[0], &what, "center", loc, notes, ev)?;
            let s = point_arg(b, &vals[1], &what, "start", loc, notes, ev)?;
            let e = point_arg(b, &vals[2], &what, "end", loc, notes, ev)?;
            let id = match b.model.arc(c.id, s.id, e.id, flag(&vals[3])) {
                Ok(id) => id,
                Err(err) => {
                    notes.push(model_note(&err, &what, loc));
                    return None;
                }
            };
            let _ = b.model.set_construction(id, flag(&vals[4]));
            Some(b.push(id, EntityKind::Arc, [Some(s), Some(e), Some(c)], loc))
        }
        _ => {
            let c = point_arg(b, &vals[0], &what, "center", loc, notes, ev)?;
            // `r` or `d`, if given, is a radius constraint (sugar for
            // `radius()`), and the radius's starting value.
            let radius = match (&vals[1], &vals[2]) {
                (Value::Undef, Value::Undef) => None,
                (r, Value::Undef) => Some(number(r, &what, "r", loc, notes)?),
                (Value::Undef, d) => Some(number(d, &what, "d", loc, notes)? / 2.0),
                _ => {
                    notes.push(Note {
                        severity: Severity::Error,
                        code: DiagCode::ArgumentMismatch,
                        loc,
                        text: format!("{what}: give r or d, not both"),
                        hint: None,
                    });
                    return None;
                }
            };
            let id = match b.model.circle(c.id, radius) {
                Ok(id) => id,
                Err(e) => {
                    notes.push(model_note(&e, &what, loc));
                    return None;
                }
            };
            let _ = b.model.set_construction(id, flag(&vals[3]));
            let handle = b.push(id, EntityKind::Circle, [None, None, Some(c)], loc);
            if let Some(r) = radius {
                b.stmts.push(Stmt {
                    text: format!("{what} radius"),
                    loc,
                });
                let stmt = b.stmts.len() - 1;
                if let Err(e) = b.add(
                    Constraint::Radius {
                        curve: id,
                        value: r,
                    },
                    stmt,
                ) {
                    notes.push(model_note(&e, &what, loc));
                }
            }
            Some(handle)
        }
    }
}

/// [`Evaluator::sketch_statement`] on the builder taken out of the
/// evaluator: the statement's constraints.
#[allow(clippy::too_many_arguments)]
fn statement_in(
    b: &mut Builder,
    v: Vocab,
    what: &str,
    stmt: usize,
    vals: &[Value],
    loc: Loc,
    notes: &mut Vec<Note>,
    ev: &mut Evaluator<'_>,
) {
    let params = v.params();
    macro_rules! ent {
        ($i:expr) => {
            match handle(b, &vals[$i], what, params[$i], loc, notes, ev) {
                Some(e) => e,
                None => return,
            }
        };
    }
    macro_rules! num {
        ($i:expr) => {
            match number(&vals[$i], what, params[$i], loc, notes) {
                Some(x) => x,
                None => return,
            }
        };
    }
    let add = |b: &mut Builder, c: Constraint, notes: &mut Vec<Note>| {
        if let Err(e) = b.add(c, stmt) {
            notes.push(model_note(&e, what, loc));
        }
    };
    match v {
        Vocab::Coincident => {
            let (p, q) = (ent!(0), ent!(1));
            add(b, Constraint::Coincident(p.id, q.id), notes);
            if p.kind == EntityKind::Point && q.kind == EntityKind::Point {
                b.joins.push((p.id, q.id));
            }
        }
        Vocab::On => {
            let (p, c) = (ent!(0), ent!(1));
            add(
                b,
                Constraint::On {
                    point: p.id,
                    curve: c.id,
                },
                notes,
            );
        }
        Vocab::Horizontal | Vocab::Vertical => {
            let a = ent!(0);
            let pair = if vals[1].is_undef() {
                Pair::Line(a.id)
            } else {
                Pair::Points(a.id, ent!(1).id)
            };
            let c = if v == Vocab::Horizontal {
                Constraint::Horizontal(pair)
            } else {
                Constraint::Vertical(pair)
            };
            add(b, c, notes);
        }
        Vocab::Parallel => {
            let (l1, l2) = (ent!(0), ent!(1));
            add(b, Constraint::Parallel(l1.id, l2.id), notes);
        }
        Vocab::Perpendicular => {
            let (l1, l2) = (ent!(0), ent!(1));
            add(b, Constraint::Perpendicular(l1.id, l2.id), notes);
        }
        Vocab::Tangent => {
            let (x, y) = (ent!(0), ent!(1));
            add(b, Constraint::Tangent(x.id, y.id), notes);
        }
        Vocab::Distance => {
            let (mut a, mut c) = (ent!(0), ent!(1));
            let d = num!(2);
            let along = match &vals[3] {
                Value::Undef => Along::Direct,
                Value::Str(s) if s.as_bytes() == b"x" => Along::X,
                Value::Str(s) if s.as_bytes() == b"y" => Along::Y,
                other => {
                    let mut found = Vec::new();
                    ev.write_echo_nothrow(other, &mut found);
                    notes.push(Note {
                        severity: Severity::Error,
                        code: DiagCode::InvalidArgument,
                        loc,
                        text: format!(
                            "{what}: along must be \"x\" or \"y\", found {}",
                            String::from_utf8_lossy(&found)
                        ),
                        hint: None,
                    });
                    return;
                }
            };
            // A line and a point in either order: the solver takes the
            // point first.
            if a.kind == EntityKind::Line && c.kind == EntityKind::Point {
                std::mem::swap(&mut a, &mut c);
            }
            let points = a.kind == EntityKind::Point && c.kind == EntityKind::Point;
            if along != Along::Direct && !points {
                notes.push(Note {
                    severity: Severity::Error,
                    code: DiagCode::InvalidArgument,
                    loc,
                    text: format!("{what}: along applies only between two points"),
                    hint: None,
                });
                return;
            }
            // Between two lines the distance implies that they are
            // parallel (section 4.3): the distance from one line's start to
            // the other line means nothing for lines that are not, and an
            // author writing it means "these edges are d apart". The
            // parallel equation is added first, unless an earlier
            // `parallel()` already says so, which would make it redundant.
            if a.kind == EntityKind::Line && c.kind == EntityKind::Line {
                let (x, y) = (a.id, c.id);
                let stated = b.model.constraints().iter().any(|k| {
                    matches!(k, Constraint::Parallel(p, q)
                        if (*p == x && *q == y) || (*p == y && *q == x))
                });
                if !stated {
                    add(b, Constraint::Parallel(x, y), notes);
                }
            }
            add(
                b,
                Constraint::Distance {
                    a: a.id,
                    b: c.id,
                    value: d,
                    along,
                },
                notes,
            );
        }
        Vocab::Length => {
            let l = ent!(0);
            let d = num!(1);
            add(
                b,
                Constraint::Length {
                    line: l.id,
                    value: d,
                },
                notes,
            );
        }
        Vocab::Radius | Vocab::Diameter => {
            let c = ent!(0);
            let x = num!(1);
            let k = if v == Vocab::Radius {
                Constraint::Radius {
                    curve: c.id,
                    value: x,
                }
            } else {
                Constraint::Diameter {
                    curve: c.id,
                    value: x,
                }
            };
            add(b, k, notes);
        }
        Vocab::Angle => {
            let from = ent!(0);
            // `angle(arc, deg)`: the arc's sweep.
            if from.kind == EntityKind::Arc && vals[2].is_undef() {
                let deg = num!(1);
                add(
                    b,
                    Constraint::Sweep {
                        arc: from.id,
                        degrees: deg,
                    },
                    notes,
                );
                return;
            }
            let to = ent!(1);
            let deg = num!(2);
            add(
                b,
                Constraint::Angle {
                    from: from.id,
                    to: to.id,
                    degrees: deg,
                },
                notes,
            );
        }
        Vocab::Equal => {
            let (x, y) = (ent!(0), ent!(1));
            add(b, Constraint::Equal(x.id, y.id), notes);
        }
        Vocab::Midpoint => {
            let (p, l) = (ent!(0), ent!(1));
            add(
                b,
                Constraint::Midpoint {
                    point: p.id,
                    line: l.id,
                },
                notes,
            );
        }
        Vocab::Symmetric => {
            let (p, q, about) = (ent!(0), ent!(1), ent!(2));
            add(
                b,
                Constraint::Symmetric {
                    a: p.id,
                    b: q.id,
                    about: about.id,
                },
                notes,
            );
        }
        Vocab::Fix => {
            let p = ent!(0);
            let at = match &vals[1] {
                Value::Undef => None,
                v => match v.as_vec2(true) {
                    Some(xy) => Some(xy),
                    None => {
                        notes.push(Note {
                            severity: Severity::Error,
                            code: DiagCode::InvalidArgument,
                            loc,
                            text: format!("{what}: at must be [x, y], found {}", v.type_name()),
                            hint: None,
                        });
                        return;
                    }
                },
            };
            add(b, Constraint::Fix { entity: p.id, at }, notes);
        }
        Vocab::Fillet | Vocab::Chamfer => {
            let p = ent!(0);
            let size = num!(1);
            if p.kind != EntityKind::Point || size <= 0.0 {
                notes.push(Note {
                    severity: Severity::Error,
                    code: DiagCode::InvalidArgument,
                    loc,
                    text: format!("{what}: needs a corner point and a size greater than 0"),
                    hint: None,
                });
                return;
            }
            b.corners.push(Corner {
                point: p.id,
                size,
                round: v == Vocab::Fillet,
                stmt,
            });
        }
        Vocab::Geometry => {}
    }
}

/// What an equation is, for messages: its statement and line, or an arc's
/// own equation.
fn source_text(b: &Builder, s: Source, line: &dyn Fn(Loc) -> u32) -> String {
    match s {
        Source::Constraint(c) => {
            let st = &b.stmts[b.cons[c.index()]];
            format!("{} at line {}", st.text, line(st.loc))
        }
        Source::Arc(e) => format!(
            "arc {} (its ends are the same distance from its centre)",
            b.describe(e)
        ),
    }
}

/// The statement an equation came from, without its line (the message's
/// own location gives that).
fn source_name(b: &Builder, s: Source) -> String {
    match s {
        Source::Constraint(c) => b.stmts[b.cons[c.index()]].text.clone(),
        Source::Arc(e) => format!("arc {}", b.describe(e)),
    }
}

fn source_loc(b: &Builder, s: Source) -> Loc {
    match s {
        Source::Constraint(c) => b.stmts[b.cons[c.index()]].loc,
        Source::Arc(e) => b.ent_locs[e.index()],
    }
}

/// Which statement (or arc) an equation belongs to: a statement can be
/// several equations (`fix` is two), and one message per statement is
/// enough.
fn source_key(b: &Builder, s: Source) -> (bool, usize) {
    match s {
        Source::Constraint(c) => (true, b.cons[c.index()]),
        Source::Arc(e) => (false, e.index()),
    }
}

/// Free coordinates listed in an under-constrained message, at most.
const FREE_LISTED: usize = 8;

/// The solve's findings as messages (section 4.7, without stage 3's
/// hints): conflicts, failure to converge, redundancy, flips and free
/// degrees of freedom. `line` gives a location's line, for messages that
/// name other statements.
fn diagnose(b: &Builder, sol: &Solution, line: &dyn Fn(Loc) -> u32) -> Vec<Note> {
    let mut notes = Vec::new();
    let mut seen: Vec<(bool, usize)> = Vec::new();
    for d in &sol.conflicts {
        let key = source_key(b, d.source);
        if seen.contains(&key) {
            continue;
        }
        seen.push(key);
        let mut parts = vec![source_text(b, d.source, line)];
        parts.extend(d.with.iter().map(|w| source_text(b, *w, line)));
        notes.push(Note {
            severity: Severity::Error,
            code: DiagCode::SketchConflict,
            loc: source_loc(b, d.source),
            text: format!("constraints conflict: {}", parts.join(", ")),
            hint: Some("remove one, or make the values agree".into()),
        });
    }
    if sol.status == Status::NotConverged && sol.conflicts.is_empty() {
        notes.push(Note {
            severity: Severity::Error,
            code: DiagCode::SketchNoConvergence,
            loc: b.loc,
            text: format!(
                "did not converge (residual {}{})",
                fmt_number(sol.residual),
                if sol.continuation {
                    " after continuation"
                } else {
                    ""
                }
            ),
            hint: Some("check the guesses: the drawing may be far from any solution".into()),
        });
    }
    let mut seen: Vec<(bool, usize)> = Vec::new();
    for d in &sol.redundant {
        let key = source_key(b, d.source);
        if seen.contains(&key) {
            continue;
        }
        seen.push(key);
        let with: Vec<String> = d.with.iter().map(|w| source_text(b, *w, line)).collect();
        notes.push(Note {
            severity: Severity::Warning,
            code: DiagCode::SketchRedundant,
            loc: source_loc(b, d.source),
            text: format!(
                "{} is implied by the other constraints: {}",
                source_name(b, d.source),
                with.join(", ")
            ),
            hint: Some("remove it".into()),
        });
    }
    for o in &sol.flipped {
        let (what, loc) = match *o {
            Orientation::ArcSweep(e) => (
                format!(
                    "arc {} sweeps the other side of 180 degrees than drawn",
                    b.describe(e)
                ),
                b.ent_locs[e.index()],
            ),
            Orientation::AngleBranch(c) => {
                let s = Source::Constraint(c);
                (
                    format!(
                        "{} is met 180 degrees away from the drawn angle",
                        source_name(b, s)
                    ),
                    source_loc(b, s),
                )
            }
            Orientation::RadiusSign(e) => (
                format!("circle {} came out with a negative radius", b.describe(e)),
                b.ent_locs[e.index()],
            ),
            Orientation::TangentSide(c) => {
                let s = Source::Constraint(c);
                (
                    format!("{} solved on the other side than drawn", source_name(b, s)),
                    source_loc(b, s),
                )
            }
            Orientation::Corner { point, lines } => (
                format!(
                    "the corner at {} between {} and {} turns the other way than drawn",
                    b.describe(point),
                    b.describe(lines[0]),
                    b.describe(lines[1])
                ),
                b.ent_locs[point.index()],
            ),
        };
        notes.push(Note {
            severity: Severity::Warning,
            code: DiagCode::SketchFlipped,
            loc,
            text: what,
            hint: Some("move the guesses closer to the intended shape".into()),
        });
    }
    if sol.status == Status::Solved && sol.dof > 0 {
        let free: Vec<String> = sol
            .free
            .iter()
            .filter(|f| f.mobility >= 0.01)
            .map(|f| {
                let how = match f.coordinate {
                    Coordinate::X => "along x",
                    Coordinate::Y => "along y",
                    Coordinate::Radius => "in radius",
                };
                format!("{} {how}", b.describe(f.entity))
            })
            .collect();
        let mut list = free
            .iter()
            .take(FREE_LISTED)
            .cloned()
            .collect::<Vec<_>>()
            .join(", ");
        if free.len() > FREE_LISTED {
            list.push_str(", ...");
        }
        let s = if sol.dof == 1 { "" } else { "s" };
        notes.push(Note {
            // The owner's decision (section 13): an under-constrained
            // sketch is information, as in FreeCAD, unless the author
            // asked for `strict`.
            severity: if b.strict {
                Severity::Error
            } else {
                Severity::Info
            },
            code: DiagCode::SketchUnderconstrained,
            loc: b.loc,
            text: format!(
                "{} free degree{s} of freedom; these can still move: {list}",
                sol.dof
            ),
            hint: Some("add a dimension, or fix() what should not move".into()),
        });
    }
    notes
}

/// Segments for an arc or circle, by `circle()`'s rule.
fn segments(disc: &Discretizer, r: f64, sweep: f64) -> Option<i32> {
    io::fragments::circular_segments_for_angle(disc.fn_, disc.fa, disc.fs, r, sweep)
}

/// The inner vertices of an arc around `c` from `s` to `e`: evenly spaced
/// angles, as `circle()` takes them. The ends are left to the caller,
/// which uses the solved points themselves, so the vertex an arc shares
/// with the next curve has the same bits on both and every loop closes
/// exactly (section 4.4).
fn arc_points(
    c: [f64; 2],
    s: [f64; 2],
    e: [f64; 2],
    ccw: bool,
    disc: &Discretizer,
) -> Vec<[f64; 2]> {
    let (sx, sy) = (s[0] - c[0], s[1] - c[1]);
    let r = (sx * sx + sy * sy).sqrt();
    let a0 = atan2_degrees(sy, sx);
    let a1 = atan2_degrees(e[1] - c[1], e[0] - c[0]);
    let mut sweep = if ccw { a1 - a0 } else { a0 - a1 };
    // An arc from a point to itself is a full turn.
    if sweep <= 0.0 {
        sweep += 360.0;
    }
    let n = segments(disc, r, sweep).unwrap_or(1).max(1);
    (1..n)
        .map(|i| {
            let step = sweep * f64::from(i) / f64::from(n);
            let phi = if ccw { a0 + step } else { a0 - step };
            [c[0] + r * cos_degrees(phi), c[1] + r * sin_degrees(phi)]
        })
        .collect()
}

/// A circle's vertices around `c`, as `circle(r)` makes them
/// (`geom::primitives::circle2d`).
fn circle_points(c: [f64; 2], r: f64, disc: &Discretizer) -> Vec<[f64; 2]> {
    if r <= 0.0 || !r.is_finite() {
        return Vec::new();
    }
    let n = segments(disc, r, 360.0).unwrap_or(3);
    (0..n)
        .map(|i| {
            let phi = (360.0 * f64::from(i)) / f64::from(n);
            [c[0] + r * cos_degrees(phi), c[1] + r * sin_degrees(phi)]
        })
        .collect()
}

fn sub(a: [f64; 2], b: [f64; 2]) -> [f64; 2] {
    [a[0] - b[0], a[1] - b[1]]
}

fn norm(a: [f64; 2]) -> f64 {
    (a[0] * a[0] + a[1] * a[1]).sqrt()
}

/// Union-find root, halving the path on the way.
fn find(root: &mut [usize], mut x: usize) -> usize {
    while root[x] != x {
        root[x] = root[root[x]];
        x = root[x];
    }
    x
}

/// The profile as a graph: vertices (solved points, with `coincident`
/// points made one) and the curves between them.
struct Graph {
    verts: Vec<[f64; 2]>,
    /// Each vertex's point, for messages.
    vsrc: Vec<EntityId>,
    vertex_of: Vec<usize>,
    root: Vec<usize>,
    curves: Vec<Curve>,
}

impl Graph {
    fn vertex(&mut self, b: &Builder, sol: &Solution, id: EntityId) -> usize {
        let r = find(&mut self.root, id.index());
        if self.vertex_of[r] == usize::MAX {
            self.vertex_of[r] = self.verts.len();
            self.verts
                .push(sol.point(b.ents[r].id).unwrap_or([0.0, 0.0]));
            self.vsrc.push(b.ents[r].id);
        }
        self.vertex_of[r]
    }

    fn add_vertex(&mut self, p: [f64; 2], src: EntityId) -> usize {
        self.verts.push(p);
        self.vsrc.push(src);
        self.verts.len() - 1
    }
}

/// Cut the fillet or chamfer `k` at its corner (section 4.5): the two
/// lines there are trimmed, and an arc (or a line) joins the cuts.
fn cut_corner(b: &Builder, sol: &Solution, g: &mut Graph, k: &Corner) -> Result<(), Note> {
    let stmt = &b.stmts[k.stmt];
    let v = g.vertex(b, sol, k.point);
    let mut inc: Vec<(usize, bool)> = Vec::new();
    for (ci, c) in g.curves.iter().enumerate() {
        if c.a == v {
            inc.push((ci, false));
        }
        if c.b == v {
            inc.push((ci, true));
        }
    }
    let joins_arc = inc.iter().any(|(ci, _)| g.curves[*ci].arc.is_some());
    if inc.len() != 2 || joins_arc {
        let text = if joins_arc {
            format!(
                "{}: the corner {} joins an arc; fillets and chamfers between a line and an arc are not supported yet",
                stmt.text,
                b.describe(k.point)
            )
        } else {
            format!(
                "{}: the corner {} must join exactly two profile lines, and it joins {}",
                stmt.text,
                b.describe(k.point),
                inc.len()
            )
        };
        return Err(Note {
            severity: Severity::Error,
            code: DiagCode::InvalidArgument,
            loc: stmt.loc,
            text,
            hint: None,
        });
    }
    let far = |(ci, at_b): (usize, bool), g: &Graph| {
        if at_b { g.curves[ci].a } else { g.curves[ci].b }
    };
    let p = g.verts[v];
    let da = sub(g.verts[far(inc[0], g)], p);
    let db = sub(g.verts[far(inc[1], g)], p);
    let (la, lb) = (norm(da), norm(db));
    let u = [da[0] / la, da[1] / la];
    let w = [db[0] / lb, db[1] / lb];
    let cross = u[0] * w[1] - u[1] * w[0];
    let dot = u[0] * w[0] + u[1] * w[1];
    // A zero-length line makes the directions NaN.
    if !cross.is_finite() || cross.abs() < 1e-12 {
        return Err(Note {
            severity: Severity::Error,
            code: DiagCode::InvalidArgument,
            loc: stmt.loc,
            text: format!(
                "{}: the lines at {} are in line, so there is no corner to cut",
                stmt.text,
                b.describe(k.point)
            ),
            hint: None,
        });
    }
    // The trim along each line. A fillet of radius r touches both lines
    // r(1 + u.v)/|u x v| from the corner: r / tan(half the corner's
    // angle), written without trigonometry.
    let t = if k.round {
        k.size * (1.0 + dot) / cross.abs()
    } else {
        k.size
    };
    for (&(ci, _), l) in inc.iter().zip([la, lb]) {
        if t > l {
            let most = if k.round { k.size * l / t } else { l };
            return Err(Note {
                severity: Severity::Error,
                code: DiagCode::SketchFilletTooLarge,
                loc: stmt.loc,
                text: format!(
                    "{} needs {} along {}, which is {} long",
                    stmt.text,
                    fmt_number(t),
                    b.describe(g.curves[ci].src),
                    fmt_number(l)
                ),
                hint: Some(format!("make it at most {}", fmt_number(most))),
            });
        }
    }
    let t1 = g.add_vertex([p[0] + t * u[0], p[1] + t * u[1]], k.point);
    let t2 = g.add_vertex([p[0] + t * w[0], p[1] + t * w[1]], k.point);
    for (&(ci, at_b), t) in inc.iter().zip([t1, t2]) {
        if at_b {
            g.curves[ci].b = t;
        } else {
            g.curves[ci].a = t;
        }
    }
    let arc = k.round.then(|| {
        // The centre is r from the first cut, along the first line's
        // normal that points towards the second line.
        let s = cross.abs();
        let n = [(w[0] - dot * u[0]) / s, (w[1] - dot * u[1]) / s];
        let a = g.verts[t1];
        let c = [a[0] + k.size * n[0], a[1] + k.size * n[1]];
        let (from, to) = (sub(a, c), sub(g.verts[t2], c));
        // A fillet always turns the short way round.
        (c, from[0] * to[1] - from[1] * to[0] > 0.0)
    });
    g.curves.push(Curve {
        a: t1,
        b: t2,
        arc,
        src: k.point,
    });
    Ok(())
}

/// Every closed loop of the profile as polygon points, after cutting the
/// fillets and chamfers (sections 4.4 and 4.5), or the notes saying why
/// there is none.
fn profile(b: &Builder, sol: &Solution) -> Result<Vec<Vec<[f64; 2]>>, Vec<Note>> {
    let ents = b.model.entities();
    let mut g = Graph {
        verts: Vec::new(),
        vsrc: Vec::new(),
        vertex_of: vec![usize::MAX; ents.len()],
        root: (0..ents.len()).collect(),
        curves: Vec::new(),
    };
    // Points that `coincident()` made one are one vertex, with the lower
    // one's coordinates (exactify gave them the same bits anyway).
    for (p, q) in &b.joins {
        let (rp, rq) = (find(&mut g.root, p.index()), find(&mut g.root, q.index()));
        g.root[rp.max(rq)] = rp.min(rq);
    }
    let mut circles: Vec<([f64; 2], f64)> = Vec::new();
    for (i, e) in ents.iter().enumerate() {
        let src = b.ents[i].id;
        if b.model.is_construction(src) {
            continue;
        }
        match *e {
            sketch_solver::Entity::Line { start, end } => {
                let a = g.vertex(b, sol, start);
                let z = g.vertex(b, sol, end);
                g.curves.push(Curve {
                    a,
                    b: z,
                    arc: None,
                    src,
                });
            }
            sketch_solver::Entity::Arc {
                center,
                start,
                end,
                clockwise,
            } => {
                let a = g.vertex(b, sol, start);
                let z = g.vertex(b, sol, end);
                let c = sol.point(center).unwrap_or([0.0, 0.0]);
                g.curves.push(Curve {
                    a,
                    b: z,
                    arc: Some((c, !clockwise)),
                    src,
                });
            }
            sketch_solver::Entity::Circle { center, .. } => {
                let c = sol.point(center).unwrap_or([0.0, 0.0]);
                circles.push((c, sol.radius(src).unwrap_or(0.0)));
            }
            sketch_solver::Entity::Point { .. } => {}
        }
    }
    let mut notes = Vec::new();
    for k in &b.corners {
        if let Err(n) = cut_corner(b, sol, &mut g, k) {
            notes.push(n);
        }
    }
    if !notes.is_empty() {
        return Err(notes);
    }
    // Every vertex must join exactly two curves for the profile to be
    // closed loops.
    let mut inc: Vec<Vec<(usize, bool)>> = vec![Vec::new(); g.verts.len()];
    for (ci, c) in g.curves.iter().enumerate() {
        inc[c.a].push((ci, false));
        inc[c.b].push((ci, true));
    }
    for c in &g.curves {
        for (end, v) in [("start", c.a), ("end", c.b)] {
            if inc[v].len() == 1 {
                let kind = b.ents[c.src.index()].kind_name();
                notes.push(Note {
                    severity: Severity::Error,
                    code: DiagCode::SketchOpenProfile,
                    loc: b.ent_locs[c.src.index()],
                    text: format!(
                        "{kind} {} {end} is not joined to another profile curve",
                        b.describe(c.src)
                    ),
                    hint: Some("share the point, or mark the curve `construction = true`".into()),
                });
            }
        }
    }
    for (v, i) in inc.iter().enumerate() {
        if i.len() > 2 {
            notes.push(Note {
                severity: Severity::Error,
                code: DiagCode::SketchOpenProfile,
                loc: b.ent_locs[g.vsrc[v].index()],
                text: format!(
                    "point {} joins {} profile curves; a closed profile needs exactly two at each point",
                    b.describe(g.vsrc[v]),
                    i.len()
                ),
                hint: Some("mark the extra curves `construction = true`".into()),
            });
        }
    }
    if !notes.is_empty() {
        return Err(notes);
    }
    let mut loops = Vec::new();
    let mut used = vec![false; g.curves.len()];
    for start in 0..g.curves.len() {
        if used[start] {
            continue;
        }
        let mut pts: Vec<[f64; 2]> = Vec::new();
        let (mut ci, mut reversed) = (start, false);
        for _ in 0..g.curves.len() {
            used[ci] = true;
            let c = g.curves[ci];
            let (from, to) = if reversed { (c.b, c.a) } else { (c.a, c.b) };
            pts.push(g.verts[from]);
            if let Some((centre, ccw)) = c.arc {
                let mut inner = arc_points(centre, g.verts[c.a], g.verts[c.b], ccw, &b.disc);
                if reversed {
                    inner.reverse();
                }
                pts.extend(inner);
            }
            // `ci` arrives at `to` by its end there (`b` unless reversed);
            // leave by the other curve end at `to`.
            let here = !reversed;
            let Some(&(next, at_b)) = inc[to].iter().find(|&&(cj, e)| !(cj == ci && e == here))
            else {
                break;
            };
            if next == start {
                break;
            }
            ci = next;
            reversed = at_b;
        }
        loops.push(pts);
    }
    for (c, r) in circles {
        let pts = circle_points(c, r, &b.disc);
        if !pts.is_empty() {
            loops.push(pts);
        }
    }
    Ok(loops)
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

    #[test]
    fn statements_are_named_by_their_text_on_one_line() {
        assert_eq!(
            statement_text(b"  length(axis,\n      slot_len) ;"),
            "length(axis, slot_len)"
        );
        assert_eq!(statement_text(b"fix(o);"), "fix(o)");
    }

    /// An arc's inner vertices are evenly spaced, in its direction, by
    /// `circle()`'s segment count for its sweep; its ends are left to the
    /// caller.
    #[test]
    fn arcs_are_tessellated_like_circles() {
        let (c, s, e) = ([0.0, 0.0], [2.0, 0.0], [0.0, 2.0]);
        // $fn = 8: 2 segments for 90 degrees, so one inner vertex at 45.
        let ccw = arc_points(c, s, e, true, &disc(8.0));
        assert_eq!(ccw.len(), 1);
        assert!((ccw[0][0] - 2f64.sqrt()).abs() < 1e-15 && (ccw[0][1] - 2f64.sqrt()).abs() < 1e-15);
        // Clockwise from the same ends: the other 270 degrees, 6 segments.
        let cw = arc_points(c, s, e, false, &disc(8.0));
        assert_eq!(cw.len(), 5);
        assert!(cw[0][1] < 0.0, "{cw:?}");
        // An arc from a point to itself is a full circle.
        assert_eq!(arc_points(c, s, s, true, &disc(8.0)).len(), 7);
        // A circle has `circle()`'s vertices: 5 at $fn = 5, from angle 0.
        let p = circle_points([1.0, 0.0], 3.0, &disc(5.0));
        assert_eq!(p.len(), 5);
        assert_eq!(p[0], [4.0, 0.0]);
    }

    #[test]
    fn handles_print_their_kind_and_name() {
        let e = Entity {
            sketch: 1,
            id: {
                let mut s = Sketch::new();
                s.point(None).unwrap()
            },
            kind: EntityKind::Point,
            label: OnceCell::new(),
            parts: [None, None, None],
        };
        let mut out = Vec::new();
        e.write(&mut out);
        assert_eq!(out, b"<sketch point>");
        let _ = e.label.set("p".into());
        out.clear();
        e.write(&mut out);
        assert_eq!(out, b"<sketch point \"p\">");
        assert!(e.member("start").is_undef() && e.member("x").is_undef());
    }
}
