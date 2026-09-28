//! The evaluator: units, the context stack, messages and expressions.

use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::{Rc, Weak};
use std::sync::atomic::Ordering;

use lang::Program;
use lang::ast::{Arg, Ast, BinaryOp, ExprId, ExprKind, Scope, UnaryOp};
use lang::diag::{DiagCode, Diagnostic, PathBase, Severity};
use lang::source::Span;

use crate::Camera;
use crate::context::{Ctx, CtxKind, ScopeRef};
use crate::message::{Loc, Message, Output, Pending, R, Unwind, UnwindKind};
use crate::node::{Node, NodeKind};
use crate::ops::{self, Bitwise};
use crate::resolve::{BUILTIN_REGION, Cand, NO_SLOT, Ref, Region};
use crate::rng::Mt19937;
use crate::sym::{FxBuild, Sym, Syms};
use crate::value::{FunctionValue, Str, Value};
use crate::{Evaluation, Library, Options};

/// Per-scope lookup tables, built once per evaluation.
pub(crate) struct ScopeInfo<'a> {
    pub scope: &'a Scope,
    pub functions: HashMap<Sym, u32, FxBuild>,
    pub modules: HashMap<Sym, u32, FxBuild>,
    /// Scope id of each instantiation's children.
    pub children: Vec<u32>,
    /// Scope id of each instantiation's `else` branch, or `u32::MAX`.
    pub else_children: Vec<u32>,
    /// Scope id of each module definition's body.
    pub bodies: Vec<u32>,
}

/// One parsed program (the main file with its includes, or a library).
pub(crate) struct Unit<'a> {
    pub program: &'a Program,
    pub ast: &'a Ast,
    /// Maps the program's own names to evaluation-wide symbols.
    pub syms: Rc<[Sym]>,
    pub scopes: Vec<ScopeInfo<'a>>,
    /// Constant values of literal expressions (strings and vectors of
    /// literals), so hot loops do not rebuild them.
    pub consts: Vec<Option<Value>>,
    /// Used libraries, as unit indices, in search order.
    pub uses: Vec<u32>,
    /// Per call expression, whether an argument is an accumulator
    /// (`Evaluator::move_accumulators`): 0 not yet known, 1 no, 2 yes.
    /// Sized on the first call, so a unit that calls nothing costs nothing.
    pub accumulates: Vec<u8>,
    /// Where each name reference can be bound (see [`crate::resolve`]).
    pub res: crate::resolve::UnitRes,
}

impl<'a> Unit<'a> {
    fn new(program: &'a Program, syms: &mut Syms) -> Unit<'a> {
        let ast = &program.ast;
        let names: Rc<[Sym]> = ast.names.iter().map(|s| syms.intern(s)).collect();
        let mut u = Unit {
            program,
            ast,
            syms: names,
            scopes: Vec::new(),
            consts: Vec::new(),
            uses: Vec::new(),
            accumulates: Vec::new(),
            res: crate::resolve::UnitRes::default(),
        };
        u.add_scope(&ast.root);
        u.consts = (0..ast.exprs.len())
            .map(|i| const_value(ast, ExprId(i as u32)))
            .collect();
        u
    }

    fn add_scope(&mut self, scope: &'a Scope) -> u32 {
        let id = self.scopes.len() as u32;
        let mut functions = HashMap::default();
        for (i, f) in scope.functions.iter().enumerate() {
            functions.insert(self.syms[f.name.0 as usize], i as u32);
        }
        let mut modules = HashMap::default();
        for (i, m) in scope.modules.iter().enumerate() {
            modules.insert(self.syms[m.name.0 as usize], i as u32);
        }
        self.scopes.push(ScopeInfo {
            scope,
            functions,
            modules,
            children: Vec::new(),
            else_children: Vec::new(),
            bodies: Vec::new(),
        });
        let bodies: Vec<u32> = scope
            .modules
            .iter()
            .map(|m| self.add_scope(&m.body))
            .collect();
        let mut children = Vec::new();
        let mut elses = Vec::new();
        for inst in &scope.instantiations {
            children.push(self.add_scope(&inst.children));
            elses.push(match &inst.kind {
                lang::ast::InstKind::If {
                    else_children: Some(e),
                } => self.add_scope(e),
                _ => u32::MAX,
            });
        }
        let info = &mut self.scopes[id as usize];
        info.bodies = bodies;
        info.children = children;
        info.else_children = elses;
        id
    }

    pub fn sym(&self, n: lang::ast::Name) -> Sym {
        self.syms[n.0 as usize]
    }
}

/// The value of an expression that is the same every time and prints no
/// warnings: literals and vectors of them. Ranges are excluded because a
/// literal backwards range warns each time it is evaluated.
fn const_value(ast: &Ast, id: ExprId) -> Option<Value> {
    match &ast.expr(id).kind {
        ExprKind::String(s) => Some(Value::Str(Str::new(s))),
        ExprKind::Vector(items) if !items.is_empty() => {
            let mut out = Vec::with_capacity(items.len());
            for &e in items {
                out.push(match &ast.expr(e).kind {
                    ExprKind::Undef => Value::Undef,
                    ExprKind::Bool(b) => Value::Bool(*b),
                    ExprKind::Number(n) => Value::Number(*n),
                    ExprKind::String(_) | ExprKind::Vector(_) => const_value(ast, e)?,
                    _ => return None,
                });
            }
            Some(Value::vector(out))
        }
        _ => None,
    }
}

/// Symbols the evaluator refers to by name.
pub(crate) struct Known {
    pub children: Sym,
    pub parent_modules: Sym,
    pub fn_: Sym,
    pub fa: Sym,
    pub fs: Sym,
    pub preview: Sym,
    pub t: Sym,
    pub vpt: Sym,
    pub vpr: Sym,
    pub vpd: Sym,
    pub vpf: Sym,
    pub pi: Sym,
    pub condition: Sym,
    pub message: Sym,
    pub empty: Sym,
    pub concat: Sym,
}

pub(crate) struct Evaluator<'a> {
    pub units: Vec<Unit<'a>>,
    pub syms: Syms,
    pub k: Known,
    /// Live contexts, newest last: where `$` variables are looked up.
    pub stack: Vec<Rc<Ctx>>,
    /// Names of the user modules being instantiated (`parent_module`).
    pub module_names: Vec<Sym>,
    pub out: &'a mut dyn Output,
    pub opts: Options,
    pub rng: Mt19937,
    stack_base: usize,
    /// [`Options::stack_limit`], capped on wasm32 by the stack actually left
    /// (see [`crate::recursion`]).
    stack_limit: usize,
    /// Frames in use for [`Options::frame_limit`]: nested function calls
    /// and statement instantiations.
    pub frames: u32,
    pub main_dir: PathBuf,
    node_index: usize,
    pub builtin_ctx: Rc<Ctx>,
    /// Contexts captured by function literals: cleared at the end to break
    /// reference cycles (a literal stored in the scope it captured).
    captured: Vec<Weak<Ctx>>,
    captured_limit: usize,
    /// Index of each unit's used-library keys.
    pub builtin_fns: HashMap<Sym, crate::builtins::functions::Builtin, FxBuild>,
    pub builtin_mods: HashMap<Sym, crate::builtins::modules::BuiltinModule, FxBuild>,
    /// Scratch for location-less warnings from `ops::mul`.
    pub op_warnings: Vec<String>,
    /// Deprecation messages already printed, with their location: OpenSCAD
    /// prints each one once (`printedDeprecations`).
    deprecations: std::collections::HashSet<DeprecationKey>,
    /// `--hardwarnings` progress; see [`Hard`].
    hard: std::cell::Cell<Hard>,
    /// The dotted names of the `part()`s being instantiated, innermost
    /// last. Instantiation nests exactly as the node tree does, so the top
    /// is the enclosing part of any node made now.
    pub part_stack: Vec<String>,
    /// Every part name used so far, for the duplicate warning.
    pub part_names: std::collections::HashSet<String>,
    /// [`Options::guard`]'s limits as plain numbers for hot checks.
    pub caps: Caps,
    /// Checks since the clock and memory were last looked at.
    limit_ticks: u32,
    /// A limit passed and printed, waiting to be raised at the next check
    /// (as [`Hard`] does for a warning): builtins that find it cannot fail.
    limit: std::cell::Cell<Hard>,
    /// Whether `hard` or `limit` is [`Hard::Pending`]: the one flag that
    /// [`Evaluator::check_hard`] tests after every expression. Testing the
    /// two states there instead (one more load and branch per expression)
    /// cost measurably on evaluation-bound models.
    pending: std::cell::Cell<bool>,
    /// Accumulators moved out of a dying frame for a tail call's arguments
    /// (see `Evaluator::move_accumulators`), innermost last.
    pub moved: Vec<Moved>,
    /// Every unit's regions (see [`crate::resolve`]), indexed by
    /// [`Ctx::region`].
    pub regions: Vec<Region>,
    /// Names passed as named arguments (see `resolve::Cand::Extra`).
    extras: std::cell::OnceCell<crate::resolve::NameSet>,
    /// Lookups that fell back to the by-name walk.
    pub stats: crate::resolve::Stats,
    /// An empty context for `eval_call`'s stack slot (see there).
    pub placeholder: Rc<Ctx>,
    /// Emptied argument vectors for user and builtin calls to reuse.
    pub arg_pool: Vec<Vec<crate::call::ArgVal>>,
    /// Dead contexts for new ones to reuse (see [`Ctx::recycle`]).
    pub ctx_pool: Vec<Rc<Ctx>>,
    /// The registers: the variables of every live instance of a register
    /// region ([`Region::reg`]), innermost last. An instance takes
    /// `regions[r].len()` consecutive registers from the top when it opens
    /// and gives them back when it closes, so a `let`, a comprehension
    /// variable or a pure call frame costs no context allocation, no
    /// reference counting and no link in the chain walks.
    pub regs: Vec<Option<Value>>,
    /// Per region: the first register of its live instance, or
    /// [`NO_BASE`]. Only the innermost instance of a region is ever read:
    /// a reference inside a register region is evaluated only while the
    /// instance around it is the newest one, since nothing that could run
    /// it later or from inside a newer instance (a function literal made
    /// in it) is allowed in one (`resolve::Resolver::materialize`).
    pub reg_base: Vec<u32>,
    /// Bases replaced by instances that the tail-call loop owns (the tail
    /// `let`s and pure frames of the step being evaluated), to restore
    /// when the step is replaced or the loop ends: (region, base).
    pub reg_saves: Vec<(u32, u32)>,
    /// Reuse of top-level statements from an earlier evaluation (see
    /// [`crate::memo`]), when the host keeps a memo.
    pub(crate) memo: Option<crate::memo::MemoRun<'a>>,
    /// The top-level statement being recorded for the memo, if one is.
    pub(crate) rec: Option<Box<crate::memo::Recording>>,
}

/// A variable's value moved out of its frame, to be handed to the one read
/// of it in a tail call's arguments.
pub(crate) struct Moved {
    /// The binding it was moved out of: only a read that resolves to this
    /// binding takes it.
    pub owner: Owner,
    pub sym: Sym,
    pub value: Option<Value>,
}

/// Where a variable is bound, for [`Moved`]: a context, or a register.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Owner {
    Ctx(*const Ctx),
    Reg(usize),
}

/// [`Evaluator::reg_base`] of a region with no live register instance: not
/// open, or a function body bound as a context for this call, whose
/// [`Cand::Reg`] candidates are then found by the chain walk.
pub(crate) const NO_BASE: u32 = u32::MAX;

/// Resource limits as numbers for the checks on hot paths; `usize::MAX`
/// (and so on) when unlimited. See [`crate::limits`].
#[derive(Debug, Clone, Copy)]
pub(crate) struct Caps {
    pub list: usize,
    pub string: usize,
    pub rands: f64,
}

impl Caps {
    fn of(l: Option<&crate::limits::Limits>) -> Caps {
        let n = |x: Option<u64>| x.map_or(usize::MAX, |x| usize::try_from(x).unwrap_or(usize::MAX));
        Caps {
            list: n(l.and_then(|l| l.list)),
            string: n(l.and_then(|l| l.string)),
            rands: l.and_then(|l| l.rands).map_or(f64::INFINITY, |x| x as f64),
        }
    }
}

/// Estimated bytes of one node of the tree, for the memory limit: the
/// node, its origin (name and location), its parameters and its slot in
/// the parent's children, with allocator overhead. Measured: a million
/// `cube(1)` nodes in two nested loops peak at 337 MB, three million in
/// three at 1.49 GB (about 500 bytes each).
const NODE_BYTES: u64 = 512;

/// How many evaluator checks pass between looks at the clock and the
/// memory estimate: a few milliseconds of evaluation at most.
const LIMIT_TICKS: u32 = 4096;

/// Estimated bytes of one printed message besides three copies of its
/// text, for the memory limit: a host keeps each as a diagnostic, an
/// output record and JSON. Measured through `neoscad mcp`: 200,000 echoes
/// hold 69 MB, and 200,000 warnings (which carry a location and a file)
/// about 300 MB besides their nodes.
fn message_bytes(located: bool) -> u64 {
    if located { 1536 } else { 256 }
}

/// Where a `--hardwarnings` run stands. OpenSCAD throws a
/// `HardWarningException` from `PRINT` itself, right after printing the
/// first warning (`printutils.cc:125-130`); the exception is an
/// `EvaluationException`, so every call site it passes adds its `TRACE:`
/// line, and the command line maps it to exit 1 (`openscad.cc:1186`).
/// Here a warning only arms the abort ([`Hard::Pending`]) because many
/// warnings are printed from functions that cannot fail; the next check
/// (after any expression, call step, assignment or instantiation) turns
/// it into an [`UnwindKind::HardWarning`] error, which travels the same
/// call sites as OpenSCAD's exception and so collects the same traces.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Hard {
    Off,
    Armed,
    Pending,
    Thrown,
}

/// A printed deprecation: its text and location.
type DeprecationKey = (Vec<u8>, Option<(u32, Span)>);

pub(crate) enum Step {
    Done(Value),
    Next {
        unit: u32,
        expr: Option<ExprId>,
        ctx: Option<Rc<Ctx>>,
        call: Option<(u32, ExprId)>,
    },
    /// A call into a pure frame (see `Evaluator::pure_frame`): its body is
    /// evaluated in the callee's defining context `ctx`, with the frame's
    /// variables in the registers of `region` from `base`.
    Pure {
        unit: u32,
        expr: ExprId,
        ctx: Rc<Ctx>,
        call: (u32, ExprId),
        region: u32,
        base: u32,
    },
}

/// What the context a tail-call step is evaluated in is (see
/// `Evaluator::eval_call`).
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Mode {
    /// The loop's first step: the caller's context, borrowed.
    Entry,
    /// The loop's own context: a call frame or `let` it made.
    Ctx,
    /// A pure frame's defining context: the frame is in registers.
    Pure,
}

impl<'a> Evaluator<'a> {
    pub fn new(
        main: &'a Program,
        main_uses: &[String],
        libraries: &[Library<'a>],
        main_dir: PathBuf,
        opts: Options,
        out: &'a mut dyn Output,
    ) -> Self {
        let mut syms = Syms::default();
        let k = Known {
            children: syms.intern("$children"),
            parent_modules: syms.intern("$parent_modules"),
            fn_: syms.intern("$fn"),
            fa: syms.intern("$fa"),
            fs: syms.intern("$fs"),
            preview: syms.intern("$preview"),
            t: syms.intern("$t"),
            vpt: syms.intern("$vpt"),
            vpr: syms.intern("$vpr"),
            vpd: syms.intern("$vpd"),
            vpf: syms.intern("$vpf"),
            pi: syms.intern("PI"),
            condition: syms.intern("condition"),
            message: syms.intern("message"),
            empty: syms.intern(""),
            concat: syms.intern("concat"),
        };
        let builtin_fns = crate::builtins::functions::table(&mut syms);
        let builtin_mods = crate::builtins::modules::table(&mut syms, opts.parts);
        let mut units = vec![Unit::new(main, &mut syms)];
        let mut keys: HashMap<&str, u32> = HashMap::new();
        for lib in libraries {
            if let Some(p) = lib.program
                && !p.has_syntax_errors()
            {
                keys.insert(lib.path, units.len() as u32);
                units.push(Unit::new(p, &mut syms));
            }
        }
        let resolve = |uses: &[String]| {
            uses.iter()
                .filter_map(|k| keys.get(k.as_str()).copied())
                .collect::<Vec<_>>()
        };
        units[0].uses = resolve(main_uses);
        let mut i = 1;
        for lib in libraries {
            if keys.contains_key(lib.path) && lib.program.is_some_and(|p| !p.has_syntax_errors()) {
                units[i].uses = resolve(lib.uses);
                i += 1;
            }
        }
        // Names are resolved as definitions are first used (see
        // `crate::resolve`); these are the empty tables.
        for unit in &mut units {
            unit.res = crate::resolve::UnitRes::new(unit);
        }
        let k_pi = k.pi;
        let seed = opts.rng_seed;
        let caps = Caps::of(opts.guard.as_deref().map(crate::limits::Guard::limits));
        // Values of an earlier evaluation on this thread are gone or are
        // not this request's to count.
        crate::limits::live::arm(opts.guard.as_ref());
        let marker = 0u8;
        let stack_base = std::ptr::addr_of!(marker) as usize;
        let stack_limit = crate::recursion::stack_limit(opts.stack_limit, stack_base);
        Evaluator {
            units,
            syms,
            k,
            stack: Vec::with_capacity(256),
            module_names: Vec::new(),
            out,
            rng: Mt19937::new(seed),
            stack_base,
            stack_limit,
            frames: 0,
            main_dir,
            node_index: 1,
            builtin_ctx: Ctx::new(None, CtxKind::Builtin, BUILTIN_REGION, 1),
            captured: Vec::new(),
            captured_limit: 1024,
            builtin_fns,
            builtin_mods,
            op_warnings: Vec::new(),
            deprecations: Default::default(),
            hard: std::cell::Cell::new(if opts.hardwarnings {
                Hard::Armed
            } else {
                Hard::Off
            }),
            part_stack: Vec::new(),
            part_names: Default::default(),
            caps,
            limit_ticks: 0,
            limit: std::cell::Cell::new(Hard::Off),
            pending: std::cell::Cell::new(false),
            moved: Vec::new(),
            regions: vec![Region::default(), crate::resolve::builtin_region(k_pi)],
            extras: std::cell::OnceCell::new(),
            stats: crate::resolve::Stats::default(),
            arg_pool: Vec::new(),
            ctx_pool: Vec::new(),
            regs: Vec::new(),
            reg_base: vec![NO_BASE; 2],
            reg_saves: Vec::new(),
            placeholder: Ctx::new(None, CtxKind::Plain, crate::resolve::NONE_REGION, 0),
            memo: None,
            rec: None,
            opts,
        }
    }

    // --- the stack -------------------------------------------------------

    /// Bytes of native stack used since evaluation started.
    #[inline]
    pub fn stack_used(&self) -> usize {
        let marker = 0u8;
        let here = std::ptr::addr_of!(marker) as usize;
        std::hint::black_box(&marker);
        self.stack_base.abs_diff(here)
    }

    /// `StackCheck::check`, made of the two limits in [`crate::recursion`]:
    /// the stack measured, and the frame budget.
    #[inline]
    pub fn recursion_exhausted(&self) -> bool {
        self.stack_used() >= self.stack_limit || self.frames >= self.opts.frame_limit
    }

    /// The measured stack limit alone (for printing, which has its own
    /// depth count).
    #[inline]
    pub fn stack_limit(&self) -> usize {
        self.stack_limit
    }

    #[inline]
    pub fn interrupted(&self) -> bool {
        self.opts
            .interrupt
            .as_ref()
            .is_some_and(|f| f.load(Ordering::Relaxed))
    }

    pub fn check_interrupt(&mut self) -> R<()> {
        if self.interrupted() {
            return self.stop();
        }
        if self.opts.guard.is_some() {
            self.limit_ticks = self.limit_ticks.wrapping_add(1);
            if self.rec.is_some() {
                self.track_peak(0);
            }
            if self.limit_ticks.is_multiple_of(LIMIT_TICKS) {
                self.check_limits(None)?;
            }
        }
        self.check_hard()
    }

    /// The time and memory limits, now: a passed one is printed (at `loc`,
    /// or with no location; the unwinding error collects the call sites)
    /// and raised.
    pub fn check_limits(&mut self, loc: Option<Loc>) -> R<()> {
        let Some(g) = self.opts.guard.clone() else {
            return Ok(());
        };
        crate::limits::live::beside(self.node_bytes());
        let e = if g.over_time() {
            Some(g.time_exceeded())
        } else {
            g.memory_exceeds(self.live_bytes(), "the evaluation")
        };
        if let Some(e) = e {
            self.limit_exceeded(loc, e);
            return self.check_hard();
        }
        Ok(())
    }

    /// The interrupt flag is up: a cancellation, or the memory limit,
    /// which `crate::limits::live` trips from inside the allocation that
    /// passed it (an interrupt costs the hot path nothing to notice, where
    /// asking the count at every call would not). The limit is reported
    /// like any other, with the call sites that led to it.
    #[cold]
    #[inline(never)]
    fn stop(&mut self) -> R<()> {
        if self.limit.get() == Hard::Off && crate::limits::live::over() {
            self.memory_passed(None);
            if self.limit.get() != Hard::Off {
                return self.check_hard();
            }
        }
        Err(Unwind::new(UnwindKind::Interrupted, 0))
    }

    /// Report the memory limit if a value just made passed it (see
    /// [`Evaluator::stop`]); it is raised at the next check. For the end
    /// of an element-wise operator, one uninterruptible piece of work that
    /// may have stopped early at the limit.
    #[inline]
    fn check_memory(&mut self, loc: Option<Loc>) {
        if crate::limits::live::over() && self.limit.get() == Hard::Off {
            self.memory_passed(loc);
        }
    }

    #[cold]
    #[inline(never)]
    fn memory_passed(&mut self, loc: Option<Loc>) {
        let Some(g) = self.opts.guard.clone() else {
            return;
        };
        // What the guard recorded when the limit tripped, or the estimate
        // now (printing a value passes the limit without making one).
        let e = g
            .exceeded()
            .filter(|e| e.limit == crate::limits::Limit::Memory)
            .or_else(|| g.memory_exceeds(self.live_bytes(), "the evaluation"));
        if let Some(e) = e {
            self.limit_exceeded(loc, e);
        }
    }

    /// A limit passed: print it at `loc` with its hint, record it on the
    /// guard, and raise it at the next check (a builtin that finds it
    /// returns `undef` meanwhile). Only the first limit is reported.
    pub fn limit_exceeded(&mut self, loc: Option<Loc>, e: crate::limits::Exceeded) {
        if self.limit.get() != Hard::Off {
            return;
        }
        if let Some(g) = &self.opts.guard {
            g.record(e.clone());
        }
        let text = e.message();
        self.emit_hinted(
            Severity::Error,
            DiagCode::ResourceLimit,
            text.as_bytes(),
            loc,
            Some(e.hint()),
        );
        self.limit.set(Hard::Pending);
        self.pending.set(true);
    }

    /// Whether a list of `n` elements fits the list limit; otherwise the
    /// limit is reported at `loc` as made by `what`.
    pub fn list_fits(&mut self, n: usize, loc: Loc, what: &str) -> bool {
        if n <= self.caps.list {
            return true;
        }
        self.over_limit(crate::limits::Limit::List, n as f64, loc, what);
        false
    }

    /// Whether a string of `n` bytes fits the string limit.
    pub fn string_fits(&mut self, n: usize, loc: Loc, what: &str) -> bool {
        if n <= self.caps.string {
            return true;
        }
        self.over_limit(crate::limits::Limit::String, n as f64, loc, what);
        false
    }

    /// Report limit `l` passed by `asked` at `loc`.
    pub fn over_limit(&mut self, l: crate::limits::Limit, asked: f64, loc: Loc, what: &str) {
        if let Some(g) = self.opts.guard.clone()
            && let Some(e) = g.exceeds(l, asked, what)
        {
            self.limit_exceeded(Some(loc), e);
        }
    }

    /// Whether `bytes` more of live values fit the memory limit.
    pub fn memory_fits(&mut self, bytes: u64, loc: Loc, what: &str) -> bool {
        let Some(g) = self.opts.guard.clone() else {
            return true;
        };
        self.track_peak(bytes);
        match g.memory_exceeds(self.live_bytes().saturating_add(bytes), what) {
            None => true,
            Some(e) => {
                self.limit_exceeded(Some(loc), e);
                false
            }
        }
    }

    /// Raise the `--hardwarnings` abort if a warning has armed it (see
    /// [`Hard`]). Only the first warning raises it: once thrown, warnings
    /// printed while the error travels up (none, as nothing is evaluated
    /// then) cannot start a second one.
    ///
    /// This runs after every expression, so the common case is one load
    /// and one branch, and the rest lives out of line.
    #[inline(always)]
    pub fn check_hard(&self) -> R<()> {
        if self.pending.get() {
            return Err(self.raise_pending());
        }
        Ok(())
    }

    /// [`Evaluator::check_hard`]'s slow path: a passed limit first, then
    /// an armed `--hardwarnings` abort (which stays pending for the next
    /// check, as it did when both were tested in turn).
    #[cold]
    #[inline(never)]
    fn raise_pending(&self) -> Box<Unwind> {
        let kind = if self.limit.get() == Hard::Pending {
            self.limit.set(Hard::Thrown);
            UnwindKind::Limit
        } else {
            debug_assert_eq!(self.hard.get(), Hard::Pending);
            self.hard.set(Hard::Thrown);
            UnwindKind::HardWarning
        };
        self.pending
            .set(self.limit.get() == Hard::Pending || self.hard.get() == Hard::Pending);
        self.unwind(kind)
    }

    pub fn push(&mut self, c: Rc<Ctx>) -> usize {
        self.stack.push(c);
        self.stack.len() - 1
    }

    pub fn truncate(&mut self, len: usize) {
        self.stack.truncate(len);
    }

    /// `EvaluationSession::try_lookup_special_variable`.
    pub fn lookup_special(&self, s: Sym) -> Option<Value> {
        for c in self.stack.iter().rev() {
            let vars = c.vars.borrow();
            if vars.has_config
                && let Some(v) = vars.get(s)
            {
                return Some(v.clone());
            }
        }
        None
    }

    /// `Context::try_lookup_variable`, by name.
    pub fn try_lookup(&self, ctx: &Ctx, s: Sym) -> Option<Value> {
        if self.syms.is_config(s) {
            self.lookup_special(s)
        } else {
            ctx.lookup_lexical(s, &self.regions)
        }
    }

    /// A variable reference's value: resolved, or by name when the
    /// resolver left it (`$` names).
    pub fn read_var(&self, u: u32, id: ExprId, s: Sym, ctx: &Ctx) -> Option<Value> {
        match self.units[u as usize].res.var(id).cands() {
            Some(r) => self.find_var(u, r, s, ctx),
            None => self.try_lookup(ctx, s),
        }
    }

    /// A resolved variable lookup: walk the chain from `ctx`, looking only
    /// in contexts of the candidates' regions (see [`crate::resolve`]).
    #[inline]
    pub fn find_var(&self, u: u32, r: Ref, s: Sym, ctx: &Ctx) -> Option<Value> {
        let found = self.find_binding(u, r, s, ctx);
        // Debug builds check every resolved lookup against the by-name
        // walk it replaces: they must stop at the same context. A value
        // found in a register has no context to compare (the walk cannot
        // see registers, and no context between could bind the name:
        // registers are the innermost bindings).
        #[cfg(debug_assertions)]
        if !matches!(found, Some((None, _))) {
            assert_eq!(
                found.as_ref().and_then(|(c, _)| c.map(std::ptr::from_ref)),
                ctx.binder(s, &self.regions),
                "resolved lookup of {} disagrees with the scope chain",
                self.name(s)
            );
        }
        found.map(|(_, v)| v)
    }

    /// The register a resolved reference's live binding is in: the first
    /// set one of its register candidates (`None`: none is set, so the
    /// binding, if any, is in a context).
    #[inline]
    pub(crate) fn reg_binding(&self, cands: &[Cand]) -> Option<usize> {
        for cand in cands {
            let Cand::Reg { region, slot } = *cand else {
                break;
            };
            let base = self.reg_base[region as usize];
            if base != NO_BASE {
                let i = base as usize + slot as usize;
                if self.regs[i].is_some() {
                    return Some(i);
                }
            }
        }
        None
    }

    /// A resolved lookup: the value, and the context it was found in
    /// (`None`: a register).
    #[inline]
    fn find_binding<'c>(
        &self,
        u: u32,
        r: Ref,
        s: Sym,
        ctx: &'c Ctx,
    ) -> Option<(Option<&'c Ctx>, Value)> {
        let cands = self.units[u as usize].res.cands(r);
        // Most names have one binding that can see them (a parameter, a
        // `let`, a global): a tighter loop for those.
        match *cands {
            [Cand::Slot { region, slot }] => {
                let mut c = ctx;
                loop {
                    if c.region == region
                        && let Some(v) = c.slot(slot)
                    {
                        return Some((Some(c), v));
                    }
                    c = c.parent.as_deref()?;
                }
            }
            [Cand::Reg { region, slot }] => {
                let base = self.reg_base[region as usize];
                if base != NO_BASE {
                    return self.regs[base as usize + slot as usize]
                        .clone()
                        .map(|v| (None, v));
                }
            }
            [] => return None,
            _ => {}
        }
        self.find_binding_slow(cands, s, ctx)
    }

    /// [`Self::find_binding`] for a name with several candidates. Out of
    /// line, so that its locals are not part of the frame of `eval_expr`,
    /// which every level of a recursion holds.
    #[inline(never)]
    fn find_binding_slow<'c>(
        &self,
        cands: &[Cand],
        s: Sym,
        ctx: &'c Ctx,
    ) -> Option<(Option<&'c Ctx>, Value)> {
        // Registers first: they are the innermost candidates. A chain walk
        // is needed after them only for a region bound as a context (a
        // function body with a non-pure frame) or for candidates beyond.
        let mut walk = false;
        for cand in cands {
            let Cand::Reg { region, slot } = *cand else {
                walk = true;
                break;
            };
            let base = self.reg_base[region as usize];
            if base == NO_BASE {
                walk = true;
            } else if let Some(v) = &self.regs[base as usize + slot as usize] {
                return Some((None, v.clone()));
            }
        }
        if !walk {
            return None;
        }
        let mut c = ctx;
        loop {
            let region = c.region;
            for cand in cands {
                if cand.region() != region {
                    continue;
                }
                match *cand {
                    Cand::Slot { slot, .. } | Cand::Reg { slot, .. } => {
                        if let Some(v) = c.slot(slot) {
                            return Some((Some(c), v));
                        }
                    }
                    Cand::Extra { .. } => {
                        if let Some(v) = c.vars.borrow().get(s) {
                            return Some((Some(c), v.clone()));
                        }
                    }
                    Cand::Def { .. } | Cand::Use { .. } => {}
                }
            }
            c = c.parent.as_deref()?;
        }
    }

    /// Resolve unit `u`'s top level, if not yet (see [`crate::resolve`]).
    pub fn resolve_root(&mut self, u: u32) {
        if self.units[u as usize].res.root {
            return;
        }
        let mut res = std::mem::take(&mut self.units[u as usize].res);
        let mut regions = std::mem::take(&mut self.regions);
        let t = self.tables();
        crate::resolve::resolve_root(&t, u, &mut regions, &mut res);
        self.regions = regions;
        self.units[u as usize].res = res;
        self.reg_base.resize(self.regions.len(), NO_BASE);
    }

    /// The body region of function `index` of scope `scope`, resolving the
    /// function at its first call.
    #[inline]
    pub fn function_region(&mut self, u: u32, scope: u32, index: u32) -> u32 {
        match self.units[u as usize].res.fn_region[scope as usize][index as usize] {
            0 => self.resolve_function(u, scope, index),
            r => r,
        }
    }

    #[cold]
    #[inline(never)]
    fn resolve_function(&mut self, u: u32, scope: u32, index: u32) -> u32 {
        let mut res = std::mem::take(&mut self.units[u as usize].res);
        let mut regions = std::mem::take(&mut self.regions);
        let t = self.tables();
        let r = crate::resolve::resolve_function(&t, u, scope, index, &mut regions, &mut res);
        self.regions = regions;
        self.units[u as usize].res = res;
        self.reg_base.resize(self.regions.len(), NO_BASE);
        r
    }

    /// The body region of module `index` of scope `scope`, resolving the
    /// module at its first instantiation.
    #[inline]
    pub fn module_region(&mut self, u: u32, scope: u32, index: u32) -> u32 {
        let body = self.units[u as usize].scopes[scope as usize].bodies[index as usize];
        match self.units[u as usize].res.scope_region[body as usize] {
            0 => self.resolve_module(u, scope, index),
            r => r,
        }
    }

    #[cold]
    #[inline(never)]
    fn resolve_module(&mut self, u: u32, scope: u32, index: u32) -> u32 {
        let mut res = std::mem::take(&mut self.units[u as usize].res);
        let mut regions = std::mem::take(&mut self.regions);
        let t = self.tables();
        let r = crate::resolve::resolve_module(&t, u, scope, index, &mut regions, &mut res);
        self.regions = regions;
        self.units[u as usize].res = res;
        self.reg_base.resize(self.regions.len(), NO_BASE);
        r
    }

    /// What the resolver reads (the unit being resolved and the regions are
    /// taken out of `self` meanwhile).
    fn tables(&self) -> crate::resolve::Tables<'_, 'a> {
        crate::resolve::Tables {
            units: &self.units,
            syms: &self.syms,
            builtin_fns: &self.builtin_fns,
            builtin_mods: &self.builtin_mods,
            extras: &self.extras,
            empty: self.k.empty,
            children: self.k.children,
        }
    }

    /// A new context of `region`, reusing a recycled one when there is one
    /// (see [`Ctx::recycle`]).
    #[inline]
    pub fn new_ctx(&mut self, parent: &Rc<Ctx>, kind: CtxKind, region: u32) -> Rc<Ctx> {
        let n = self.regions[region as usize].len();
        Ctx::reuse(&mut self.ctx_pool, Some(parent.clone()), kind, region, n)
    }

    /// [`Self::new_ctx`] taking over a reference the caller owns (a
    /// callee's defining context), which saves a clone and a drop per call.
    #[inline]
    pub fn new_ctx_in(&mut self, parent: Rc<Ctx>, kind: CtxKind, region: u32) -> Rc<Ctx> {
        let n = self.regions[region as usize].len();
        Ctx::reuse(&mut self.ctx_pool, Some(parent), kind, region, n)
    }

    /// Open an instance of register region `region` (see [`Self::regs`]):
    /// its registers, unset, on top. Returns the base it replaces, for
    /// [`Self::reg_close`].
    #[inline]
    pub fn reg_open(&mut self, region: u32) -> u32 {
        let base = self.regs.len();
        let n = self.regions[region as usize].len();
        self.regs.resize(base + n, None);
        std::mem::replace(&mut self.reg_base[region as usize], base as u32)
    }

    /// Close the instance [`Self::reg_open`] opened, dropping its values
    /// where dropping its context would have.
    #[inline]
    pub fn reg_close(&mut self, region: u32, old: u32) {
        let base = self.reg_base[region as usize] as usize;
        self.regs.truncate(base);
        self.reg_base[region as usize] = old;
    }

    /// Restore the bases saved in [`Self::reg_saves`] above `mark`, newest
    /// first, and drop the registers above `regs`: the end of a tail-call
    /// step's register instances.
    #[inline]
    pub fn reg_unwind(&mut self, mark: usize, regs: usize) {
        if self.reg_saves.len() > mark {
            self.reg_restore(mark);
        }
        self.regs.truncate(regs);
    }

    #[inline(never)]
    pub fn reg_restore(&mut self, mark: usize) {
        while self.reg_saves.len() > mark {
            let (region, old) = self.reg_saves.pop().expect("above the mark");
            self.reg_base[region as usize] = old;
        }
    }

    /// Set a variable by name: in its slot when the context's region has
    /// one, else in the name map.
    pub fn set_var(&mut self, ctx: &Ctx, s: Sym, v: Value) {
        let config = self.syms.is_config(s);
        if !config && let Some(i) = self.regions[ctx.region as usize].slot_of(s) {
            ctx.set_slot(i, v);
            return;
        }
        ctx.vars.borrow_mut().set(s, v, config);
    }

    /// Set the `k`th binder of `ctx`'s region (see `Region::binds`). A
    /// context of an unresolved construct (none are known; its references
    /// are looked up by name too) keeps everything in its name map.
    #[inline]
    pub fn set_bound(&mut self, ctx: &Ctx, k: usize, s: Sym, v: Value) {
        let binds = &self.regions[ctx.region as usize].binds;
        match binds.get(k).copied().unwrap_or(NO_SLOT) {
            NO_SLOT => {
                let config = self.syms.is_config(s);
                ctx.vars.borrow_mut().set(s, v, config);
            }
            i => ctx.set_slot(i, v),
        }
    }

    /// The memory estimate: live lists, strings and messages
    /// ([`crate::limits::live`]), and every node made so far. Nodes live
    /// until evaluation ends (a nest of loops can make a billion of them),
    /// so they are counted from the node counter rather than charged one by
    /// one on a hot path.
    fn live_bytes(&self) -> u64 {
        crate::limits::live::get().saturating_add(self.node_bytes())
    }

    /// The nodes' share of [`Evaluator::live_bytes`].
    fn node_bytes(&self) -> u64 {
        let nodes = (self.node_index as u64).saturating_sub(1);
        nodes.saturating_mul(NODE_BYTES)
    }

    pub fn next_node_index(&mut self) -> usize {
        let i = self.node_index;
        self.node_index += 1;
        i
    }

    // --- the memo's view (see `crate::memo`) ------------------------------

    /// The next node index.
    pub(crate) fn node_counter(&self) -> usize {
        self.node_index
    }

    /// The limits' check counter.
    pub(crate) fn ticks(&self) -> u32 {
        self.limit_ticks
    }

    pub(crate) fn live_bytes_now(&self) -> u64 {
        self.live_bytes()
    }

    /// Whether a resource limit has been passed.
    pub(crate) fn limit_passed(&self) -> bool {
        self.limit.get() != Hard::Off
    }

    /// Account for a replayed statement: the node indices and limit checks
    /// it consumed when it ran, so what follows is numbered and sampled as
    /// in a full evaluation.
    pub(crate) fn advance(&mut self, indices: usize, ticks: u32) {
        self.node_index += indices;
        self.limit_ticks = self.limit_ticks.wrapping_add(ticks);
    }

    /// The most memory seen while recording, with `bytes` about to be
    /// allocated: a replay must not skip over a memory limit.
    fn track_peak(&mut self, bytes: u64) {
        let live = self.live_bytes().saturating_add(bytes);
        if let Some(r) = &mut self.rec {
            r.peak = r.peak.max(live);
        }
    }

    /// Something happened that a statement's fingerprint does not cover
    /// (see `crate::memo`): the statement being recorded is not kept.
    pub(crate) fn untracked(&mut self) {
        if let Some(r) = &mut self.rec {
            r.untrack();
        }
    }

    /// Print a replayed message as [`Evaluator::emit_hinted`] printed it.
    pub(crate) fn replay_message(&mut self, m: &Message<'_>) {
        crate::limits::live::charge(message_bytes(m.diag.span.is_some()) + 3 * m.text.len() as u64);
        self.out.message(m);
    }

    // --- messages --------------------------------------------------------

    pub fn expr_loc(&self, unit: u32, e: ExprId) -> Loc {
        Loc {
            unit,
            span: self.units[unit as usize].ast.expr(e).span,
        }
    }

    pub fn emit(&mut self, severity: Severity, code: DiagCode, text: &[u8], loc: Option<Loc>) {
        self.emit_hinted(severity, code, text, loc, None);
    }

    /// [`Evaluator::emit`] with a fix hint for the tools' JSON.
    pub fn emit_hinted(
        &mut self,
        severity: Severity,
        code: DiagCode,
        text: &[u8],
        loc: Option<Loc>,
        hint: Option<String>,
    ) {
        // OpenSCAD would already be unwinding from the first warning, so
        // nothing printed between it and the check that raises it exists
        // there (a builtin warning about several arguments, an unknown
        // function after a disabled experimental one).
        if self.hard.get() == Hard::Pending {
            return;
        }
        if severity == Severity::Deprecated {
            // Whether it prints depends on the statements before.
            self.untracked();
        }
        if severity == Severity::Deprecated
            && !self
                .deprecations
                .insert((text.to_vec(), loc.map(|l| (l.unit, l.span))))
        {
            return;
        }
        // Text printed after the memory limit passed may have been cut
        // short by it (the value printer stops there): the limit is
        // printed instead.
        if code != DiagCode::ResourceLimit
            && self.limit.get() == Hard::Off
            && crate::limits::live::over()
        {
            self.memory_passed(loc);
            if self.limit.get() != Hard::Off {
                return;
            }
        }
        // A host keeps what was printed (as bytes, a record and JSON), so
        // an echo in a long loop is memory like any value, and so is a
        // million short warnings: each is a diagnostic and a record with
        // their own allocations besides the text.
        crate::limits::live::charge(message_bytes(loc.is_some()) + 3 * text.len() as u64);
        let mut diag = Diagnostic::new(code, severity, String::from_utf8_lossy(text).into_owned())
            .with_base(PathBase::MainFileDir);
        if let Some(h) = hint {
            diag = diag.with_hint(h);
        }
        let mut sources = None;
        if let Some(l) = loc {
            let src = &self.units[l.unit as usize].program.sources;
            let line = src.get(l.span.file).line_of(l.span.start);
            diag = diag.at(l.span, line);
            sources = Some(src);
        }
        if let Some(r) = &mut self.rec
            && !r.untracked
        {
            r.record(crate::memo::Recorded {
                unit: loc.map(|l| l.unit),
                diag: diag.clone(),
                text: text.to_vec(),
            });
        }
        self.out.message(&Message {
            diag,
            text,
            sources,
        });
        if severity == Severity::Warning && self.hard.get() == Hard::Armed {
            self.hard.set(Hard::Pending);
            self.pending.set(true);
        }
    }

    pub fn warn(&mut self, loc: Loc, code: DiagCode, text: impl AsRef<[u8]>) {
        self.emit(Severity::Warning, code, text.as_ref(), Some(loc));
    }

    pub fn warn_noloc(&mut self, code: DiagCode, text: impl AsRef<[u8]>) {
        self.emit(Severity::Warning, code, text.as_ref(), None);
    }

    pub fn error(&mut self, loc: Option<Loc>, code: DiagCode, text: impl AsRef<[u8]>) {
        self.emit(Severity::Error, code, text.as_ref(), loc);
    }

    pub fn emit_pending(&mut self, p: Pending) {
        self.emit(p.severity, p.code, &p.text, p.loc);
    }

    /// Add a trace line to an error on its way up (`e.LOG(Trace, ...)`
    /// followed by `e.traceDepth--`).
    pub fn trace(&mut self, e: &mut Unwind, loc: Loc, text: Vec<u8>) {
        if let Some(p) = e.log(Pending {
            severity: Severity::Trace,
            code: DiagCode::Trace,
            text,
            loc: Some(loc),
        }) {
            self.emit_pending(p);
        }
        e.depth -= 1;
    }

    pub fn unwind(&self, kind: UnwindKind) -> Box<Unwind> {
        Unwind::new(kind, self.opts.trace_depth)
    }

    pub fn quote_sym(&self, s: Sym) -> String {
        format!("\"{}\"", self.syms.name(s))
    }

    pub fn name(&self, s: Sym) -> &str {
        self.syms.name(s)
    }

    // --- running -----------------------------------------------------------

    pub fn run(&mut self) -> Evaluation {
        // BuiltinContext::init, then RenderVariables::applyToContext.
        let b = self.builtin_ctx.clone();
        self.push(b.clone());
        let k = &self.k;
        let (fn_, fs, fa, t, preview, vpt, vpr, vpd, vpf, pi) = (
            k.fn_, k.fs, k.fa, k.t, k.preview, k.vpt, k.vpr, k.vpd, k.vpf, k.pi,
        );
        let zero = Value::vector(vec![Value::Number(0.0); 3]);
        for (s, v) in [
            (fn_, Value::Number(0.0)),
            (fs, Value::Number(2.0)),
            (fa, Value::Number(12.0)),
            (t, Value::Number(0.0)),
            (preview, Value::Undef),
            (vpt, zero.clone()),
            (vpr, zero),
            (vpd, Value::Number(500.0)),
            (vpf, Value::Number(22.5)),
            (pi, Value::Number(std::f64::consts::PI)),
        ] {
            self.set_var(&b, s, v);
        }
        let cam = self.opts.camera;
        let vec3 = |v: [f64; 3]| Value::vector(v.iter().map(|&x| Value::Number(x)).collect());
        self.set_var(&b, preview, Value::Bool(self.opts.preview));
        self.set_var(&b, t, Value::Number(self.opts.time));
        self.set_var(&b, vpr, vec3(cam.vpr));
        self.set_var(&b, vpt, vec3(cam.vpt));
        self.set_var(&b, vpd, Value::Number(cam.vpd));
        self.set_var(&b, vpf, Value::Number(cam.vpf));

        let mut root = Node {
            kind: NodeKind::Root,
            children: Vec::new(),
            origin: None,
            index: 0,
        };
        root.index = self.next_node_index();
        let scope = ScopeRef { unit: 0, scope: 0 };
        self.resolve_root(0);
        let region = self.units[0].res.scope_region[0];
        let file = self.new_ctx(&b, CtxKind::File(scope), region);
        let mark = self.push(file.clone());
        let result = self
            .init_scope(&file, scope)
            .and_then(|_| self.instantiate_top(&file, &mut root.children));
        self.truncate(mark);
        let mut aborted = false;
        let mut interrupted = false;
        let mut camera = self.opts.camera;
        let mut camera_assigned = crate::CameraAssigned::default();
        // A warning in the last expression evaluated may still be armed.
        let result = result.and_then(|_| self.check_hard());
        if let Err(e) = result {
            interrupted = e.kind == UnwindKind::Interrupted;
            aborted = !interrupted;
            for p in e.finish() {
                self.emit_pending(p);
            }
        } else {
            (camera, camera_assigned) = self.update_camera(&file);
        }
        let (tagged, next) = root.find_root_tag();
        let next = next.map(|o| Loc {
            unit: o.unit,
            span: o.span,
        });
        if tagged.is_some()
            && let Some(l) = next
        {
            self.warn(l, DiagCode::Evaluation, "More than one Root Modifier (!)");
        }
        self.truncate(0);
        // Every register instance is closed on every path, errors included.
        debug_assert!(self.regs.is_empty() && self.reg_saves.is_empty());
        self.release_cycles();
        let reuse = self
            .memo
            .take()
            .map(|m| m.finish(!aborted && !interrupted))
            .unwrap_or_default();
        Evaluation {
            reuse,
            root,
            aborted,
            interrupted,
            // Also set by a warning printed after instantiation (the root
            // modifier check), which OpenSCAD raises from `do_export`.
            hard_warning: matches!(self.hard.get(), Hard::Pending | Hard::Thrown),
            camera,
            camera_assigned,
            resolution: {
                let mut st = self.stats;
                for u in &self.units {
                    st.add(&u.res.stats);
                }
                st
            },
        }
    }

    /// `Camera::updateView`: top-level `$vp*` assignments, returning the
    /// camera they leave and which of them the file set.
    fn update_camera(&mut self, file: &Rc<Ctx>) -> (Camera, crate::CameraAssigned) {
        let mut cam = self.opts.camera;
        let mut set = crate::CameraAssigned::default();
        if cam.locked {
            return (cam, set);
        }
        let mut noauto = false;
        let (vpr, vpt, vpd, vpf) = (self.k.vpr, self.k.vpt, self.k.vpd, self.k.vpf);
        for (s, is_vec) in [(vpr, true), (vpt, true), (vpd, false), (vpf, false)] {
            let Some(v) = file.get_local(s, &self.regions) else {
                continue;
            };
            let ok = if is_vec {
                // `getVec3(x, y, z, 0.0)` fills its outputs only on
                // success, so start from the current values as the C++
                // locals would be overwritten.
                let mut out = if s == vpr { cam.vpr } else { cam.vpt };
                let ok = v.get_vec3_or2(&mut out, 0.0);
                if ok {
                    if s == vpr {
                        cam.vpr = out;
                    } else {
                        cam.vpt = out;
                    }
                }
                ok
            } else if let Some(n) = v.as_number() {
                if s == vpd {
                    cam.vpd = n;
                } else {
                    cam.vpf = n;
                }
                true
            } else {
                false
            };
            if ok {
                noauto = true;
                let flag = if s == vpr {
                    &mut set.vpr
                } else if s == vpt {
                    &mut set.vpt
                } else if s == vpd {
                    &mut set.vpd
                } else {
                    &mut set.vpf
                };
                *flag = true;
            } else {
                let what = if is_vec {
                    "a vec3 or vec2 of numbers"
                } else {
                    "a number"
                };
                let mut text = format!("Unable to convert {}=", self.name(s)).into_bytes();
                self.write_echo(&v, &mut text);
                text.extend_from_slice(format!(" to {what}").as_bytes());
                self.warn_noloc(DiagCode::InvalidArgument, text);
            }
        }
        if cam.auto && noauto {
            self.warn_noloc(
                DiagCode::Evaluation,
                "Viewall and autocenter disabled in favor of $vp*",
            );
            cam.auto = false;
        }
        (cam, set)
    }

    pub fn register_capture(&mut self, c: &Rc<Ctx>) {
        self.captured.push(Rc::downgrade(c));
        if self.captured.len() >= self.captured_limit {
            self.captured.retain(|w| w.strong_count() > 0);
            self.captured_limit = (self.captured.len() * 2).max(1024);
        }
    }

    /// Break the cycles function literals create (a literal stored in a
    /// context it captured, or in one of its ancestors), so an evaluation
    /// frees all its memory: emptying the variables of every context from
    /// the captured one outward cuts every such cycle, since a cycle has to
    /// pass through a stored value.
    fn release_cycles(&mut self) {
        for w in std::mem::take(&mut self.captured) {
            let Some(c) = w.upgrade() else { continue };
            let mut cur: Option<&Ctx> = Some(&c);
            while let Some(c) = cur {
                c.vars.borrow_mut().clear();
                for v in c.slots.borrow_mut().iter_mut() {
                    *v = None;
                }
                cur = c.parent.as_deref();
            }
        }
    }

    // --- expressions -------------------------------------------------------

    /// Evaluate an expression. The common kinds are handled here; the rest
    /// live in [`Self::eval_cold`] so this function's stack frame stays
    /// small, which matters both for speed and for how deep OpenSCAD
    /// programs can recurse within the stack limit.
    #[inline]
    pub fn eval(&mut self, u: u32, id: ExprId, ctx: &Rc<Ctx>) -> R<Value> {
        // A nested expression is a frame for the frame budget (see
        // `crate::recursion`): a recursive function whose body nests
        // deeply costs stack between its calls too.
        self.frames += crate::recursion::EXPRESSION_FRAMES;
        let v = self.eval_expr(u, id, ctx);
        self.frames -= crate::recursion::EXPRESSION_FRAMES;
        let v = v?;
        self.check_hard()?;
        Ok(v)
    }

    fn eval_expr(&mut self, u: u32, id: ExprId, ctx: &Rc<Ctx>) -> R<Value> {
        let ast: &'a Ast = self.units[u as usize].ast;
        let e = ast.expr(id);
        match &e.kind {
            ExprKind::Undef | ExprKind::Invalid => Ok(Value::Undef),
            ExprKind::Bool(b) => Ok(Value::Bool(*b)),
            ExprKind::Number(n) => Ok(Value::Number(*n)),
            ExprKind::Var(n) => {
                let unit = &self.units[u as usize];
                let s = unit.sym(*n);
                let found = match unit.res.var(id).cands() {
                    Some(r) => self.find_var(u, r, s, ctx),
                    None => self.var_fallback(ctx, s),
                };
                if let Some(v) = found {
                    // A moved accumulator leaves `undef` in its frame (see
                    // `move_accumulators`), so only an `undef` needs the
                    // check, which keeps it off the common path.
                    if v.is_undef()
                        && !self.moved.is_empty()
                        && let Some(m) = self.take_moved(u, id, ctx, s)
                    {
                        return Ok(m);
                    }
                    return Ok(v);
                }
                Ok(self.lookup_variable(
                    ctx,
                    s,
                    Loc {
                        unit: u,
                        span: e.span,
                    },
                ))
            }
            ExprKind::Binary(op, l, r) => self.eval_binary(u, *op, *l, *r, e.span, ctx),
            ExprKind::Ternary(c, a, b) => {
                let next = if self.eval(u, *c, ctx)?.to_bool() {
                    *a
                } else {
                    *b
                };
                self.eval(u, next, ctx)
            }
            ExprKind::Index(a, i) => {
                let a = self.eval(u, *a, ctx)?;
                let i = self.eval(u, *i, ctx)?;
                Ok(ops::index(&a, &i))
            }
            ExprKind::Call(..) => self.eval_call(u, id, ctx),
            _ => self.eval_cold(u, id, ctx),
        }
    }

    #[inline(never)]
    fn eval_cold(&mut self, u: u32, id: ExprId, ctx: &Rc<Ctx>) -> R<Value> {
        let ast: &'a Ast = self.units[u as usize].ast;
        let e = ast.expr(id);
        match &e.kind {
            ExprKind::String(_) => Ok(self.units[u as usize].consts[id.0 as usize]
                .clone()
                .unwrap_or_default()),
            ExprKind::Unary(op, x) => {
                let v = self.eval(u, *x, ctx)?;
                let r = match op {
                    UnaryOp::Not => return Ok(Value::Bool(!v.to_bool())),
                    UnaryOp::Negate => ops::neg(&v),
                    UnaryOp::BinaryNot => ops::bit_not(&v),
                };
                Ok(self.check_undef(r, u, e.span))
            }
            ExprKind::Member(x, n) => {
                let v = self.eval(u, *x, ctx)?;
                let name = ast.name(*n);
                let i = match (&v, name) {
                    (Value::Vector(_), "x") | (Value::Range(_), "begin") => 0.0,
                    (Value::Vector(_), "y") | (Value::Range(_), "step") => 1.0,
                    (Value::Vector(_), "z") | (Value::Range(_), "end") => 2.0,
                    _ => return Ok(Value::Undef),
                };
                Ok(ops::index(&v, &Value::Number(i)))
            }
            ExprKind::Range { begin, step, end } => {
                self.eval_range(u, id, *begin, *step, *end, ctx)
            }
            ExprKind::Vector(items) => {
                if let Some(c) = &self.units[u as usize].consts[id.0 as usize] {
                    return Ok(c.clone());
                }
                if let Some((&first, rest)) = items.split_first()
                    && let ExprKind::LcEach(x) = ast.expr(first).kind
                    && !self.is_lc(u, x)
                {
                    return self.each_then(u, first, x, rest, ctx);
                }
                let mut out = Vec::with_capacity(items.len());
                for &it in items {
                    self.eval_element(u, it, ctx, &mut out)?;
                }
                Ok(Value::vector(out))
            }
            ExprKind::Function(..) => {
                self.register_capture(ctx);
                Ok(Value::Function(Rc::new(FunctionValue::new(
                    u,
                    id,
                    ctx.clone(),
                ))))
            }
            ExprKind::Let(args, body) => {
                let region = self.units[u as usize].res.expr[id.0 as usize];
                if self.regions[region as usize].reg() {
                    // Inline: a recursion through a `let` holds this frame
                    // at every level, and a helper would add one.
                    let old = self.reg_open(region);
                    let r = self
                        .assign_regs(u, args, e.span, region, ctx)
                        .and_then(|_| self.eval(u, *body, ctx));
                    self.reg_close(region, old);
                    return r;
                }
                let c = self.new_ctx(ctx, CtxKind::Plain, region);
                let mark = self.push(c.clone());
                let r = self
                    .sequential_assign(u, args, e.span, &c)
                    .and_then(|_| self.eval(u, *body, &c));
                self.truncate(mark);
                Ctx::recycle(c, &mut self.ctx_pool);
                r
            }
            ExprKind::Assert(args, body) => {
                self.perform_assert(u, args, e.span, ctx)?;
                match body {
                    Some(b) => self.eval(u, *b, ctx),
                    None => Ok(Value::Undef),
                }
            }
            ExprKind::Echo(args, body) => {
                self.echo(u, args, ctx)?;
                match body {
                    Some(b) => self.eval(u, *b, ctx),
                    None => Ok(Value::Undef),
                }
            }
            ExprKind::LcIf(..)
            | ExprKind::LcEach(_)
            | ExprKind::LcFor(..)
            | ExprKind::LcForC { .. }
            | ExprKind::LcLet(..) => {
                let mut out = Vec::new();
                self.eval_lc(u, id, ctx, &mut out)?;
                Ok(Value::vector(out))
            }
            _ => unreachable!("handled in eval"),
        }
    }

    /// `Expression::checkUndef`: print why an operator gave `undef`.
    #[inline(never)]
    fn check_undef(&mut self, r: ops::OpResult, u: u32, span: Span) -> Value {
        // An element-wise operator stops at the memory limit with a partial
        // result (`ops::map_vec`), which must not be used.
        if !matches!(r, Ok(Value::Number(_) | Value::Bool(_))) {
            self.check_memory(Some(Loc { unit: u, span }));
        }
        match r {
            Ok(v) => v,
            Err(why) => {
                self.warn(
                    Loc { unit: u, span },
                    DiagCode::UndefinedOperation,
                    why.message(),
                );
                Value::Undef
            }
        }
    }

    fn eval_binary(
        &mut self,
        u: u32,
        op: BinaryOp,
        l: ExprId,
        r: ExprId,
        span: Span,
        ctx: &Rc<Ctx>,
    ) -> R<Value> {
        match op {
            BinaryOp::LogicalAnd => {
                let a = self.eval(u, l, ctx)?.to_bool();
                return Ok(Value::Bool(a && self.eval(u, r, ctx)?.to_bool()));
            }
            BinaryOp::LogicalOr => {
                let a = self.eval(u, l, ctx)?.to_bool();
                return Ok(Value::Bool(a || self.eval(u, r, ctx)?.to_bool()));
            }
            _ => {}
        }
        let a = self.eval(u, l, ctx)?;
        let b = self.eval(u, r, ctx)?;
        // Fast path for the overwhelmingly common case.
        if let (Value::Number(x), Value::Number(y)) = (&a, &b) {
            let (x, y) = (*x, *y);
            match op {
                BinaryOp::Plus => return Ok(Value::Number(x + y)),
                BinaryOp::Minus => return Ok(Value::Number(x - y)),
                BinaryOp::Multiply => return Ok(Value::Number(x * y)),
                BinaryOp::Divide => return Ok(Value::Number(x / y)),
                BinaryOp::Less => return Ok(Value::Bool(x < y)),
                BinaryOp::LessEqual => return Ok(Value::Bool(x <= y)),
                BinaryOp::Greater => return Ok(Value::Bool(x > y)),
                BinaryOp::GreaterEqual => return Ok(Value::Bool(x >= y)),
                BinaryOp::Equal => return Ok(Value::Bool(x == y)),
                BinaryOp::NotEqual => return Ok(Value::Bool(x != y)),
                _ => {}
            }
        }
        self.binary_slow(op, &a, &b, u, span)
    }

    #[inline(never)]
    fn binary_slow(&mut self, op: BinaryOp, a: &Value, b: &Value, u: u32, span: Span) -> R<Value> {
        let res = match op {
            BinaryOp::Plus => ops::add(a, b),
            BinaryOp::Minus => ops::sub(a, b),
            BinaryOp::Multiply => {
                let mut w = std::mem::take(&mut self.op_warnings);
                let r = ops::mul(a, b, &mut w);
                for s in w.drain(..) {
                    self.warn_noloc(DiagCode::UndefinedOperation, s);
                }
                self.op_warnings = w;
                r
            }
            BinaryOp::Divide => ops::div(a, b),
            BinaryOp::Modulo => ops::rem(a, b),
            BinaryOp::Exponent => ops::pow(a, b),
            BinaryOp::Less
            | BinaryOp::LessEqual
            | BinaryOp::Greater
            | BinaryOp::GreaterEqual
            | BinaryOp::Equal
            | BinaryOp::NotEqual => {
                // A comparison of two shared list trees is one long call:
                // it polls the request's flags and stops (see `ops`).
                let (i, g) = (self.opts.interrupt.as_deref(), self.opts.guard.as_deref());
                match ops::relation(op, a, b, ops::Stop::new(i, g)) {
                    Ok(r) => r,
                    Err(ops::Stopped) => {
                        self.check_limits(None)?;
                        self.check_interrupt()?;
                        Ok(Value::Undef)
                    }
                }
            }
            BinaryOp::BinaryAnd => ops::bitwise(a, b, Bitwise::And),
            BinaryOp::BinaryOr => ops::bitwise(a, b, Bitwise::Or),
            BinaryOp::ShiftLeft => ops::bitwise(a, b, Bitwise::Shl),
            BinaryOp::ShiftRight => ops::bitwise(a, b, Bitwise::Shr),
            BinaryOp::LogicalAnd | BinaryOp::LogicalOr => unreachable!("handled above"),
        };
        Ok(self.check_undef(res, u, span))
    }

    /// A lexical lookup by name, for a reference the resolver did not
    /// reach (counted), or `None` for a `$` name.
    #[cold]
    #[inline(never)]
    fn var_fallback(&mut self, ctx: &Ctx, s: Sym) -> Option<Value> {
        if self.syms.is_config(s) {
            return None;
        }
        self.stats.fallbacks += 1;
        ctx.lookup_lexical(s, &self.regions)
    }

    /// `Context::lookup_variable`.
    #[inline(never)]
    pub fn lookup_variable(&mut self, ctx: &Ctx, s: Sym, loc: Loc) -> Value {
        match self.try_lookup(ctx, s) {
            Some(v) => v,
            None => {
                let text = format!("Ignoring unknown variable {}", self.quote_sym(s));
                self.warn(loc, DiagCode::UnknownVariable, text);
                Value::Undef
            }
        }
    }

    #[inline(never)]
    fn eval_range(
        &mut self,
        u: u32,
        id: ExprId,
        begin: ExprId,
        step: Option<ExprId>,
        end: ExprId,
        ctx: &Rc<Ctx>,
    ) -> R<Value> {
        let loc = self.expr_loc(u, id);
        let b = self.eval(u, begin, ctx)?;
        let e = self.eval(u, end, ctx)?;
        let (Some(bd), Some(ed)) = (b.as_number(), e.as_number()) else {
            let mut t = b"Unable to convert [".to_vec();
            self.write_echo_nothrow(&b, &mut t);
            t.extend_from_slice(b":...:");
            self.write_echo_nothrow(&e, &mut t);
            t.extend_from_slice(b"] to a range");
            self.warn(loc, DiagCode::InvalidArgument, t);
            return Ok(Value::Undef);
        };
        let mut sd = 1.0;
        if let Some(s) = step {
            let sv = self.eval(u, s, ctx)?;
            match sv.as_number() {
                Some(x) => sd = x,
                None => {
                    let mut t = b"Unable to convert [...:".to_vec();
                    self.write_echo_nothrow(&sv, &mut t);
                    t.extend_from_slice(b":...] to a step value");
                    self.warn(loc, DiagCode::InvalidArgument, t);
                    return Ok(Value::Undef);
                }
            }
        }
        if self.units[u as usize].ast.is_literal(id) {
            if sd > 0.0 && ed < bd {
                self.warn(
                    loc,
                    DiagCode::InvalidArgument,
                    "begin is greater than the end, but step is positive",
                );
            } else if sd < 0.0 && ed > bd {
                self.warn(
                    loc,
                    DiagCode::InvalidArgument,
                    "begin is smaller than the end, but step is negative",
                );
            }
        }
        Ok(Value::range(bd, sd, ed))
    }

    fn is_lc(&self, u: u32, id: ExprId) -> bool {
        matches!(
            self.units[u as usize].ast.expr(id).kind,
            ExprKind::LcIf(..)
                | ExprKind::LcEach(_)
                | ExprKind::LcFor(..)
                | ExprKind::LcForC { .. }
                | ExprKind::LcLet(..)
        )
    }

    /// One element of a vector literal: list comprehensions splice their
    /// values in (OpenSCAD's embedded vectors), anything else is one value.
    pub fn eval_element(
        &mut self,
        u: u32,
        id: ExprId,
        ctx: &Rc<Ctx>,
        out: &mut Vec<Value>,
    ) -> R<()> {
        if self.is_lc(u, id) {
            self.eval_lc(u, id, ctx, out)?;
        } else {
            let v = self.eval(u, id, ctx)?;
            out.push(v);
        }
        // Checked as the list grows, so a comprehension that would make a
        // billion elements stops at the limit instead of at the end.
        if out.len() > self.caps.list {
            return self.list_overflow(u, id, out.len());
        }
        Ok(())
    }

    /// [`Evaluator::eval_element`]'s list limit, passed: kept out of line
    /// so the per-element path stays one compare.
    #[cold]
    #[inline(never)]
    fn list_overflow(&mut self, u: u32, id: ExprId, n: usize) -> R<()> {
        let loc = self.expr_loc(u, id);
        self.list_fits(n, loc, "a list");
        self.check_hard()
    }

    /// `[each x, rest...]`, appending `rest` to `x`'s list in place when
    /// nothing else holds it (see [`crate::value::Growable`]): a
    /// tail-recursive `[each acc, n]` is then linear, not quadratic. The
    /// result is the same as [`Evaluator::eval_element`] on each item.
    fn each_then(
        &mut self,
        u: u32,
        first: ExprId,
        x: ExprId,
        rest: &[ExprId],
        ctx: &Rc<Ctx>,
    ) -> R<Value> {
        self.frames += crate::recursion::COMPREHENSION_FRAMES;
        let v = self.eval(u, x, ctx);
        self.frames -= crate::recursion::COMPREHENSION_FRAMES;
        let mut g = match v? {
            Value::Vector(v) => match v.into_growable() {
                Ok(g) => g,
                Err(v) => return self.each_copied(u, first, Value::Vector(v), rest, ctx),
            },
            other => return self.each_copied(u, first, other, rest, ctx),
        };
        // The list limit is checked where `eval_element` checks it: after
        // each element, on the whole list so far.
        if g.len() > self.caps.list {
            self.list_overflow(u, first, g.len())?;
        }
        let mut tail = Vec::with_capacity(rest.len());
        for &it in rest {
            self.eval_element(u, it, ctx, &mut tail)?;
            let n = g.len() + tail.len();
            if n > self.caps.list {
                self.list_overflow(u, it, n)?;
            }
        }
        g.reserve(tail.len());
        g.extend(tail);
        Ok(Value::Vector(g.finish()))
    }

    /// [`Evaluator::each_then`] when `x`'s value is shared or not a list.
    fn each_copied(
        &mut self,
        u: u32,
        first: ExprId,
        v: Value,
        rest: &[ExprId],
        ctx: &Rc<Ctx>,
    ) -> R<Value> {
        let loc = self.expr_loc(u, first);
        let mut out = Vec::new();
        self.each_value(v, loc, &mut out);
        if out.len() > self.caps.list {
            self.list_overflow(u, first, out.len())?;
        }
        out.reserve(rest.len());
        for &it in rest {
            self.eval_element(u, it, ctx, &mut out)?;
        }
        Ok(Value::vector(out))
    }

    /// The value moved out for this read of `s`, if it resolves to a
    /// binding [`Evaluator::move_accumulators`] moved.
    #[inline(never)]
    fn take_moved(&mut self, u: u32, id: ExprId, ctx: &Ctx, s: Sym) -> Option<Value> {
        if !self.moved.iter().any(|m| m.sym == s && m.value.is_some()) {
            return None;
        }
        let in_reg = match self.units[u as usize].res.var(id).cands() {
            Some(r) => self.reg_binding(self.units[u as usize].res.cands(r)),
            None => None,
        };
        let owner = match in_reg {
            Some(i) => Owner::Reg(i),
            None => Owner::Ctx(ctx.binder(s, &self.regions)?),
        };
        self.moved
            .iter_mut()
            .rev()
            .find(|m| m.sym == s && m.owner == owner)?
            .value
            .take()
    }

    /// A list comprehension element holds frames of the frame budget, like
    /// an expression (see `crate::recursion`).
    fn eval_lc(&mut self, u: u32, id: ExprId, ctx: &Rc<Ctx>, out: &mut Vec<Value>) -> R<()> {
        self.frames += crate::recursion::COMPREHENSION_FRAMES;
        let r = self.eval_lc_frame(u, id, ctx, out);
        self.frames -= crate::recursion::COMPREHENSION_FRAMES;
        r
    }

    fn eval_lc_frame(&mut self, u: u32, id: ExprId, ctx: &Rc<Ctx>, out: &mut Vec<Value>) -> R<()> {
        let ast: &'a Ast = self.units[u as usize].ast;
        let e = ast.expr(id);
        match &e.kind {
            ExprKind::LcIf(c, a, b) => {
                if self.eval(u, *c, ctx)?.to_bool() {
                    self.eval_element(u, *a, ctx, out)
                } else if let Some(b) = b {
                    self.eval_element(u, *b, ctx, out)
                } else {
                    Ok(())
                }
            }
            ExprKind::LcEach(x) => {
                let loc = Loc {
                    unit: u,
                    span: e.span,
                };
                if self.is_lc(u, *x) {
                    let mut inner = Vec::new();
                    self.eval_lc(u, *x, ctx, &mut inner)?;
                    for v in inner {
                        self.each_value(v, loc, out);
                    }
                } else {
                    let v = self.eval(u, *x, ctx)?;
                    self.each_value(v, loc, out);
                }
                Ok(())
            }
            ExprKind::LcFor(args, body) => {
                let loc = Loc {
                    unit: u,
                    span: e.span,
                };
                let body = *body;
                let region = self.units[u as usize].res.expr[id.0 as usize];
                self.for_each(u, args, region, loc, ctx, &mut |ev, c| {
                    ev.eval_element(u, body, c, out)
                })
            }
            ExprKind::LcForC {
                init,
                cond,
                incr,
                body,
            } => self.lc_for_c(u, id, e.span, (init, *cond, incr, *body), ctx, out),
            ExprKind::LcLet(args, body) => {
                let region = self.units[u as usize].res.expr[id.0 as usize];
                if self.regions[region as usize].reg() {
                    let old = self.reg_open(region);
                    let r = self
                        .assign_regs(u, args, e.span, region, ctx)
                        .and_then(|_| self.eval_element(u, *body, ctx, out));
                    self.reg_close(region, old);
                    return r;
                }
                let c = self.new_ctx(ctx, CtxKind::Plain, region);
                let mark = self.push(c.clone());
                let r = self
                    .sequential_assign(u, args, e.span, &c)
                    .and_then(|_| self.eval_element(u, *body, &c, out));
                self.truncate(mark);
                r
            }
            _ => {
                let v = self.eval(u, id, ctx)?;
                out.push(v);
                Ok(())
            }
        }
    }

    /// `LcForC::evaluate`: a C-style `for` comprehension. Out of line, so
    /// its locals are not part of `eval_lc_frame`'s frame, which a
    /// recursion through comprehensions holds at every level.
    #[inline(never)]
    fn lc_for_c(
        &mut self,
        u: u32,
        id: ExprId,
        span: Span,
        (init, cond, incr, body): (&'a [Arg], ExprId, &'a [Arg], ExprId),
        ctx: &Rc<Ctx>,
        out: &mut Vec<Value>,
    ) -> R<()> {
        let loc = Loc { unit: u, span };
        let first = self.units[u as usize].res.expr[id.0 as usize];
        let initial = self.new_ctx(ctx, CtxKind::Plain, first);
        let mark = self.push(initial.clone());
        let r = (|| {
            self.sequential_assign(u, init, span, &initial)?;
            let iteration = crate::resolve::next_region(first);
            let mut current = self.new_ctx(&initial, CtxKind::Plain, iteration);
            let slot = self.push(current.clone());
            let mut counter: u32 = 0;
            while self.eval(u, cond, &current)?.to_bool() {
                self.check_interrupt()?;
                self.eval_element(u, body, &current, out)?;
                if counter == 1_000_000 {
                    self.error(
                        Some(loc),
                        DiagCode::IterationLimit,
                        "For loop counter exceeded limit",
                    );
                    return Err(self.unwind(UnwindKind::LoopLimit));
                }
                counter += 1;
                // `LcForC::evaluate` assigns the increment in a
                // child of the current iteration (so `i = i + 1`
                // reads the old `i`), then re-parents it to the
                // initial context so the chain stays two deep.
                // Contexts' parents are fixed here, so the values
                // move to a fresh context of the initial one
                // instead. A function literal made in the
                // increment keeps the first context, which differs
                // only by also reaching the old iteration, whose
                // names that one binds all over again.
                let step = self.new_ctx(&current, CtxKind::Plain, iteration);
                self.push(step.clone());
                self.sequential_assign(u, incr, span, &step)?;
                let next = self.new_ctx(&initial, CtxKind::Plain, iteration);
                next.slots.borrow_mut().clone_from(&step.slots.borrow());
                next.vars.borrow_mut().clone_from(&step.vars.borrow());
                drop(step);
                self.truncate(slot);
                self.push(next.clone());
                current = next;
            }
            Ok(())
        })();
        self.truncate(mark);
        r
    }

    /// `LcEach::evalRecur` for one value.
    fn each_value(&mut self, v: Value, loc: Loc, out: &mut Vec<Value>) {
        match v {
            Value::Range(r) => {
                let n = r.num_values();
                if n >= 1_000_000 {
                    self.warn(
                        loc,
                        DiagCode::IterationLimit,
                        format!("Bad range parameter in for statement: too many elements ({n})"),
                    );
                } else {
                    out.extend(r.iter().map(Value::Number));
                }
            }
            Value::Vector(vec) => out.extend(vec.into_vec()),
            Value::Str(s) => out.extend(crate::utf8::chars(s.as_bytes()).map(Value::str)),
            Value::Undef => {}
            other => out.push(other),
        }
    }

    /// `LcFor::forEach`: nested iteration over the assignments, calling
    /// `op` with each innermost iteration context. Variable `k` binds in
    /// region `region + k`.
    pub fn for_each(
        &mut self,
        u: u32,
        args: &'a [Arg],
        region: u32,
        loc: Loc,
        ctx: &Rc<Ctx>,
        op: &mut dyn FnMut(&mut Self, &Rc<Ctx>) -> R<()>,
    ) -> R<()> {
        let Some((first, rest)) = args.split_first() else {
            return op(self, ctx);
        };
        let name = first
            .name
            .map_or(self.k.empty, |n| self.units[u as usize].sym(n));
        let values = self.eval(u, first.expr, ctx)?;
        if self.regions[region as usize].reg() {
            return self.for_each_reg(u, rest, region, loc, ctx, op, &values);
        }
        // The variable's slot, looked up once rather than per iteration.
        let slot = self.regions[region as usize]
            .binds
            .first()
            .copied()
            .unwrap_or(NO_SLOT);
        let config = self.syms.is_config(name);
        self.iterate_over(&values, loc, |ev, v| {
            ev.check_interrupt()?;
            let c = match slot {
                NO_SLOT => ev.iteration_vars(ctx, region, name, config, v),
                i => Ctx::with_slot(&mut ev.ctx_pool, ctx, region, i, v),
            };
            let mark = ev.push(c.clone());
            let r = ev.for_each(u, rest, crate::resolve::next_region(region), loc, &c, op);
            ev.truncate(mark);
            // Each iteration's context dies here unless the body captured
            // it (a function literal); the next iteration reuses it.
            Ctx::recycle(c, &mut ev.ctx_pool);
            r
        })
    }

    /// [`Self::for_each`] for a variable in a register region: one
    /// register for the whole loop, set per iteration, and the body
    /// evaluated in `ctx` itself. Out of line, as `iteration_vars` is.
    #[allow(clippy::too_many_arguments)]
    #[inline(never)]
    fn for_each_reg(
        &mut self,
        u: u32,
        rest: &'a [Arg],
        region: u32,
        loc: Loc,
        ctx: &Rc<Ctx>,
        op: &mut dyn FnMut(&mut Self, &Rc<Ctx>) -> R<()>,
        values: &Value,
    ) -> R<()> {
        debug_assert_eq!(self.regions[region as usize].binds.first(), Some(&0));
        let old = self.reg_open(region);
        let i = self.reg_base[region as usize] as usize;
        let r = self.iterate_over(values, loc, |ev, v| {
            ev.check_interrupt()?;
            ev.regs[i] = Some(v);
            // The last variable calls the body itself: a recursion through
            // a comprehension holds this frame at every level, and the
            // extra `for_each` frame would cost wasm32 stack depth.
            let r = if rest.is_empty() {
                op(ev, ctx)
            } else {
                ev.for_each(u, rest, crate::resolve::next_region(region), loc, ctx, op)
            };
            // The iteration's value dies here, where its context would.
            ev.regs[i] = None;
            r
        });
        self.reg_close(region, old);
        r
    }

    /// `f` with each value a `for` iterates over `values`.
    #[inline]
    fn iterate_over(
        &mut self,
        values: &Value,
        loc: Loc,
        mut f: impl FnMut(&mut Self, Value) -> R<()>,
    ) -> R<()> {
        match values {
            Value::Range(r) => {
                let n = r.num_values();
                if n >= 1_000_000 {
                    self.warn(
                        loc,
                        DiagCode::IterationLimit,
                        format!("Bad range parameter in for statement: too many elements ({n})"),
                    );
                } else {
                    for x in r.iter() {
                        f(self, Value::Number(x))?;
                    }
                }
            }
            Value::Vector(v) => {
                for x in v.iter() {
                    f(self, x.clone())?;
                }
            }
            Value::Str(s) => {
                for c in crate::utf8::chars(s.as_bytes()) {
                    f(self, Value::str(c))?;
                }
            }
            Value::Undef => {}
            other => f(self, other.clone())?,
        }
        Ok(())
    }

    /// A `for` iteration's context binding a variable that has no slot (a
    /// `$` name). Out of line: `for_each` is on every level of a recursion
    /// through a comprehension.
    #[inline(never)]
    fn iteration_vars(
        &mut self,
        ctx: &Rc<Ctx>,
        region: u32,
        name: Sym,
        config: bool,
        v: Value,
    ) -> Rc<Ctx> {
        let c = self.new_ctx(ctx, CtxKind::Plain, region);
        c.vars.borrow_mut().set(name, v, config);
        c
    }

    /// `Let::doSequentialAssignment` into `target`.
    pub fn sequential_assign(
        &mut self,
        u: u32,
        args: &'a [Arg],
        span: Span,
        target: &Rc<Ctx>,
    ) -> R<()> {
        let loc = Loc { unit: u, span };
        // Names bound earlier in this `let`, for the duplicate warning. A
        // name with a slot is a duplicate when its slot is already set
        // (`target` is new, so only this `let` set it); the rest (`$`
        // names, which `target` may hold copies of from the caller) are
        // listed.
        let mut seen: Vec<Sym> = Vec::new();
        for (k, a) in args.iter().enumerate() {
            let v = self.eval(u, a.expr, target)?;
            match a.name {
                None => self.unnamed_assignment(loc, &v),
                Some(n) => {
                    let s = self.units[u as usize].sym(n);
                    let slot = self.regions[target.region as usize]
                        .binds
                        .get(k)
                        .copied()
                        .unwrap_or(NO_SLOT);
                    let duplicate = match slot {
                        NO_SLOT => seen.contains(&s),
                        i => target.has_slot(i),
                    };
                    if duplicate {
                        self.duplicate_assignment(loc, s, &v);
                    } else if slot == NO_SLOT {
                        let config = self.syms.is_config(s);
                        target.vars.borrow_mut().set(s, v, config);
                        seen.push(s);
                    } else {
                        target.set_slot(slot, v);
                    }
                }
            }
        }
        Ok(())
    }

    /// [`Self::sequential_assign`] into the registers of `region`'s
    /// instance, just opened: each argument is evaluated in `ctx` with the
    /// registers bound so far visible, as the new context's would be. A
    /// register region binds no `$` name, so every named binder has a
    /// register, and a duplicate is one already set.
    pub fn assign_regs(
        &mut self,
        u: u32,
        args: &'a [Arg],
        span: Span,
        region: u32,
        ctx: &Rc<Ctx>,
    ) -> R<()> {
        let loc = Loc { unit: u, span };
        for (k, a) in args.iter().enumerate() {
            let v = self.eval(u, a.expr, ctx)?;
            match a.name {
                None => self.unnamed_assignment(loc, &v),
                Some(n) => {
                    let slot = self.regions[region as usize].binds[k];
                    debug_assert_ne!(slot, NO_SLOT, "a register region binds no `$` name");
                    let i = self.reg_base[region as usize] as usize + slot as usize;
                    if self.regs[i].is_some() {
                        let s = self.units[u as usize].sym(n);
                        self.duplicate_assignment(loc, s, &v);
                    } else {
                        self.regs[i] = Some(v);
                    }
                }
            }
        }
        Ok(())
    }

    #[cold]
    #[inline(never)]
    fn unnamed_assignment(&mut self, loc: Loc, v: &Value) {
        let mut t = b"Assignment without variable name ".to_vec();
        self.write_echo_nothrow(v, &mut t);
        self.warn(loc, DiagCode::Evaluation, t);
    }

    #[cold]
    #[inline(never)]
    fn duplicate_assignment(&mut self, loc: Loc, s: Sym, v: &Value) {
        let mut t = format!(
            "Ignoring duplicate variable assignment {} = ",
            self.quote_sym(s)
        )
        .into_bytes();
        self.write_echo_nothrow(v, &mut t);
        self.warn(loc, DiagCode::Overwrite, t);
    }

    /// `echo(...)`: print the evaluated arguments.
    pub fn echo(&mut self, u: u32, args: &'a [Arg], ctx: &Rc<Ctx>) -> R<()> {
        let values = self.eval_args(u, args, ctx)?;
        let mut text = Vec::new();
        for (i, a) in values.iter().enumerate() {
            if i > 0 {
                text.extend_from_slice(b", ");
            }
            if let Some(n) = a.name {
                text.extend_from_slice(self.name(n).as_bytes());
                text.extend_from_slice(b" = ");
            }
            if self.write_echo_checked(&a.value, &mut text).is_err() {
                let msg = "Stack exhausted while trying to convert a vector to EchoString";
                let mut e = self.unwind(UnwindKind::EchoStack);
                if let Some(p) = e.log(Pending {
                    severity: Severity::Error,
                    code: DiagCode::RecursionLimit,
                    text: msg.into(),
                    loc: None,
                }) {
                    self.emit_pending(p);
                }
                return Err(e);
            }
        }
        self.emit(Severity::Echo, DiagCode::Echo, &text, None);
        Ok(())
    }

    /// `Assert::performAssert`.
    pub fn perform_assert(&mut self, u: u32, args: &'a [Arg], span: Span, ctx: &Rc<Ctx>) -> R<()> {
        let loc = Loc { unit: u, span };
        let values = self.eval_args(u, args, ctx)?;
        let (condition, message) = (self.k.condition, self.k.message);
        let frame = self.bind_builtin(values, loc, &[condition], &[message], true);
        let cond = frame.get(condition).cloned().unwrap_or_default();
        if cond.to_bool() {
            return Ok(());
        }
        let ast = self.units[u as usize].ast;
        let cond_expr = args
            .iter()
            .find(|a| a.name.is_none() || a.name.is_some_and(|n| ast.name(n) == "condition"));
        let mut text = b"Assertion".to_vec();
        if let Some(a) = cond_expr {
            text.extend_from_slice(b" '");
            lang::dump::write_expr(ast, a.expr, &mut text);
            text.push(b'\'');
        }
        text.extend_from_slice(b" failed");
        if let Some(m) = frame.get(message) {
            let m = m.clone();
            text.extend_from_slice(b": ");
            self.write_echo_nothrow(&m, &mut text);
        }
        self.error(Some(loc), DiagCode::AssertionFailed, text);
        Err(self.unwind(UnwindKind::Assertion))
    }
}
