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
//!   printed with the constraints' spans and with hints that are exact
//!   edits where one is known (constraints to add, measured on the
//!   solution; a statement to delete; the drawing pinned to the
//!   solution), fillets and chamfers are cut at their corners, and the
//!   profile's closed loops become a polygon (`NodeKind::Sketch`),
//!   tessellated by `circle()`'s rule.
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

use lang::ast::Ast;
use lang::diag::{DiagCode, Hint, Severity};
use lang::number::fmt_number;
use lang::source::Span;
use sketch_solver::{
    Along, Constraint, ConstraintId, Coordinate, EntityId, EntityKind, ModelError, Orientation,
    Pair, Sketch, Solution, SolveError, SolveOptions, Source, Status,
};

use crate::builtins::functions::Builtin;
use crate::builtins::modules::{BuiltinModule, Params};
use crate::call::ArgVal;
use crate::context::{Ctx, ScopeRef};
use crate::eval::{Evaluator, Unit};
use crate::limits::Limit;
use crate::message::{Loc, R};
use crate::node::{
    ConstraintStatus, Discretizer, Node, NodeKind, SketchConstraint, SketchEdit, SketchEntity,
    SketchNode, SketchReport, SketchValues,
};
use crate::sym::{FxBuild, Sym, Syms};
use crate::trig::{atan2_degrees, cos_degrees, sin_degrees};
use crate::value::Value;

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
    /// Written as a statement of its own, so deleting its text removes
    /// it; not the radius a `circle(c, r = 5)` call states.
    written: bool,
    /// Its arguments as written: name, and the span of the expression if
    /// it is a number literal (which an edit may replace).
    args: Vec<(Option<String>, Option<lang::source::Span>)>,
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
    /// The outermost `sketch()` call's body: where suggested constraints
    /// are inserted, so they can name only the variables it binds.
    body: ScopeRef,
    /// Per entity: whether its label is a variable of `body` (or a member
    /// or element of one), so an edit inserted there can name it.
    reach: Vec<bool>,
    /// Per entity: for a point drawn at `[x, y]` in the source, the call
    /// and its argument (position and name) holding that literal, which
    /// "pin the drawing" rewrites.
    drawn: Vec<Option<(Loc, usize, &'static str)>>,
    /// Per solver constraint: added by the binding rather than written
    /// (the parallel that `distance(l1, l2, d)` implies), so it is not the
    /// author's to remove.
    implied: Vec<bool>,
    /// `anchor(name, entity)` statements: the anchors to export at the
    /// entities' solved positions, after the named entities' own
    /// (`crate::query`).
    anchors: Vec<(String, EntityId)>,
    /// An error was printed: the sketch gives an empty shape.
    failed: bool,
    /// The codes of the diagnostics printed about it so far, each once,
    /// for the tools' summary (`SketchReport::codes`).
    codes: Vec<&'static str>,
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
        drawn: Option<(Loc, usize, &'static str)>,
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
        self.reach.push(false);
        self.drawn.push(drawn);
        e
    }

    fn add(&mut self, c: Constraint, stmt: usize) -> Result<ConstraintId, ModelError> {
        let id = self.model.add(c)?;
        debug_assert_eq!(self.cons.len(), id.index());
        self.cons.push(stmt);
        self.implied.push(false);
        Ok(id)
    }

    /// An entity's name in source an edit can use, if it has one there.
    fn name_in_body(&self, id: EntityId) -> Option<&str> {
        if self.reach[id.index()] {
            self.ents[id.index()].label()
        } else {
            None
        }
    }

    /// Remember the codes of messages about to be printed about it.
    fn record(&mut self, notes: &[Note]) {
        for n in notes {
            self.code(n.code);
        }
    }

    fn code(&mut self, c: DiagCode) {
        let c = c.as_str();
        if !self.codes.contains(&c) {
            self.codes.push(c);
        }
    }

    /// Every entity with its solved values (none without a solution), for
    /// the tools (`SketchReport::entities`).
    fn entities(&self, sol: Option<&Solution>) -> Vec<SketchEntity> {
        let model = self.model.entities();
        // The coordinates the message about free degrees of freedom names
        // (`diagnose`), so the picture and the text agree.
        let mut free = vec![false; model.len()];
        if let Some(s) = sol.filter(|s| s.status == Status::Solved && s.dof > 0) {
            for f in s.free.iter().filter(|f| f.mobility >= 0.01) {
                free[f.entity.index()] = true;
            }
        }
        let moves = |i: usize| -> bool {
            free[i]
                || match model[i] {
                    sketch_solver::Entity::Point { .. } => false,
                    sketch_solver::Entity::Line { start, end } => {
                        free[start.index()] || free[end.index()]
                    }
                    sketch_solver::Entity::Arc {
                        center, start, end, ..
                    } => free[center.index()] || free[start.index()] || free[end.index()],
                    sketch_solver::Entity::Circle { center, .. } => free[center.index()],
                }
        };
        self.ents
            .iter()
            .zip(&self.ent_locs)
            .enumerate()
            .map(|(i, (e, loc))| SketchEntity {
                label: e.label().map(str::to_string),
                kind: e.kind_name(),
                construction: self.model.is_construction(e.id),
                unit: loc.unit,
                span: loc.span,
                solved: sol.and_then(|s| solved_values(&model[i], e.id, s)),
                free: moves(i),
            })
            .collect()
    }

    /// Every statement that constrains the sketch, with what the solve
    /// made of it (`SketchReport::constraints`): one entry per statement
    /// run, its solver constraints taken together.
    fn constraints(&self, sol: Option<&Solution>) -> Vec<SketchConstraint> {
        let model = self.model.constraints();
        let mut out: Vec<Option<SketchConstraint>> = vec![None; self.stmts.len()];
        let is = |s: &Source, c: usize| matches!(s, Source::Constraint(id) if id.index() == c);
        let deps = |list: &[sketch_solver::Dependency], c: usize, with: bool| {
            list.iter()
                .any(|d| is(&d.source, c) || (with && d.with.iter().any(|w| is(w, c))))
        };
        for (c, con) in model.iter().enumerate() {
            let stmt = self.cons[c];
            let st = &self.stmts[stmt];
            let e = out[stmt].get_or_insert_with(|| SketchConstraint {
                kind: constraint_kind(con),
                text: st.text.clone(),
                unit: st.loc.unit,
                span: st.loc.span,
                entities: Vec::new(),
                value: None,
                status: if sol.is_some() {
                    ConstraintStatus::Satisfied
                } else {
                    ConstraintStatus::Unknown
                },
                residual: None,
            });
            for id in constraint_entities(con) {
                if !e.entities.contains(&id.index()) {
                    e.entities.push(id.index());
                }
            }
            // The parallel that `distance(l1, l2, d)` implies is the
            // binding's: its dimension and its state are the distance's.
            if self.implied[c] {
                continue;
            }
            if e.value.is_none() {
                e.value = constraint_value(con);
            }
            let Some(s) = sol else { continue };
            let status = if deps(&s.conflicts, c, true) {
                ConstraintStatus::Conflicting
            } else if let Some((_, r)) = s.unmet.iter().find(|(x, _)| is(x, c)) {
                e.residual = Some(e.residual.map_or(*r, |o: f64| o.max(*r)));
                ConstraintStatus::Unmet
            } else if deps(&s.redundant, c, false) {
                ConstraintStatus::Redundant
            } else {
                ConstraintStatus::Satisfied
            };
            // The worst of its constraints' states.
            let rank = |s: ConstraintStatus| match s {
                ConstraintStatus::Conflicting => 3,
                ConstraintStatus::Unmet => 2,
                ConstraintStatus::Redundant => 1,
                _ => 0,
            };
            if rank(status) > rank(e.status) {
                e.status = status;
            }
        }
        for k in &self.corners {
            let st = &self.stmts[k.stmt];
            out[k.stmt].get_or_insert_with(|| SketchConstraint {
                kind: if k.round { "fillet" } else { "chamfer" },
                text: st.text.clone(),
                unit: st.loc.unit,
                span: st.loc.span,
                entities: vec![k.point.index()],
                value: Some(k.size),
                status: if sol.is_some() {
                    ConstraintStatus::Satisfied
                } else {
                    ConstraintStatus::Unknown
                },
                residual: None,
            });
        }
        out.into_iter().flatten().collect()
    }
}

/// A constraint's statement name.
fn constraint_kind(c: &Constraint) -> &'static str {
    match c {
        Constraint::Coincident(..) => "coincident",
        Constraint::On { .. } => "on",
        Constraint::Horizontal(_) => "horizontal",
        Constraint::Vertical(_) => "vertical",
        Constraint::Parallel(..) => "parallel",
        Constraint::Perpendicular(..) => "perpendicular",
        Constraint::Tangent(..) => "tangent",
        Constraint::Distance { .. } => "distance",
        Constraint::Length { .. } => "length",
        Constraint::Radius { .. } => "radius",
        Constraint::Diameter { .. } => "diameter",
        Constraint::Angle { .. } | Constraint::Sweep { .. } => "angle",
        Constraint::Equal(..) => "equal",
        Constraint::Midpoint { .. } => "midpoint",
        Constraint::Symmetric { .. } => "symmetric",
        Constraint::Fix { .. } => "fix",
    }
}

/// The entities a constraint is about, in argument order.
fn constraint_entities(c: &Constraint) -> Vec<EntityId> {
    let pair = |p: &Pair| match *p {
        Pair::Line(l) => vec![l],
        Pair::Points(a, b) => vec![a, b],
    };
    match c {
        Constraint::Coincident(a, b)
        | Constraint::Parallel(a, b)
        | Constraint::Perpendicular(a, b)
        | Constraint::Tangent(a, b)
        | Constraint::Equal(a, b) => vec![*a, *b],
        Constraint::On { point, curve } => vec![*point, *curve],
        Constraint::Horizontal(p) | Constraint::Vertical(p) => pair(p),
        Constraint::Distance { a, b, .. } => vec![*a, *b],
        Constraint::Length { line, .. } => vec![*line],
        Constraint::Radius { curve, .. } | Constraint::Diameter { curve, .. } => vec![*curve],
        Constraint::Angle { from, to, .. } => vec![*from, *to],
        Constraint::Sweep { arc, .. } => vec![*arc],
        Constraint::Midpoint { point, line } => vec![*point, *line],
        Constraint::Symmetric { a, b, about } => vec![*a, *b, *about],
        Constraint::Fix { entity, .. } => vec![*entity],
    }
}

/// A dimensional constraint's value.
fn constraint_value(c: &Constraint) -> Option<f64> {
    match *c {
        Constraint::Distance { value, .. }
        | Constraint::Length { value, .. }
        | Constraint::Radius { value, .. }
        | Constraint::Diameter { value, .. } => Some(value),
        Constraint::Angle { degrees, .. } | Constraint::Sweep { degrees, .. } => Some(degrees),
        _ => None,
    }
}

/// An entity's solved values: its points, and the lengths, angles and
/// radii they give.
fn solved_values(e: &sketch_solver::Entity, id: EntityId, s: &Solution) -> Option<SketchValues> {
    Some(match *e {
        sketch_solver::Entity::Point { .. } => SketchValues::Point(s.point(id)?),
        sketch_solver::Entity::Line { start, end } => {
            let (a, b) = (s.point(start)?, s.point(end)?);
            let d = sub(b, a);
            SketchValues::Line {
                start: a,
                end: b,
                length: norm(d),
                angle: atan2_degrees(d[1], d[0]),
            }
        }
        sketch_solver::Entity::Arc {
            center,
            start,
            end,
            clockwise,
        } => {
            let (c, a, b) = (s.point(center)?, s.point(start)?, s.point(end)?);
            let a0 = atan2_degrees(a[1] - c[1], a[0] - c[0]);
            let a1 = atan2_degrees(b[1] - c[1], b[0] - c[0]);
            // As the tessellation measures it (`arc_points`): an arc
            // from a point to itself is a full turn.
            let mut sweep = if clockwise { a0 - a1 } else { a1 - a0 };
            if sweep <= 0.0 {
                sweep += 360.0;
            }
            SketchValues::Arc {
                center: c,
                start: a,
                end: b,
                radius: s.radius(id)?,
                sweep,
                cw: clockwise,
            }
        }
        sketch_solver::Entity::Circle { center, .. } => SketchValues::Circle {
            center: s.point(center)?,
            radius: s.radius(id)?,
        },
    })
}

/// A message about a sketch, with what is needed to print it.
struct Note {
    severity: Severity,
    code: DiagCode,
    loc: Loc,
    text: String,
    hints: Vec<Fix>,
}

/// A fix hint: what to do, and the exact edit when one is known (the
/// text replacing a span; empty text deletes, an empty span inserts).
struct Fix {
    message: String,
    edit: Option<(Loc, String)>,
}

/// A hint that is advice only.
fn say(message: impl Into<String>) -> Vec<Fix> {
    vec![Fix {
        message: message.into(),
        edit: None,
    }]
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
    pub(crate) fn sketch_open(&mut self, p: &Params, loc: Loc, body: ScopeRef) {
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
            body,
            reach: Vec::new(),
            drawn: Vec::new(),
            implied: Vec::new(),
            anchors: Vec::new(),
            failed: false,
            codes: Vec::new(),
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
    ///
    /// A call that is an assignment's whole expression names what it made
    /// by its span. Otherwise the variable's value does: `p = f(point(..))`
    /// or `p = c ? point(a) : point(b)` names the point it holds, and a
    /// list of handles names its elements (`pts[0]`), so messages and
    /// suggested edits can name them as the source can.
    pub(crate) fn sketch_label(&mut self, from: usize, sr: ScopeRef, ctx: &Rc<Ctx>) {
        let Some(mut b) = self.sketch.take() else {
            return;
        };
        let unit = &self.units[sr.unit as usize];
        let scope = self.scope(sr);
        let start = from.min(b.ents.len());
        let made = &b.ents[start..];
        let locs = &b.ent_locs[start..];
        let mut named: Vec<usize> = Vec::new();
        let name = |e: &Entity, label: String, named: &mut Vec<usize>| {
            if e.sketch == b.serial && e.id.index() >= start && e.label.set(label).is_ok() {
                named.push(e.id.index());
            }
        };
        for a in &scope.assignments {
            let span = unit.ast.expr(a.expr).span;
            if let Some((e, _)) = made
                .iter()
                .zip(locs)
                .rev()
                .find(|(_, l)| l.unit == sr.unit && l.span == span)
            {
                name(e, unit.ast.name(a.name).to_string(), &mut named);
            }
        }
        for a in &scope.assignments {
            let var = unit.ast.name(a.name);
            match ctx.get_local(unit.sym(a.name), &self.regions) {
                Some(Value::Entity(e)) => name(&e, var.to_string(), &mut named),
                Some(Value::Vector(v)) => {
                    for (i, x) in v.as_slice().iter().enumerate() {
                        match x {
                            Value::Entity(e) => name(e, format!("{var}[{i}]"), &mut named),
                            Value::Vector(w) => {
                                for (j, y) in w.as_slice().iter().enumerate() {
                                    if let Value::Entity(e) = y {
                                        name(e, format!("{var}[{i}][{j}]"), &mut named);
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                }
                _ => {}
            }
        }
        let mut k = 0;
        while k < named.len() {
            let e = b.ents[named[k]].clone();
            let l = e.label().unwrap_or_default().to_string();
            for (i, part) in ["start", "end", "center"].iter().enumerate() {
                if let Some(p) = &e.parts[i] {
                    name(p, format!("{l}.{part}"), &mut named);
                }
            }
            k += 1;
        }
        // Labels given here name variables of this body: the outermost
        // sketch's body can use them in an edit; a helper's cannot.
        if sr == b.body {
            for i in named {
                b.reach[i] = true;
            }
        }
        self.sketch = Some(b);
    }

    /// `point`, `line`, `arc` and `circle`: a new entity of the sketch
    /// being built, as a handle.
    pub(crate) fn sketch_entity(&mut self, f: Builtin, loc: Loc, a: &mut Vec<ArgVal>) -> R<Value> {
        // Making an entity changes the sketch: a call or statement that
        // does cannot be replayed from a memo.
        self.untracked_sketch();
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
        b.record(&notes);
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
        self.untracked_sketch();
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
        let args = {
            let ast = self.units[sr.unit as usize].ast;
            self.inst(sr, i)
                .args
                .iter()
                .map(|a| {
                    let e = ast.expr(a.expr);
                    let number = match &e.kind {
                        lang::ast::ExprKind::Number(_) => true,
                        lang::ast::ExprKind::Unary(_, x) => {
                            matches!(ast.expr(*x).kind, lang::ast::ExprKind::Number(_))
                        }
                        _ => false,
                    };
                    (
                        a.name.map(|n| ast.name(n).to_string()),
                        number.then_some(e.span),
                    )
                })
                .collect()
        };
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
                hints: say(format!(
                    "inside a sketch, write `c = {name}(...);` with entities as its arguments"
                )),
            });
        } else {
            b.stmts.push(Stmt {
                text: text.clone(),
                loc,
                written: true,
                args,
            });
            let stmt = b.stmts.len() - 1;
            statement_in(&mut b, v, &text, stmt, &vals, loc, &mut notes, self);
        }
        if notes.iter().any(|n| n.severity == Severity::Error) {
            b.failed = true;
        }
        b.record(&notes);
        let prefix = b.prefix();
        self.sketch = Some(b);
        self.print_notes(&prefix, notes);
        Ok(())
    }

    fn print_notes(&mut self, prefix: &str, notes: Vec<Note>) {
        for n in notes {
            let text = format!("{prefix}{}", n.text);
            // An edit is located through the message's unit, so one in
            // another unit (a library's helper) is left as advice.
            let hints = n
                .hints
                .into_iter()
                .map(|f| Hint {
                    message: f.message,
                    replacement: f
                        .edit
                        .filter(|(l, _)| l.unit == n.loc.unit)
                        .map(|(l, t)| (l.span, t)),
                })
                .collect();
            self.emit_with_hints(n.severity, n.code, text.as_bytes(), Some(n.loc), hints);
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
            b.code(DiagCode::SketchGeometryInBody);
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
        // `anchor()`s with coordinates in the body, a helper module's
        // included (its nodes are dropped with the rest), stay on the
        // sketch: they are in its frame.
        let written = crate::query::anchors_in(&kids);
        drop(kids);
        if !written.is_empty() {
            node.anchors
                .get_or_insert_with(Default::default)
                .extend(written);
        }
        if !top {
            // A helper's sketch merges and leaves no node: its anchors go
            // to the node around it, which the outer sketch collects.
            if let Some(a) = node.anchors.take() {
                self.add_anchors(*a);
            }
            return Ok(None);
        }
        let b = self.sketch.take().expect("the sketch being built");
        let (kind, solved) = self.sketch_solve(*b)?;
        node.kind = kind;
        if !solved.is_empty() {
            // The entities' own first, then those written in the body.
            let mut all = solved;
            all.extend(node.anchors.take().map(|a| *a).unwrap_or_default());
            node.anchors = Some(Box::new(all));
        }
        Ok(Some(node))
    }

    /// `anchor(name, e)` with entity `e`, in a sketch body: exported at
    /// `e`'s solved position once the sketch is solved.
    pub(crate) fn sketch_anchor(&mut self, name: String, e: Rc<Entity>, loc: Loc) {
        // It changes the sketch being built, as a constraint does.
        self.untracked_sketch();
        match self.sketch.as_mut() {
            Some(b) if b.serial == e.sketch => b.anchors.push((name, e.id)),
            _ => {
                let t = format!(
                    "anchor('{name}', ...): the entity belongs to another sketch; an entity's anchor can only be set in the body of the sketch that made it"
                );
                self.error(Some(loc), DiagCode::SketchForeignEntity, t);
            }
        }
    }

    /// Drop the sketch being built after an error unwound its body.
    pub(crate) fn sketch_abandon(&mut self) {
        self.sketch = None;
    }

    /// Solve, report, and turn the profile into a polygon.
    fn sketch_solve(&mut self, mut b: Builder) -> R<(NodeKind, Vec<crate::node::Anchor>)> {
        let mut report = SketchReport {
            name: b.name.clone(),
            failed: true,
            codes: b.codes.clone(),
            entities: b.entities(None),
            constraints: b.constraints(None),
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
            return Ok((empty(report, b.convexity), Vec::new()));
        }
        // The unknowns limit, before any O(n³) work: two per point, one per
        // circle (the solver's own count).
        let unknowns: usize = b
            .model
            .entities()
            .iter()
            .map(|e| match e {
                sketch_solver::Entity::Point { .. } => 2,
                sketch_solver::Entity::Circle { .. } => 1,
                _ => 0,
            })
            .sum();
        self.over_limit(Limit::SketchUnknowns, unknowns as f64, b.loc, "sketch()");
        self.check_hard()?;
        // The solve stops between iterations when the request is
        // cancelled or out of time; the evaluator then reports which.
        let flag = self.opts.interrupt.clone();
        let guard = self.opts.guard.clone();
        let stop = move || {
            flag.as_ref().is_some_and(|f| f.load(Ordering::Relaxed))
                || guard.as_ref().is_some_and(|g| g.over_time())
        };
        let opts = SolveOptions {
            interrupt: Some(&stop),
            ..SolveOptions::default()
        };
        let prefix = b.prefix();
        let solved = b.model.solve_with(&opts).and_then(|sol| {
            let src = Src { units: &self.units };
            let notes = diagnose(&b, &sol, &src, &opts)?;
            Ok((sol, notes))
        });
        let (sol, mut notes) = match solved {
            Ok(s) => s,
            Err(SolveError::Interrupted) => {
                self.check_interrupt()?;
                self.check_limits(Some(b.loc))?;
                return Ok((empty(report, b.convexity), Vec::new()));
            }
            Err(e @ SolveError::TooManyUnknowns { .. }) => {
                // The solve itself sets no cap (the limit is checked
                // above), so this does not happen; it is reported as the
                // limit it would be.
                let t = format!("{prefix}{e}");
                self.error(Some(b.loc), DiagCode::ResourceLimit, t);
                report.codes.push(DiagCode::ResourceLimit.as_str());
                return Ok((empty(report, b.convexity), Vec::new()));
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
        report.entities = b.entities(Some(&sol));
        report.constraints = b.constraints(Some(&sol));
        report.pin = pin_drawing(&b, &sol, &Src { units: &self.units }).and_then(|f| {
            let (loc, text) = f.edit?;
            Some(SketchEdit {
                unit: loc.unit,
                span: loc.span,
                text,
            })
        });
        let mut ok = !notes.iter().any(|n| n.severity == Severity::Error);
        let mut loops = Vec::new();
        if ok {
            match profile(&b, &sol) {
                Ok((l, warnings)) => {
                    loops = l;
                    notes.extend(warnings);
                }
                Err(n) => {
                    notes.extend(n);
                    ok = false;
                }
            }
        }
        b.record(&notes);
        report.codes = b.codes.clone();
        self.print_notes(&prefix, notes);
        if !ok {
            return Ok((empty(report, b.convexity), Vec::new()));
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
        // Solved positions as anchors, for `child_anchors()`; only with
        // the queries on, the one thing that reads them.
        let anchors = if self.opts.extensions.has(crate::Extension::Query) {
            solved_anchors(&b, &sol)
        } else {
            Vec::new()
        };
        let kind = NodeKind::Sketch(Box::new(SketchNode {
            points,
            paths,
            convexity: b.convexity,
            report: Arc::new(report),
        }));
        Ok((kind, anchors))
    }
}

/// The anchors a solved sketch exports (`docs/language-extensions.md`,
/// section 5.3): every entity the outermost body names by a variable,
/// under that name (`top`, `top.start`, `pts[0]`; a helper module's
/// variables are not the sketch's to name), in entity order, then each
/// `anchor(name, entity)` in statement order. An explicit anchor replaces
/// a named entity's of the same name, so `anchor("c1", c2)` is not a
/// duplicate for `child_anchors()` to warn about.
fn solved_anchors(b: &Builder, sol: &Solution) -> Vec<crate::node::Anchor> {
    let explicit: Vec<(&str, EntityId)> = b.anchors.iter().map(|(n, e)| (n.as_str(), *e)).collect();
    let named = b.ents.iter().enumerate().filter_map(|(i, e)| {
        let label = e.label().filter(|_| b.reach[i])?;
        (!explicit.iter().any(|(n, _)| *n == label)).then_some((label, e.id))
    });
    named
        .chain(explicit.iter().copied())
        .filter_map(|(name, id)| entity_anchor(b, sol, name, id))
        .collect()
}

/// An entity's anchor: a point where it solved; a line's midpoint, with
/// its direction from start to end; an arc's or a circle's centre.
fn entity_anchor(
    b: &Builder,
    sol: &Solution,
    name: &str,
    id: EntityId,
) -> Option<crate::node::Anchor> {
    let at = |p: EntityId| sol.point(p).map(|[x, y]| [x, y, 0.0]);
    let (point, dir) = match b.model.entity(id)? {
        sketch_solver::Entity::Point { .. } => (at(id)?, None),
        sketch_solver::Entity::Line { start, end } => {
            let (s, e) = (at(*start)?, at(*end)?);
            let d = [e[0] - s[0], e[1] - s[1], 0.0];
            let mid = [(s[0] + e[0]) / 2.0, (s[1] + e[1]) / 2.0, 0.0];
            (mid, (d != [0.0; 3]).then_some(d))
        }
        sketch_solver::Entity::Arc { center, .. }
        | sketch_solver::Entity::Circle { center, .. } => (at(*center)?, None),
    };
    Some(crate::node::Anchor {
        name: name.to_string(),
        point,
        dir,
    })
}

/// The program text the diagnosis quotes and edits.
struct Src<'u, 'a> {
    units: &'u [Unit<'a>],
}

impl Src<'_, '_> {
    fn line(&self, l: Loc) -> u32 {
        self.units[l.unit as usize]
            .program
            .sources
            .get(l.span.file)
            .line_of(l.span.start)
    }

    /// The whole text of the file `l` is in.
    fn file(&self, l: Loc) -> &[u8] {
        &self.units[l.unit as usize]
            .program
            .sources
            .get(l.span.file)
            .text
    }

    fn ast(&self, l: Loc) -> &Ast {
        self.units[l.unit as usize].ast
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
                hints: Vec::new(),
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
                hints: say("pass a variable assigned from point(), line(), arc() or circle()"),
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
                hints: Vec::new(),
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
        hints: Vec::new(),
    }
}

/// A point argument of `line`, `arc` or `circle`: a point handle, or
/// `[x, y]` for a new point drawn there.
#[allow(clippy::too_many_arguments)]
fn point_arg(
    b: &mut Builder,
    v: &Value,
    what: &str,
    (pos, param): (usize, &'static str),
    loc: Loc,
    notes: &mut Vec<Note>,
    ev: &mut Evaluator<'_>,
) -> Option<Rc<Entity>> {
    if let Some(xy) = v.as_vec2(true) {
        let drawn = Some((loc, pos, param));
        return match b.model.point(Some(xy)) {
            Ok(id) => Some(b.push(id, EntityKind::Point, [None, None, None], loc, drawn)),
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
            hints: Vec::new(),
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
                            hints: Vec::new(),
                        });
                        return None;
                    }
                },
            };
            let drawn = guess.map(|_| (loc, 0, "at"));
            match b.model.point(guess) {
                Ok(id) => Some(b.push(id, EntityKind::Point, [None, None, None], loc, drawn)),
                Err(e) => {
                    notes.push(model_note(&e, &what, loc));
                    None
                }
            }
        }
        Builtin::SketchLine => {
            let p = point_arg(b, &vals[0], &what, (0, "p"), loc, notes, ev)?;
            let q = point_arg(b, &vals[1], &what, (1, "q"), loc, notes, ev)?;
            let id = match b.model.line(p.id, q.id) {
                Ok(id) => id,
                Err(e) => {
                    notes.push(model_note(&e, &what, loc));
                    return None;
                }
            };
            let _ = b.model.set_construction(id, flag(&vals[2]));
            Some(b.push(id, EntityKind::Line, [Some(p), Some(q), None], loc, None))
        }
        Builtin::SketchArc => {
            let c = point_arg(b, &vals[0], &what, (0, "center"), loc, notes, ev)?;
            let s = point_arg(b, &vals[1], &what, (1, "start"), loc, notes, ev)?;
            let e = point_arg(b, &vals[2], &what, (2, "end"), loc, notes, ev)?;
            let id = match b.model.arc(c.id, s.id, e.id, flag(&vals[3])) {
                Ok(id) => id,
                Err(err) => {
                    notes.push(model_note(&err, &what, loc));
                    return None;
                }
            };
            let _ = b.model.set_construction(id, flag(&vals[4]));
            Some(b.push(id, EntityKind::Arc, [Some(s), Some(e), Some(c)], loc, None))
        }
        _ => {
            let c = point_arg(b, &vals[0], &what, (0, "center"), loc, notes, ev)?;
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
                        hints: Vec::new(),
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
            let handle = b.push(id, EntityKind::Circle, [None, None, Some(c)], loc, None);
            if let Some(r) = radius {
                b.stmts.push(Stmt {
                    text: format!("{what} radius"),
                    loc,
                    written: false,
                    args: Vec::new(),
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
                        hints: Vec::new(),
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
                    hints: Vec::new(),
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
                    let n = b.implied.len();
                    add(b, Constraint::Parallel(x, y), notes);
                    if b.implied.len() > n {
                        b.implied[n] = true;
                    }
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
                            hints: Vec::new(),
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
                    hints: Vec::new(),
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
fn source_text(b: &Builder, s: Source, src: &Src) -> String {
    match s {
        Source::Constraint(_) => format!(
            "{} at line {}",
            source_name(b, s),
            src.line(source_loc(b, s))
        ),
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
        Source::Constraint(c) if b.implied[c.index()] => format!(
            "{} (which makes the lines parallel)",
            b.stmts[b.cons[c.index()]].text
        ),
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

/// Free coordinates listed in an under-constrained message, at most, and
/// suggested constraints offered one by one.
const FREE_LISTED: usize = 8;

/// Sketches with more unknowns get no suggested constraints: finding them
/// is a rank update per candidate equation, O(n²) each.
const SUGGEST_UNKNOWNS: usize = 400;

/// The hint that deletes statement `stmt`, when deleting its text removes
/// exactly it: written as a statement (not a circle's `r`), and run once
/// (a statement in a loop, or in a helper called twice, made several).
fn removal(b: &Builder, stmt: usize, src: &Src) -> Fix {
    let st = &b.stmts[stmt];
    let once = b
        .stmts
        .iter()
        .filter(|o| o.loc.unit == st.loc.unit && o.loc.span == st.loc.span)
        .count()
        == 1;
    let edit = (st.written && once).then(|| {
        let span = deletion(src.file(st.loc), st.loc.span);
        (
            Loc {
                unit: st.loc.unit,
                span,
            },
            String::new(),
        )
    });
    let message = if edit.is_none() && st.written {
        format!(
            "remove `{}` (it runs more than once: in a loop or a module called again)",
            st.text
        )
    } else {
        format!("remove `{}`", st.text)
    };
    Fix { message, edit }
}

/// The span that deletes the statement at `span`: with its `;`, and its
/// whole line when nothing else is on it, or the spaces after it when
/// something is, so no blank line or double space is left.
fn deletion(text: &[u8], span: Span) -> Span {
    let blank = |c: u8| c == b' ' || c == b'\t';
    let (s, mut e) = (span.start as usize, (span.end as usize).min(text.len()));
    if e == 0 || text[e - 1] != b';' {
        let mut k = e;
        while k < text.len() && blank(text[k]) {
            k += 1;
        }
        if k < text.len() && text[k] == b';' {
            e = k + 1;
        }
    }
    let mut ls = s;
    while ls > 0 && blank(text[ls - 1]) {
        ls -= 1;
    }
    let line_start = ls == 0 || text[ls - 1] == b'\n';
    let mut le = e;
    while le < text.len() && blank(text[le]) {
        le += 1;
    }
    let line_end = le == text.len() || text[le] == b'\n' || text[le] == b'\r';
    let (from, to) = match (line_start, line_end) {
        (true, true) => {
            let mut to = le;
            if to < text.len() && text[to] == b'\r' {
                to += 1;
            }
            if to < text.len() && text[to] == b'\n' {
                to += 1;
            }
            (ls, to)
        }
        (false, true) => (ls, e),
        _ => (s, le),
    };
    Span::new(span.file, from as u32, to as u32)
}

/// Where statements are inserted at the end of the body of the `sketch()`
/// call at `call`, and how each is written there: on a line of its own
/// before the closing brace, indented like the body's last line, or before
/// the brace on the same line when the body is written on one line. `None`
/// when the body is not a `{ ... }` block.
fn insertion(text: &[u8], call: Span) -> Option<(Span, String, bool)> {
    let blank = |c: u8| c == b' ' || c == b'\t';
    let open = call.start as usize;
    let mut k = (call.end as usize).min(text.len());
    while k > open && (text[k - 1].is_ascii_whitespace() || text[k - 1] == b';') {
        k -= 1;
    }
    if k == open || text[k - 1] != b'}' {
        return None;
    }
    let brace = k - 1;
    let first_brace = open + text[open..brace].iter().position(|&c| c == b'{')?;
    let mut ls = brace;
    while ls > 0 && text[ls - 1] != b'\n' {
        ls -= 1;
    }
    if ls <= first_brace || !text[ls..brace].iter().all(|&c| blank(c)) {
        let at = Span::new(call.file, brace as u32, brace as u32);
        let pad = brace > 0 && !text[brace - 1].is_ascii_whitespace();
        return Some((at, if pad { " ".into() } else { String::new() }, true));
    }
    // The indentation of the last non-blank line before the brace, unless
    // that is the line that opens the body (then one level deeper than the
    // brace).
    let brace_indent: String = text[ls..brace].iter().map(|&c| c as char).collect();
    let mut e = ls;
    let mut indent = None;
    while e > 0 {
        let mut s = e - 1;
        while s > 0 && text[s - 1] != b'\n' {
            s -= 1;
        }
        let line = &text[s..e - 1];
        if line.iter().any(|c| !c.is_ascii_whitespace()) {
            if s > first_brace {
                indent = Some(
                    line.iter()
                        .take_while(|&&c| blank(c))
                        .map(|&c| c as char)
                        .collect(),
                );
            }
            break;
        }
        e = s;
    }
    let indent = indent.unwrap_or(format!("{brace_indent}  "));
    Some((Span::new(call.file, ls as u32, ls as u32), indent, false))
}

/// A fix that inserts `stmts` (each a statement with its `;`) at the end of
/// the sketch's body, or advice only when there is nowhere to insert.
fn insert_fix(b: &Builder, src: &Src, message: String, stmts: &[&str]) -> Fix {
    let edit = insertion(src.file(b.loc), b.loc.span).map(|(span, lead, inline)| {
        let text = if inline {
            format!("{lead}{} ", stmts.join(" "))
        } else {
            stmts.iter().map(|s| format!("{lead}{s}\n")).collect()
        };
        (
            Loc {
                unit: b.loc.unit,
                span,
            },
            text,
        )
    });
    Fix { message, edit }
}

/// A number for a suggested statement: 6 significant digits, as OpenSCAD
/// prints numbers.
fn num(x: f64) -> String {
    fmt_number(if x == 0.0 { 0.0 } else { x })
}

/// `x` rounded towards zero to 6 significant digits, so that a printed
/// "at most" never exceeds what it bounds.
fn at_most(x: f64) -> f64 {
    if !(x > 0.0 && x.is_finite()) {
        return x;
    }
    let mut scale = 1.0;
    while x * scale < 1e5 {
        scale *= 10.0;
    }
    while x * scale >= 1e6 {
        scale /= 10.0;
    }
    let y = (x * scale).floor() / scale;
    if y > x {
        (x * scale - 1.0).floor() / scale
    } else {
        y
    }
}

/// A point's coordinates as source: `[x, y]`.
fn coords(p: [f64; 2]) -> String {
    format!("[{}, {}]", num(p[0]), num(p[1]))
}

/// The edit that rewrites the guesses written in the sketch's call to the
/// solved coordinates ("pin the drawing", section 4.8): one replacement
/// of the whole `sketch()` call, since a hint carries one edit. Only
/// literal `[x, y]` guesses are rewritten (a guess computed from
/// parameters keeps its expression), and only in the outermost call's
/// text.
fn pin_drawing(b: &Builder, sol: &Solution, src: &Src) -> Option<Fix> {
    use lang::ast::ExprKind;
    let call = b.loc;
    let ast = src.ast(call);
    let inside = |l: Loc| {
        l.unit == call.unit
            && l.span.file == call.span.file
            && l.span.start >= call.span.start
            && l.span.end <= call.span.end
    };
    let mut calls: HashMap<(u32, u32), &[lang::ast::Arg]> = HashMap::new();
    for e in &ast.exprs {
        if let ExprKind::Call(_, args) = &e.kind
            && e.span.file == call.span.file
            && e.span.start >= call.span.start
            && e.span.end <= call.span.end
        {
            calls.entry((e.span.start, e.span.end)).or_insert(args);
        }
    }
    let number = |id: lang::ast::ExprId| match &ast.expr(id).kind {
        ExprKind::Number(_) => true,
        ExprKind::Unary(_, x) => matches!(ast.expr(*x).kind, ExprKind::Number(_)),
        _ => false,
    };
    let text = src.file(call);
    let mut edits: Vec<(Span, String)> = Vec::new();
    for (i, d) in b.drawn.iter().enumerate() {
        let Some((at, pos, param)) = *d else { continue };
        let Some(p) = sol.point(b.ents[i].id) else {
            continue;
        };
        if !inside(at) {
            continue;
        }
        let Some(args) = calls.get(&(at.span.start, at.span.end)) else {
            continue;
        };
        let arg = args
            .iter()
            .find(|a| a.name.is_some_and(|n| ast.name(n) == param))
            .or_else(|| args.iter().filter(|a| a.name.is_none()).nth(pos));
        let Some(arg) = arg else { continue };
        let e = ast.expr(arg.expr);
        let ExprKind::Vector(v) = &e.kind else {
            continue;
        };
        if v.len() != 2 || !v.iter().all(|&x| number(x)) {
            continue;
        }
        let new = coords(p);
        if text.get(e.span.start as usize..e.span.end as usize) != Some(new.as_bytes()) {
            edits.push((e.span, new));
        }
    }
    if edits.is_empty() {
        return None;
    }
    edits.sort_by_key(|(s, _)| s.start);
    edits.dedup_by_key(|(s, _)| s.start);
    let mut out = String::new();
    let mut at = call.span.start as usize;
    for (s, t) in &edits {
        out.push_str(&String::from_utf8_lossy(&text[at..s.start as usize]));
        out.push_str(t);
        at = s.end as usize;
    }
    out.push_str(&String::from_utf8_lossy(
        &text[at..(call.span.end as usize).min(text.len())],
    ));
    Some(Fix {
        message: format!(
            "if the solved shape is the one you meant, pin the drawing to it: {} guess{} rewritten to the solved coordinates",
            edits.len(),
            if edits.len() == 1 { "" } else { "es" }
        ),
        edit: Some((call, out)),
    })
}

/// The entities an unmet constraint ties together, as points (a line's
/// ends, an arc's centre and ends, a circle's centre), for "move these
/// guesses".
fn points_of(b: &Builder, s: Source, out: &mut Vec<EntityId>) {
    let ents = b.model.entities();
    let add = |e: EntityId, out: &mut Vec<EntityId>| match ents[e.index()] {
        sketch_solver::Entity::Point { .. } => out.push(e),
        sketch_solver::Entity::Line { start, end } => out.extend([start, end]),
        sketch_solver::Entity::Arc {
            center, start, end, ..
        } => out.extend([center, start, end]),
        sketch_solver::Entity::Circle { center, .. } => out.push(center),
    };
    match s {
        Source::Arc(e) => add(e, out),
        Source::Constraint(c) => match &b.model.constraints()[c.index()] {
            Constraint::Coincident(x, y)
            | Constraint::Parallel(x, y)
            | Constraint::Perpendicular(x, y)
            | Constraint::Tangent(x, y)
            | Constraint::Equal(x, y)
            | Constraint::Distance { a: x, b: y, .. }
            | Constraint::Angle { from: x, to: y, .. } => {
                add(*x, out);
                add(*y, out);
            }
            Constraint::Horizontal(p) | Constraint::Vertical(p) => match *p {
                Pair::Line(l) => add(l, out),
                Pair::Points(x, y) => {
                    add(x, out);
                    add(y, out);
                }
            },
            Constraint::On { point, curve } => {
                add(*point, out);
                add(*curve, out);
            }
            Constraint::Midpoint { point, line } => {
                add(*point, out);
                add(*line, out);
            }
            Constraint::Symmetric { a, b: q, about } => {
                add(*a, out);
                add(*q, out);
                add(*about, out);
            }
            Constraint::Length { line: e, .. }
            | Constraint::Radius { curve: e, .. }
            | Constraint::Diameter { curve: e, .. }
            | Constraint::Sweep { arc: e, .. }
            | Constraint::Fix { entity: e, .. } => add(*e, out),
        },
    }
}

/// Constraints that would remove the free degrees of freedom, as statements
/// to insert (section 4.7): candidates measured on the solution, in the
/// order an author would usually reach for them (a line that is drawn
/// level made horizontal, lengths, radii, angles at shared corners, a
/// fixed point, and last a coordinate measured from a point that cannot
/// move), of which the solver keeps those that each remove freedom
/// ([`Sketch::completion`]). Only entities the body names can be
/// suggested.
fn suggestions(
    b: &Builder,
    sol: &Solution,
    opts: &SolveOptions<'_>,
) -> Result<Vec<String>, SolveError> {
    if sol.unknowns > SUGGEST_UNKNOWNS {
        return Ok(Vec::new());
    }
    let tiny = 1e-9 * sol.size;
    let ents = b.model.entities();
    let mut cands: Vec<(Constraint, String)> = Vec::new();
    let named = |id: EntityId| b.name_in_body(id);
    let ends = |l: EntityId| match ents[l.index()] {
        sketch_solver::Entity::Line { start, end } => Some((start, end)),
        _ => None,
    };
    let mut lines = Vec::new();
    for (i, e) in ents.iter().enumerate() {
        let id = b.ents[i].id;
        if let (sketch_solver::Entity::Line { start, end }, Some(n)) = (e, named(id))
            && let (Some(p), Some(q)) = (sol.point(*start), sol.point(*end))
        {
            lines.push((id, n, p, q));
        }
    }
    for &(id, n, p, q) in &lines {
        let d = sub(q, p);
        if d[1].abs() <= tiny && d[0].abs() > tiny {
            cands.push((
                Constraint::Horizontal(Pair::Line(id)),
                format!("horizontal({n});"),
            ));
        } else if d[0].abs() <= tiny && d[1].abs() > tiny {
            cands.push((
                Constraint::Vertical(Pair::Line(id)),
                format!("vertical({n});"),
            ));
        }
    }
    for &(id, n, p, q) in &lines {
        let len = norm(sub(q, p));
        if len > tiny {
            cands.push((
                Constraint::Length {
                    line: id,
                    value: len,
                },
                format!("length({n}, {});", num(len)),
            ));
        }
    }
    for (i, e) in ents.iter().enumerate() {
        let id = b.ents[i].id;
        if matches!(
            e,
            sketch_solver::Entity::Arc { .. } | sketch_solver::Entity::Circle { .. }
        ) && let (Some(n), Some(r)) = (named(id), sol.radius(id))
            && r > tiny
        {
            cands.push((
                Constraint::Radius {
                    curve: id,
                    value: r,
                },
                format!("radius({n}, {});", num(r)),
            ));
        }
    }
    for (k, &(l1, n1, p1, q1)) in lines.iter().enumerate() {
        for &(l2, n2, p2, q2) in &lines[k + 1..] {
            let (Some((a1, b1)), Some((a2, b2))) = (ends(l1), ends(l2)) else {
                continue;
            };
            if !(a1 == a2 || a1 == b2 || b1 == a2 || b1 == b2) {
                continue;
            }
            let (u, v) = (sub(q1, p1), sub(q2, p2));
            let deg = atan2_degrees(u[0] * v[1] - u[1] * v[0], u[0] * v[0] + u[1] * v[1]);
            // Parallel lines are a `parallel()`, and an angle near 0 or 180
            // degrees is barely a corner.
            if deg.abs() < 1.0 || deg.abs() > 179.0 {
                continue;
            }
            cands.push((
                Constraint::Angle {
                    from: l1,
                    to: l2,
                    degrees: deg,
                },
                format!("angle({n1}, {n2}, {});", num(deg)),
            ));
        }
    }
    let mut points = Vec::new();
    for (i, e) in ents.iter().enumerate() {
        let id = b.ents[i].id;
        if let (sketch_solver::Entity::Point { guess }, Some(n), Some(p)) =
            (e, named(id), sol.point(id))
        {
            points.push((id, n, p));
            let at = match guess {
                Some(g) if num(g[0]) == num(p[0]) && num(g[1]) == num(p[1]) => String::new(),
                _ => format!(", {}", coords(p)),
            };
            cands.push((
                Constraint::Fix {
                    entity: id,
                    at: Some(p),
                },
                format!("fix({n}{at});"),
            ));
        }
    }
    let free =
        |id: EntityId, c: Coordinate| sol.free.iter().any(|f| f.entity == id && f.coordinate == c);
    for f in &sol.free {
        if f.mobility < 0.01 || f.coordinate == Coordinate::Radius {
            continue;
        }
        let Some(&(id, n, p)) = points.iter().find(|x| x.0 == f.entity) else {
            continue;
        };
        let Some(&(rid, rn, r)) = points
            .iter()
            .find(|x| x.0 != id && !free(x.0, f.coordinate))
        else {
            continue;
        };
        let (k, axis, along) = match f.coordinate {
            Coordinate::X => (0, "x", Along::X),
            _ => (1, "y", Along::Y),
        };
        cands.push((
            Constraint::Distance {
                a: rid,
                b: id,
                value: p[k] - r[k],
                along,
            },
            format!(
                "distance({rn}, {n}, {}, along = \"{axis}\");",
                num(p[k] - r[k])
            ),
        ));
    }
    let cons: Vec<Constraint> = cands.iter().map(|c| c.0.clone()).collect();
    let taken = b.model.completion(sol, &cons, opts)?;
    Ok(taken.into_iter().map(|k| cands[k].1.clone()).collect())
}

/// The solve's findings as messages (section 4.7): conflicts, failure to
/// converge, redundancy, flips, points placed without a guess and free
/// degrees of freedom, each with hints that are edits where one is known.
fn diagnose(
    b: &Builder,
    sol: &Solution,
    src: &Src,
    opts: &SolveOptions<'_>,
) -> Result<Vec<Note>, SolveError> {
    let mut notes = Vec::new();
    let mut seen: Vec<(bool, usize)> = Vec::new();
    for d in &sol.conflicts {
        let key = source_key(b, d.source);
        if seen.contains(&key) {
            continue;
        }
        seen.push(key);
        let parts = listed(
            std::iter::once(d.source)
                .chain(d.with.iter().copied())
                .map(|w| source_text(b, w, src)),
        );
        // Removing any one of them may resolve it: the later one first,
        // since the earlier ones were met before it came.
        let mut hints = Vec::new();
        let mut offered: Vec<usize> = Vec::new();
        for s in std::iter::once(d.source).chain(d.with.iter().rev().copied()) {
            if let Source::Constraint(c) = s {
                let stmt = b.cons[c.index()];
                if !offered.contains(&stmt) && offered.len() < 4 && b.stmts[stmt].written {
                    offered.push(stmt);
                    hints.push(removal(b, stmt, src));
                }
            }
        }
        hints.extend(say("or change the values so that they agree"));
        notes.push(Note {
            severity: Severity::Error,
            code: DiagCode::SketchConflict,
            loc: source_loc(b, d.source),
            text: format!("constraints conflict: {parts}"),
            hints,
        });
    }
    if sol.status == Status::NotConverged && sol.conflicts.is_empty() {
        let unmet: Vec<String> = sol
            .unmet
            .iter()
            .take(3)
            .map(|(s, _)| source_text(b, *s, src))
            .collect();
        let mut pts = Vec::new();
        for (s, _) in sol.unmet.iter().take(3) {
            points_of(b, *s, &mut pts);
        }
        let mut names: Vec<String> = Vec::new();
        for p in pts {
            let n = b.describe(p);
            if !names.contains(&n) {
                names.push(n);
            }
        }
        let hint = if names.is_empty() {
            "check the guesses: the drawing may be far from any solution".to_string()
        } else {
            format!(
                "move the guesses of {} closer to a shape that meets {}",
                names.join(", "),
                if unmet.len() == 1 {
                    "that constraint"
                } else {
                    "those constraints"
                }
            )
        };
        notes.push(Note {
            severity: Severity::Error,
            code: DiagCode::SketchNoConvergence,
            loc: b.loc,
            text: format!(
                "did not converge (residual {}{}){}",
                fmt_number(sol.residual),
                if sol.continuation {
                    " after continuation"
                } else {
                    ""
                },
                if unmet.is_empty() {
                    String::new()
                } else {
                    format!("; not met: {}", unmet.join(", "))
                }
            ),
            hints: say(hint),
        });
    }
    let mut seen: Vec<(bool, usize)> = Vec::new();
    for d in &sol.redundant {
        // The parallel a line-to-line distance adds is the binding's, not
        // the author's: lines already parallel (two horizontal edges) make
        // it redundant, and the warning would name the distance, whose
        // removal would lose a dimension.
        if let Source::Constraint(c) = d.source
            && b.implied[c.index()]
        {
            continue;
        }
        let key = source_key(b, d.source);
        if seen.contains(&key) {
            continue;
        }
        seen.push(key);
        let with = listed(d.with.iter().map(|w| source_text(b, *w, src)));
        let hints = match d.source {
            Source::Constraint(c) => vec![removal(b, b.cons[c.index()], src)],
            Source::Arc(_) => Vec::new(),
        };
        notes.push(Note {
            severity: Severity::Warning,
            code: DiagCode::SketchRedundant,
            loc: source_loc(b, d.source),
            text: format!(
                "{} is implied by the other constraints: {}",
                source_name(b, d.source),
                with
            ),
            hints,
        });
    }
    let pin = if sol.flipped.is_empty() || sol.status != Status::Solved {
        None
    } else {
        pin_drawing(b, sol, src)
    };
    for o in &sol.flipped {
        let (what, loc, advice) = match *o {
            Orientation::ArcSweep(e) => (
                format!(
                    "arc {} sweeps the other side of 180 degrees than drawn",
                    b.describe(e)
                ),
                b.ent_locs[e.index()],
                format!(
                    "otherwise draw arc {}'s ends so that it sweeps the way you mean",
                    b.describe(e)
                ),
            ),
            Orientation::AngleBranch(c) => {
                let s = Source::Constraint(c);
                (
                    format!(
                        "{} is met 180 degrees away from the drawn angle",
                        source_name(b, s)
                    ),
                    source_loc(b, s),
                    "otherwise draw the lines closer to the angle you mean".to_string(),
                )
            }
            Orientation::RadiusSign(e) => (
                format!("circle {} came out with a negative radius", b.describe(e)),
                b.ent_locs[e.index()],
                format!("otherwise give circle {} a radius", b.describe(e)),
            ),
            Orientation::TangentSide(c) => {
                let s = Source::Constraint(c);
                (
                    format!("{} solved on the other side than drawn", source_name(b, s)),
                    source_loc(b, s),
                    "otherwise draw the curves touching on the side you mean".to_string(),
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
                format!(
                    "otherwise move the guesses of {} and its neighbours closer to the shape you mean",
                    b.describe(point)
                ),
            ),
        };
        let mut hints: Vec<Fix> = pin
            .iter()
            .map(|f| Fix {
                message: f.message.clone(),
                edit: f.edit.clone(),
            })
            .collect();
        hints.extend(say(if pin.is_some() {
            advice
        } else {
            "move the guesses closer to the intended shape".to_string()
        }));
        notes.push(Note {
            severity: Severity::Warning,
            code: DiagCode::SketchFlipped,
            loc,
            text: what,
            hints,
        });
    }
    // Points written without a guess (section 4.2): the solver placed
    // them, so where they end up depends on that placement rather than on
    // the drawing. The fix writes the solved position in.
    for &id in &sol.placed {
        if b.ents[id.index()].kind != EntityKind::Point {
            continue;
        }
        let Some(p) = sol.point(id) else { continue };
        let loc = b.ent_locs[id.index()];
        let text = src.file(loc);
        let call = text
            .get(loc.span.start as usize..loc.span.end as usize)
            .map(statement_text)
            .unwrap_or_default();
        let new = format!("point({})", coords(p));
        let edit = (call == "point()").then(|| (loc, new.clone()));
        notes.push(Note {
            severity: Severity::Info,
            code: DiagCode::SketchNoGuess,
            loc,
            text: format!(
                "point {} has no guess, so the solver placed it; it solved to {}",
                b.describe(id),
                coords(p)
            ),
            hints: vec![Fix {
                message: format!("give it a guess: `{new}`"),
                edit,
            }],
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
        let add = suggestions(b, sol, opts)?;
        let mut hints = Vec::new();
        if add.len() > 1 {
            let all: Vec<&str> = add.iter().map(String::as_str).collect();
            hints.push(insert_fix(
                b,
                src,
                format!("add all {}: `{}`", add.len(), all.join(" ")),
                &all,
            ));
        }
        for a in add.iter().take(FREE_LISTED) {
            hints.push(insert_fix(b, src, format!("add `{a}`"), &[a]));
        }
        if add.is_empty() {
            hints.extend(say(
                "add a dimension, or fix() what should not move (suggestions name the entities assigned to variables in the sketch's own body)",
            ));
        }
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
            hints,
        });
    }
    // A statement in a loop is many constraints with one text and span:
    // one message for them all.
    let mut kept: Vec<Note> = Vec::with_capacity(notes.len());
    for n in notes {
        if !kept
            .iter()
            .any(|k| k.code == n.code && k.loc == n.loc && k.text == n.text)
        {
            kept.push(n);
        }
    }
    Ok(kept)
}

/// Statements named in one message, at most: a constraint in a loop can
/// depend on hundreds of others.
const STATEMENTS_LISTED: usize = 6;

/// `items` joined with commas, each once, the first [`STATEMENTS_LISTED`]
/// and how many more.
fn listed(items: impl Iterator<Item = String>) -> String {
    let mut out: Vec<String> = Vec::new();
    for i in items {
        if !out.contains(&i) {
            out.push(i);
        }
    }
    let more = out.len().saturating_sub(STATEMENTS_LISTED);
    out.truncate(STATEMENTS_LISTED);
    let mut s = out.join(", ");
    if more > 0 {
        s.push_str(&format!(" and {more} more"));
    }
    s
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
    let arcs = inc
        .iter()
        .filter(|(ci, _)| g.curves[*ci].arc.is_some())
        .count();
    if inc.len() != 2 || arcs == 2 {
        let text = if inc.len() == 2 {
            format!(
                "{}: the corner {} joins two arcs; fillets and chamfers between two arcs are not supported yet",
                stmt.text,
                b.describe(k.point)
            )
        } else {
            format!(
                "{}: the corner {} must join exactly two profile curves, lines or a line and an arc, and it joins {}",
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
            hints: Vec::new(),
        });
    }
    let far = |(ci, at_b): (usize, bool), g: &Graph| {
        if at_b { g.curves[ci].a } else { g.curves[ci].b }
    };
    if arcs == 1 {
        // The line first, then the arc.
        let (line, arc) = if g.curves[inc[0].0].arc.is_none() {
            (inc[0], inc[1])
        } else {
            (inc[1], inc[0])
        };
        return cut_line_arc(b, g, k, v, line, arc, far(line, g), far(arc, g));
    }
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
            hints: Vec::new(),
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
                hints: vec![size_fix(b, k, at_most(most))],
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

/// Where a fillet or chamfer of a line–arc corner cuts: the point on the
/// line, the point on the arc, and for a fillet its centre.
struct LineArcCut {
    on_line: [f64; 2],
    on_arc: [f64; 2],
    center: Option<[f64; 2]>,
}

/// Why a line–arc corner cannot be cut at the size asked: the cut needs
/// more of the line or of the arc than there is (lengths along each), a
/// fillet inside the arc's circle is not smaller than it, no circle of
/// that size touches both, or there is no corner at all (the line is
/// tangent to the arc there).
enum NoCut {
    Line { need: f64, have: f64 },
    Arc { need: f64, have: f64 },
    Radius { have: f64 },
    Fit,
    Smooth,
}

/// The geometry of a line–arc corner, all from the solved points.
struct LineArc {
    /// The corner.
    p: [f64; 2],
    /// Unit direction of the line away from the corner, and its length.
    u: [f64; 2],
    len: f64,
    /// The arc's centre and radius (the corner's distance from it).
    c: [f64; 2],
    r: f64,
    /// Unit tangent of the arc leaving the corner, and whether that is
    /// counter-clockwise about the centre.
    w: [f64; 2],
    ccw: bool,
    /// The arc's sweep from the corner to its other end, in degrees.
    sweep: f64,
}

impl LineArc {
    /// How far round the arc from the corner `x` is, in degrees, in the
    /// arc's direction from the corner: (0, 360].
    fn angle_to(&self, x: [f64; 2]) -> f64 {
        let (a, b) = (sub(self.p, self.c), sub(x, self.c));
        let cross = a[0] * b[1] - a[1] * b[0];
        let dot = a[0] * b[0] + a[1] * b[1];
        let mut t = atan2_degrees(cross, dot);
        if !self.ccw {
            t = -t;
        }
        if t <= 0.0 {
            t += 360.0;
        }
        t
    }

    /// Arc length for `deg` degrees of the arc.
    fn arc_len(&self, deg: f64) -> f64 {
        self.r * deg * std::f64::consts::PI / 180.0
    }

    /// The cut for a fillet of radius `size` (`round`) or a chamfer of
    /// `size` along each curve from the corner. Only square roots: no
    /// trigonometry decides where it lands (section 4.5).
    ///
    /// A fillet's centre is `size` from the line, on the corner's side,
    /// and `r + size` from the arc's centre, or `r - size` when the line
    /// runs into the arc's circle (the corner is then inside it): a point
    /// at `s` along the line's offset meets that circle where
    /// `s^2 + 2s(D.u) + |D|^2 - rho^2 = 0`, and the root nearest the
    /// corner is the fillet next to it. A chamfer cuts the arc where the
    /// circle of radius `size` about the corner crosses it.
    fn cut(&self, size: f64, round: bool) -> Result<LineArcCut, NoCut> {
        let (p, u, c, w, r) = (self.p, self.u, self.c, self.w, self.r);
        let cross = u[0] * w[1] - u[1] * w[0];
        let dot = u[0] * w[0] + u[1] * w[1];
        if !cross.is_finite() || cross.abs() < 1e-12 {
            return Err(NoCut::Smooth);
        }
        let (on_line, on_arc, center) = if round {
            let s_abs = cross.abs();
            // The line's normal towards the arc's tangent: the corner's
            // inside.
            let n = [(w[0] - dot * u[0]) / s_abs, (w[1] - dot * u[1]) / s_abs];
            let inside = u[0] * (c[0] - p[0]) + u[1] * (c[1] - p[1]) > 0.0;
            let rho = if inside { r - size } else { r + size };
            if rho <= 0.0 {
                return Err(NoCut::Radius { have: r });
            }
            let d = [p[0] + size * n[0] - c[0], p[1] + size * n[1] - c[1]];
            let bq = d[0] * u[0] + d[1] * u[1];
            let disc = bq * bq - (d[0] * d[0] + d[1] * d[1] - rho * rho);
            if disc < 0.0 {
                return Err(NoCut::Fit);
            }
            let root = disc.sqrt();
            let s = [-bq - root, -bq + root]
                .into_iter()
                .filter(|s| *s > 0.0)
                .fold(f64::INFINITY, f64::min);
            if !s.is_finite() {
                return Err(NoCut::Fit);
            }
            let f = [p[0] + s * u[0] + size * n[0], p[1] + s * u[1] + size * n[1]];
            let fc = sub(f, c);
            let l = norm(fc);
            let on_arc = [c[0] + r * fc[0] / l, c[1] + r * fc[1] / l];
            ([p[0] + s * u[0], p[1] + s * u[1]], on_arc, Some(f))
        } else {
            if size >= 2.0 * r {
                return Err(NoCut::Arc {
                    need: size,
                    have: 2.0 * r,
                });
            }
            // Along the radius towards the centre by size^2 / 2r, then
            // along the leaving tangent by what is left of `size`.
            let e = [(c[0] - p[0]) / r, (c[1] - p[1]) / r];
            let along = size * size / (2.0 * r);
            let side = (size * size - along * along).max(0.0).sqrt();
            let on_arc = [
                p[0] + along * e[0] + side * w[0],
                p[1] + along * e[1] + side * w[1],
            ];
            ([p[0] + size * u[0], p[1] + size * u[1]], on_arc, None)
        };
        let s = norm(sub(on_line, p));
        if s > self.len {
            return Err(NoCut::Line {
                need: s,
                have: self.len,
            });
        }
        let t = self.angle_to(on_arc);
        if t >= self.sweep {
            return Err(NoCut::Arc {
                need: self.arc_len(t.min(360.0)),
                have: self.arc_len(self.sweep),
            });
        }
        Ok(LineArcCut {
            on_line,
            on_arc,
            center,
        })
    }
}

/// [`cut_corner`] for a corner between a line and an arc (section 4.5):
/// the line is trimmed, the arc shortened on its own circle, and a
/// tangent arc (or a line) joins the cuts.
#[allow(clippy::too_many_arguments)]
fn cut_line_arc(
    b: &Builder,
    g: &mut Graph,
    k: &Corner,
    v: usize,
    line: (usize, bool),
    arc: (usize, bool),
    line_far: usize,
    arc_far: usize,
) -> Result<(), Note> {
    let stmt = &b.stmts[k.stmt];
    let p = g.verts[v];
    let q = sub(g.verts[line_far], p);
    let len = norm(q);
    let (c, arc_ccw) = g.curves[arc.0].arc.expect("an arc");
    let r = norm(sub(p, c));
    // The arc runs counter-clockwise from its `a` end to its `b` end when
    // `arc_ccw`; leaving the corner, it turns the other way when the
    // corner is its `b` end.
    let ccw = arc_ccw != arc.1;
    let rad = sub(p, c);
    let w = if ccw {
        [-rad[1] / r, rad[0] / r]
    } else {
        [rad[1] / r, -rad[0] / r]
    };
    let mut geo = LineArc {
        p,
        u: [q[0] / len, q[1] / len],
        len,
        c,
        r,
        w,
        ccw,
        sweep: 360.0,
    };
    let z = g.verts[arc_far];
    geo.sweep = if z == p { 360.0 } else { geo.angle_to(z) };
    let fail = |why: NoCut| -> Note {
        match why {
            NoCut::Smooth => Note {
                severity: Severity::Error,
                code: DiagCode::InvalidArgument,
                loc: stmt.loc,
                text: format!(
                    "{}: the line and the arc at {} are tangent there, so there is no corner to cut",
                    stmt.text,
                    b.describe(k.point)
                ),
                hints: Vec::new(),
            },
            why => {
                // The largest size that fits, by bisection: the cut grows
                // with the size, but not in closed form.
                let (mut lo, mut hi) = (0.0, k.size);
                for _ in 0..60 {
                    let mid = (lo + hi) / 2.0;
                    if geo.cut(mid, k.round).is_ok() {
                        lo = mid;
                    } else {
                        hi = mid;
                    }
                }
                let (l, e) = (
                    b.describe(g.curves[line.0].src),
                    b.describe(g.curves[arc.0].src),
                );
                let text = match why {
                    NoCut::Line { need, have } => format!(
                        "{} needs {} along {l}, which is {} long",
                        stmt.text,
                        fmt_number(need),
                        fmt_number(have)
                    ),
                    NoCut::Arc { need, have } => format!(
                        "{} needs {} along {e}, which is {} long",
                        stmt.text,
                        fmt_number(need),
                        fmt_number(have)
                    ),
                    NoCut::Radius { have } => format!(
                        "{} sits inside arc {e}, so its radius must be under the arc's, {}",
                        stmt.text,
                        fmt_number(have)
                    ),
                    _ => format!(
                        "{}: no arc of that size touches both {l} and {e} near {}",
                        stmt.text,
                        b.describe(k.point)
                    ),
                };
                Note {
                    severity: Severity::Error,
                    code: DiagCode::SketchFilletTooLarge,
                    loc: stmt.loc,
                    text,
                    hints: vec![size_fix(b, k, at_most(lo))],
                }
            }
        }
    };
    let cut = geo.cut(k.size, k.round).map_err(fail)?;
    let t1 = g.add_vertex(cut.on_line, k.point);
    let t2 = g.add_vertex(cut.on_arc, k.point);
    for (&(ci, at_b), t) in [line, arc].iter().zip([t1, t2]) {
        if at_b {
            g.curves[ci].b = t;
        } else {
            g.curves[ci].a = t;
        }
    }
    let arc_of = cut.center.map(|f| {
        let (from, to) = (sub(cut.on_line, f), sub(cut.on_arc, f));
        // A fillet always turns the short way round.
        (f, from[0] * to[1] - from[1] * to[0] > 0.0)
    });
    g.curves.push(Curve {
        a: t1,
        b: t2,
        arc: arc_of,
        src: k.point,
    });
    Ok(())
}

/// The hint for a fillet or chamfer too large for its corner: its size
/// argument replaced with the largest that fits, when the statement runs
/// once and the size is written as a number.
fn size_fix(b: &Builder, k: &Corner, most: f64) -> Fix {
    let st = &b.stmts[k.stmt];
    let param = if k.round { "r" } else { "d" };
    let once = b
        .stmts
        .iter()
        .filter(|o| o.loc.unit == st.loc.unit && o.loc.span == st.loc.span)
        .count()
        == 1;
    let arg = st
        .args
        .iter()
        .find(|(n, _)| n.as_deref() == Some(param))
        .or_else(|| st.args.iter().filter(|(n, _)| n.is_none()).nth(1));
    let edit = match arg {
        Some((_, Some(span))) if once => Some((
            Loc {
                unit: st.loc.unit,
                span: *span,
            },
            num(most),
        )),
        _ => None,
    };
    Fix {
        message: format!("make it at most {}", num(most)),
        edit,
    }
}

/// Where the profile's loops cross each other or themselves (section 4.4):
/// the even-odd fill then gives a shape the author probably did not mean.
/// `loops[i][k]` to the next point is a segment of curve `srcs[i][k]`.
/// Proper crossings only: loops that touch at a point, or run along each
/// other, are not reported. One note per pair of curves, at most
/// [`CROSSINGS_LISTED`].
fn crossings(b: &Builder, loops: &[Vec<[f64; 2]>], srcs: &[Vec<EntityId>], size: f64) -> Vec<Note> {
    struct Seg {
        p: [f64; 2],
        q: [f64; 2],
        lo: [f64; 2],
        hi: [f64; 2],
        ring: usize,
        k: usize,
    }
    let mut segs = Vec::new();
    for (ring, l) in loops.iter().enumerate() {
        let n = l.len();
        for k in 0..n {
            let (p, q) = (l[k], l[(k + 1) % n]);
            segs.push(Seg {
                p,
                q,
                lo: [p[0].min(q[0]), p[1].min(q[1])],
                hi: [p[0].max(q[0]), p[1].max(q[1])],
                ring,
                k,
            });
        }
    }
    // A sweep along x: each segment meets only those whose x range starts
    // before its own ends. Sorted with ties in input order, so the notes
    // come out the same everywhere.
    segs.sort_by(|a, b| a.lo[0].total_cmp(&b.lo[0]));
    let orient = |a: [f64; 2], b: [f64; 2], c: [f64; 2]| {
        (b[0] - a[0]) * (c[1] - a[1]) - (b[1] - a[1]) * (c[0] - a[0])
    };
    let eps = 1e-12 * size * size;
    let mut found: Vec<(EntityId, EntityId, [f64; 2])> = Vec::new();
    // A bound on the work for a profile of very many vertices.
    let mut budget: u64 = 20_000_000;
    'outer: for (i, s) in segs.iter().enumerate() {
        for t in &segs[i + 1..] {
            if t.lo[0] > s.hi[0] {
                break;
            }
            budget = budget.saturating_sub(1);
            if budget == 0 {
                break 'outer;
            }
            if t.lo[1] > s.hi[1] || t.hi[1] < s.lo[1] {
                continue;
            }
            if s.ring == t.ring {
                let n = loops[s.ring].len();
                if (s.k + 1) % n == t.k || (t.k + 1) % n == s.k {
                    continue;
                }
            }
            let d1 = orient(t.p, t.q, s.p);
            let d2 = orient(t.p, t.q, s.q);
            let d3 = orient(s.p, s.q, t.p);
            let d4 = orient(s.p, s.q, t.q);
            let proper = (d1 > eps && d2 < -eps || d1 < -eps && d2 > eps)
                && (d3 > eps && d4 < -eps || d3 < -eps && d4 > eps);
            if !proper {
                continue;
            }
            let (x, y) = (srcs[s.ring][s.k], srcs[t.ring][t.k]);
            let (x, y) = if x <= y { (x, y) } else { (y, x) };
            if found.iter().any(|f| f.0 == x && f.1 == y) {
                continue;
            }
            let f = d1 / (d1 - d2);
            let at = [
                s.p[0] + f * (s.q[0] - s.p[0]),
                s.p[1] + f * (s.q[1] - s.p[1]),
            ];
            found.push((x, y, at));
        }
    }
    found.sort_by_key(|f| (f.0, f.1));
    let curve = |id: EntityId| {
        let e = &b.ents[id.index()];
        if e.kind == EntityKind::Point {
            format!("the corner cut at {}", b.describe(id))
        } else {
            format!("{} {}", e.kind_name(), b.describe(id))
        }
    };
    found
        .into_iter()
        .take(CROSSINGS_LISTED)
        .map(|(x, y, at)| Note {
            severity: Severity::Warning,
            code: DiagCode::SketchSelfIntersection,
            loc: b.ent_locs[x.index()],
            text: if x == y {
                format!("{} crosses itself near {}", curve(x), coords(at))
            } else {
                format!(
                    "{} crosses {} near {}; the profile fills even-odd, so where loops cross the overlap is left out",
                    curve(x),
                    curve(y),
                    coords(at)
                )
            },
            hints: say(
                "move the guesses (or the dimensions) so that the curves do not cross, or mark one `construction = true`",
            ),
        })
        .collect()
}

/// Crossings reported per sketch, at most.
const CROSSINGS_LISTED: usize = 4;

/// Every closed loop of the profile as polygon points, after cutting the
/// fillets and chamfers (sections 4.4 and 4.5), with warnings about loops
/// that cross; or the notes saying why there is no profile.
#[allow(clippy::type_complexity)]
fn profile(b: &Builder, sol: &Solution) -> Result<(Vec<Vec<[f64; 2]>>, Vec<Note>), Vec<Note>> {
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
    let mut circles: Vec<([f64; 2], f64, EntityId)> = Vec::new();
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
                circles.push((c, sol.radius(src).unwrap_or(0.0), src));
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
                    hints: say("share the point, or mark the curve `construction = true`"),
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
                hints: say("mark the extra curves `construction = true`"),
            });
        }
    }
    if !notes.is_empty() {
        return Err(notes);
    }
    let mut loops = Vec::new();
    let mut srcs: Vec<Vec<EntityId>> = Vec::new();
    let mut used = vec![false; g.curves.len()];
    for start in 0..g.curves.len() {
        if used[start] {
            continue;
        }
        let mut pts: Vec<[f64; 2]> = Vec::new();
        let mut from_curve: Vec<EntityId> = Vec::new();
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
            from_curve.resize(pts.len(), c.src);
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
        srcs.push(from_curve);
    }
    for (c, r, id) in circles {
        let pts = circle_points(c, r, &b.disc);
        if !pts.is_empty() {
            srcs.push(vec![id; pts.len()]);
            loops.push(pts);
        }
    }
    let warnings = crossings(b, &loops, &srcs, sol.size);
    Ok((loops, warnings))
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

    fn span(text: &str, of: &str) -> Span {
        let at = text.find(of).unwrap() as u32;
        Span::new(lang::source::FileId(0), at, at + of.len() as u32)
    }

    fn cut(text: &str, s: Span) -> String {
        format!("{}{}", &text[..s.start as usize], &text[s.end as usize..])
    }

    /// A deleted statement takes its line when it is alone on it, and its
    /// `;` and the space after it when it is not, so the edit leaves
    /// neither a blank line nor a double space.
    #[test]
    fn deleting_a_statement_takes_its_line_or_its_space() {
        let t = "sketch() {\n  fix(o);\n  length(l, 3);\n}\n";
        assert_eq!(
            cut(t, deletion(t.as_bytes(), span(t, "length(l, 3);"))),
            "sketch() {\n  fix(o);\n}\n"
        );
        // A span without its `;` still takes it.
        assert_eq!(
            cut(t, deletion(t.as_bytes(), span(t, "length(l, 3)"))),
            "sketch() {\n  fix(o);\n}\n"
        );
        let t = "  fix(o); horizontal(l); length(l, 3);\n";
        assert_eq!(
            cut(t, deletion(t.as_bytes(), span(t, "horizontal(l);"))),
            "  fix(o); length(l, 3);\n"
        );
        assert_eq!(
            cut(t, deletion(t.as_bytes(), span(t, "length(l, 3);"))),
            "  fix(o); horizontal(l);\n"
        );
    }

    /// Statements are inserted before the body's closing brace: on their
    /// own lines, indented like the body's last line, or on the brace's
    /// line when the body is written on one line.
    #[test]
    fn insertions_go_before_the_closing_brace() {
        let t = "sketch(name = \"s\") {\n    a = point([0, 0]);\n    fix(a);\n}\n";
        let (at, indent, inline) = insertion(t.as_bytes(), span(t, t.trim_end())).unwrap();
        assert_eq!((at.start, at.end), (t.rfind('}').unwrap() as u32, at.start));
        assert_eq!((indent.as_str(), inline), ("    ", false));
        let t = "sketch() { a = point(); }";
        let (at, lead, inline) = insertion(t.as_bytes(), span(t, t)).unwrap();
        assert_eq!(at.start as usize, t.rfind('}').unwrap());
        assert_eq!((lead.as_str(), inline), ("", true));
        // An empty body: one level deeper than the brace.
        let t = "  sketch() {\n  }\n";
        let (_, indent, _) = insertion(t.as_bytes(), span(t, t.trim())).unwrap();
        assert_eq!(indent, "    ");
        // A body that is not a block has nowhere to insert.
        let t = "sketch() fix(a);";
        assert!(insertion(t.as_bytes(), span(t, t)).is_none());
    }

    /// "At most" is rounded down, so the printed number fits.
    #[test]
    fn at_most_never_rounds_up() {
        let x = 7.071_067_811_865_476;
        assert!(at_most(x) <= x);
        assert_eq!(num(at_most(x)), "7.07106");
        assert_eq!(at_most(10.0), 10.0);
        assert_eq!(
            listed(
                ["a", "b", "a", "c", "d", "e", "f", "g", "h"]
                    .iter()
                    .map(|s| s.to_string())
            ),
            "a, b, c, d, e, f and 2 more"
        );
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
