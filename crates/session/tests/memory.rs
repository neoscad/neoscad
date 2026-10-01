//! The measured memory limit in a session (`session::memory`), with a
//! probe these tests control: it reads a fixed base plus the session's
//! cached geometry, as a native host's footprint would grow with the
//! cache, so eviction lowers the reading.
//!
//! The configurations have no clock, so the probe is read at every check
//! (the 10 ms interval needs one) and the tests do not depend on timing.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock, Weak};

use lang::loader::LibraryPath;
use lang::vfs::MemFs;
use session::{Config, Limits, MemoryProbe, Mode, Run, Session};

const MIB: u64 = 1 << 20;

/// What the fake probe reads: `base` bytes, plus the session's cached
/// geometry when `with_cache`.
#[derive(Default)]
struct Fake {
    session: OnceLock<Weak<Session>>,
    base: AtomicU64,
    with_cache: bool,
    calls: AtomicU64,
}

impl Fake {
    fn probe(self: &Arc<Self>) -> MemoryProbe {
        let f = self.clone();
        Arc::new(move || {
            f.calls.fetch_add(1, Ordering::Relaxed);
            let cache = match (f.with_cache, f.session.get().and_then(Weak::upgrade)) {
                (true, Some(s)) => s.stats().geometry.bytes as u64,
                _ => 0,
            };
            f.base.load(Ordering::Relaxed) + cache
        })
    }
}

fn files() -> Arc<MemFs> {
    let fs = Arc::new(MemFs::new());
    fs.insert("/doc/big.scad", b"sphere(10, $fn = 600);\n".to_vec());
    fs.insert(
        "/doc/small.scad",
        b"difference() { cube(2); sphere(1, $fn = 24); }\n".to_vec(),
    );
    fs
}

fn session(fs: &Arc<MemFs>, fake: Option<&Arc<Fake>>) -> Arc<Session> {
    let mut cfg = Config::new(fs.clone(), LibraryPath(Vec::new()));
    cfg.work_dir = PathBuf::from("/doc");
    cfg.memory_probe = fake.map(Fake::probe);
    let s = Arc::new(Session::new(cfg));
    if let Some(f) = fake {
        let _ = f.session.set(Arc::downgrade(&s));
    }
    s
}

fn render(s: &Session, file: &str, memory: Option<u64>) -> session::Rendered {
    let mut run = Run::new(file);
    run.limits = Some(Limits {
        memory,
        ..Limits::NONE
    });
    s.render(&run, Mode::Render, &render::ColorScheme::cornfield())
        .expect("not cancelled")
}

#[test]
fn cached_geometry_is_evicted_before_a_request_fails() {
    let fs = files();
    let fake = Arc::new(Fake {
        with_cache: true,
        ..Fake::default()
    });
    fake.base.store(MIB, Ordering::Relaxed);
    let s = session(&fs, Some(&fake));
    assert_eq!(render(&s, "big.scad", None).exit_code, 0);
    let cached = s.stats().geometry.bytes as u64;
    assert!(cached > 64 << 10, "the sphere fills the cache: {cached}");
    // The process is over the limit only by what the cache holds: a
    // request evicts, measures again and goes on.
    let limit = (MIB + cached * 3 / 4).next_multiple_of(MIB);
    assert!(limit < MIB + cached, "{cached}");
    let r = render(&s, "small.scad", Some(limit));
    assert_eq!(r.exit_code, 0, "{}", String::from_utf8_lossy(&r.log.stderr));
    let g = s.stats().geometry;
    assert!((g.bytes as u64) < cached, "{g:?}");
    assert!(fake.calls.load(Ordering::Relaxed) > 0);
}

#[test]
fn a_measurement_over_the_limit_stops_the_request() {
    // A native host's probe, over the limit whatever the caches hold: the
    // request stops with a measured resource-limit error, after one round
    // of eviction that did not help.
    let fs = files();
    let fake = Arc::new(Fake::default());
    fake.base.store(1 << 30, Ordering::Relaxed);
    let s = session(&fs, Some(&fake));
    assert_eq!(render(&s, "big.scad", None).exit_code, 0);
    let cached = s.stats().geometry.bytes;
    let r = render(&s, "small.scad", Some(512 * MIB));
    assert_eq!(r.exit_code, 1);
    let d = r.log.diagnostics_json();
    assert_eq!(d[0]["code"], "resource-limit", "{d:?}");
    let msg = d[0]["message"].as_str().unwrap();
    assert!(
        msg.contains("uses 1,024 MiB") && msg.contains("(measured)"),
        "{msg}"
    );
    // Eviction was tried (the sphere's one entry is dropped whole).
    let after = s.stats().geometry.bytes;
    assert!(after < cached, "{cached} -> {after}");
    // Under the limit, the same session renders again.
    fake.base.store(MIB, Ordering::Relaxed);
    assert_eq!(render(&s, "small.scad", Some(512 * MIB)).exit_code, 0);
}

#[test]
fn nothing_changes_under_the_limit_or_without_one() {
    let fs = files();
    let plain = session(&fs, None);
    let fake = Arc::new(Fake::default());
    fake.base.store(MIB, Ordering::Relaxed);
    let probed = session(&fs, Some(&fake));
    let scheme = render::ColorScheme::cornfield().geometry_scheme();
    for file in ["big.scad", "small.scad"] {
        let want = render(&plain, file, None);
        // Without a memory limit the probe is never read.
        let calls = fake.calls.load(Ordering::Relaxed);
        let r = render(&probed, file, None);
        assert_eq!(fake.calls.load(Ordering::Relaxed), calls);
        assert_eq!(r.log.stderr, want.log.stderr);
        assert_eq!(r.geometry_json(&scheme), want.geometry_json(&scheme));
        // Under it, it is read and changes nothing.
        let r = render(&probed, file, Some(512 * MIB));
        assert_eq!(r.exit_code, 0);
        assert_eq!(r.log.stderr, want.log.stderr);
        assert_eq!(r.geometry_json(&scheme), want.geometry_json(&scheme));
    }
    assert!(fake.calls.load(Ordering::Relaxed) > 0);
    assert_eq!(probed.stats().geometry.evictions, 0);
}
