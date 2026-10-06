//! The measured memory limit: the host's probe, read at most every
//! [`PROBE_INTERVAL_MS`], and the geometry caches that give memory back
//! before a measurement fails a request.
//!
//! A native host's probe measures the whole process (the app's footprint,
//! the server's resident set), so the memory limit is a budget that every
//! document, request and cache of the process shares. Measured alone, an
//! app whose renderers had each filled their 200 MiB geometry cache would
//! fail every later request near the limit, though most of that memory is
//! only kept for speed. So when a reading is over the limit, the session
//! first evicts the least recently used half of every renderer's cache,
//! asks the host to hand freed memory back to the system
//! ([`crate::Config::memory_release`]) and measures again, until the
//! reading is under the limit or eviction stops helping; only then does
//! the request fail. The request that fails is whichever one checked, not
//! necessarily the one that grew: the measurement cannot tell them apart.
//!
//! Nothing here changes what a request makes: eviction only drops cached
//! results, which are computed again the same way, and a reading under the
//! limit does nothing.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use crate::{Clock, MemoryProbe};

/// The least time between two readings of the host's probe. The guard
/// checks the memory limit as often as it reads the clock (each geometry
/// node and every ring of a primitive), and a native probe is a system
/// call (on Linux, reading a file in `/proc`); the footprint does not
/// change much in 10 ms, and a stale reading only delays a trip.
pub const PROBE_INTERVAL_MS: f64 = 10.0;

/// A cache below this many (estimated) bytes is emptied rather than
/// halved, so eviction ends in a few rounds.
const SMALL_CACHE: usize = 1 << 20;

/// The renderers of a session, shared with its requests' probes.
pub(crate) type Renderers = Arc<Mutex<Vec<(u64, Arc<geom::Renderer>)>>>;

/// What a session keeps for its probe: the last reading (for the
/// interval) and the caches that eviction shrinks.
pub(crate) struct Pressure {
    renderers: Renderers,
    /// The last probe's reading, kept in atomics so that a check finding
    /// a recent one takes no lock. Every kernel check of every thread
    /// reads it (`Guard::stopped`), and a mutex here made a 14-thread
    /// render spend most of its time waiting on it (2.4 times slower
    /// under a memory limit than without one).
    last: Last,
    /// Set while one thread probes again, so a stale reading is replaced
    /// by one probe rather than one per thread that noticed.
    probing: AtomicBool,
    /// Held while caches are evicted, so threads that find the process
    /// over the limit at once evict in turn, each measuring again first,
    /// instead of all emptying the caches together.
    relieving: Mutex<()>,
}

impl Default for Pressure {
    fn default() -> Pressure {
        Pressure::new(Renderers::default())
    }
}

impl Pressure {
    pub(crate) fn new(renderers: Renderers) -> Pressure {
        Pressure {
            renderers,
            last: Last::default(),
            probing: AtomicBool::new(false),
            relieving: Mutex::new(()),
        }
    }
}

/// The clock reading and bytes of the last probe, as two atomics.
///
/// The pair is not read as one, so the order of the stores and loads is
/// what keeps it honest: the bytes are stored before the time (which is
/// stored with `Release`), and the time is loaded first (with
/// `Acquire`), so the bytes a reader sees are from that probe or a later
/// one, never older than the time says. A reading is only ever used as
/// "recent enough", so a newer one is as good.
struct Last {
    /// The `f64` bits of the clock reading; [`Last::NONE`] before the
    /// first probe.
    at: AtomicU64,
    bytes: AtomicU64,
}

impl Default for Last {
    fn default() -> Last {
        Last {
            at: AtomicU64::new(Last::NONE),
            bytes: AtomicU64::new(0),
        }
    }
}

impl Last {
    /// No reading yet. These are the bits of a NaN, which every
    /// comparison in [`read`] fails, so it is never taken as recent.
    const NONE: u64 = u64::MAX;

    /// The time and bytes of the last reading, if there is one.
    fn get(&self) -> Option<(f64, u64)> {
        let at = self.at.load(Ordering::Acquire);
        (at != Last::NONE).then(|| (f64::from_bits(at), self.bytes.load(Ordering::Relaxed)))
    }

    fn set(&self, at: f64, bytes: u64) {
        self.bytes.store(bytes, Ordering::Relaxed);
        self.at.store(at.to_bits(), Ordering::Release);
    }
}

/// The probe one request's guard checks against its memory limit of
/// `limit` bytes: `probe` read at most every [`PROBE_INTERVAL_MS`] on
/// `clock` (each read without a clock), with the caches in `state`
/// evicted before a reading over the limit is returned.
pub(crate) fn request_probe(
    state: Arc<Pressure>,
    probe: MemoryProbe,
    release: Option<crate::MemoryRelease>,
    clock: Option<Clock>,
    limit: u64,
) -> MemoryProbe {
    Arc::new(move || {
        let used = read(&state, &probe, clock.as_ref());
        if used <= limit {
            return used;
        }
        relieve(&state, &probe, release.as_ref(), clock.as_ref(), limit)
    })
}

/// `probe` read at most every [`PROBE_INTERVAL_MS`] on `clock`, for a
/// guard outside any session (the one-shot command line's), which has no
/// caches to evict.
pub fn throttled(probe: MemoryProbe, clock: Clock) -> MemoryProbe {
    request_probe(Arc::default(), probe, None, Some(clock), u64::MAX)
}

/// The probe's reading, or the last one if it is under the interval old.
///
/// Without a clock every call probes, as there is no interval to
/// measure. No shipped host gives a probe without a clock (the command
/// line, the apps' core and the web build all pass one), so that path is
/// for embedders and tests, not the hot one.
fn read(state: &Pressure, probe: &MemoryProbe, clock: Option<&Clock>) -> u64 {
    let Some(clock) = clock else {
        return probe();
    };
    // The reading is loaded before the clock is read: a thread that read
    // its clock first could find a reading another thread stamped after
    // that, fail the `t >= at` check below (which is there for a clock
    // that went backwards) and probe again needlessly.
    let last = state.last.get();
    let t = clock();
    if let Some((at, bytes)) = last
        && t - at < PROBE_INTERVAL_MS
        && t >= at
    {
        return bytes;
    }
    // Stale: one thread probes again, and the others use the last reading
    // meanwhile; they see the new one as soon as that probe returns, so a
    // stale reading outlives the interval by one probe's duration at
    // most. Before the first reading there is nothing to use, so every
    // thread probes.
    let won = state
        .probing
        .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
        .is_ok();
    if !won && let Some((_, bytes)) = last {
        return bytes;
    }
    // The flag is cleared on the way out even if the probe panics: left
    // set, every later check would use this stale reading forever. Only
    // the thread that set it clears it; one that probes without it (before
    // the first reading) must not end another's turn.
    struct Done<'a>(&'a AtomicBool);
    impl Drop for Done<'_> {
        fn drop(&mut self) {
            self.0.store(false, Ordering::Release);
        }
    }
    let _done = won.then_some(Done(&state.probing));
    // A thread that found the reading stale may win its turn just after
    // another's probe replaced it; that reading is fresh, so take it.
    if won
        && let Some((at, bytes)) = state.last.get()
        && t - at < PROBE_INTERVAL_MS
        && t >= at
    {
        return bytes;
    }
    let bytes = probe();
    state.last.set(t, bytes);
    bytes
}

/// Evict cached geometry until the probe reads at most `limit`, eviction
/// leaves nothing to evict, or a round gives nothing back (as with a probe
/// of the request's peak, which eviction cannot lower); the last reading.
fn relieve(
    state: &Pressure,
    probe: &MemoryProbe,
    release: Option<&crate::MemoryRelease>,
    clock: Option<&Clock>,
    limit: u64,
) -> u64 {
    let _turn = state
        .relieving
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    // Another thread may have evicted while this one waited. Memory the
    // allocator holds freed (a stopped request's, say) is handed back
    // first, as it costs no cached work.
    let mut used = probe();
    if used > limit
        && let Some(r) = release
    {
        r();
        used = probe();
    }
    while used > limit {
        if evict_half(&state.renderers) == 0 {
            break;
        }
        if let Some(r) = release {
            r();
        }
        let now = probe();
        let helped = now < used;
        used = now;
        if !helped {
            break;
        }
    }
    if let Some(c) = clock {
        state.last.set(c(), used);
    }
    used
}

/// Drop the least recently used half (by estimated size) of every
/// renderer's geometry cache, or all of one that will not halve, keeping
/// each one's budget; the estimated bytes dropped.
///
/// The renderers' list and each cache are locked here from within a
/// request's limit check. That is safe because neither lock is held while
/// a guard is checked: the session takes the list only to pick or count
/// renderers, and `geom` takes a cache's lock only to look up or insert
/// one entry.
pub(crate) fn evict_half(renderers: &Renderers) -> usize {
    let rs: Vec<Arc<geom::Renderer>> = renderers
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .iter()
        .map(|(_, r)| r.clone())
        .collect();
    let mut dropped = 0;
    for r in rs {
        let before = r.stats();
        if before.bytes == 0 {
            continue;
        }
        let target = if before.bytes < SMALL_CACHE {
            0
        } else {
            before.bytes / 2
        };
        r.set_budget(target);
        r.set_budget(before.budget);
        let mut after = r.stats().bytes;
        if after >= before.bytes {
            // The cache never evicts its last entry by budget (one big
            // model is often the whole cache), so it is dropped here.
            r.clear();
            after = 0;
        }
        dropped += before.bytes.saturating_sub(after);
    }
    dropped
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Barrier;

    /// A clock that reads whatever the test sets, and a probe that counts
    /// its calls and reads `100 + calls` bytes, so a test can tell which
    /// probe a reading came from.
    struct Fake {
        now: AtomicU64,
        calls: AtomicU64,
    }

    fn fake() -> (Arc<Fake>, MemoryProbe, Clock) {
        let f = Arc::new(Fake {
            now: AtomicU64::new(0f64.to_bits()),
            calls: AtomicU64::new(0),
        });
        let (p, c) = (f.clone(), f.clone());
        let probe: MemoryProbe = Arc::new(move || 101 + p.calls.fetch_add(1, Ordering::SeqCst));
        let clock: Clock = Arc::new(move || f64::from_bits(c.now.load(Ordering::SeqCst)));
        (f, probe, clock)
    }

    impl Fake {
        fn set(&self, ms: f64) {
            self.now.store(ms.to_bits(), Ordering::SeqCst);
        }
        fn calls(&self) -> u64 {
            self.calls.load(Ordering::SeqCst)
        }
    }

    /// Sixteen threads read 200 times each, all starting together; the
    /// readings.
    fn read_together(state: &Arc<Pressure>, probe: &MemoryProbe, clock: &Clock) -> Vec<u64> {
        let (threads, times) = (16, 200);
        let start = Arc::new(Barrier::new(threads));
        let handles: Vec<_> = (0..threads)
            .map(|_| {
                let (state, probe, clock, start) =
                    (state.clone(), probe.clone(), clock.clone(), start.clone());
                std::thread::spawn(move || {
                    start.wait();
                    (0..times)
                        .map(|_| read(&state, &probe, Some(&clock)))
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        handles
            .into_iter()
            .flat_map(|h| h.join().unwrap())
            .collect()
    }

    /// Concurrent checks within one interval share one probe's reading,
    /// and a stale reading is probed again once, not once per thread.
    #[test]
    fn concurrent_reads_probe_once_per_interval() {
        let (f, probe, clock) = fake();
        let state = Arc::new(Pressure::default());
        // Before the first reading every thread that finds none probes,
        // so the first reading is taken before the threads start.
        f.set(1000.0);
        assert_eq!(read(&state, &probe, Some(&clock)), 101);

        // Inside the interval: the cached reading, no probe.
        f.set(1000.0 + PROBE_INTERVAL_MS / 2.0);
        let cached = read_together(&state, &probe, &clock);
        assert!(cached.iter().all(|&b| b == 101), "{cached:?}");
        assert_eq!(f.calls(), 1);

        // Past it: exactly one more probe. A thread that lost the turn
        // uses the old reading while the probe runs.
        f.set(1000.0 + PROBE_INTERVAL_MS);
        let next = read_together(&state, &probe, &clock);
        assert_eq!(f.calls(), 2);
        assert!(next.iter().all(|&b| b == 101 || b == 102), "{next:?}");
        assert_eq!(read(&state, &probe, Some(&clock)), 102);
        assert_eq!(f.calls(), 2);
    }

    /// A clock that reads earlier than the last reading's time (it went
    /// backwards) does not keep that reading: the probe is read again.
    #[test]
    fn a_clock_that_went_backwards_probes_again() {
        let (f, probe, clock) = fake();
        let state = Pressure::default();
        f.set(500.0);
        assert_eq!(read(&state, &probe, Some(&clock)), 101);
        f.set(499.0);
        assert_eq!(read(&state, &probe, Some(&clock)), 102);
        assert_eq!(f.calls(), 2);
    }

    /// Without a clock every check probes.
    #[test]
    fn without_a_clock_every_read_probes() {
        let (f, probe, _) = fake();
        let state = Pressure::default();
        for _ in 0..5 {
            read(&state, &probe, None);
        }
        assert_eq!(f.calls(), 5);
    }
}
