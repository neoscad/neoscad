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
use crate::ops::{self, Bitwise, Cmp};
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
        let seed = opts.rng_seed;
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
            builtin_ctx: Ctx::new(None, CtxKind::Builtin),
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

    pub fn check_interrupt(&self) -> R<()> {
        if self.interrupted() {
            Err(Unwind::new(UnwindKind::Interrupted, 0))
        } else {
            self.check_hard()
        }
    }

    /// Raise the `--hardwarnings` abort if a warning has armed it (see
    /// [`Hard`]). Only the first warning raises it: once thrown, warnings
    /// printed while the error travels up (none, as nothing is evaluated
    /// then) cannot start a second one.
    #[inline]
    pub fn check_hard(&self) -> R<()> {
        if self.hard.get() == Hard::Pending {
            self.hard.set(Hard::Thrown);
            Err(self.unwind(UnwindKind::HardWarning))
        } else {
            Ok(())
        }
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

    /// `Context::try_lookup_variable`.
    pub fn try_lookup(&self, ctx: &Ctx, s: Sym) -> Option<Value> {
        if self.syms.is_config(s) {
            self.lookup_special(s)
        } else {
            ctx.lookup_lexical(s)
        }
    }

    pub fn set_var(&mut self, ctx: &Ctx, s: Sym, v: Value) {
        let config = self.syms.is_config(s);
        ctx.vars.borrow_mut().set(s, v, config);
    }

    pub fn next_node_index(&mut self) -> usize {
        let i = self.node_index;
        self.node_index += 1;
        i
    }

    // --- messages --------------------------------------------------------

    pub fn expr_loc(&self, unit: u32, e: ExprId) -> Loc {
        Loc {
            unit,
            span: self.units[unit as usize].ast.expr(e).span,
        }
    }

    pub fn emit(&mut self, severity: Severity, code: DiagCode, text: &[u8], loc: Option<Loc>) {
        // OpenSCAD would already be unwinding from the first warning, so
        // nothing printed between it and the check that raises it exists
        // there (a builtin warning about several arguments, an unknown
        // function after a disabled experimental one).
        if self.hard.get() == Hard::Pending {
            return;
        }
        if severity == Severity::Deprecated
            && !self
                .deprecations
                .insert((text.to_vec(), loc.map(|l| (l.unit, l.span))))
        {
            return;
        }
        let mut diag = Diagnostic::new(code, severity, String::from_utf8_lossy(text).into_owned())
            .with_base(PathBase::MainFileDir);
        let mut sources = None;
        if let Some(l) = loc {
            let src = &self.units[l.unit as usize].program.sources;
            let line = src.get(l.span.file).line_of(l.span.start);
            diag = diag.at(l.span, line);
            sources = Some(src);
        }
        self.out.message(&Message {
            diag,
            text,
            sources,
        });
        if severity == Severity::Warning && self.hard.get() == Hard::Armed {
            self.hard.set(Hard::Pending);
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
        let file = Ctx::new(Some(b.clone()), CtxKind::File(scope));
        let mark = self.push(file.clone());
        let result = self
            .init_scope(&file, scope)
            .and_then(|_| self.instantiate_scope(scope, &file, &mut root.children, None));
        self.truncate(mark);
        let mut aborted = false;
        let mut interrupted = false;
        let mut camera = self.opts.camera;
        // A warning in the last expression evaluated may still be armed.
        let result = result.and_then(|_| self.check_hard());
        if let Err(e) = result {
            interrupted = e.kind == UnwindKind::Interrupted;
            aborted = !interrupted;
            for p in e.finish() {
                self.emit_pending(p);
            }
        } else {
            camera = self.update_camera(&file);
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
        self.release_cycles();
        Evaluation {
            root,
            aborted,
            interrupted,
            // Also set by a warning printed after instantiation (the root
            // modifier check), which OpenSCAD raises from `do_export`.
            hard_warning: matches!(self.hard.get(), Hard::Pending | Hard::Thrown),
            camera,
        }
    }

    /// `Camera::updateView`: top-level `$vp*` assignments, returning the
    /// camera they leave.
    fn update_camera(&mut self, file: &Rc<Ctx>) -> Camera {
        let mut cam = self.opts.camera;
        if cam.locked {
            return cam;
        }
        let mut noauto = false;
        let (vpr, vpt, vpd, vpf) = (self.k.vpr, self.k.vpt, self.k.vpd, self.k.vpf);
        for (s, is_vec) in [(vpr, true), (vpt, true), (vpd, false), (vpf, false)] {
            let Some(v) = file.get_local(s) else { continue };
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
        cam
    }

    pub fn register_capture(&mut self, c: &Rc<Ctx>) {
        self.captured.push(Rc::downgrade(c));
        if self.captured.len() >= self.captured_limit {
            self.captured.retain(|w| w.strong_count() > 0);
            self.captured_limit = (self.captured.len() * 2).max(1024);
        }
    }

    /// Break the cycles function literals create (a literal stored in a
    /// context it captured), so an evaluation frees all its memory.
    fn release_cycles(&mut self) {
        for w in std::mem::take(&mut self.captured) {
            let mut cur = w.upgrade();
            while let Some(c) = cur {
                c.vars.borrow_mut().clear();
                cur = c.parent.borrow_mut().take();
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
                let s = self.units[u as usize].sym(*n);
                if !self.syms.is_config(s)
                    && let Some(v) = ctx.lookup_lexical(s)
                {
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
                let mut out = Vec::with_capacity(items.len());
                for &it in items {
                    self.eval_element(u, it, ctx, &mut out)?;
                }
                Ok(Value::vector(out))
            }
            ExprKind::Function(..) => {
                self.register_capture(ctx);
                Ok(Value::Function(Rc::new(FunctionValue {
                    unit: u,
                    expr: id,
                    ctx: ctx.clone(),
                })))
            }
            ExprKind::Let(args, body) => {
                let c = Ctx::child(ctx);
                let mark = self.push(c.clone());
                let r = self
                    .sequential_assign(u, args, e.span, &c)
                    .and_then(|_| self.eval(u, *body, &c));
                self.truncate(mark);
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
            BinaryOp::Less => ops::compare(a, b, Cmp::Less).map(Value::Bool),
            BinaryOp::LessEqual => ops::compare(a, b, Cmp::LessEqual).map(Value::Bool),
            BinaryOp::Greater => ops::compare(a, b, Cmp::Greater).map(Value::Bool),
            BinaryOp::GreaterEqual => ops::compare(a, b, Cmp::GreaterEqual).map(Value::Bool),
            BinaryOp::Equal => Ok(Value::Bool(ops::equals(a, b))),
            BinaryOp::NotEqual => Ok(Value::Bool(!ops::equals(a, b))),
            BinaryOp::BinaryAnd => ops::bitwise(a, b, Bitwise::And),
            BinaryOp::BinaryOr => ops::bitwise(a, b, Bitwise::Or),
            BinaryOp::ShiftLeft => ops::bitwise(a, b, Bitwise::Shl),
            BinaryOp::ShiftRight => ops::bitwise(a, b, Bitwise::Shr),
            BinaryOp::LogicalAnd | BinaryOp::LogicalOr => unreachable!("handled above"),
        };
        Ok(self.check_undef(res, u, span))
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
            self.eval_lc(u, id, ctx, out)
        } else {
            let v = self.eval(u, id, ctx)?;
            out.push(v);
            Ok(())
        }
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
                self.for_each(u, args, loc, ctx, &mut |ev, c| {
                    ev.eval_element(u, body, c, out)
                })
            }
            ExprKind::LcForC {
                init,
                cond,
                incr,
                body,
            } => {
                let loc = Loc {
                    unit: u,
                    span: e.span,
                };
                let initial = Ctx::child(ctx);
                let mark = self.push(initial.clone());
                let r = (|| {
                    self.sequential_assign(u, init, e.span, &initial)?;
                    let mut current = Ctx::child(&initial);
                    let slot = self.push(current.clone());
                    let mut counter: u32 = 0;
                    while self.eval(u, *cond, &current)?.to_bool() {
                        self.check_interrupt()?;
                        self.eval_element(u, *body, &current, out)?;
                        if counter == 1_000_000 {
                            self.error(
                                Some(loc),
                                DiagCode::IterationLimit,
                                "For loop counter exceeded limit",
                            );
                            return Err(self.unwind(UnwindKind::LoopLimit));
                        }
                        counter += 1;
                        let next = Ctx::child(&current);
                        self.push(next.clone());
                        self.sequential_assign(u, incr, e.span, &next)?;
                        *next.parent.borrow_mut() = Some(initial.clone());
                        self.truncate(slot);
                        self.push(next.clone());
                        current = next;
                    }
                    Ok(())
                })();
                self.truncate(mark);
                r
            }
            ExprKind::LcLet(args, body) => {
                let c = Ctx::child(ctx);
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
    /// `op` with each innermost iteration context.
    pub fn for_each(
        &mut self,
        u: u32,
        args: &'a [Arg],
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
        let mut iterate = |ev: &mut Self, v: Value| -> R<()> {
            ev.check_interrupt()?;
            let c = Ctx::child(ctx);
            ev.set_var(&c, name, v);
            let mark = ev.push(c.clone());
            let r = ev.for_each(u, rest, loc, &c, op);
            ev.truncate(mark);
            r
        };
        match &values {
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
                        iterate(self, Value::Number(x))?;
                    }
                }
            }
            Value::Vector(v) => {
                for x in v.iter() {
                    iterate(self, x.clone())?;
                }
            }
            Value::Str(s) => {
                for c in crate::utf8::chars(s.as_bytes()) {
                    iterate(self, Value::str(c))?;
                }
            }
            Value::Undef => {}
            other => iterate(self, other.clone())?,
        }
        Ok(())
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
        let mut seen: Vec<Sym> = Vec::new();
        for a in args {
            let v = self.eval(u, a.expr, target)?;
            match a.name {
                None => {
                    let mut t = b"Assignment without variable name ".to_vec();
                    self.write_echo_nothrow(&v, &mut t);
                    self.warn(loc, DiagCode::Evaluation, t);
                }
                Some(n) => {
                    let s = self.units[u as usize].sym(n);
                    if seen.contains(&s) {
                        let mut t = format!(
                            "Ignoring duplicate variable assignment {} = ",
                            self.quote_sym(s)
                        )
                        .into_bytes();
                        self.write_echo_nothrow(&v, &mut t);
                        self.warn(loc, DiagCode::Overwrite, t);
                    } else {
                        self.set_var(target, s, v);
                        seen.push(s);
                    }
                }
            }
        }
        Ok(())
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
