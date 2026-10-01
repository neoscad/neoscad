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
#[derive(Default)]
pub(crate) struct Pressure {
    renderers: Renderers,
    /// The clock reading and bytes of the last probe.
    last: Mutex<Option<(f64, u64)>>,
    /// Held while caches are evicted, so threads that find the process
    /// over the limit at once evict in turn, each measuring again first,
    /// instead of all emptying the caches together.
    relieving: Mutex<()>,
}

impl Pressure {
    pub(crate) fn new(renderers: Renderers) -> Pressure {
        Pressure {
            renderers,
            ..Pressure::default()
        }
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
fn read(state: &Pressure, probe: &MemoryProbe, clock: Option<&Clock>) -> u64 {
    let now = clock.map(|c| c());
    let mut last = state.last.lock().unwrap_or_else(PoisonError::into_inner);
    if let (Some(t), Some((at, bytes))) = (now, *last)
        && t - at < PROBE_INTERVAL_MS
        && t >= at
    {
        return bytes;
    }
    let bytes = probe();
    *last = now.map(|t| (t, bytes));
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
    *state.last.lock().unwrap_or_else(PoisonError::into_inner) = clock.map(|c| (c(), used));
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
