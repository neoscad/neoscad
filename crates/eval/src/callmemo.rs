//! Reusing repeated user module calls within one evaluation ([`CallMemo`]).
//!
//! Recursive and patterned models instantiate the same module with the same
//! arguments many times: BOSL2's `fractal_tree.scad` calls `tree()` 2,047
//! times with 11 distinct argument sets. A call whose inputs repeat
//! evaluates once; later calls replay its node subtree (renumbered) and its
//! messages (in order), as [`crate::memo`] does for top-level statements
//! across edits. This memo lives for one evaluation and holds nothing after
//! it.
//!
//! **What a call can observe**, and how each is covered:
//!
//! - its definition and the name it was called by: in the key;
//! - its bound frame after argument binding (parameters with defaults
//!   evaluated, `$` arguments, `$children`, `$parent_modules`): every value
//!   digested into the key. `$parent_modules` is the absolute stack depth,
//!   so calls at different depths never share an entry;
//! - lexical names outside the body: the definition's context. Only
//!   modules defined at the top level of a file are memoised. The main
//!   file's context is one object whose variables are fixed before any
//!   statement runs, so it is keyed by identity; a `use`d library's
//!   context is made afresh at each lookup (its assignments can read `$`
//!   variables of the moment), so its variables are digested into the key;
//! - `$` variables bound outside the call (dynamic scope, builtins' `$fn`
//!   included): every such read is noted while the call first runs (in
//!   `Evaluator::lookup_special`), and an entry replays only when each of
//!   those names has the same value (digest) from the new call site. The
//!   frames below a call cannot change while it runs, so the first read of
//!   a name is its only value. Reads made by a nested call's replay are
//!   noted the same way, since checking its entry reads them. A name read
//!   only as `$v = $v * e` is keyed on its shape instead ([`Dep::Shape`]:
//!   BOSL2's `$transform`, different under every transform, would
//!   otherwise make every call distinct);
//! - `$`-named functions and modules found outside the call, and
//!   `parent_module(n)` returning a caller's name: not keyed; a call that
//!   does either is not kept. (`parent_module(n)` past the bottom of the
//!   stack depends only on the depth, which is in the key);
//! - `children()`: only calls without children are memoised, so it yields
//!   nothing and depends on nothing outside;
//! - anything [`Evaluator::untracked`] reports (`rands()`, file reads,
//!   `import()`, `surface()`, `part()`, deprecation messages, font
//!   metrics), an error message, a passed limit, or a message flood: the
//!   call is not kept, and neither is any call around it;
//! - the recursion limit: an entry replays only where the native stack
//!   used and the frame count are no more than where it was recorded. A
//!   fresh evaluation from there would take the same path, so each of its
//!   recursion checks would see no more than the recording's did, and
//!   those all passed;
//! - the memory limit: the call's peak estimate above its start is kept,
//!   and under a memory limit an entry replays only with twice that (and
//!   its nodes, and a margin) to spare, since a fresh evaluation's
//!   allocations can differ a little from the recorded ones (a list
//!   updated in place when the caller does not share it).
//!
//! Where any of that is in doubt the call evaluates as usual, which is
//! always correct. `--hardwarnings` turns the memo off.
//!
//! **Cost control.** Keying costs a digest of the bound arguments per call,
//! so frames and `$` values over [`MAX_KEY_VALUES`] are not keyed, and a
//! module stops being keyed once it has shown it does not repeat (see
//! [`DefStats::disabled`]). Recording costs a copy of the call's nodes and
//! a check on every `$` read while it runs, so a key's first call is only
//! noted and its second recorded ([`CallMemo::can_record`]), and a call is
//! kept only when it took at least [`MIN_WORK`] steps. Entries are bounded
//! by [`BUDGET`] estimated bytes and [`MAX_PER_KEY`] per key; past them
//! nothing more is kept. All of these depend only on the evaluation's own
//! order of events, so reuse is deterministic.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use sha2::{Digest as _, Sha256};

use crate::context::{Ctx, CtxKind};
use crate::memo::{Digest, Recorded, digest, entry_bytes, value_digest};
use crate::node::Node;
use crate::sym::{FxBuild, Sym};
use crate::value::Value;

/// Steps (statement instantiations and user function calls) a call must
/// take to be kept: replaying a cheaper call saves less than keying it.
const MIN_WORK: u64 = 32;

/// Estimated bytes of kept entries per evaluation (the measure of
/// [`crate::memo`]'s entries).
const BUDGET: usize = 64 << 20;

/// Entries kept per key (same call and arguments, different outside `$`
/// values); a lookup checks each in turn.
const MAX_PER_KEY: usize = 8;

/// Values a key or a dependency may hold, counted by [`small`]: digesting
/// a call's arguments costs their size on every call, and a module handed a
/// big VNF (`vnf_polyhedron`) would pay milliseconds a call for a key that
/// rarely repeats.
const MAX_KEY_VALUES: usize = 4096;

/// Messages a kept call may print (as [`crate::memo`]).
const MAX_MESSAGES: usize = 10_000;

/// A module definition: unit, scope and index.
type DefId = (u32, u32, u32);

/// A matrix's rows and columns (see [`Dep::Shape`]).
type MatrixShape = (u32, u32);

#[derive(Default)]
pub(crate) struct CallMemo {
    pub on: bool,
    /// The main file's context, which modules defined in it close over.
    pub main_file: Option<Rc<Ctx>>,
    table: HashMap<Digest, Vec<Entry>, FxBuild>,
    /// Keys called at least once (see [`CallMemo::can_record`]).
    seen: std::collections::HashSet<Digest, FxBuild>,
    defs: HashMap<DefId, DefStats, FxBuild>,
    bytes: usize,
    /// Bumped by [`crate::eval::Evaluator::untracked`].
    pub impure: u64,
    /// Messages printed while any call is being recorded.
    pub log: Vec<Recorded>,
    /// The calls being recorded, innermost last.
    recs: RefCell<Vec<Rec>>,
    /// Whether `recs` is non-empty, for the hot paths' one test.
    pub active: Cell<bool>,
    /// The `$` name whose next read is the left operand of `$v = $v * e`
    /// (see [`Dep::Shape`]).
    armed: Cell<Option<Sym>>,
    /// Bumped as each recording starts: the clock of [`Stamp`].
    epoch: Cell<u64>,
    /// Per `$` name, by symbol number (see [`Stamp`]).
    stamps: RefCell<Vec<Stamp>>,
    /// The names `opaque` has seen, to list those a recording read.
    opaque_names: RefCell<Vec<Sym>>,
    pub stats: CallStats,
}

/// What the memo knows of one `$` name, so a read costs no search.
#[derive(Clone, Copy, Default)]
struct Stamp {
    /// The epoch of its last read other than as [`CallMemo::armed`] (0:
    /// never): a recording that started at or before it read it so.
    opaque: u64,
    /// The epoch of the recording whose dependencies last listed it.
    dep: u64,
}

/// How often the memo was used, for tests and tuning.
#[derive(Default, Debug, Clone, Copy, PartialEq, Eq)]
pub struct CallStats {
    /// Calls looked up.
    pub lookups: u64,
    /// Calls replayed.
    pub hits: u64,
    /// Calls recorded (kept or not).
    pub recorded: u64,
    /// Calls recorded and kept.
    pub kept: u64,
}

#[derive(Default)]
struct DefStats {
    lookups: u32,
    hits: u32,
    cheap: u32,
    refused: u32,
}

impl DefStats {
    /// A module that has not repeated after this much is no longer keyed:
    /// every lookup costs a digest of its arguments.
    fn disabled(&self) -> bool {
        self.hits == 0 && (self.lookups >= 64 || self.cheap >= 16 || self.refused >= 8)
    }
}

/// How an entry depends on a `$` name bound outside the call.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Dep {
    /// On its value: the digest (`None`: unbound).
    Value(Option<Digest>),
    /// Only on its shape: a matrix of numbers, this many rows and columns.
    ///
    /// BOSL2 tracks the current transformation in `$transform` by
    /// redefining `translate`, `rotate`, `scale` and `multmatrix` as
    /// modules that do `$transform = $transform * m; ... children();`, so
    /// every call below a transform reads a different `$transform`, and
    /// keying on its value would make no two calls alike. But a read as
    /// the left operand of `$v = $v * e` (the same name on both sides)
    /// only computes the next `$v`: with a numeric matrix on the left, the
    /// product's shape, and every warning `*` can print, depend on the
    /// left operand's shape alone (`ops::mul`), never on its numbers. The
    /// value can reach the call's output only through a later read of
    /// the name in any other way, inside the call or out; so when a
    /// recording read a name only like this, its entry keys on the shape.
    Shape(u32, u32),
}

struct Entry {
    /// `$` names read from outside the call, and how it depends on them.
    deps: Vec<(Sym, Dep)>,
    /// The `$` names it read other than as [`Dep::Shape`] allows, for the
    /// calls around a replay to see.
    opaque: Vec<Sym>,
    node: Node,
    first_index: usize,
    indices: usize,
    ticks: u32,
    work: u64,
    messages: Vec<Recorded>,
    /// The native stack used and the frames held where it was recorded.
    stack: usize,
    frames: u32,
    /// Its peak memory estimate above where it started.
    rel_live: u64,
}

/// A call being recorded.
struct Rec {
    key: Digest,
    def: DefId,
    /// The stack index of the call's own context: a `$` read resolved
    /// below it (or not at all) is outside the call.
    base: usize,
    /// The index of the call's own name in `module_names`.
    module_base: usize,
    /// `$` names read from outside, with where they were found (stack
    /// index plus one; zero for unbound), and the value's shape if the
    /// first read was as [`Dep::Shape`] allows.
    deps: Vec<(Sym, usize, Option<MatrixShape>)>,
    /// The epoch this recording started at.
    epoch: u64,
    /// The lowest stack index at which something unkeyable was read.
    unkeyable: usize,
    /// The lowest `module_names` index `parent_module()` read.
    module_read: usize,
    log_start: usize,
    impure: u64,
    first_index: usize,
    ticks: u32,
    work: u64,
    live: u64,
    saved_high: u64,
    stack: usize,
    frames: u32,
}

/// What to do with a call.
enum Plan {
    Plain,
    Record(Digest, DefId),
    Replay(Digest, usize),
}

/// A call's start, as measured where it is looked up and recorded.
pub(crate) struct Start {
    pub base: usize,
    pub module_base: usize,
    pub stack: usize,
    pub frames: u32,
    pub first_index: usize,
    pub ticks: u32,
    pub work: u64,
}

impl CallMemo {
    pub fn new(on: bool) -> CallMemo {
        CallMemo {
            on,
            ..CallMemo::default()
        }
    }

    // --- the hot paths' hooks ---------------------------------------------

    /// A `$` variable was read, found at stack index `found` (`None`:
    /// unbound).
    #[cold]
    #[inline(never)]
    pub fn read(&self, s: Sym, found: Option<usize>, v: Option<&Value>) {
        let shape = if self.armed.take() == Some(s) {
            v.and_then(matrix_shape)
        } else {
            None
        };
        let mut stamps = self.stamps.borrow_mut();
        let st = stamp(&mut stamps, s);
        if shape.is_none() {
            if st.opaque == 0 {
                self.opaque_names.borrow_mut().push(s);
            }
            st.opaque = self.epoch.get();
        }
        let pos = found.map_or(0, |j| j + 1);
        let mut recs = self.recs.borrow_mut();
        let r = recs.last_mut().expect("active");
        // A name a nested recording passed up is listed without a stamp,
        // so the search runs once per name and recording.
        if pos <= r.base && st.dep != r.epoch {
            if !r.deps.iter().any(|&(t, _, _)| t == s) {
                r.deps.push((s, pos, shape));
            }
            st.dep = r.epoch;
        }
    }

    /// `s` was read in a way that can pass its value on.
    pub fn note_opaque(&self, s: Sym) {
        let mut stamps = self.stamps.borrow_mut();
        let st = stamp(&mut stamps, s);
        if st.opaque == 0 {
            self.opaque_names.borrow_mut().push(s);
        }
        st.opaque = self.epoch.get();
    }

    /// Whether the innermost recording read `s` other than as
    /// [`Dep::Shape`] allows.
    pub fn opaque_in_innermost(&self, s: Sym) -> bool {
        let Some(epoch) = self.recs.borrow().last().map(|r| r.epoch) else {
            return true;
        };
        self.stamps
            .borrow()
            .get(s.0 as usize)
            .is_some_and(|st| st.opaque >= epoch)
    }

    /// The next read of `s` is the left operand of `$v = $v * e`.
    pub fn arm(&self, s: Sym) {
        self.armed.set(Some(s));
    }

    pub fn disarm(&self) {
        self.armed.set(None);
    }

    /// Something that cannot be keyed was read at stack index `at` (a `$`
    /// function or module; 0 when searched for and not found).
    pub fn unkeyable(&self, at: usize) {
        if let Some(r) = self.recs.borrow_mut().last_mut() {
            r.unkeyable = r.unkeyable.min(at);
        }
    }

    /// `parent_module()` read `module_names[at]`. (One that looked past
    /// the bottom of the stack found only how deep the call is, which
    /// `$parent_modules` puts in the key, and reads nothing here.)
    pub fn module_read(&self, at: usize) {
        if let Some(r) = self.recs.borrow_mut().last_mut() {
            r.module_read = r.module_read.min(at);
        }
    }

    // --- lookup -------------------------------------------------------------

    fn def(&mut self, d: DefId) -> &mut DefStats {
        self.defs.entry(d).or_default()
    }

    pub fn def_disabled(&self, d: DefId) -> bool {
        self.defs.get(&d).is_some_and(DefStats::disabled)
    }

    /// The key of a call: its definition, name, definition context and
    /// bound frame; `None` when one of them cannot be digested.
    pub fn key(&self, def: DefId, name: Sym, dctx: &Rc<Ctx>, mctx: &Ctx) -> Option<Digest> {
        let mut h = Sha256::new();
        h.update(b"call");
        for x in [def.0, def.1, def.2, name.0] {
            h.update(x.to_le_bytes());
        }
        let mut budget = MAX_KEY_VALUES;
        if self.main_file.as_ref().is_some_and(|m| Rc::ptr_eq(m, dctx)) {
            h.update([0]);
        } else if let CtxKind::File(sr) = &dctx.kind {
            if !frame_small(dctx, &mut budget) {
                return None;
            }
            h.update([1]);
            h.update(sr.unit.to_le_bytes());
            h.update(dctx.region.to_le_bytes());
            frame_digest(dctx, &mut h)?;
        } else {
            return None;
        }
        if !frame_small(mctx, &mut budget) {
            return None;
        }
        h.update(mctx.region.to_le_bytes());
        frame_digest(mctx, &mut h)?;
        Some(digest(h))
    }

    /// Candidates for a key, to check in order.
    pub fn candidates(&mut self, key: &Digest, def: DefId) -> Vec<Vec<(Sym, Dep)>> {
        self.stats.lookups += 1;
        self.def(def).lookups += 1;
        self.table
            .get(key)
            .map(|v| v.iter().map(|e| e.deps.clone()).collect())
            .unwrap_or_default()
    }

    /// Whether to record a call with this key that no entry matched. A
    /// key is recorded from its second call on: recording costs a copy of
    /// the call's nodes and a check on every `$` read while it runs, which
    /// a call made once (a model's one big `path_sweep`) would pay for
    /// nothing.
    pub fn can_record(&mut self, key: &Digest) -> bool {
        if self.seen.insert(*key) {
            return false;
        }
        self.bytes < BUDGET && self.table.get(key).is_none_or(|v| v.len() < MAX_PER_KEY)
    }

    /// What an entry needs to replay: the stack and frames it was
    /// recorded at, its memory peak and its node count.
    pub fn needs(&self, key: &Digest, i: usize) -> (usize, u32, u64, usize) {
        let e = &self.table[key][i];
        (e.stack, e.frames, e.rel_live, e.indices)
    }

    /// An entry's result, with what replaying it consumes.
    pub fn take_replay(&mut self, key: &Digest, i: usize, def: DefId) -> Replay {
        self.stats.hits += 1;
        self.def(def).hits += 1;
        let e = &self.table[key][i];
        Replay {
            node: e.node.clone(),
            first_index: e.first_index,
            indices: e.indices,
            ticks: e.ticks,
            work: e.work,
            messages: e.messages.clone(),
            opaque: e.opaque.clone(),
            rel_live: e.rel_live,
        }
    }

    // --- recording ----------------------------------------------------------

    pub fn begin(&mut self, key: Digest, def: DefId, st: &Start, live: u64, saved_high: u64) {
        self.stats.recorded += 1;
        let epoch = self.epoch.get() + 1;
        self.epoch.set(epoch);
        self.recs.get_mut().push(Rec {
            key,
            def,
            base: st.base,
            module_base: st.module_base,
            deps: Vec::new(),
            epoch,
            unkeyable: usize::MAX,
            module_read: usize::MAX,
            log_start: self.log.len(),
            impure: self.impure,
            first_index: st.first_index,
            ticks: st.ticks,
            work: st.work,
            live,
            saved_high,
            stack: st.stack,
            frames: st.frames,
        });
        self.active.set(true);
    }

    /// Whether the innermost recording is of the call whose context sits
    /// at stack index `base`.
    #[inline]
    pub fn recording_at(&self, base: usize) -> bool {
        self.active.get() && self.recs.borrow().last().is_some_and(|r| r.base == base)
    }

    /// The `$` names the innermost recording read from outside, in order:
    /// what [`CallMemo::end`] needs the digests of.
    pub fn deps(&self) -> Vec<(Sym, Option<MatrixShape>)> {
        let recs = self.recs.borrow();
        recs.last()
            .map(|r| r.deps.iter().map(|&(s, _, shape)| (s, shape)).collect())
            .unwrap_or_default()
    }

    /// End the innermost recording, keeping its result when `node` is the
    /// call's (it succeeded) and nothing refused it. `counters` are the
    /// node counter, the limits' tick counter and the work counter now;
    /// `high` the live-memory high-water mark; `digests` the values of
    /// [`CallMemo::deps`] at the call's site (`Err` for a function).
    /// Returns the high-water mark to restore for the calls around it.
    pub fn end(
        &mut self,
        node: Option<&Node>,
        counters: (usize, u32, u64),
        high: u64,
        clean: bool,
        digests: Vec<Result<Dep, ()>>,
    ) -> u64 {
        let rec = self.recs.get_mut().pop().expect("recording");
        let opaque: Vec<Sym> = {
            let stamps = self.stamps.borrow();
            let names = self.opaque_names.borrow();
            names
                .iter()
                .copied()
                .filter(|s| stamps[s.0 as usize].opaque >= rec.epoch)
                .collect()
        };
        let outer = self.recs.get_mut().last_mut();
        // What the call read from outside is read by its callers too (from
        // their point of view, outside or not).
        if let Some(o) = outer {
            for &(s, pos, shape) in &rec.deps {
                if pos <= o.base && !o.deps.iter().any(|&(t, _, _)| t == s) {
                    o.deps.push((s, pos, shape));
                }
            }
            o.unkeyable = o.unkeyable.min(rec.unkeyable);
            o.module_read = o.module_read.min(rec.module_read);
        } else {
            self.active.set(false);
        }
        let log = &self.log[rec.log_start..];
        let refused = !clean
            || rec.unkeyable < rec.base
            || rec.module_read < rec.module_base
            || self.impure != rec.impure
            || log.len() > MAX_MESSAGES
            || log
                .iter()
                .any(|m| m.diag.severity == lang::diag::Severity::Error);
        let messages = if node.is_some() && !refused {
            log.to_vec()
        } else {
            Vec::new()
        };
        let (counter, ticks, work) = counters;
        let work = work - rec.work;
        let result = (|| {
            let node = node?;
            if refused {
                self.def(rec.def).refused += 1;
                return None;
            }
            if work < MIN_WORK {
                self.def(rec.def).cheap += 1;
                return None;
            }
            let mut deps = Vec::with_capacity(rec.deps.len());
            for (&(s, _, _), d) in rec.deps.iter().zip(&digests) {
                match d {
                    Ok(d) => deps.push((s, *d)),
                    Err(()) => {
                        self.def(rec.def).refused += 1;
                        return None;
                    }
                }
            }
            let bytes = entry_bytes(Some(node), &messages) + deps.len() * 32;
            if self.bytes + bytes > BUDGET {
                return None;
            }
            Some((
                Entry {
                    deps,
                    opaque,
                    node: node.clone(),
                    first_index: rec.first_index,
                    indices: counter - rec.first_index,
                    ticks: ticks.wrapping_sub(rec.ticks),
                    work,
                    messages,
                    stack: rec.stack,
                    frames: rec.frames,
                    rel_live: high.saturating_sub(rec.live),
                },
                bytes,
            ))
        })();
        if let Some((entry, bytes)) = result {
            self.bytes += bytes;
            self.stats.kept += 1;
            let v = self.table.entry(rec.key).or_default();
            if v.len() < MAX_PER_KEY {
                v.push(entry);
            }
        }
        if !self.active.get() {
            self.log.clear();
        }
        rec.saved_high.max(high)
    }
}

/// A kept call's result to put in place.
pub(crate) struct Replay {
    pub node: Node,
    pub first_index: usize,
    pub indices: usize,
    pub ticks: u32,
    pub work: u64,
    pub messages: Vec<Recorded>,
    pub opaque: Vec<Sym>,
    pub rel_live: u64,
}

/// Digest a context's variables: each set slot with its number (a slot
/// vector not yet sized and one of unset slots digest alike), then each
/// named variable in order. `None` for a function value, which a digest
/// cannot identify.
fn frame_digest(c: &Ctx, h: &mut Sha256) -> Option<()> {
    for (i, v) in c.slots.borrow().iter().enumerate() {
        if let Some(v) = v {
            h.update([1]);
            h.update((i as u32).to_le_bytes());
            if !value_digest(v, h) {
                return None;
            }
        }
    }
    h.update([0]);
    for (s, v) in c.vars.borrow().iter() {
        h.update([2]);
        h.update(s.0.to_le_bytes());
        if !value_digest(v, h) {
            return None;
        }
    }
    h.update([0]);
    Some(())
}

/// A value's shape if it is a matrix of numbers (at least one row, all of
/// the same non-zero length): see [`Dep::Shape`].
pub(crate) fn matrix_shape(v: &Value) -> Option<MatrixShape> {
    let rows = v.as_vector()?;
    let cols = rows.first()?.as_vector()?.len();
    if cols == 0 {
        return None;
    }
    for r in rows.iter() {
        let r = r.as_vector()?;
        if r.len() != cols || !r.iter().all(|x| matches!(x, Value::Number(_))) {
            return None;
        }
    }
    Some((u32::try_from(rows.len()).ok()?, u32::try_from(cols).ok()?))
}

/// Whether a context's variables hold at most `budget` values (see
/// [`small`]), taking what they hold from it.
fn frame_small(c: &Ctx, budget: &mut usize) -> bool {
    c.slots.borrow().iter().flatten().all(|v| small(v, budget))
        && c.vars.borrow().iter().all(|(_, v)| small(v, budget))
}

/// Whether `v` is at most `budget` values (a list counts itself and its
/// elements, a string one per 8 bytes, an object 256), taking its size
/// from the budget. A shared sublist counts each time it appears, and the
/// walk stops as soon as the budget runs out, so it costs at most the
/// budget.
fn small(v: &Value, budget: &mut usize) -> bool {
    let mut todo = vec![v];
    while let Some(v) = todo.pop() {
        let n = match v {
            Value::Vector(items) => {
                if items.len() >= *budget {
                    return false;
                }
                todo.extend(items.iter());
                1
            }
            Value::Str(s) => 1 + s.as_bytes().len() / 8,
            Value::Object(_) => 256,
            _ => 1,
        };
        let Some(left) = budget.checked_sub(n) else {
            return false;
        };
        *budget = left;
    }
    true
}

/// A `$` value's digest for an entry's dependencies; `Err` for a function
/// or a value too big to digest on every call (see [`MAX_KEY_VALUES`]).
pub(crate) fn dep_value_digest(v: Option<&Value>) -> Result<Option<Digest>, ()> {
    match v {
        None => Ok(None),
        Some(v) => {
            let mut budget = MAX_KEY_VALUES;
            if !small(v, &mut budget) {
                return Err(());
            }
            let mut h = Sha256::new();
            if value_digest(v, &mut h) {
                Ok(Some(digest(h)))
            } else {
                Err(())
            }
        }
    }
}

/// The stamp of `s`, growing the table to it.
fn stamp(stamps: &mut Vec<Stamp>, s: Sym) -> &mut Stamp {
    let i = s.0 as usize;
    if i >= stamps.len() {
        stamps.resize(i + 1, Stamp::default());
    }
    &mut stamps[i]
}

/// Move a replayed subtree's node indices by `shift`.
fn renumber(n: &mut Node, shift: i64) {
    let mut stack = vec![n];
    while let Some(n) = stack.pop() {
        n.index = (n.index as i64 + shift) as usize;
        stack.extend(n.children.iter_mut());
    }
}

impl crate::eval::Evaluator<'_> {
    /// Plan a call of module `def` bound in `mctx` (not yet on the stack),
    /// at instantiation `at`: its node (boxed, to keep the caller's frame
    /// small) if it was replayed, or `None` to evaluate it, recording it
    /// if [`CallMemo::recording_at`] says so.
    #[inline(never)]
    pub(crate) fn call_enter(
        &mut self,
        def: DefId,
        dctx: &Rc<Ctx>,
        mctx: &Ctx,
        at: (crate::context::ScopeRef, usize),
    ) -> Option<Box<Node>> {
        // Measured here for recording and replay alike, so the two are
        // comparable (see `replay_fits`).
        let stack = self.stack_used();
        let name = self.module_names[self.module_names.len() - 1];
        match self.call_plan(def, name, dctx, mctx, stack) {
            Plan::Plain => None,
            Plan::Replay(key, k) => Some(Box::new(self.call_replay(&key, k, def, at))),
            Plan::Record(key, def) => {
                self.call_begin(key, def, stack);
                None
            }
        }
    }

    /// Whether and how to reuse a call of module `def` by `name`, bound in
    /// `mctx` and not yet on the stack; `stack` is the native stack used
    /// where the call is recorded and replayed.
    #[inline(never)]
    fn call_plan(
        &mut self,
        def: DefId,
        name: Sym,
        dctx: &Rc<Ctx>,
        mctx: &Ctx,
        stack: usize,
    ) -> Plan {
        if self.cm.def_disabled(def) {
            return Plan::Plain;
        }
        let Some(key) = self.cm.key(def, name, dctx, mctx) else {
            self.cm.def(def).refused += 1;
            return Plan::Plain;
        };
        let candidates = self.cm.candidates(&key, def);
        for (k, deps) in candidates.iter().enumerate() {
            // Reading the names through the noting lookup makes them the
            // dependencies of any call being recorded around this one,
            // exactly as running this call would.
            let same = deps.iter().all(|&(s, d)| match d {
                Dep::Value(d) => {
                    dep_value_digest(self.lookup_special(s).as_ref()).is_ok_and(|x| x == d)
                }
                // Read as the entry's call reads it, so a recording around
                // this call can key on the shape too.
                Dep::Shape(r, c) => {
                    self.cm.arm(s);
                    let v = self.lookup_special(s);
                    self.cm.disarm();
                    v.as_ref().and_then(matrix_shape) == Some((r, c))
                }
            });
            if same {
                return if self.replay_fits(&key, k, stack) {
                    Plan::Replay(key, k)
                } else {
                    Plan::Plain
                };
            }
        }
        if self.cm.can_record(&key) {
            Plan::Record(key, def)
        } else {
            Plan::Plain
        }
    }

    /// Whether a fresh evaluation of entry `k` would stay under the
    /// recursion and memory limits from here: otherwise it runs, and meets
    /// the limit where it would have.
    fn replay_fits(&self, key: &Digest, k: usize, stack: usize) -> bool {
        let (at_stack, at_frames, rel_live, indices) = self.cm.needs(key, k);
        if stack > at_stack
            || self.frames > at_frames
            || self.limit_passed()
            || crate::limits::live::over()
        {
            return false;
        }
        let memory = self.opts.guard.as_deref().and_then(|g| g.limits().memory);
        memory.is_none_or(|max| {
            let need = self
                .live_bytes_now()
                .saturating_add(rel_live.saturating_mul(2))
                .saturating_add(indices as u64 * crate::eval::NODE_BYTES)
                .saturating_add(1 << 20);
            need <= max
        })
    }

    /// Start recording a call whose context goes on the stack next.
    fn call_begin(&mut self, key: Digest, def: DefId, stack: usize) {
        let live = crate::limits::live::get();
        let saved_high = crate::limits::live::high();
        crate::limits::live::set_high(live);
        let st = Start {
            base: self.stack.len(),
            module_base: self.module_names.len() - 1,
            stack,
            frames: self.frames,
            first_index: self.node_counter(),
            ticks: self.ticks(),
            work: self.work,
        };
        self.cm.begin(key, def, &st, live, saved_high);
    }

    /// Finish recording the innermost call, with its result.
    #[inline(never)]
    pub(crate) fn call_end(&mut self, node: Option<&Node>) {
        let digests = if node.is_some() {
            self.cm
                .deps()
                .into_iter()
                .map(|(s, shape)| {
                    let v = self.lookup_special_quiet(s);
                    match shape {
                        Some(sh)
                            if !self.cm.opaque_in_innermost(s)
                                && v.as_ref().and_then(matrix_shape) == Some(sh) =>
                        {
                            Ok(Dep::Shape(sh.0, sh.1))
                        }
                        _ => dep_value_digest(v.as_ref()).map(Dep::Value),
                    }
                })
                .collect()
        } else {
            Vec::new()
        };
        let clean = !self.limit_passed();
        let counters = (self.node_counter(), self.ticks(), self.work);
        let high = crate::limits::live::high();
        let restore = self.cm.end(node, counters, high, clean, digests);
        crate::limits::live::set_high(restore);
    }

    /// Put entry `k`'s result where the call at instantiation `i` of `sr`
    /// would have: renumbered from the node counter, with the call's own
    /// origin, its messages printed again in order, and what it consumed
    /// (node indices, limit checks, work and the memory peak) counted.
    #[inline(never)]
    fn call_replay(
        &mut self,
        key: &Digest,
        k: usize,
        def: DefId,
        (sr, i): (crate::context::ScopeRef, usize),
    ) -> Node {
        let rp = self.cm.take_replay(key, k, def);
        let mut node = rp.node;
        renumber(
            &mut node,
            self.node_counter() as i64 - rp.first_index as i64,
        );
        node.origin = Some(self.origin(sr, i));
        self.advance(rp.indices, rp.ticks);
        self.work += rp.work;
        let peak = crate::limits::live::get().saturating_add(rp.rel_live);
        if peak > crate::limits::live::high() {
            crate::limits::live::set_high(peak);
        }
        self.track_peak(rp.rel_live);
        if self.cm.active.get() {
            for &s in &rp.opaque {
                self.cm.note_opaque(s);
            }
        }
        for m in rp.messages {
            self.replay_recorded(m);
        }
        node
    }
}
