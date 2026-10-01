//! Reusing top-level statements' evaluation across edits ([`Memo`]).
//!
//! A long-lived host evaluates the same document again after every edit,
//! and most edits touch one statement. OpenSCAD evaluates a file's
//! top-level assignments first (all of them: they are hoisted, and the last
//! assignment to a name wins), then instantiates its top-level statements in
//! order. Each statement's result (its node subtree and the messages it
//! printed) is a function of a small set of inputs, so a statement whose
//! inputs are unchanged can replay its last result instead of running.
//!
//! **The fingerprint.** A statement's inputs are, conservatively:
//!
//! - its own syntax: the structure of its AST and the exact source text it
//!   spans, with the path of its file (messages print the path);
//! - every name it mentions, followed transitively through the top-level
//!   functions and modules of the main program that those names can reach
//!   (by name, over all three namespaces, so a local binder that happens to
//!   share a top-level name only makes the key larger). For each such name:
//!   the value of the top-level variable, and the syntax, text and path of
//!   the top-level function and module definitions;
//! - the value of every top-level `$` variable, since any code can read
//!   those dynamically, builtins included (`$fn`, `$fa`, `$fs`);
//! - the evaluation's options and `main_dir`, and the full text of every
//!   `use`d library ([`global_key`]): a library cannot see the main file's
//!   ordinary names, so its content and the `$` values cover what it reads.
//!
//! Top-level assignments are always evaluated anew (they are cheap next to
//! the statements, and their messages come first), so the values above are
//! this evaluation's. Evaluation is otherwise a deterministic function of
//! the syntax it runs and the values it reads, so equal fingerprints mean
//! equal results, up to where they sit in the file.
//!
//! **Positions.** Edits above a statement move it without changing it. The
//! fingerprint therefore leaves out absolute positions; instead an entry
//! keeps each input's source range (an *anchor*: the statement, and each
//! definition it reached) at recording time, and a replay moves every span
//! of the main program in the recorded nodes and messages by its anchor's
//! offset, and every line by its anchor's line offset. The anchor's text is
//! part of the fingerprint, so everything inside it is at the same relative
//! offset and line. A recording with any main-program span outside its
//! anchors is not kept, since it could not be moved correctly. Node indices
//! (the counter every node takes one from) are renumbered by the same
//! offset, and the counter advances by what the statement consumed.
//!
//! **What always runs** (a statement that did any of these is not kept):
//! `rands()` (seeded or not: seeding resets the shared generator later
//! statements draw from), reading files (`dxf_dim`, `dxf_cross`), `import()`
//! and `surface()`, deprecation messages (each prints once per evaluation,
//! so whether one prints depends on earlier statements), `part()`
//! (duplicate names are checked across the file), an error or passed
//! resource limit (evaluation stops there), function values among the
//! variables it reads (a function literal's behaviour is its code and
//! captured scope, which a value hash cannot see), and syntax whose spans
//! leave its file (an `include` inside a module body). With
//! `--hardwarnings` nothing is reused. Under a memory limit, an entry is
//! only replayed when its recorded peak fits under the limit from where
//! this evaluation stands, so a replay cannot hide a limit a full
//! evaluation would pass.
//!
//! **Memory.** A memo holds a copy of each kept statement's nodes and
//! messages, per setting, within a byte budget ([`Memo::with_budget`]); a
//! statement that prints more than [`MAX_RECORDED`] messages, or whose
//! result alone would pass the budget, is evaluated every time.
//!
//! `parent_module()` and `$parent_modules` are deterministic here and do
//! not stop reuse: they read the stack of user modules being instantiated,
//! which is empty when a top-level statement starts, so what they return is
//! decided by the statement's own calls. (BOSL2 calls `parent_module(1)` in
//! almost every module, through `no_children()`.)
//!
//! Not part of the identity: [`crate::Evaluation::resolution`] counts
//! only the definitions that ran, so it is smaller when statements replay.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::path::Path;
use std::rc::Rc;

use lang::Program;
use lang::ast::{Arg, Ast, ExprId, ExprKind, InstKind, Instantiation, Name, Param, Scope};
use lang::diag::Diagnostic;
use lang::source::{FileId, Span};
use sha2::{Digest as _, Sha256};

use crate::context::{Ctx, ScopeRef};
use crate::eval::Evaluator;
use crate::message::{Message, R, Unwind, UnwindKind};
use crate::node::Node;
use crate::sym::{FxBuild, Sym};
use crate::value::{Value, Vector};
use crate::{Library, Options};

/// 128 bits of SHA-256: a collision would show a stale model, so the key
/// is a cryptographic hash rather than a fast one.
pub(crate) type Digest = [u8; 16];

pub(crate) fn digest(h: Sha256) -> Digest {
    let d = h.finalize();
    let mut out = [0; 16];
    out.copy_from_slice(&d[..16]);
    out
}

/// Evaluation settings memos are kept apart by (preview and render differ
/// in `$preview`, so a host alternating them keeps both warm).
const SLOTS: usize = 4;

/// Entries kept at most per setting: past this a memo starts again, so a
/// run of failed evaluations cannot grow it without bound.
const MAX_ENTRIES: usize = 8192;

/// A memo's default budget: estimated bytes of recorded results (see
/// [`Memo::with_budget`]).
pub const MEMO_BUDGET: usize = 128 << 20;

/// Estimated bytes of one node, as the evaluator's memory estimate counts
/// them (its origin, parameters and slot included).
const NODE_BYTES: usize = 512;

/// What a host keeps between evaluations of one document to reuse
/// top-level statements' results (see the module documentation and
/// [`crate::evaluate_incremental`]). It holds node trees and messages, no
/// evaluator values, so it can move between threads.
pub struct Memo {
    /// Most recently used last.
    slots: Vec<Slot>,
    /// Top-level definitions' syntax from the last evaluation (see
    /// [`Known`]), whatever the settings.
    known: HashMap<DefKey, Known>,
    /// Estimated bytes of entries it keeps at most.
    budget: usize,
}

impl Default for Memo {
    fn default() -> Memo {
        Memo::with_budget(MEMO_BUDGET)
    }
}

/// A top-level definition by kind (module or not), file and span.
type DefKey = (bool, std::path::PathBuf, u32, u32);

/// A top-level definition's digest and names, kept between evaluations.
///
/// Hashing the syntax of every definition a statement can reach is most of
/// what fingerprinting costs: a BOSL2 model reaches hundreds, and they
/// rarely change. So a definition's result is kept with its exact source
/// text, and reused when the definition is at the same place in the same
/// file with byte-identical text. That is sound because a top-level
/// definition's syntax tree is a function of its own tokens (it is kept
/// only when every span of its tree lies inside its own span), and a byte
/// comparison is far cheaper than hashing again.
struct Known {
    text: Box<[u8]>,
    digest: Digest,
    names: Vec<Box<str>>,
    /// The anchor's range: the definition's spans, which lie inside its
    /// own span.
    lo: u32,
    hi: u32,
}

struct Slot {
    key: Digest,
    entries: HashMap<Digest, Entry>,
}

impl Slot {
    fn bytes(&self) -> usize {
        self.entries.values().map(|e| e.bytes).sum()
    }
}

impl fmt::Debug for Memo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Memo")
            .field("slots", &self.slots.len())
            .field("entries", &self.len())
            .finish()
    }
}

impl Memo {
    pub fn new() -> Memo {
        Memo::default()
    }

    /// A memo keeping about `budget` bytes of recorded results at most.
    /// A statement whose result would pass it is evaluated each time, and
    /// other settings' results are dropped first. It holds a copy of the
    /// node tree per setting, so a host keeping one per document bounds
    /// what a very large model can pin.
    pub fn with_budget(budget: usize) -> Memo {
        Memo {
            slots: Vec::new(),
            known: HashMap::new(),
            budget,
        }
    }

    /// Estimated bytes of the results it keeps, and of the definitions'
    /// text it compares against.
    pub fn bytes(&self) -> usize {
        let known: usize = self.known.values().map(|k| k.text.len() + 64).sum();
        self.slots.iter().map(Slot::bytes).sum::<usize>() + known
    }

    /// Recorded statements, over all settings.
    pub fn len(&self) -> usize {
        self.slots.iter().map(|s| s.entries.len()).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn clear(&mut self) {
        self.slots.clear();
        self.known.clear();
    }

    /// The slot for `key`, made most recently used.
    fn slot(&mut self, key: Digest) -> usize {
        if let Some(i) = self.slots.iter().position(|s| s.key == key) {
            let s = self.slots.remove(i);
            self.slots.push(s);
        } else {
            if self.slots.len() >= SLOTS {
                self.slots.remove(0);
            }
            self.slots.push(Slot {
                key,
                entries: HashMap::new(),
            });
        }
        self.slots.len() - 1
    }
}

/// How an evaluation used its [`Memo`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ReuseStats {
    /// Top-level statements reached.
    pub statements: usize,
    /// Of those, replayed from the memo.
    pub reused: usize,
    /// Evaluated and recorded for next time.
    pub recorded: usize,
    /// Evaluated and not recorded (see the module documentation).
    pub unrecorded: usize,
}

/// A source range an entry's output is placed relative to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Anchor {
    file: FileId,
    start: u32,
    end: u32,
    line: u32,
}

/// One statement's recorded result.
struct Entry {
    node: Option<Node>,
    /// The node counter when it started, and how many it consumed.
    first_index: usize,
    indices: usize,
    messages: Vec<Recorded>,
    /// Its inputs' positions when it was recorded, in fingerprint order.
    anchors: Vec<Anchor>,
    /// Evaluator checks it made (the limits' sampling phase), and the most
    /// estimated memory it added (only tracked under a limit).
    ticks: u32,
    peak: u64,
    /// Estimated bytes it holds.
    bytes: usize,
}

/// Estimated bytes of a recorded result: its nodes (with the point lists
/// polyhedra and polygons carry) and its messages.
pub(crate) fn entry_bytes(node: Option<&Node>, messages: &[Recorded]) -> usize {
    let mut n = messages
        .iter()
        .map(|m| 2 * m.text.len() + 256)
        .sum::<usize>();
    let mut stack: Vec<&Node> = node.into_iter().collect();
    while let Some(x) = stack.pop() {
        n += NODE_BYTES;
        n += match &x.kind {
            crate::node::NodeKind::Polyhedron { points, faces, .. } => {
                points.len() * 24 + faces.iter().map(|f| 24 + f.len() * 8).sum::<usize>()
            }
            crate::node::NodeKind::Polygon { points, paths, .. } => {
                points.len() * 16 + paths.iter().map(|p| 24 + p.len() * 8).sum::<usize>()
            }
            _ => 0,
        };
        stack.extend(x.children.iter());
    }
    n
}

/// A message as the evaluator printed it.
#[derive(Clone)]
pub(crate) struct Recorded {
    /// The unit its location is in, if it has one.
    pub unit: Option<u32>,
    pub diag: Diagnostic,
    pub text: Vec<u8>,
}

/// What a statement being recorded did besides return its node.
pub(crate) struct Recording {
    pub messages: Vec<Recorded>,
    /// Something the fingerprint cannot see happened (see the module
    /// documentation), or it printed too much: don't keep the result.
    pub untracked: bool,
    /// The highest memory estimate seen (under a memory limit).
    pub peak: u64,
}

/// Messages a statement may print and still be kept. A loop printing a
/// warning per iteration can print millions, and a recording would hold a
/// second copy of each beside what the host keeps; such a statement is
/// evaluated every time instead.
const MAX_RECORDED: usize = 10_000;

impl Recording {
    pub fn record(&mut self, m: Recorded) {
        if self.messages.len() >= MAX_RECORDED {
            self.untrack();
        } else {
            self.messages.push(m);
        }
    }

    /// Give up on keeping this statement, and free what was recorded.
    pub fn untrack(&mut self) {
        self.untracked = true;
        self.messages = Vec::new();
    }
}

/// A digest of everything outside the main program's own statements and
/// definitions that evaluation depends on: the options, `main_dir`, which
/// libraries the main program uses, and each library's full text.
///
/// `rng_seed` is left out on purpose: only `rands()` reads it, and a
/// statement that calls `rands()` is never kept, so a host that seeds each
/// request differently still reuses everything else. `interrupt` and `fs`
/// are left out too: an interrupted statement is not kept, and the one
/// evaluator read of `fs` (`dxf_dim`) marks its statement untracked.
fn global_key(
    main: &Program,
    main_uses: &[String],
    libraries: &[Library<'_>],
    main_dir: &Path,
    o: &Options,
) -> Digest {
    let mut h = Sha256::new();
    let f = |h: &mut Sha256, x: f64| h.update(x.to_bits().to_le_bytes());
    let bytes = |h: &mut Sha256, b: &[u8]| {
        h.update((b.len() as u64).to_le_bytes());
        h.update(b);
    };
    h.update(b"neoscad-memo-1");
    h.update([
        u8::from(o.preview),
        u8::from(o.camera.auto),
        u8::from(o.camera.locked),
        u8::from(o.trace_usermodule_parameters),
        u8::from(o.check_parameters),
        u8::from(o.check_parameter_ranges),
        u8::from(o.parts),
    ]);
    h.update(o.features.bits().to_le_bytes());
    f(&mut h, o.time);
    for x in o.camera.vpt.iter().chain(&o.camera.vpr) {
        f(&mut h, *x);
    }
    f(&mut h, o.camera.vpd);
    f(&mut h, o.camera.vpf);
    h.update(o.trace_depth.to_le_bytes());
    h.update((o.stack_limit as u64).to_le_bytes());
    h.update(o.frame_limit.to_le_bytes());
    for x in o.version {
        f(&mut h, x);
    }
    let limits = format!("{:?}", o.guard.as_deref().map(|g| *g.limits()));
    bytes(&mut h, limits.as_bytes());
    bytes(&mut h, main_dir.as_os_str().as_encoded_bytes());
    // The main program's files are in the fingerprints (as anchors); its
    // own path decides where relative imports resolve.
    bytes(
        &mut h,
        main.sources.path(main.main).as_os_str().as_encoded_bytes(),
    );
    h.update((main_uses.len() as u64).to_le_bytes());
    for u in main_uses {
        bytes(&mut h, u.as_bytes());
    }
    h.update((libraries.len() as u64).to_le_bytes());
    for lib in libraries {
        bytes(&mut h, lib.path.as_bytes());
        h.update((lib.uses.len() as u64).to_le_bytes());
        for u in lib.uses {
            bytes(&mut h, u.as_bytes());
        }
        match lib.program {
            None => h.update([0]),
            Some(p) => {
                h.update([1, u8::from(p.has_syntax_errors())]);
                h.update((p.sources.len() as u64).to_le_bytes());
                for (_, file) in p.sources.iter() {
                    bytes(&mut h, file.path.as_os_str().as_encoded_bytes());
                    bytes(&mut h, &file.text);
                }
            }
        }
    }
    digest(h)
}

/// A statement's or definition's syntax, ready to go into fingerprints.
struct Anchored {
    digest: Digest,
    /// The names it mentions (variables, calls, instantiations), first
    /// mention first.
    names: Vec<Sym>,
    anchor: Anchor,
}

/// The evaluation's side of a [`Memo`].
pub(crate) struct MemoRun<'a> {
    memo: &'a mut Memo,
    slot: usize,
    /// Entries this evaluation replayed or recorded: what the memo keeps.
    used: HashMap<Digest, Entry>,
    /// Top-level definitions' syntax, by (is a module, index); `None` when
    /// one cannot be anchored.
    defs: HashMap<(bool, u32), Option<Rc<Anchored>>>,
    /// The [`Known`] definitions this evaluation used or made.
    known: HashMap<DefKey, Known>,
    /// Top-level variables' value digests; `None` for a value holding a
    /// function.
    vars: HashMap<Sym, Option<Option<Digest>>>,
    /// All top-level `$` variables, once computed.
    dollar: Option<Option<Digest>>,
    /// Estimated bytes of `used`.
    used_bytes: usize,
    pub stats: ReuseStats,
}

impl<'a> MemoRun<'a> {
    pub fn new(
        memo: &'a mut Memo,
        main: &Program,
        main_uses: &[String],
        libraries: &[Library<'_>],
        main_dir: &Path,
        o: &Options,
    ) -> MemoRun<'a> {
        let key = global_key(main, main_uses, libraries, main_dir, o);
        let slot = memo.slot(key);
        MemoRun {
            memo,
            slot,
            used: HashMap::new(),
            defs: HashMap::new(),
            known: HashMap::new(),
            vars: HashMap::new(),
            dollar: None,
            used_bytes: 0,
            stats: ReuseStats::default(),
        }
    }

    fn entry(&self, fp: &Digest) -> Option<&Entry> {
        self.used
            .get(fp)
            .or_else(|| self.memo.slots[self.slot].entries.get(fp))
    }

    /// Keep what this evaluation used. After a complete evaluation that is
    /// exactly the file's statements; after one that stopped early the
    /// statements it did not reach keep their old entries too.
    pub fn finish(self, complete: bool) -> ReuseStats {
        if complete {
            self.memo.known = self.known;
        } else {
            self.memo.known.extend(self.known);
        }
        let memo = self.memo;
        let slot = &mut memo.slots[self.slot];
        if complete {
            slot.entries = self.used;
        } else {
            slot.entries.extend(self.used);
            if slot.entries.len() > MAX_ENTRIES || slot.bytes() > memo.budget {
                slot.entries.clear();
            }
        }
        // Over budget: drop the other settings' results, oldest first.
        while memo.slots.len() > 1 && memo.bytes() > memo.budget {
            memo.slots.remove(0);
        }
        self.stats
    }
}

/// What to do with one top-level statement.
enum Plan {
    /// Evaluate it and keep nothing.
    Plain,
    /// Evaluate it and record it under this fingerprint.
    Record(Digest, Vec<Anchor>),
    /// Replay the entry with this fingerprint at these anchors.
    Reuse(Digest, Vec<Anchor>),
}

impl<'a> Evaluator<'a> {
    /// `LocalScope::instantiateModules` for the main file's top level, with
    /// each statement replayed from the memo when its inputs are unchanged.
    ///
    /// Every statement is instantiated from this one frame, memo or not, so
    /// the native stack a statement starts on (which the recursion limit
    /// measures) is the same for an incremental and a full evaluation.
    pub(crate) fn instantiate_top(&mut self, file: &Rc<Ctx>, out: &mut Vec<Node>) -> R<()> {
        let sr = ScopeRef { unit: 0, scope: 0 };
        let n = self.scope(sr).instantiations.len();
        for i in 0..n {
            // `parent_module()` and `$parent_modules` read the stack of
            // user modules being instantiated; it is empty here, so they
            // depend only on the statement itself.
            let plan = if self.memo.is_some() && self.module_names.is_empty() {
                self.plan(file, i)
            } else {
                Plan::Plain
            };
            match plan {
                Plan::Plain => {
                    if let Some(node) = self.instantiate(sr, i, file)? {
                        out.push(node);
                    }
                }
                Plan::Reuse(fp, anchors) => {
                    if let Some(node) = self.replay(fp, &anchors)? {
                        out.push(node);
                    }
                }
                Plan::Record(fp, anchors) => {
                    let start = self.begin_recording();
                    let r = self.instantiate(sr, i, file);
                    let rec = self.rec.take();
                    let node = r?;
                    self.keep(fp, anchors, start, rec, &node);
                    if let Some(node) = node {
                        out.push(node);
                    }
                }
            }
        }
        Ok(())
    }

    #[inline(never)]
    fn plan(&mut self, file: &Rc<Ctx>, i: usize) -> Plan {
        if let Some(m) = self.memo.as_mut() {
            m.stats.statements += 1;
        }
        let Some((fp, anchors)) = self.fingerprint(file, i) else {
            if let Some(m) = self.memo.as_mut() {
                m.stats.unrecorded += 1;
            }
            return Plan::Plain;
        };
        let live = self.live_bytes_now();
        let memory = self.opts.guard.as_deref().and_then(|g| g.limits().memory);
        let m = self.memo.as_ref().expect("memo");
        match m.entry(&fp) {
            Some(e)
                if e.anchors.len() == anchors.len()
                    && memory.is_none_or(|max| live.saturating_add(e.peak) <= max) =>
            {
                Plan::Reuse(fp, anchors)
            }
            _ => Plan::Record(fp, anchors),
        }
    }

    /// What a recording starts from: the node counter, the limits' tick
    /// counter and the memory estimate.
    fn begin_recording(&mut self) -> (usize, u32, u64) {
        let live = self.live_bytes_now();
        self.rec = Some(Box::new(Recording {
            messages: Vec::new(),
            untracked: false,
            peak: live,
        }));
        (self.node_counter(), self.ticks(), live)
    }

    #[inline(never)]
    fn keep(
        &mut self,
        fp: Digest,
        anchors: Vec<Anchor>,
        (first_index, ticks, live): (usize, u32, u64),
        rec: Option<Box<Recording>>,
        node: &Option<Node>,
    ) {
        let (limit, counter, now) = (self.limit_passed(), self.node_counter(), self.ticks());
        let Some(m) = self.memo.as_mut() else { return };
        let Some(rec) = rec else { return };
        let fits = !rec.untracked
            && !limit
            && node.as_ref().is_none_or(|n| nodes_anchored(n, &anchors))
            && rec.messages.iter().all(|r| message_anchored(r, &anchors));
        let bytes = entry_bytes(node.as_ref(), &rec.messages);
        if !fits || m.used_bytes + bytes > m.memo.budget {
            m.stats.unrecorded += 1;
            return;
        }
        m.used_bytes += bytes;
        let entry = Entry {
            node: node.clone(),
            first_index,
            indices: counter - first_index,
            messages: rec.messages,
            anchors,
            ticks: now.wrapping_sub(ticks),
            peak: rec.peak.saturating_sub(live),
            bytes,
        };
        m.used.insert(fp, entry);
        m.stats.recorded += 1;
    }

    /// Put a recorded statement's result where a fresh evaluation would
    /// have: its nodes moved to `anchors` and renumbered from the counter,
    /// its messages printed again in order.
    #[inline(never)]
    fn replay(&mut self, fp: Digest, anchors: &[Anchor]) -> R<Option<Node>> {
        if self.interrupted() {
            return Err(Unwind::new(UnwindKind::Interrupted, 0));
        }
        let counter = self.node_counter();
        let m = self.memo.as_mut().expect("memo");
        if !m.used.contains_key(&fp) {
            let e = m.memo.slots[m.slot]
                .entries
                .remove(&fp)
                .expect("planned entry");
            m.used_bytes += e.bytes;
            m.used.insert(fp, e);
        }
        m.stats.reused += 1;
        let e = &m.used[&fp];
        let moves = Moves::new(&e.anchors, anchors);
        let shift = counter as i64 - e.first_index as i64;
        let node = e.node.clone().map(|mut n| {
            place(&mut n, &moves, shift);
            n
        });
        let (indices, ticks) = (e.indices, e.ticks);
        let messages: Vec<(Option<u32>, Diagnostic, Vec<u8>)> = e
            .messages
            .iter()
            .map(|r| {
                let mut d = r.diag.clone();
                if r.unit == Some(0)
                    && let Some(s) = d.span
                {
                    let (s, line) = moves.map(s, d.line);
                    d.span = Some(s);
                    d.line = line;
                }
                (r.unit, d, r.text.clone())
            })
            .collect();
        self.advance(indices, ticks);
        for (unit, diag, text) in messages {
            let sources = unit.map(|u| &self.units[u as usize].program.sources);
            self.replay_message(&Message {
                diag,
                text: &text,
                sources,
            });
        }
        Ok(node)
    }

    /// The digest of statement `i`'s inputs, and its anchors; `None` when
    /// it cannot be keyed (a function value among its variables, syntax
    /// that spans files).
    #[inline(never)]
    fn fingerprint(&mut self, file: &Rc<Ctx>, i: usize) -> Option<(Digest, Vec<Anchor>)> {
        let dollar = self.dollar_digest(file)?;
        let stmt = {
            let unit = &self.units[0];
            let inst = &unit.scopes[0].scope.instantiations[i];
            anchored(unit.program, unit.ast, &unit.syms, Item::Inst(inst), b'S')?
        };
        let mut h = Sha256::new();
        h.update(stmt.digest);
        h.update(dollar);
        let mut anchors = vec![stmt.anchor];
        let mut seen: HashSet<Sym> = stmt.names.iter().copied().collect();
        let mut queue = stmt.names;
        let mut k = 0;
        while k < queue.len() {
            let s = queue[k];
            k += 1;
            let name = self.syms.name(s);
            h.update((name.len() as u32).to_le_bytes());
            h.update(name.as_bytes());
            match self.var_digest(file, s)? {
                None => h.update([0]),
                Some(d) => {
                    h.update([1]);
                    h.update(d);
                }
            }
            for module in [false, true] {
                let info = &self.units[0].scopes[0];
                let index = if module {
                    info.modules.get(&s)
                } else {
                    info.functions.get(&s)
                };
                let Some(&index) = index else {
                    h.update([2]);
                    continue;
                };
                let def = self.def_anchored(module, index)?;
                h.update([3]);
                h.update(def.digest);
                anchors.push(def.anchor);
                for &n in &def.names {
                    if seen.insert(n) {
                        queue.push(n);
                    }
                }
            }
        }
        Some((digest(h), anchors))
    }

    fn def_anchored(&mut self, module: bool, index: u32) -> Option<Rc<Anchored>> {
        let m = self.memo.as_mut().expect("memo");
        if let Some(d) = m.defs.get(&(module, index)) {
            return d.clone();
        }
        let unit = &self.units[0];
        let scope = unit.scopes[0].scope;
        let (item, span) = if module {
            let d = &scope.modules[index as usize];
            (Item::Module(d), d.span)
        } else {
            let d = &scope.functions[index as usize];
            (Item::Function(d), d.span)
        };
        let src = unit.program.sources.get(span.file);
        let text = (span.start <= span.end && span.end as usize <= src.text.len())
            .then(|| src.slice(span.start, span.end));
        let key: DefKey = (module, src.path.clone(), span.start, span.end);
        let m = self.memo.as_mut().expect("memo");
        if !m.known.contains_key(&key)
            && let Some(k) = m.memo.known.remove(&key)
        {
            m.known.insert(key.clone(), k);
        }
        let d = match (m.known.get(&key), text) {
            (Some(k), Some(text)) if *k.text == *text => {
                let names: Option<Vec<Sym>> = k.names.iter().map(|n| self.syms.get(n)).collect();
                names.map(|names| {
                    Rc::new(Anchored {
                        digest: k.digest,
                        names,
                        anchor: Anchor {
                            file: span.file,
                            start: k.lo,
                            end: k.hi,
                            line: src.line_of(k.lo),
                        },
                    })
                })
            }
            _ => {
                let tag = if module { b'M' } else { b'F' };
                let d = anchored(unit.program, unit.ast, &unit.syms, item, tag).map(Rc::new);
                match (&d, text) {
                    (Some(a), Some(text))
                        if a.anchor.start >= span.start && a.anchor.end <= span.end =>
                    {
                        let names = a.names.iter().map(|&n| self.syms.name(n).into()).collect();
                        m.known.insert(
                            key,
                            Known {
                                text: text.into(),
                                digest: a.digest,
                                names,
                                lo: a.anchor.start,
                                hi: a.anchor.end,
                            },
                        );
                    }
                    _ => {
                        m.known.remove(&key);
                    }
                }
                d
            }
        };
        m.defs.insert((module, index), d.clone());
        d
    }

    /// The digest of top-level variable `s`'s value: `Some(None)` when the
    /// top level does not bind it, `None` when the value holds a function.
    fn var_digest(&mut self, file: &Rc<Ctx>, s: Sym) -> Option<Option<Digest>> {
        let m = self.memo.as_mut().expect("memo");
        if let Some(d) = m.vars.get(&s) {
            return *d;
        }
        let d = match file.get_local(s, &self.regions) {
            None => Some(None),
            Some(v) => {
                let mut h = Sha256::new();
                value_digest(&v, &mut h).then(|| Some(digest(h)))
            }
        };
        let m = self.memo.as_mut().expect("memo");
        m.vars.insert(s, d);
        d
    }

    /// Every top-level `$` assignment's name and value, in file order.
    fn dollar_digest(&mut self, file: &Rc<Ctx>) -> Option<Digest> {
        if let Some(d) = self.memo.as_ref().expect("memo").dollar {
            return d;
        }
        let mut h = Sha256::new();
        let mut ok = true;
        let unit = &self.units[0];
        for a in &unit.scopes[0].scope.assignments {
            let s = unit.sym(a.name);
            let name = self.syms.name(s);
            if !name.starts_with('$') {
                continue;
            }
            h.update((name.len() as u32).to_le_bytes());
            h.update(name.as_bytes());
            match file.get_local(s, &self.regions) {
                None => h.update([0]),
                Some(v) => {
                    h.update([1]);
                    ok &= value_digest(&v, &mut h);
                }
            }
        }
        let d = ok.then(|| digest(h));
        self.memo.as_mut().expect("memo").dollar = Some(d);
        d
    }
}

/// Hash a value exactly (numbers by their bits); false if it holds a
/// function.
///
/// Lists and strings are shared, not copied, so a value can hold far more
/// paths than it holds data: a tree whose halves are one list (`t = [t,
/// t]`, a few dozen steps of a tail-recursive function) has 2^depth paths,
/// and a long list can hold one long string at every element. Hashing path
/// by path took 3.4 s at depth 26 and hours at depth 40, in every edit's
/// fingerprint of a statement that mentions `t`, with no check that could
/// stop it. So the big parts are hashed on their own, once each (by
/// address), and wherever one occurs its digest stands in for it: lists
/// that hold lists or have [`OWN_LIST`] elements or more, and strings of
/// [`OWN_STR`] bytes or more. The rest (a point, a short name) goes inline,
/// as hashing every point on its own would cost a hash per point. The walk
/// then takes a bounded number of steps per element slot of a distinct
/// list, at most the work of allocating the lists, which the evaluation
/// already did under its limits; so it does not poll them itself.
///
/// The digest stays a function of the value alone, however it was built:
/// which parts are hashed on their own depends only on their content
/// (length, and whether a list holds a list), never on whether they are
/// shared, so equal values built differently digest equal, and a statement
/// replays when a variable is recomputed to the same value. The tags keep
/// the forms apart (4 a list written out, 6 a list's digest, 3 a string
/// written out, 7 a string's digest, 8 an object's digest).
///
/// Objects are always hashed on their own (tag 9, then each key and value
/// in order), by their address: they share their values as lists do, and
/// a tree of objects whose fields are one object is as cheap to build.
pub(crate) fn value_digest(v: &Value, h: &mut Sha256) -> bool {
    let mut done = Done::default();
    match v {
        Value::Vector(items) if own_list(items) => {
            let Some(d) = tree_digest(Part::List(items), &mut done) else {
                return false;
            };
            h.update([6]);
            h.update(d);
            true
        }
        _ => plain_digest(v, h, &mut done),
    }
}

/// Lists at least this long are hashed on their own (see
/// [`value_digest`]).
const OWN_LIST: usize = 16;
/// Strings at least this long are hashed on their own.
const OWN_STR: usize = 64;

/// The digests of the parts hashed on their own, by the address of their
/// elements or bytes (an object's by its own address): no two live lists,
/// strings or objects share one, and every part is alive for the whole
/// walk, inside the value being hashed.
type Done = HashMap<usize, [u8; 32], FxBuild>;

/// Whether a list is hashed on its own.
fn own_list(items: &[Value]) -> bool {
    items.len() >= OWN_LIST
        || items
            .iter()
            .any(|v| matches!(v, Value::Vector(_) | Value::Object(_)))
}

/// Hash a string: inline when short, by its own digest when long.
fn str_digest(b: &[u8], h: &mut Sha256, done: &mut Done) {
    if b.len() >= OWN_STR {
        let d = done.entry(b.as_ptr() as usize).or_insert_with(|| {
            let mut h = Sha256::new();
            h.update((b.len() as u64).to_le_bytes());
            h.update(b);
            h.finalize().into()
        });
        h.update([7]);
        h.update(*d);
    } else {
        h.update([3]);
        h.update((b.len() as u64).to_le_bytes());
        h.update(b);
    }
}

/// Hash a value that is not a list hashed on its own: a short list of
/// plain values, or a plain value. False for a function.
fn plain_digest(v: &Value, h: &mut Sha256, done: &mut Done) -> bool {
    match v {
        Value::Undef => h.update([0]),
        Value::Bool(b) => h.update([1, u8::from(*b)]),
        Value::Number(x) => {
            h.update([2]);
            h.update(x.to_bits().to_le_bytes());
        }
        Value::Str(s) => str_digest(s.as_bytes(), h, done),
        Value::Vector(items) => {
            h.update([4]);
            h.update((items.len() as u64).to_le_bytes());
            for v in items.iter() {
                if !plain_digest(v, h, done) {
                    return false;
                }
            }
        }
        Value::Range(r) => {
            h.update([5]);
            for x in [r.begin, r.step, r.end] {
                h.update(x.to_bits().to_le_bytes());
            }
        }
        Value::Function(_) => return false,
        // Only a top-level value gets here: a list holding an object is
        // hashed on its own, and its walk takes the object.
        Value::Object(o) => {
            let Some(d) = tree_digest(Part::Object(o), done) else {
                return false;
            };
            h.update([8]);
            h.update(d);
        }
    }
    true
}

/// A part hashed on its own: a list, or an object.
#[derive(Clone, Copy)]
enum Part<'v> {
    List(&'v Vector),
    Object(&'v crate::value::Object),
}

/// The digest of a part hashed on its own (see [`value_digest`]): a list's
/// length then its elements, an object's length then its keys and values,
/// each list or object among them by its digest. Iterative, as values can
/// nest deeper than the stack allows.
fn tree_digest(root: Part<'_>, done: &mut Done) -> Option<[u8; 32]> {
    struct Frame<'v> {
        items: &'v [Value],
        /// An object's keys, one per item.
        keys: Option<&'v [crate::value::Str]>,
        id: usize,
        i: usize,
        h: Sha256,
    }
    fn open(p: Part<'_>) -> Frame<'_> {
        let mut h = Sha256::new();
        let (items, keys, id, tag) = match p {
            Part::List(v) => (v.as_slice(), None, v.as_slice().as_ptr() as usize, 4),
            Part::Object(o) => (o.values(), Some(o.keys()), o.addr(), 9),
        };
        h.update([tag]);
        h.update((items.len() as u64).to_le_bytes());
        Frame {
            items,
            keys,
            id,
            i: 0,
            h,
        }
    }
    let mut stack = vec![open(root)];
    loop {
        let top = stack.last_mut().expect("a frame");
        let Some(v) = top.items.get(top.i) else {
            let f = stack.pop().expect("a frame");
            let object = f.keys.is_some();
            let d: [u8; 32] = f.h.finalize().into();
            done.insert(f.id, d);
            let Some(parent) = stack.last_mut() else {
                return Some(d);
            };
            parent.h.update([if object { 8 } else { 6 }]);
            parent.h.update(d);
            continue;
        };
        if let Some(keys) = top.keys {
            str_digest(keys[top.i].as_bytes(), &mut top.h, done);
        }
        top.i += 1;
        let (part, id, tag) = match v {
            Value::Vector(items) if own_list(items) => {
                (Part::List(items), items.as_slice().as_ptr() as usize, 6)
            }
            Value::Object(o) => (Part::Object(o), o.addr(), 8),
            v => {
                if !plain_digest(v, &mut top.h, done) {
                    return None;
                }
                continue;
            }
        };
        if let Some(d) = done.get(&id) {
            top.h.update([tag]);
            top.h.update(d);
        } else {
            stack.push(open(part));
        }
    }
}

enum Item<'x> {
    Inst(&'x Instantiation),
    Function(&'x lang::ast::FunctionDef),
    Module(&'x lang::ast::ModuleDef),
}

/// Walk an item's syntax: its structure (with spans relative to where it
/// starts) into a digest, the names it mentions, and the range its spans
/// cover, which must be in one file.
fn anchored(
    program: &Program,
    ast: &Ast,
    syms: &[Sym],
    item: Item<'_>,
    tag: u8,
) -> Option<Anchored> {
    let base = match &item {
        Item::Inst(i) => i.span,
        Item::Function(f) => f.span,
        Item::Module(m) => m.span,
    };
    let mut w = Walker {
        ast,
        h: Sha256::new(),
        names: Vec::new(),
        seen: HashSet::new(),
        file: base.file,
        base: base.start,
        lo: base.start,
        hi: base.end,
        ok: true,
    };
    w.h.update([tag]);
    match item {
        Item::Inst(i) => w.inst(i),
        Item::Function(f) => {
            w.name(f.name);
            w.span(f.span);
            w.params(&f.params);
            w.expr(f.body);
        }
        Item::Module(m) => {
            w.name(m.name);
            w.span(m.span);
            w.params(&m.params);
            w.scope(&m.body);
        }
    }
    if !w.ok {
        return None;
    }
    let src = program.sources.get(w.file);
    if w.hi as usize > src.text.len() || w.lo > w.hi {
        return None;
    }
    let text = src.slice(w.lo, w.hi);
    let path = src.path.as_os_str().as_encoded_bytes();
    let mut h = w.h;
    h.update((w.base - w.lo).to_le_bytes());
    h.update((path.len() as u64).to_le_bytes());
    h.update(path);
    h.update((text.len() as u64).to_le_bytes());
    h.update(text);
    Some(Anchored {
        digest: digest(h),
        names: w.names.iter().map(|n| syms[n.0 as usize]).collect(),
        anchor: Anchor {
            file: w.file,
            start: w.lo,
            end: w.hi,
            line: src.line_of(w.lo),
        },
    })
}

/// What [`mentions`] walks.
pub(crate) enum Mentioned<'x> {
    Scope(&'x Scope),
    Function(&'x lang::ast::FunctionDef),
    Module(&'x lang::ast::ModuleDef),
}

/// Every name some syntax mentions as a variable, function or module
/// ([`Walker::mention`]), nested scopes and definitions included, as
/// symbols: everything evaluating it can look up lexically, since every
/// lexical lookup is of a name written in the source. (`crate::callmemo`
/// keys a call's children on the variables of these names.)
pub(crate) fn mentions(ast: &Ast, syms: &[Sym], item: Mentioned<'_>) -> Vec<Sym> {
    // Only the names are kept: the digest and the span checks, which need
    // one file, go unused.
    let mut w = Walker {
        ast,
        h: Sha256::new(),
        names: Vec::new(),
        seen: HashSet::new(),
        file: FileId(0),
        base: 0,
        lo: 0,
        hi: 0,
        ok: true,
    };
    match item {
        Mentioned::Scope(s) => w.scope(s),
        Mentioned::Function(f) => {
            w.params(&f.params);
            w.expr(f.body);
        }
        Mentioned::Module(m) => {
            w.params(&m.params);
            w.scope(&m.body);
        }
    }
    w.names.iter().map(|n| syms[n.0 as usize]).collect()
}

struct Walker<'x> {
    ast: &'x Ast,
    h: Sha256,
    names: Vec<Name>,
    seen: HashSet<Name>,
    file: FileId,
    base: u32,
    lo: u32,
    hi: u32,
    ok: bool,
}

impl Walker<'_> {
    fn span(&mut self, s: Span) {
        if s.file != self.file {
            self.ok = false;
            return;
        }
        self.lo = self.lo.min(s.start);
        self.hi = self.hi.max(s.end);
        self.h.update(s.start.wrapping_sub(self.base).to_le_bytes());
        self.h.update(s.end.wrapping_sub(self.base).to_le_bytes());
    }

    fn count(&mut self, n: usize) {
        self.h.update((n as u64).to_le_bytes());
    }

    fn name(&mut self, n: Name) {
        let s = self.ast.name(n);
        self.count(s.len());
        self.h.update(s.as_bytes());
    }

    /// A name that refers to something (a variable, function or module).
    fn mention(&mut self, n: Name) {
        self.name(n);
        if self.seen.insert(n) {
            self.names.push(n);
        }
    }

    fn scope(&mut self, s: &Scope) {
        self.count(s.functions.len());
        for f in &s.functions {
            self.name(f.name);
            self.span(f.span);
            self.params(&f.params);
            self.expr(f.body);
        }
        self.count(s.modules.len());
        for m in &s.modules {
            self.name(m.name);
            self.span(m.span);
            self.params(&m.params);
            self.scope(&m.body);
        }
        self.count(s.assignments.len());
        for a in &s.assignments {
            self.name(a.name);
            self.span(a.loc.span);
            match &a.overwrite {
                None => self.h.update([0]),
                Some(o) => {
                    self.h.update([1]);
                    self.span(o.span);
                }
            }
            self.expr(a.expr);
            self.count(a.annotations.len());
            for an in &a.annotations {
                self.count(an.name.len());
                self.h.update(an.name.as_bytes());
                self.expr(an.expr);
            }
        }
        self.count(s.instantiations.len());
        for i in &s.instantiations {
            self.inst(i);
        }
    }

    fn inst(&mut self, i: &Instantiation) {
        self.mention(i.name);
        self.span(i.span);
        self.h.update([
            u8::from(i.tag_root),
            u8::from(i.tag_highlight),
            u8::from(i.tag_background),
        ]);
        self.args(&i.args);
        self.scope(&i.children);
        match &i.kind {
            InstKind::Module => self.h.update([0]),
            InstKind::If { else_children } => match else_children {
                None => self.h.update([1]),
                Some(e) => {
                    self.h.update([2]);
                    self.scope(e);
                }
            },
        }
    }

    fn params(&mut self, ps: &[Param]) {
        self.count(ps.len());
        for p in ps {
            self.name(p.name);
            self.span(p.span);
            match p.default {
                None => self.h.update([0]),
                Some(d) => {
                    self.h.update([1]);
                    self.expr(d);
                }
            }
        }
    }

    fn args(&mut self, args: &[Arg]) {
        self.count(args.len());
        for a in args {
            match a.name {
                None => self.h.update([0]),
                Some(n) => {
                    self.h.update([1]);
                    self.name(n);
                }
            }
            self.span(a.span);
            self.expr(a.expr);
        }
    }

    fn opt(&mut self, e: Option<ExprId>) {
        match e {
            None => self.h.update([0]),
            Some(e) => {
                self.h.update([1]);
                self.expr(e);
            }
        }
    }

    /// Pre-order, each node with its kind and arity, so different trees
    /// cannot hash alike.
    fn expr(&mut self, id: ExprId) {
        let ast = self.ast;
        let e = ast.expr(id);
        self.span(e.span);
        match &e.kind {
            ExprKind::Undef => self.h.update([0]),
            ExprKind::Bool(b) => self.h.update([1, u8::from(*b)]),
            ExprKind::Number(x) => {
                self.h.update([2]);
                self.h.update(x.to_bits().to_le_bytes());
            }
            ExprKind::String(s) => {
                self.h.update([3]);
                self.count(s.len());
                self.h.update(s);
            }
            ExprKind::Var(n) => {
                self.h.update([4]);
                self.mention(*n);
            }
            ExprKind::Unary(op, a) => {
                self.h.update([5, *op as u8]);
                self.expr(*a);
            }
            ExprKind::Binary(op, a, b) => {
                self.h.update([6, *op as u8]);
                self.expr(*a);
                self.expr(*b);
            }
            ExprKind::Ternary(a, b, c) => {
                self.h.update([7]);
                self.expr(*a);
                self.expr(*b);
                self.expr(*c);
            }
            ExprKind::Index(a, b) => {
                self.h.update([8]);
                self.expr(*a);
                self.expr(*b);
            }
            ExprKind::Member(a, n) => {
                self.h.update([9]);
                self.name(*n);
                self.expr(*a);
            }
            ExprKind::Call(f, args) => {
                self.h.update([10]);
                self.expr(*f);
                self.args(args);
            }
            ExprKind::Range { begin, step, end } => {
                self.h.update([11]);
                self.expr(*begin);
                self.opt(*step);
                self.expr(*end);
            }
            ExprKind::Vector(items) => {
                self.h.update([12]);
                self.count(items.len());
                for &x in items {
                    self.expr(x);
                }
            }
            ExprKind::Function(ps, body) => {
                self.h.update([13]);
                self.params(ps);
                self.expr(*body);
            }
            ExprKind::Let(args, body) => {
                self.h.update([14]);
                self.args(args);
                self.expr(*body);
            }
            ExprKind::Assert(args, body) => {
                self.h.update([15]);
                self.args(args);
                self.opt(*body);
            }
            ExprKind::Echo(args, body) => {
                self.h.update([16]);
                self.args(args);
                self.opt(*body);
            }
            ExprKind::LcIf(c, a, b) => {
                self.h.update([17]);
                self.expr(*c);
                self.expr(*a);
                self.opt(*b);
            }
            ExprKind::LcEach(a) => {
                self.h.update([18]);
                self.expr(*a);
            }
            ExprKind::LcFor(args, body) => {
                self.h.update([19]);
                self.args(args);
                self.expr(*body);
            }
            ExprKind::LcForC {
                init,
                cond,
                incr,
                body,
            } => {
                self.h.update([20]);
                self.args(init);
                self.expr(*cond);
                self.args(incr);
                self.expr(*body);
            }
            ExprKind::LcLet(args, body) => {
                self.h.update([21]);
                self.args(args);
                self.expr(*body);
            }
            ExprKind::Invalid => self.h.update([22]),
        }
    }
}

/// Recorded anchors paired with this evaluation's, for moving spans.
struct Moves {
    /// (old, new), by old file and start.
    pairs: Vec<(Anchor, Anchor)>,
    same: bool,
}

impl Moves {
    fn new(old: &[Anchor], new: &[Anchor]) -> Moves {
        let same = old == new;
        let mut pairs: Vec<(Anchor, Anchor)> =
            old.iter().copied().zip(new.iter().copied()).collect();
        pairs.sort_by_key(|(o, _)| (o.file.0, o.start, o.end));
        Moves { pairs, same }
    }

    /// Where a main-program span recorded inside an anchor is now. The
    /// recording was only kept if every such span was inside one.
    fn map(&self, s: Span, line: u32) -> (Span, u32) {
        if self.same {
            return (s, line);
        }
        match find(&self.pairs, s) {
            Some((o, n)) => (
                Span {
                    file: n.file,
                    start: s.start - o.start + n.start,
                    end: s.end - o.start + n.start,
                },
                (line as i64 - o.line as i64 + n.line as i64) as u32,
            ),
            None => (s, line),
        }
    }
}

/// The anchor pair whose old range holds `s`.
fn find(pairs: &[(Anchor, Anchor)], s: Span) -> Option<(Anchor, Anchor)> {
    // Anchors are distinct top-level items and do not overlap, so the last
    // one starting at or before `s` is the one; the scan is a safety net.
    let holds = |o: &Anchor| o.file == s.file && o.start <= s.start && s.end <= o.end;
    let i = pairs.partition_point(|(o, _)| (o.file.0, o.start) <= (s.file.0, s.start));
    if let Some(&(o, n)) = i.checked_sub(1).and_then(|i| pairs.get(i))
        && holds(&o)
    {
        return Some((o, n));
    }
    pairs.iter().copied().find(|(o, _)| holds(o))
}

fn inside(anchors: &[Anchor], s: Span) -> bool {
    anchors
        .iter()
        .any(|a| a.file == s.file && a.start <= s.start && s.end <= a.end)
}

/// Whether every main-program node origin in `n` lies in an anchor.
fn nodes_anchored(n: &Node, anchors: &[Anchor]) -> bool {
    let mut stack = vec![n];
    while let Some(n) = stack.pop() {
        if let Some(o) = &n.origin
            && o.unit == 0
            && !inside(anchors, o.span)
        {
            return false;
        }
        stack.extend(n.children.iter());
    }
    true
}

fn message_anchored(r: &Recorded, anchors: &[Anchor]) -> bool {
    // Evaluation hints carry no replacement spans; one that did could not
    // be moved, since its unit is not recorded.
    if r.diag.hints.iter().any(|h| h.replacement.is_some()) {
        return false;
    }
    match (r.unit, r.diag.span) {
        (Some(0), Some(s)) => inside(anchors, s),
        (Some(0), None) | (None, Some(_)) => false,
        _ => true,
    }
}

/// Move a replayed subtree to its anchors and renumber it.
fn place(n: &mut Node, moves: &Moves, shift: i64) {
    let mut stack = vec![n];
    while let Some(n) = stack.pop() {
        n.index = (n.index as i64 + shift) as usize;
        if let Some(o) = &mut n.origin
            && o.unit == 0
        {
            let (s, line) = moves.map(o.span, o.line);
            o.span = s;
            o.line = line;
        }
        stack.extend(n.children.iter_mut());
    }
}
