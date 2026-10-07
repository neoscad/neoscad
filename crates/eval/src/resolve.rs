//! Static name resolution: which binding each name can refer to, worked
//! out once per evaluation instead of on every lookup.
//!
//! Resolution is lazy, one definition at a time: a unit's top level when
//! its file context is first made, a function's body (and its parameter
//! defaults) at its first call, a module's body at its first
//! instantiation. A model that includes BOSL2 runs a small part of it, and
//! resolving all of it up front cost more than an edit-loop evaluation
//! gains. Everything a definition's body contains (statement scopes,
//! `let`s, literals) is resolved with it.
//!
//! Every construct that makes a context at run time (a file or statement
//! scope, a module or function body, a function literal, a `let`, each
//! `for` variable, a C-style `for`'s initial and iteration frames) is a
//! [`Region`], and the context carries the region's id. A region numbers
//! the ordinary (non-`$`) names it binds, and its contexts keep their
//! values in a vector indexed by that number ([`crate::context::Ctx`]).
//!
//! A variable reference then resolves to a short list of candidates: the
//! enclosing regions that can bind the name, innermost first, each with its
//! slot ([`Cand`]). A lookup walks the context chain and looks only in
//! contexts of a candidate region, by index. The walk is still a walk, and
//! that is deliberate: it keeps OpenSCAD's run-time rules without having
//! to prove statically which context is where:
//!
//! - a slot that is not set yet (a scope's assignment still ahead of the
//!   one being evaluated, a `let` binding that refers to one after it)
//!   sends the lookup further out, as an absent name does in OpenSCAD;
//! - contexts that bind nothing (a call's argument frame) or that a region
//!   does not get at run time (`intersection_for` redefined as a user
//!   module has no loop frames) are skipped by an integer compare;
//! - a C-style `for` has two contexts of its iteration region in the chain
//!   while its increment is evaluated, and the nearer one that has the
//!   name set wins.
//!
//! What static resolution cannot see is handled where it arises:
//!
//! - `$` names are dynamically scoped and keep the stack walk
//!   (`Evaluator::lookup_special`); they get no slots.
//! - A named argument that is not a parameter still binds in the callee's
//!   frame (`variable "x" not specified as parameter`). Such names can be
//!   passed to any function or module, so a reference to a name that some
//!   call passes by name also looks in each enclosing function or module
//!   frame's name map ([`Cand::Extra`]).
//! - An expression evaluated without having been resolved (none are known)
//!   falls back to the by-name walk; [`Stats::fallbacks`] counts those
//!   lookups.

use std::collections::HashMap;

use lang::ast::{Arg, ExprId, ExprKind, Param};

use crate::builtins::functions::Builtin;
use crate::builtins::modules::BuiltinModule;
use crate::eval::Unit;
use crate::sym::{FxBuild, Sym, Syms};

/// The region of contexts that bind nothing: call argument frames and
/// builtin modules' parameter frames.
pub(crate) const NONE_REGION: u32 = 0;
/// The builtin context at the bottom of every chain: `PI` is its one
/// ordinary variable, the rest are `$` variables.
pub(crate) const BUILTIN_REGION: u32 = 1;
/// A binder with no slot: a `$` name (kept in the context's name map) or
/// an unnamed `let` argument.
pub(crate) const NO_SLOT: u32 = u32::MAX;

/// Name maps of more names than this also get a hash index.
const INDEX_THRESHOLD: usize = 12;

/// A kind of context: the names it binds, by slot.
#[derive(Debug, Default)]
pub(crate) struct Region {
    /// The name in each slot.
    pub names: Box<[Sym]>,
    index: Option<HashMap<Sym, u32, FxBuild>>,
    /// The slot each binder writes, in the order the evaluator binds them:
    /// a `let`'s arguments, a function's parameters, a scope's assignments,
    /// or a module's parameters then its assignments then `$children`.
    pub binds: Box<[u32]>,
    /// Whether call arguments bind here (function and module bodies), so a
    /// named argument that is not a parameter can bind any name.
    pub params: bool,
    /// Whether this region's variables live in registers rather than in a
    /// context (see [`Region::reg`] and the module docs, "Registers").
    reg: bool,
}

impl Region {
    /// Whether the evaluator keeps this region's variables in registers
    /// (`Evaluator::regs`) instead of a context: an expression `let`, a
    /// comprehension `for` variable, or a function body (a *pure frame*,
    /// when the call allows it) that nothing inside can see as a context.
    #[inline]
    pub fn reg(&self) -> bool {
        self.reg
    }

    /// The slot of `s`, if this region binds it.
    pub fn slot_of(&self, s: Sym) -> Option<u32> {
        match &self.index {
            Some(m) => m.get(&s).copied(),
            None => self.names.iter().position(|&n| n == s).map(|i| i as u32),
        }
    }

    pub fn len(&self) -> usize {
        self.names.len()
    }
}

/// Builds a region's name list, one slot per distinct ordinary name.
struct RegionBuilder<'s> {
    syms: &'s Syms,
    names: Vec<Sym>,
    binds: Vec<u32>,
    /// Built once `names` passes [`INDEX_THRESHOLD`]: a file scope binds
    /// thousands of names.
    index: Option<HashMap<Sym, u32, FxBuild>>,
}

impl<'s> RegionBuilder<'s> {
    fn new(syms: &'s Syms) -> Self {
        RegionBuilder {
            syms,
            names: Vec::new(),
            binds: Vec::new(),
            index: None,
        }
    }

    /// Add a binder of `s` (`None`: unnamed), returning its slot.
    fn bind(&mut self, s: Option<Sym>) -> u32 {
        let slot = match s {
            Some(s) if !self.syms.is_config(s) => {
                let found = match &self.index {
                    Some(m) => m.get(&s).copied(),
                    None => self.names.iter().position(|&n| n == s).map(|i| i as u32),
                };
                match found {
                    Some(i) => i,
                    None => {
                        let i = self.names.len() as u32;
                        self.names.push(s);
                        if let Some(m) = &mut self.index {
                            m.insert(s, i);
                        } else if self.names.len() > INDEX_THRESHOLD {
                            self.index = Some(
                                self.names
                                    .iter()
                                    .enumerate()
                                    .map(|(i, &n)| (n, i as u32))
                                    .collect(),
                            );
                        }
                        i
                    }
                }
            }
            _ => NO_SLOT,
        };
        self.binds.push(slot);
        slot
    }

    fn finish(self, params: bool) -> Region {
        Region {
            names: self.names.into(),
            index: self.index,
            binds: self.binds.into(),
            params,
            reg: false,
        }
    }
}

/// Where a name may be bound, as seen from one reference.
///
/// `repr(C, u32)` puts `region` at the same offset in every variant, so the
/// compare on the lookup's hot path is one load.
#[derive(Debug, Clone, Copy)]
#[repr(C, u32)]
pub(crate) enum Cand {
    /// The variable in this slot of a context of `region`, when set.
    Slot { region: u32, slot: u32 },
    /// [`Cand::Slot`] of a region whose variables live in registers
    /// ([`Region::reg`]): register `slot` of the region's live instance
    /// (`Evaluator::reg_base`), or, for a function body bound as a context
    /// (a call that could not have a pure frame), the slot of that context
    /// as for `Slot`. Registers are always the innermost candidates, so a
    /// lookup tries them first and then walks the chain.
    Reg { region: u32, slot: u32 },
    /// A named argument that is not a parameter, in the name map of a
    /// function or module body context of `region`.
    Extra { region: u32 },
    /// Function or module `index` of scope `scope` (of the referring unit),
    /// defined in the scope a context of `region` is an instance of.
    Def { region: u32, scope: u32, index: u32 },
    /// Function or module `index` of used library `lib`, found from the
    /// file context of `region`.
    Use { region: u32, lib: u32, index: u32 },
}

impl Cand {
    #[inline]
    pub fn region(&self) -> u32 {
        match *self {
            Cand::Slot { region, .. }
            | Cand::Reg { region, .. }
            | Cand::Extra { region }
            | Cand::Def { region, .. }
            | Cand::Use { region, .. } => region,
        }
    }
}

/// A resolved reference: its candidates in the unit's pool.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Ref {
    pub start: u32,
    pub len: u32,
}

/// A variable reference's candidates, packed: the first in the low half,
/// their count plus one in the high half, so zero is a reference not
/// resolved. A plain integer, so the per-expression table is allocated
/// zeroed (a large program's table costs no writes until resolved).
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct VarRef(u64);

impl VarRef {
    fn new(r: Ref) -> VarRef {
        VarRef(u64::from(r.start) | (u64::from(r.len) + 1) << 32)
    }

    #[inline]
    pub fn cands(self) -> Option<Ref> {
        let n = (self.0 >> 32) as u32;
        (n != 0).then(|| Ref {
            start: self.0 as u32,
            len: n - 1,
        })
    }
}

/// A resolved function call name: candidates, then the builtin.
#[derive(Debug, Clone, Copy)]
pub(crate) struct FnRef {
    pub cands: Ref,
    pub builtin: Option<Builtin>,
}

/// A resolved module instantiation name.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ModRef {
    pub cands: Ref,
    pub builtin: Option<BuiltinModule>,
}

/// One unit's resolution.
#[derive(Debug, Default)]
pub(crate) struct UnitRes {
    /// Per expression: for a call of a name, its [`FnRef`] in `fns` plus
    /// one; for a `let`, function literal or comprehension `for`, its
    /// (first) region. Zero: not resolved, look the name up dynamically.
    pub expr: Vec<u32>,
    /// Per expression, for a variable: its candidates, stored here rather
    /// than behind an index because variables are read far more than
    /// anything else is looked up.
    var: Vec<u64>,
    /// Per scope (as numbered in `Unit::scopes`): its region.
    pub scope_region: Vec<u32>,
    /// Per scope, per function definition: the body's region.
    pub fn_region: Vec<Box<[u32]>>,
    /// Per scope, per instantiation: its [`ModRef`] plus one, and the first
    /// region of its bindings (a `for`, `intersection_for` or `let`), or 0.
    pub inst: Vec<Box<[(u32, u32)]>>,
    pub fns: Vec<FnRef>,
    pub mods: Vec<ModRef>,
    pub cands: Vec<Cand>,
    /// Whether the top-level scope has been resolved.
    pub root: bool,
    /// The resolver's scope chains, kept for the definitions resolved
    /// later.
    envs: Vec<Env>,
    /// Per scope, per function (and module) definition: the environment of
    /// the scope defining it, recorded when that scope is resolved.
    fn_env: Vec<Box<[u32]>>,
    mod_env: Vec<Box<[u32]>>,
    /// Per call with an accumulator-shaped argument (`concat(acc, ...)`,
    /// `[each acc, ...]`; see `Evaluator::move_accumulators`): the region
    /// of the innermost scope around the call. A non-tail call's move test
    /// looks at the context the call is evaluated in, and when that scope
    /// is a register region the tree-walker's context there would have
    /// been one the test always rejects (see `Evaluator::entry_moves`).
    pub acc_env: HashMap<u32, u32, FxBuild>,
    pub stats: Stats,
}

impl UnitRes {
    /// An unresolved unit's tables, sized.
    pub fn new(unit: &Unit<'_>) -> UnitRes {
        let n = unit.scopes.len();
        UnitRes {
            expr: vec![0; unit.ast.exprs.len()],
            var: vec![0; unit.ast.exprs.len()],
            scope_region: vec![0; n],
            fn_region: vec![Box::default(); n],
            inst: vec![Box::default(); n],
            fn_env: vec![Box::default(); n],
            mod_env: vec![Box::default(); n],
            ..UnitRes::default()
        }
    }

    /// Variable reference `id`'s resolution.
    #[inline]
    pub fn var(&self, id: ExprId) -> VarRef {
        VarRef(self.var[id.0 as usize])
    }

    #[inline]
    pub fn cands(&self, r: Ref) -> &[Cand] {
        &self.cands[r.start as usize..(r.start + r.len) as usize]
    }
}

/// Counts over one evaluation.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Stats {
    /// Variable, function and module name references resolved (in the
    /// definitions that ran).
    pub references: usize,
    /// Of those, references to `$` names, which stay dynamic by definition.
    pub special: usize,
    /// Lookups of other names that found no resolution and walked the
    /// scope chain by name.
    pub fallbacks: usize,
}

impl Stats {
    pub fn add(&mut self, o: &Stats) {
        self.references += o.references;
        self.special += o.special;
        self.fallbacks += o.fallbacks;
    }
}

/// A link of the resolver's scope chain.
#[derive(Debug, Clone, Copy)]
struct Env {
    region: u32,
    /// The statement scope whose functions and modules are visible here.
    scope: Option<u32>,
    file: bool,
    parent: Option<u32>,
    /// The boundary of a sketch body (`--enable sketch`): a link that binds
    /// nothing at run time, between the body and the scope around the
    /// `sketch()` call, where the sketch vocabulary is bound. A name looked
    /// up from inside the body finds what the body itself binds first (a
    /// function literal in one of its variables), then the vocabulary, and
    /// never a user's or library's `arc` or `chamfer` outside:
    /// `docs/language-extensions.md`, section 4.1.
    vocab: bool,
}

/// Everything the resolver reads besides the unit it resolves.
pub(crate) struct Tables<'t, 'a> {
    pub units: &'t [Unit<'a>],
    pub syms: &'t Syms,
    pub builtin_fns: &'t HashMap<Sym, Builtin, FxBuild>,
    pub builtin_mods: &'t HashMap<Sym, BuiltinModule, FxBuild>,
    /// The sketch vocabulary, bound inside sketch bodies only (see
    /// [`Env::vocab`]); empty without `--enable sketch`.
    pub vocab_fns: &'t HashMap<Sym, Builtin, FxBuild>,
    pub vocab_mods: &'t HashMap<Sym, BuiltinModule, FxBuild>,
    /// Names some call or instantiation passes as a named argument,
    /// collected at the first question: resolving a program's top level
    /// seldom asks, and a model that calls no function never pays for the
    /// scan of every expression.
    pub extras: &'t std::cell::OnceCell<NameSet>,
    pub empty: Sym,
    pub children: Sym,
}

/// A set of names, as a flag per symbol: a lookup is an index.
#[derive(Debug, Default)]
pub(crate) struct NameSet(Vec<bool>);

impl NameSet {
    pub fn contains(&self, s: Sym) -> bool {
        self.0.get(s.0 as usize).copied().unwrap_or(false)
    }

    fn insert(&mut self, s: Sym) {
        let i = s.0 as usize;
        if i >= self.0.len() {
            self.0.resize(i + 1, false);
        }
        self.0[i] = true;
    }
}

/// The names any call passes by name, over all units (see [`Cand::Extra`]).
pub(crate) fn named_arguments(units: &[Unit<'_>], syms: &Syms) -> NameSet {
    let mut out = NameSet::default();
    for unit in units {
        let mut add = |args: &[Arg]| {
            for a in args {
                if let Some(n) = a.name {
                    let s = unit.sym(n);
                    if !syms.is_config(s) {
                        out.insert(s);
                    }
                }
            }
        };
        for e in &unit.ast.exprs {
            if let ExprKind::Call(_, args) = &e.kind {
                add(args);
            }
        }
        for info in &unit.scopes {
            for inst in &info.scope.instantiations {
                add(&inst.args);
            }
        }
    }
    out
}

struct Resolver<'r, 't, 'a> {
    t: &'r Tables<'t, 'a>,
    unit: &'t Unit<'a>,
    regions: &'r mut Vec<Region>,
    res: &'r mut UnitRes,
    exprs: Vec<(ExprId, u32)>,
    scopes: Vec<(u32, u32)>,
    /// The first candidate this run adds: [`Resolver::run`] turns the
    /// slots of register regions among them into [`Cand::Reg`] once every
    /// region of the definition is known.
    cand_start: usize,
}

impl<'r, 't, 'a> Resolver<'r, 't, 'a> {
    fn new(
        t: &'r Tables<'t, 'a>,
        u: u32,
        regions: &'r mut Vec<Region>,
        res: &'r mut UnitRes,
    ) -> Self {
        let cand_start = res.cands.len();
        Resolver {
            t,
            unit: &t.units[u as usize],
            regions,
            res,
            exprs: Vec::new(),
            scopes: Vec::new(),
            cand_start,
        }
    }
}

/// Resolve unit `u`'s top level (its file scope, without the bodies of its
/// functions and modules), once.
pub(crate) fn resolve_root(
    t: &Tables<'_, '_>,
    u: u32,
    regions: &mut Vec<Region>,
    res: &mut UnitRes,
) {
    if res.root {
        return;
    }
    res.root = true;
    let mut r = Resolver::new(t, u, regions, res);
    let builtin = r.env(BUILTIN_REGION, None, false, None);
    let file = r.scope_region(0, None);
    let env = r.env(file, Some(0), true, Some(builtin));
    r.scopes.push((0, env));
    r.run();
}

/// Resolve function `index` of scope `scope` (resolved already): its
/// parameter defaults and body. Returns the body's region.
pub(crate) fn resolve_function(
    t: &Tables<'_, '_>,
    u: u32,
    scope: u32,
    index: u32,
    regions: &mut Vec<Region>,
    res: &mut UnitRes,
) -> u32 {
    let mut r = Resolver::new(t, u, regions, res);
    let env = r.res.fn_env[scope as usize][index as usize];
    let f = &r.unit.scopes[scope as usize].scope.functions[index as usize];
    // Defaults are evaluated in the defining context
    // (`Parameters::parse`), so they see neither the parameters before
    // them nor the body.
    for p in &f.params {
        if let Some(d) = p.default {
            r.exprs.push((d, env));
        }
    }
    let region = r.params_region(&f.params);
    // A body can have a pure frame unless a parameter is a `$` name, which
    // every call binds in the frame's name map (see `Region::reg`).
    r.regions[region as usize].reg = !r.regions[region as usize].binds.contains(&NO_SLOT);
    r.res.fn_region[scope as usize][index as usize] = region;
    let body_env = r.env(region, None, false, Some(env));
    r.exprs.push((f.body, body_env));
    r.run();
    region
}

/// Resolve module `index` of scope `scope` (resolved already): its
/// parameter defaults and body. Returns the body's region.
pub(crate) fn resolve_module(
    t: &Tables<'_, '_>,
    u: u32,
    scope: u32,
    index: u32,
    regions: &mut Vec<Region>,
    res: &mut UnitRes,
) -> u32 {
    let mut r = Resolver::new(t, u, regions, res);
    let env = r.res.mod_env[scope as usize][index as usize];
    let info = &r.unit.scopes[scope as usize];
    let m = &info.scope.modules[index as usize];
    for p in &m.params {
        if let Some(d) = p.default {
            r.exprs.push((d, env));
        }
    }
    let body = info.bodies[index as usize];
    let region = r.scope_region(body, Some(&m.params));
    let body_env = r.env(region, Some(body), false, Some(env));
    r.scopes.push((body, body_env));
    r.run();
    region
}

impl<'r, 't, 'a> Resolver<'r, 't, 'a> {
    fn env(&mut self, region: u32, scope: Option<u32>, file: bool, parent: Option<u32>) -> u32 {
        self.res.envs.push(Env {
            region,
            scope,
            file,
            parent,
            vocab: false,
        });
        self.res.envs.len() as u32 - 1
    }

    /// The vocabulary boundary around a sketch body ([`Env::vocab`]). Its
    /// region binds nothing, so no candidate ever names it.
    fn vocab_env(&mut self, parent: u32) -> u32 {
        self.res.envs.push(Env {
            region: NONE_REGION,
            scope: None,
            file: false,
            parent: Some(parent),
            vocab: true,
        });
        self.res.envs.len() as u32 - 1
    }

    /// Whether module reference `m` (as [`Self::module_ref`] returns it) is
    /// the builtin `sketch`: no definition anywhere can take the name, so
    /// the lookup can only end at the builtin. A program's own
    /// `module sketch` (roof.scad has one) is a candidate and wins.
    fn is_builtin_sketch(&self, m: u32) -> bool {
        m != 0 && {
            let r = self.res.mods[m as usize - 1];
            r.cands.len == 0 && r.builtin == Some(BuiltinModule::Sketch)
        }
    }

    fn add_region(&mut self, b: RegionBuilder<'_>, params: bool) -> u32 {
        self.regions.push(b.finish(params));
        self.regions.len() as u32 - 1
    }

    fn sym(&self, n: lang::ast::Name) -> Sym {
        self.unit.sym(n)
    }

    /// The region of statement scope `sid`: its assignments, after a
    /// module's parameters when it is a module body.
    fn scope_region(&mut self, sid: u32, params: Option<&[Param]>) -> u32 {
        let scope = self.unit.scopes[sid as usize].scope;
        let mut b = RegionBuilder::new(self.t.syms);
        if let Some(ps) = params {
            for p in ps {
                b.bind(Some(self.sym(p.name)));
            }
        }
        for a in &scope.assignments {
            b.bind(Some(self.sym(a.name)));
        }
        if params.is_some() {
            b.bind(Some(self.t.children));
        }
        let r = self.add_region(b, params.is_some());
        self.res.scope_region[sid as usize] = r;
        r
    }

    /// A region binding `args` in order (a `let`).
    fn args_region(&mut self, args: &[Arg]) -> u32 {
        let mut b = RegionBuilder::new(self.t.syms);
        for a in args {
            b.bind(a.name.map(|n| self.sym(n)));
        }
        self.add_region(b, false)
    }

    /// One region per `for` variable, with consecutive ids; returns the
    /// first and the environment inside all of them. Argument `k` is
    /// evaluated inside the first `k`.
    fn for_regions(&mut self, args: &[Arg], mut env: u32) -> (u32, u32) {
        let first = self.regions.len() as u32;
        for a in args {
            self.exprs.push((a.expr, env));
            let mut b = RegionBuilder::new(self.t.syms);
            b.bind(Some(a.name.map_or(self.t.empty, |n| self.sym(n))));
            let r = self.add_region(b, false);
            env = self.env(r, None, false, Some(env));
        }
        (first, env)
    }

    fn params_region(&mut self, params: &[Param]) -> u32 {
        let mut b = RegionBuilder::new(self.t.syms);
        for p in params {
            b.bind(Some(self.sym(p.name)));
        }
        self.add_region(b, true)
    }

    fn run(&mut self) {
        loop {
            if let Some((e, env)) = self.exprs.pop() {
                self.expr(e, env);
            } else if let Some((sid, env)) = self.scopes.pop() {
                self.scope(sid, env);
            } else {
                break;
            }
        }
        // Only now is every region of the definition final: a trigger
        // (see `materialize`) can come after a reference in the worklist.
        // Earlier runs' candidates never name this run's regions, and this
        // run never materializes theirs (a definition's register regions
        // are all inside its own body), so theirs stay right.
        for c in &mut self.res.cands[self.cand_start..] {
            if let Cand::Slot { region, slot } = *c
                && self.regions[region as usize].reg
            {
                *c = Cand::Reg { region, slot };
            }
        }
    }

    /// A register region (`Region::reg`) whose contexts something needs,
    /// with every register region around it: a function literal captures
    /// its context chain, a `$` binding is looked up through the context
    /// stack, and a C-style `for` evaluates by name (`lc_for_c`). The
    /// regions around must follow so that a context's parent chain has no
    /// register region in it: lookups try registers first and then walk
    /// the chain, which is only the tree-walker's order when registers
    /// are always the innermost bindings.
    ///
    /// The walk stops at the first region that is not a register one:
    /// everything outside it is not one either, because it was either
    /// materialized by the same rule or is a statement scope, and no
    /// expression region encloses a statement scope.
    fn materialize(&mut self, mut env: u32) {
        loop {
            let e = self.res.envs[env as usize];
            let r = &mut self.regions[e.region as usize];
            if !r.reg {
                return;
            }
            r.reg = false;
            match e.parent {
                Some(p) => env = p,
                None => return,
            }
        }
    }

    /// Whether `e` is an argument [`Evaluator::move_accumulators`] may move
    /// from (`call::accumulator`'s shape; being wrong only costs a map
    /// entry).
    fn accumulator_shaped(&self, e: ExprId) -> bool {
        let ast = self.unit.ast;
        match &ast.expr(e).kind {
            ExprKind::Call(callee, args) => {
                matches!(ast.expr(*callee).kind, ExprKind::Var(n) if ast.name(n) == "concat")
                    && args.first().is_some_and(|a| {
                        a.name.is_none() && matches!(ast.expr(a.expr).kind, ExprKind::Var(_))
                    })
            }
            ExprKind::Vector(items) => items.first().is_some_and(|&i| {
                matches!(ast.expr(i).kind, ExprKind::LcEach(x)
                    if matches!(ast.expr(x).kind, ExprKind::Var(_)))
            }),
            _ => false,
        }
    }

    /// The statements of scope `sid`, whose own environment is `env`.
    fn scope(&mut self, sid: u32, env: u32) {
        let info = &self.unit.scopes[sid as usize];
        let scope = info.scope;
        for a in &scope.assignments {
            self.exprs.push((a.expr, env));
            // Customizer annotations are never evaluated; resolving them
            // keeps the fallback count about expressions that are.
            self.exprs
                .extend(a.annotations.iter().map(|n| (n.expr, env)));
        }
        // Function and module bodies wait for their first use.
        self.res.fn_region[sid as usize] = vec![0; scope.functions.len()].into();
        self.res.fn_env[sid as usize] = vec![env; scope.functions.len()].into();
        self.res.mod_env[sid as usize] = vec![env; scope.modules.len()].into();
        let mut insts = Vec::with_capacity(scope.instantiations.len());
        for (j, inst) in scope.instantiations.iter().enumerate() {
            let name = self.sym(inst.name);
            let m = self.module_ref(name, env);
            // Only the builtin `for`, `intersection_for` and `let` bind
            // their arguments around their children; if a user module
            // takes the name, their frames are simply never made.
            let (first, child_env) = match self.unit.ast.name(inst.name) {
                "for" | "intersection_for" => self.for_regions(&inst.args, env),
                "let" => {
                    let r = self.args_region(&inst.args);
                    let e = self.env(r, None, false, Some(env));
                    for a in &inst.args {
                        self.exprs.push((a.expr, e));
                    }
                    (r, e)
                }
                _ => {
                    for a in &inst.args {
                        self.exprs.push((a.expr, env));
                    }
                    (0, env)
                }
            };
            insts.push((m, first));
            let children = info.children[j];
            let r = self.scope_region(children, None);
            let child_env = if self.is_builtin_sketch(m) {
                self.vocab_env(child_env)
            } else {
                child_env
            };
            let e = self.env(r, Some(children), false, Some(child_env));
            self.scopes.push((children, e));
            let els = info.else_children[j];
            if els != u32::MAX {
                let r = self.scope_region(els, None);
                let e = self.env(r, Some(els), false, Some(env));
                self.scopes.push((els, e));
            }
        }
        self.res.inst[sid as usize] = insts.into();
    }

    fn expr(&mut self, id: ExprId, env: u32) {
        let ast = self.unit.ast;
        let push_args = |exprs: &mut Vec<(ExprId, u32)>, args: &[Arg], env: u32| {
            exprs.extend(args.iter().map(|a| (a.expr, env)));
        };
        match &ast.expr(id).kind {
            ExprKind::Var(n) => {
                let s = self.sym(*n);
                self.res.stats.references += 1;
                if self.t.syms.is_config(s) {
                    self.res.stats.special += 1;
                } else {
                    let r = self.var_ref(s, env);
                    self.res.var[id.0 as usize] = VarRef::new(r).0;
                }
            }
            ExprKind::Undef
            | ExprKind::Bool(_)
            | ExprKind::Number(_)
            | ExprKind::String(_)
            | ExprKind::Invalid => {}
            ExprKind::Unary(_, x) | ExprKind::Member(x, _) | ExprKind::LcEach(x) => {
                self.exprs.push((*x, env));
            }
            ExprKind::Binary(_, a, b) | ExprKind::Index(a, b) => {
                self.exprs.extend([(*a, env), (*b, env)]);
            }
            ExprKind::Ternary(a, b, c) => self.exprs.extend([(*a, env), (*b, env), (*c, env)]),
            ExprKind::LcIf(a, b, c) => {
                self.exprs.extend([(*a, env), (*b, env)]);
                self.exprs.extend(c.map(|c| (c, env)));
            }
            ExprKind::Range { begin, step, end } => {
                self.exprs.extend([(*begin, env), (*end, env)]);
                self.exprs.extend(step.map(|s| (s, env)));
            }
            ExprKind::Vector(items) => self.exprs.extend(items.iter().map(|&e| (e, env))),
            ExprKind::Call(callee, args) => {
                match ast.expr(*callee).kind {
                    ExprKind::Var(n) => {
                        let s = self.sym(n);
                        self.res.stats.references += 1;
                        if self.t.syms.is_config(s) {
                            self.res.stats.special += 1;
                        } else {
                            let r = self.function_ref(s, env);
                            self.res.fns.push(r);
                            self.res.expr[id.0 as usize] = self.res.fns.len() as u32;
                        }
                    }
                    _ => self.exprs.push((*callee, env)),
                }
                if args.iter().any(|a| self.accumulator_shaped(a.expr)) {
                    let region = self.res.envs[env as usize].region;
                    self.res.acc_env.insert(id.0, region);
                }
                push_args(&mut self.exprs, args, env);
            }
            ExprKind::Function(params, body) => {
                // The literal captures the context it is made in.
                self.materialize(env);
                for p in params {
                    if let Some(d) = p.default {
                        self.exprs.push((d, env));
                    }
                }
                let r = self.params_region(params);
                self.regions[r as usize].reg = !self.regions[r as usize].binds.contains(&NO_SLOT);
                self.res.expr[id.0 as usize] = r;
                let e = self.env(r, None, false, Some(env));
                self.exprs.push((*body, e));
            }
            ExprKind::Let(args, body) | ExprKind::LcLet(args, body) => {
                let r = self.args_region(args);
                if self.binds_special(args) {
                    self.materialize(env);
                } else {
                    self.regions[r as usize].reg = true;
                }
                self.res.expr[id.0 as usize] = r;
                let e = self.env(r, None, false, Some(env));
                push_args(&mut self.exprs, args, e);
                self.exprs.push((*body, e));
            }
            ExprKind::LcFor(args, body) => {
                let (first, e) = self.for_regions(args, env);
                // Variable `k` binds in region `first + k`, inside the ones
                // before it: a `$` variable keeps its own region and those
                // outside it as contexts.
                let special = args.iter().rposition(|a| {
                    a.name
                        .is_some_and(|n| self.t.syms.is_config(self.unit.sym(n)))
                });
                if special.is_some() {
                    self.materialize(env);
                }
                for k in 0..args.len() {
                    self.regions[first as usize + k].reg = special.is_none_or(|j| k > j);
                }
                self.res.expr[id.0 as usize] = first;
                self.exprs.push((*body, e));
            }
            ExprKind::LcForC {
                init,
                cond,
                incr,
                body,
            } => {
                // Evaluated by name, in contexts (see `lc_for_c`).
                self.materialize(env);
                let first = self.args_region(init);
                let next = self.args_region(incr);
                debug_assert_eq!(next, first + 1);
                self.res.expr[id.0 as usize] = first;
                let e_init = self.env(first, None, false, Some(env));
                let e = self.env(next, None, false, Some(e_init));
                push_args(&mut self.exprs, init, e_init);
                push_args(&mut self.exprs, incr, e);
                self.exprs.extend([(*cond, e), (*body, e)]);
            }
            ExprKind::Assert(args, body) | ExprKind::Echo(args, body) => {
                push_args(&mut self.exprs, args, env);
                self.exprs.extend(body.map(|b| (b, env)));
            }
        }
    }

    /// Whether a `let` binds a `$` name, which lives in its context's name
    /// map for the dynamic lookup to find.
    fn binds_special(&self, args: &[Arg]) -> bool {
        args.iter().any(|a| {
            a.name
                .is_some_and(|n| self.t.syms.is_config(self.unit.sym(n)))
        })
    }

    /// The variable candidates of `s` in `e`'s region, pushed.
    fn var_cands(&mut self, s: Sym, e: Env) {
        let region = &self.regions[e.region as usize];
        if let Some(slot) = region.slot_of(s) {
            self.res.cands.push(Cand::Slot {
                region: e.region,
                slot,
            });
        } else if region.params
            && self
                .t
                .extras
                .get_or_init(|| named_arguments(self.t.units, self.t.syms))
                .contains(s)
        {
            self.res.cands.push(Cand::Extra { region: e.region });
        }
    }

    fn start(&self) -> u32 {
        self.res.cands.len() as u32
    }

    fn finish(&self, start: u32) -> Ref {
        Ref {
            start,
            len: self.start() - start,
        }
    }

    fn var_ref(&mut self, s: Sym, env: u32) -> Ref {
        let start = self.start();
        let mut cur = Some(env);
        while let Some(i) = cur {
            let e = self.res.envs[i as usize];
            self.var_cands(s, e);
            cur = e.parent;
        }
        self.finish(start)
    }

    /// `lookup_local_function` of each context, outward: a scope's
    /// functions, then a variable holding a function literal, then (at a
    /// file) the used libraries; the builtins are tried at the builtin
    /// context.
    fn function_ref(&mut self, s: Sym, env: u32) -> FnRef {
        let start = self.start();
        let mut cur = Some(env);
        while let Some(i) = cur {
            let e = self.res.envs[i as usize];
            cur = e.parent;
            if e.vocab
                && let Some(&b) = self.t.vocab_fns.get(&s)
            {
                return FnRef {
                    cands: self.finish(start),
                    builtin: Some(b),
                };
            }
            if let Some(sid) = e.scope
                && let Some(&index) = self.unit.scopes[sid as usize].functions.get(&s)
            {
                self.res.cands.push(Cand::Def {
                    region: e.region,
                    scope: sid,
                    index,
                });
            }
            self.var_cands(s, e);
            if e.file
                && let Some((lib, index)) = self.unit.uses.iter().find_map(|&lib| {
                    let f = self.t.units[lib as usize].scopes[0].functions.get(&s)?;
                    Some((lib, *f))
                })
            {
                self.res.cands.push(Cand::Use {
                    region: e.region,
                    lib,
                    index,
                });
            }
        }
        FnRef {
            cands: self.finish(start),
            builtin: self.t.builtin_fns.get(&s).copied(),
        }
    }

    fn module_ref(&mut self, s: Sym, env: u32) -> u32 {
        self.res.stats.references += 1;
        if self.t.syms.is_config(s) {
            self.res.stats.special += 1;
            return 0;
        }
        let start = self.start();
        let mut cur = Some(env);
        let mut builtin = self.t.builtin_mods.get(&s).copied();
        while let Some(i) = cur {
            let e = self.res.envs[i as usize];
            cur = e.parent;
            if e.vocab
                && let Some(&b) = self.t.vocab_mods.get(&s)
            {
                builtin = Some(b);
                break;
            }
            if let Some(sid) = e.scope
                && let Some(&index) = self.unit.scopes[sid as usize].modules.get(&s)
            {
                self.res.cands.push(Cand::Def {
                    region: e.region,
                    scope: sid,
                    index,
                });
            }
            if e.file
                && let Some((lib, index)) = self.unit.uses.iter().find_map(|&lib| {
                    let m = self.t.units[lib as usize].scopes[0].modules.get(&s)?;
                    Some((lib, *m))
                })
            {
                self.res.cands.push(Cand::Use {
                    region: e.region,
                    lib,
                    index,
                });
            }
        }
        let r = ModRef {
            cands: self.finish(start),
            builtin,
        };
        self.res.mods.push(r);
        self.res.mods.len() as u32
    }
}

/// The region after `r` of a construct with consecutive regions (the
/// variables of a `for`, a C-style `for`'s iteration after its start),
/// keeping an unresolved construct's [`NONE_REGION`].
#[inline]
pub(crate) fn next_region(r: u32) -> u32 {
    if r == NONE_REGION { r } else { r + 1 }
}

/// The builtin context's region: `PI`.
pub(crate) fn builtin_region(pi: Sym) -> Region {
    Region {
        names: Box::new([pi]),
        index: None,
        binds: Box::new([0]),
        params: false,
        reg: false,
    }
}
