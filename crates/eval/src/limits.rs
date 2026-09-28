//! Per-request resource limits, for hosts that run models they did not
//! write: `neoscad serve`, `neoscad mcp` and the app.
//!
//! OpenSCAD has no such limits: `$fn` has no upper bound and `rands()`
//! reserves whatever count it is given, so one mistyped number can ask for
//! tens of gigabytes. On the one-shot command line that is the user's own
//! run, and it stays unlimited (as OpenSCAD is) unless `--limit` flags are
//! given. A long-lived server fed by an agent is different: one bad guess
//! takes the machine down, and in the app it takes unsaved work with it.
//! So those hosts set [`Limits`], and a request that would pass one fails
//! with a `resource-limit` error that names the limit and how to raise it.
//!
//! Enforcement is cooperative and happens *before* the big allocations:
//!
//! - **Counts** ([`Limits::fragments`], [`Limits::slices`],
//!   [`Limits::list`], [`Limits::string`], [`Limits::rands`],
//!   [`Limits::triangles`]) are compared with what an operation is about
//!   to build, so `sphere(10, $fn=1e5)` fails before it allocates a vertex.
//! - **Memory** ([`Limits::memory`]) is an estimate kept at the
//!   allocation-heavy points, not a measurement of the process (the
//!   workspace forbids the `unsafe` a counting global allocator needs, and
//!   a process-wide count cannot tell concurrent requests apart). The
//!   evaluator counts every list and string alive on its thread, every
//!   node it builds and every message it prints ([`live`]); the
//!   geometry stage counts the bytes of the results computed and not yet
//!   used by their parents (the geometry cache has its own budget).
//! - **Time** ([`Limits::time`]) is checked against the host's clock at
//!   the evaluator's calls and loop iterations (every few thousand) and
//!   before every geometry node and inside long primitive and extrusion
//!   loops. A single kernel operation (one big boolean) is not
//!   interrupted, so a request can overrun by that much.
//!
//! A [`Guard`] holds one request's limits and state. Tripping it records
//! the first limit passed ([`Guard::exceeded`]) and sets the request's
//! interrupt flag, so every stage stops at its next check exactly as a
//! cancelled request does; the host then reports the limit instead of a
//! cancellation.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

/// Milliseconds on any monotonic clock (the host's; library crates never
/// read the clock themselves).
pub type Clock = Arc<dyn Fn() -> f64 + Send + Sync>;

/// One kind of limit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Limit {
    Time,
    Memory,
    Fragments,
    Slices,
    List,
    String,
    Rands,
    Triangles,
}

impl Limit {
    pub const ALL: [Limit; 8] = [
        Limit::Time,
        Limit::Memory,
        Limit::Fragments,
        Limit::Slices,
        Limit::List,
        Limit::String,
        Limit::Rands,
        Limit::Triangles,
    ];

    /// The name `--limit NAME=VALUE` and the JSON `limits` object use.
    pub fn key(self) -> &'static str {
        match self {
            Limit::Time => "time",
            Limit::Memory => "memory",
            Limit::Fragments => "fragments",
            Limit::Slices => "slices",
            Limit::List => "list",
            Limit::String => "string",
            Limit::Rands => "rands",
            Limit::Triangles => "triangles",
        }
    }

    pub fn from_key(k: &str) -> Option<Limit> {
        Limit::ALL.into_iter().find(|l| l.key() == k)
    }

    /// What the number counts, for messages.
    fn unit(self) -> &'static str {
        match self {
            Limit::Time => "s",
            Limit::Memory => "MiB",
            Limit::Fragments => "fragments per primitive",
            Limit::Slices => "slices per extrusion",
            Limit::List => "elements per list",
            Limit::String => "bytes per string",
            Limit::Rands => "numbers per rands() call",
            Limit::Triangles => "triangles per result",
        }
    }
}

/// The limits of a request; `None` is unlimited. [`Limits::default`] is
/// unlimited everywhere, as OpenSCAD is.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Limits {
    /// Wall time, seconds.
    pub time: Option<f64>,
    /// Estimated bytes (see the module documentation).
    pub memory: Option<u64>,
    /// Segments of one circle, sphere, cylinder, `rotate_extrude` or
    /// round `offset`.
    pub fragments: Option<u64>,
    /// Slices of one `linear_extrude`.
    pub slices: Option<u64>,
    /// Elements of one list.
    pub list: Option<u64>,
    /// Bytes of one string.
    pub string: Option<u64>,
    /// Numbers one `rands()` call returns.
    pub rands: Option<u64>,
    /// Triangles (2D: vertices) of one geometry result, the final model
    /// included.
    pub triangles: Option<u64>,
}

impl Limits {
    /// Unlimited: OpenSCAD's behaviour, and the one-shot command line's.
    pub const NONE: Limits = Limits {
        time: None,
        memory: None,
        fragments: None,
        slices: None,
        list: None,
        string: None,
        rands: None,
        triangles: None,
    };

    /// The defaults of the agent and app surfaces (`serve`, `mcp`, the
    /// app): generous for real models, safe for a 16 GB machine.
    ///
    /// - 60 s: the heaviest benchmark models render in a few seconds.
    /// - 4 GiB of estimated memory.
    /// - 10,000 fragments: `$fn` in real models is at most a few hundred;
    ///   a sphere at 10,000 is already 50 million vertices, which the
    ///   triangle limit refuses anyway.
    /// - 10,000 slices, the same order as fragments.
    /// - 10 million list elements (160 MB of numbers) and 64 MiB strings.
    /// - 10 million numbers from one `rands()`.
    /// - 10 million triangles in one result (a few GB as a Manifold mesh).
    pub const AGENT: Limits = Limits {
        time: Some(60.0),
        memory: Some(4 << 30),
        fragments: Some(10_000),
        slices: Some(10_000),
        list: Some(10_000_000),
        string: Some(64 << 20),
        rands: Some(10_000_000),
        triangles: Some(10_000_000),
    };

    pub fn is_none(&self) -> bool {
        *self == Limits::NONE
    }

    pub fn get(&self, l: Limit) -> Option<f64> {
        match l {
            Limit::Time => self.time,
            Limit::Memory => self.memory.map(|b| b as f64 / (1u64 << 20) as f64),
            Limit::Fragments => self.fragments.map(|n| n as f64),
            Limit::Slices => self.slices.map(|n| n as f64),
            Limit::List => self.list.map(|n| n as f64),
            Limit::String => self.string.map(|n| n as f64),
            Limit::Rands => self.rands.map(|n| n as f64),
            Limit::Triangles => self.triangles.map(|n| n as f64),
        }
    }

    /// Set limit `l` from a user's value: a number (seconds for `time`,
    /// MiB for `memory`, a count otherwise; `memory` also takes a `G` or
    /// `M` suffix), or `off`/`none` for unlimited.
    pub fn set(&mut self, l: Limit, value: &str) -> Result<(), String> {
        let v = value.trim();
        if matches!(v, "off" | "none" | "unlimited") {
            self.put(l, None);
            return Ok(());
        }
        let (num, scale) = match l {
            Limit::Memory => {
                let lower = v.to_ascii_lowercase();
                if let Some(n) = lower
                    .strip_suffix("gib")
                    .or_else(|| lower.strip_suffix('g'))
                {
                    (n.trim().to_string(), 1024.0)
                } else if let Some(n) = lower
                    .strip_suffix("mib")
                    .or_else(|| lower.strip_suffix('m'))
                {
                    (n.trim().to_string(), 1.0)
                } else {
                    (lower, 1.0)
                }
            }
            _ => (v.to_string(), 1.0),
        };
        let n: f64 = num
            .parse()
            .ok()
            .filter(|n: &f64| n.is_finite() && *n > 0.0)
            .ok_or_else(|| {
                format!(
                    "limit {} must be a positive number ({}) or 'off' (got '{value}')",
                    l.key(),
                    l.unit()
                )
            })?;
        self.put(l, Some(n * scale));
        Ok(())
    }

    /// Set limit `l` to `v` in its unit ([`Limit::unit`]), or unlimited.
    pub fn put(&mut self, l: Limit, v: Option<f64>) {
        let count = |x: f64| x.min(u64::MAX as f64).ceil() as u64;
        match l {
            Limit::Time => self.time = v,
            Limit::Memory => self.memory = v.map(|m| count(m * (1u64 << 20) as f64)),
            Limit::Fragments => self.fragments = v.map(count),
            Limit::Slices => self.slices = v.map(count),
            Limit::List => self.list = v.map(count),
            Limit::String => self.string = v.map(count),
            Limit::Rands => self.rands = v.map(count),
            Limit::Triangles => self.triangles = v.map(count),
        }
    }

    /// Parse `NAME=VALUE` (a `--limit` flag) onto these limits.
    pub fn apply_flag(&mut self, flag: &str) -> Result<(), String> {
        let (k, v) = flag
            .split_once('=')
            .ok_or_else(|| format!("--limit takes NAME=VALUE (got '{flag}'); {}", names()))?;
        let l = Limit::from_key(k.trim())
            .ok_or_else(|| format!("unknown limit '{}'; {}", k.trim(), names()))?;
        self.set(l, v)
    }
}

fn names() -> String {
    let keys: Vec<&str> = Limit::ALL.iter().map(|l| l.key()).collect();
    format!("the limits are {}", keys.join(", "))
}

/// A limit a request passed: which, its value, and what asked for more.
#[derive(Debug, Clone, PartialEq)]
pub struct Exceeded {
    pub limit: Limit,
    /// The limit's value, in its unit.
    pub max: f64,
    /// What the model asked for, in the same unit (0 when unknown).
    pub asked: f64,
    /// What asked, e.g. `sphere()` or `rands()`.
    pub what: String,
    /// `asked` is only how far the work got before it stopped, not what it
    /// would have made: printing a list whose elements are shared stops
    /// at the string limit rather than build text that can be exponentially
    /// longer, so the full length is never known.
    pub at_least: bool,
    /// Where, when the geometry stage found it (the evaluator prints its
    /// own at the call).
    pub at: Option<At>,
}

/// A node's source location: its unit (0 the main program, `1 + i` the
/// i-th library), span and line.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct At {
    pub unit: u32,
    pub span: lang::source::Span,
    pub line: u32,
}

fn fmt_num(x: f64) -> String {
    if x >= 1e15 || x.fract() != 0.0 {
        format!("{x:.3e}")
            .replace(".000e", "e")
            .trim_end_matches('0')
            .to_string()
    } else {
        // Thousands separators: "100,000 fragments" reads at a glance.
        let s = format!("{}", x as u64);
        let mut out = String::new();
        for (i, c) in s.chars().enumerate() {
            if i > 0 && (s.len() - i) % 3 == 0 {
                out.push(',');
            }
            out.push(c);
        }
        out
    }
}

impl Exceeded {
    /// The error's text, without OpenSCAD's `ERROR: ` prefix.
    pub fn message(&self) -> String {
        let max = fmt_num(self.max);
        match self.limit {
            Limit::Time => format!(
                "Resource limit exceeded: the request ran longer than the time limit of {max} s"
            ),
            Limit::Memory => format!(
                "Resource limit exceeded: {} needs more than the memory limit of {max} MiB (estimated)",
                self.what
            ),
            l => format!(
                "Resource limit exceeded: {} would make {}{} {}, over the {} limit of {max}",
                self.what,
                if self.at_least { "at least " } else { "" },
                fmt_num(self.asked),
                match l {
                    Limit::Fragments => "fragments",
                    Limit::Slices => "slices",
                    Limit::List => "list elements",
                    Limit::String => "bytes of string",
                    Limit::Rands => "random numbers",
                    _ => "triangles",
                },
                l.key(),
            ),
        }
    }

    /// How to get past it: the model's side first, then the host's flag.
    pub fn hint(&self) -> String {
        let fix = match self.limit {
            Limit::Fragments => "lower $fn (or raise $fa/$fs)",
            Limit::Slices => "lower slices (or $fn with twist)",
            Limit::List | Limit::String => {
                "build a smaller value; check the recursion or loop that grows it"
            }
            Limit::Rands => "ask rands() for fewer numbers",
            Limit::Triangles => "lower $fn or simplify the model",
            Limit::Memory => "simplify the model or lower $fn",
            Limit::Time => "simplify the model or lower $fn",
        };
        format!(
            "{fix}; or raise the limit: start `neoscad serve` or `neoscad mcp` with `--limit {}=N` (`=off` removes it)",
            self.limit.key()
        )
    }
}

/// One request's limits and their state, shared by its evaluation and
/// geometry stages (and the geometry stage's threads).
pub struct Guard {
    limits: Limits,
    interrupt: Arc<AtomicBool>,
    clock: Option<Clock>,
    /// The clock reading past which the request is out of time.
    deadline: f64,
    tripped: Mutex<Option<Exceeded>>,
    geometry: AtomicU64,
    /// Calls since the clock was last read ([`Guard::poll`]).
    ticks: AtomicU32,
}

impl std::fmt::Debug for Guard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Guard")
            .field("limits", &self.limits)
            .field("tripped", &self.exceeded())
            .finish_non_exhaustive()
    }
}

impl Guard {
    /// A guard over `limits` that stops a request by setting `interrupt`
    /// (the request's cancel flag). Without a clock there is no time
    /// limit.
    pub fn new(limits: Limits, interrupt: Arc<AtomicBool>, clock: Option<Clock>) -> Guard {
        let deadline = match (&clock, limits.time) {
            (Some(c), Some(t)) => c() + t * 1000.0,
            _ => f64::INFINITY,
        };
        Guard {
            limits,
            interrupt,
            clock,
            deadline,
            tripped: Mutex::new(None),
            geometry: AtomicU64::new(0),
            ticks: AtomicU32::new(0),
        }
    }

    pub fn limits(&self) -> &Limits {
        &self.limits
    }

    /// Record `e` (the first limit passed wins) and stop the request.
    pub fn trip(&self, e: Exceeded) {
        self.record(e);
        self.interrupt.store(true, Ordering::Relaxed);
    }

    /// Record `e` without stopping anything: the evaluator reports a limit
    /// through its own error path (with the call sites that led to it).
    ///
    /// The first limit passed is kept, except that of two located ones
    /// the earlier in the source wins: the geometry stage runs siblings
    /// in parallel, and when several pass a limit before the rest stop,
    /// this makes the report the same from run to run as far as it can
    /// (a sibling that stopped before its own check never records).
    pub fn record(&self, e: Exceeded) {
        let mut t = self
            .tripped
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let key = |x: &Exceeded| x.at.map(|a| (a.unit, a.span.file.0, a.span.start));
        let earlier = match (t.as_ref().and_then(key), key(&e)) {
            (Some(old), Some(new)) => new < old,
            _ => false,
        };
        if t.is_none() || earlier {
            *t = Some(e);
        }
    }

    /// The limit that stopped the request, if one did.
    pub fn exceeded(&self) -> Option<Exceeded> {
        self.tripped
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// Whether the request should stop: cancelled, a limit passed, or out
    /// of time (which trips the time limit). Reads the clock every call;
    /// for hot loops use [`Guard::poll`].
    pub fn stopped(&self) -> bool {
        if self.interrupt.load(Ordering::Relaxed) {
            return true;
        }
        if self.over_time() {
            self.trip(self.time_exceeded());
            return true;
        }
        false
    }

    /// [`Guard::stopped`], reading the clock only every 1,024th call (a
    /// clock read costs about as much as a small evaluator call).
    #[inline]
    pub fn poll(&self) -> bool {
        if self.interrupt.load(Ordering::Relaxed) {
            return true;
        }
        if self.ticks.fetch_add(1, Ordering::Relaxed) & 1023 != 0 {
            return false;
        }
        self.stopped()
    }

    /// Whether the deadline has passed (nothing is recorded).
    pub fn over_time(&self) -> bool {
        match &self.clock {
            Some(c) if self.deadline.is_finite() => c() >= self.deadline,
            _ => false,
        }
    }

    /// The time limit, as passed.
    pub fn time_exceeded(&self) -> Exceeded {
        Exceeded {
            limit: Limit::Time,
            max: self.limits.time.unwrap_or(0.0),
            asked: 0.0,
            what: String::new(),
            at_least: false,
            at: None,
        }
    }

    /// `asked` of `l` as a passed limit, if it is over (nothing is
    /// recorded).
    pub fn exceeds(&self, l: Limit, asked: f64, what: &str) -> Option<Exceeded> {
        let max = self.limits.get(l)?;
        (asked > max).then(|| Exceeded {
            limit: l,
            max,
            asked,
            what: what.to_string(),
            at_least: false,
            at: None,
        })
    }

    /// Check that `asked` of `l` is within its limit; otherwise trip it.
    pub fn check(&self, l: Limit, asked: f64, what: &str) -> Result<(), Exceeded> {
        match self.exceeds(l, asked, what) {
            None => Ok(()),
            Some(e) => {
                self.trip(e.clone());
                Err(e)
            }
        }
    }

    /// Count `bytes` of geometry alive (see `geom`'s accounting: a result
    /// counts until its parent has used it); trips the memory limit when
    /// the total passes it.
    pub fn charge_geometry(&self, bytes: u64, what: &str) -> Result<(), Exceeded> {
        let total = self.geometry.fetch_add(bytes, Ordering::Relaxed) + bytes;
        match self.memory_exceeds(total, what) {
            None => Ok(()),
            Some(e) => {
                self.trip(e.clone());
                Err(e)
            }
        }
    }

    /// `bytes` of geometry no longer alive.
    pub fn credit_geometry(&self, bytes: u64) {
        let _ = self
            .geometry
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |b| {
                Some(b.saturating_sub(bytes))
            });
    }

    /// An estimate of `bytes` in use as a passed memory limit, if it is
    /// over (nothing is recorded).
    pub fn memory_exceeds(&self, bytes: u64, what: &str) -> Option<Exceeded> {
        let max = self.limits.memory?;
        let mib = |b: u64| b as f64 / (1u64 << 20) as f64;
        (bytes > max).then(|| Exceeded {
            limit: Limit::Memory,
            max: mib(max),
            asked: mib(bytes),
            what: what.to_string(),
            at_least: false,
            at: None,
        })
    }

    /// Bytes of geometry alive by the estimate.
    pub fn geometry_bytes(&self) -> u64 {
        self.geometry.load(Ordering::Relaxed)
    }
}

/// The evaluator's estimate of its live memory: bytes of every list and
/// string alive on this thread, and messages printed (nodes are counted by
/// the evaluator from its node counter).
///
/// Values are `Rc`-based and never leave the evaluation thread, so a
/// thread-local count is exact about *which* request they belong to. The
/// count starts at zero with each evaluation ([`live::arm`]).
///
/// Every list and string counts, however small. Counting only large ones
/// (as this once did, from 1,024 elements) let a program build a tree of
/// two-element lists sharing their halves, which costs nothing, and then
/// materialise it element-wise (`-t`, `t + t`, `str(t)`), which allocates
/// 2^depth small lists: 1.1 GB under a 64 MiB limit before an outside
/// guard killed it. Charging is one thread-local add and compare, next to
/// an allocation that costs far more.
///
/// The memory limit also fires *from* a charge. An element-wise operator
/// or the value printer runs as one Rust call that the evaluator's
/// periodic checks never interrupt, so the charge that first passes the
/// limit trips the request's [`Guard`] at once: that records the limit
/// and raises the interrupt flag the evaluator already polls at every
/// call, so noticing costs its hot path nothing. The loops that
/// materialise values ask [`live::over`] and stop, and the evaluator
/// reports the limit with the usual `resource-limit` error.
pub mod live {
    use std::cell::{Cell, RefCell};
    use std::sync::Arc;

    use super::Guard;

    /// The count and the thresholds it is compared with, in one
    /// thread-local so a charge is one access.
    struct Count {
        live: Cell<u64>,
        /// The memory limit (`u64::MAX` without one).
        limit: Cell<u64>,
        /// Where the next charge trips the guard: the limit, until it has
        /// been tripped once (then `u64::MAX`, so a flood of charges past
        /// the limit does not lock the guard each time).
        trip_at: Cell<u64>,
        /// Bytes the evaluator counts itself ([`beside`]).
        beside: Cell<u64>,
        /// The limit was passed during this evaluation.
        passed: Cell<bool>,
    }

    thread_local! {
        static COUNT: Count = const {
            Count {
                live: Cell::new(0),
                limit: Cell::new(u64::MAX),
                trip_at: Cell::new(u64::MAX),
                beside: Cell::new(0),
                passed: Cell::new(false),
            }
        };
        /// The guard of the evaluation running on this thread, if it has
        /// a memory limit. Only read on the cold path.
        static GUARD: RefCell<Option<Arc<Guard>>> = const { RefCell::new(None) };
    }

    /// Bytes of one value slot in a list.
    pub const SLOT: u64 = std::mem::size_of::<crate::value::Value>() as u64;
    /// An `Rc` allocation's counts and header with the allocator's
    /// rounding: what a list or string costs before its first element.
    pub const BOX: u64 = 64;

    /// Start counting an evaluation on this thread from zero, against the
    /// memory limit of `guard` (none without one). Values of an earlier
    /// evaluation on this thread are gone or are not this request's to
    /// count.
    pub fn arm(guard: Option<&Arc<Guard>>) {
        let guard = guard.filter(|g| g.limits().memory.is_some());
        let limit = guard.and_then(|g| g.limits().memory).unwrap_or(u64::MAX);
        COUNT.with(|c| {
            c.live.set(0);
            c.limit.set(limit);
            c.trip_at.set(limit);
            c.beside.set(0);
            c.passed.set(false);
        });
        GUARD.with(|g| *g.borrow_mut() = guard.cloned());
    }

    /// Stop comparing with the limit of the evaluation [`arm`] started,
    /// so nothing done on this thread afterwards (dropping the results,
    /// another request) can trip that request's guard.
    pub fn disarm() {
        COUNT.with(|c| {
            c.limit.set(u64::MAX);
            c.trip_at.set(u64::MAX);
            c.beside.set(0);
            c.passed.set(false);
        });
        GUARD.with(|g| *g.borrow_mut() = None);
    }

    /// `bytes` more alive; trips the memory limit when the count passes
    /// it.
    #[inline]
    pub fn charge(bytes: u64) {
        COUNT.with(|c| {
            let n = c.live.get().saturating_add(bytes);
            c.live.set(n);
            if n > c.trip_at.get() {
                trip(c, n);
            }
        });
    }

    /// The count has passed the limit: record it on the guard and raise
    /// the request's interrupt flag.
    #[cold]
    #[inline(never)]
    fn trip(c: &Count, n: u64) {
        c.trip_at.set(u64::MAX);
        c.passed.set(true);
        let total = n.saturating_add(c.beside.get());
        GUARD.with(|g| {
            if let Some(g) = &*g.borrow()
                && let Some(e) = g.memory_exceeds(total, "the evaluation")
            {
                g.trip(e);
            }
        });
    }

    /// `bytes` no longer alive.
    #[inline]
    pub fn credit(bytes: u64) {
        COUNT.with(|c| c.live.set(c.live.get().saturating_sub(bytes)));
    }

    /// Whether the memory limit has been passed during this evaluation:
    /// what a loop that materialises values checks to stop early.
    #[inline]
    pub fn over() -> bool {
        COUNT.with(|c| c.passed.get())
    }

    /// Whether `extra` bytes on top of the count (a buffer being filled
    /// that is not a value yet, like the text of a value being printed)
    /// pass the limit. When they do, the limit trips as a charge would.
    #[inline]
    pub fn passes(extra: u64) -> bool {
        COUNT.with(|c| {
            if c.passed.get() {
                return true;
            }
            let n = c.live.get().saturating_add(extra);
            if n > c.limit.get().saturating_sub(c.beside.get()) {
                trip(c, n);
                return true;
            }
            false
        })
    }

    /// `bytes` the evaluator counts itself (the nodes it has built) on top
    /// of the values: the next charge trips once the two together pass
    /// the limit. Called at the evaluator's periodic limit checks.
    pub fn beside(bytes: u64) {
        COUNT.with(|c| {
            c.beside.set(bytes);
            if !c.passed.get() && c.limit.get() != u64::MAX {
                c.trip_at.set(c.limit.get().saturating_sub(bytes));
            }
        });
    }

    pub fn get() -> u64 {
        COUNT.with(|c| c.live.get())
    }

    /// Zero the count (unarmed).
    pub fn reset() {
        COUNT.with(|c| c.live.set(0));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flags_parse_and_refuse() {
        let mut l = Limits::NONE;
        l.apply_flag("fragments=20000").unwrap();
        l.apply_flag("memory=2G").unwrap();
        l.apply_flag("time=0.5").unwrap();
        assert_eq!(l.fragments, Some(20_000));
        assert_eq!(l.memory, Some(2 << 30));
        assert_eq!(l.time, Some(0.5));
        l.apply_flag("fragments=off").unwrap();
        assert_eq!(l.fragments, None);
        assert!(l.apply_flag("fragments").is_err());
        assert!(l.apply_flag("fragments=-1").is_err());
        assert!(l.apply_flag("frags=1").unwrap_err().contains("fragments"));
    }

    #[test]
    fn a_guard_trips_once_and_interrupts() {
        let flag = Arc::new(AtomicBool::new(false));
        let g = Guard::new(Limits::AGENT, flag.clone(), None);
        assert!(g.check(Limit::Fragments, 10_000.0, "circle()").is_ok());
        let e = g.check(Limit::Fragments, 1e5, "sphere()").unwrap_err();
        assert!(flag.load(Ordering::Relaxed));
        assert_eq!(
            e.message(),
            "Resource limit exceeded: sphere() would make 100,000 fragments, over the fragments limit of 10,000"
        );
        assert!(e.hint().contains("--limit fragments=N"));
        // The first limit passed is the one reported.
        let _ = g.check(Limit::Rands, 1e9, "rands()");
        assert_eq!(g.exceeded().unwrap().limit, Limit::Fragments);
    }

    #[test]
    fn time_runs_out_on_the_hosts_clock() {
        let now = Arc::new(Mutex::new(0.0));
        let n = now.clone();
        let clock: Clock = Arc::new(move || *n.lock().unwrap());
        let mut limits = Limits::NONE;
        limits.time = Some(1.0);
        let g = Guard::new(limits, Arc::new(AtomicBool::new(false)), Some(clock));
        assert!(!g.stopped());
        *now.lock().unwrap() = 1500.0;
        assert!(g.stopped());
        assert_eq!(g.exceeded().unwrap().limit, Limit::Time);
    }
}
