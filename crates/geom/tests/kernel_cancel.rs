//! One kernel operation honours a cancel and the limits: the request's
//! interrupt flag and its guard reach inside a boolean
//! (`geom::manifold_geom::kernel_token`), so a single huge boolean stops
//! instead of running to its end. On wasm32 running to the end could
//! mean growing past the address space, which traps the instance.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use geom::manifold_geom::{GlobalIds, ManifoldGeometry, OpType, kernel_token};
use geom::{Geometry, RenderOptions, Renderer};

fn tree(src: &str) -> eval::Evaluation {
    let path = PathBuf::from("/nonexistent/test.scad");
    let mut text = src.as_bytes().to_vec();
    text.extend_from_slice(b"\n\x03\n");
    let program = lang::parse_file(path, text);
    assert!(!program.has_syntax_errors(), "syntax error in test program");
    let mut out = eval::Collect::default();
    eval::with_stack(eval::DEFAULT_THREAD_STACK, || {
        eval::evaluate(
            &program,
            &[],
            &[],
            PathBuf::from("/nonexistent"),
            &eval::Options::default(),
            &mut out,
        )
    })
}

/// One boolean of two dense spheres: nearly all of the render's time is
/// in that single kernel operation, so only a check inside it can stop
/// the render early.
const ONE_BOOLEAN: &str =
    "difference() { sphere(10, $fn = 160); translate([1, 1, 1]) sphere(10, $fn = 160); }";

/// The rendered solid as comparable data: its triangles' vertex
/// positions, in output order.
fn mesh(g: &Option<Geometry>) -> Vec<[u64; 3]> {
    let Some(Geometry::Manifold(m)) = g else {
        panic!("expected a solid, got {}", g.is_some());
    };
    let ps = m.to_polyset(&geom::color::CORNFIELD);
    ps.faces
        .iter()
        .flatten()
        .map(|&i| ps.vertices[i as usize].map(f64::to_bits))
        .collect()
}

fn render(
    r: &Renderer,
    src: &str,
    opts: RenderOptions,
) -> Result<geom::Rendered, geom::Unsupported> {
    let ev = tree(src);
    let keys = eval::dump::Keys::new(&ev.root, &lang::loader::StdFs);
    r.render(&ev.root, &keys, opts)
}

/// The two operands of [`ONE_BOOLEAN`] as solids.
fn operands() -> (ManifoldGeometry, ManifoldGeometry) {
    let solid = |src: &str| {
        let g = render(&Renderer::new(), src, RenderOptions::default()).expect("operand");
        let Some(Geometry::PolySet(ps)) = g.geometry else {
            panic!("expected a mesh");
        };
        let (mut w, mut e) = (Vec::new(), Vec::new());
        ManifoldGeometry::from_polyset(&ps, &GlobalIds, &mut w, &mut e)
    };
    (
        solid("sphere(10, $fn = 160);"),
        solid("translate([1, 1, 1]) sphere(10, $fn = 160);"),
    )
}

/// A cancel that lands inside one boolean stops it there: the kernel looks
/// at the token between its stages and inside its long loops. The cancel
/// is triggered at a check count, not after a sleep: timed, a busy machine
/// (a shared CI runner, Nix's sandbox) can let the boolean finish before
/// the cancel or stall it after, which says nothing about the checks.
#[test]
fn a_cancel_stops_a_single_boolean() {
    let (a, b) = operands();
    let full = a.boolean(&b, OpType::Subtract);
    assert!(!full.is_empty() && !full.is_cancelled());

    // Count the checks a whole boolean makes, with a token that never fires.
    let total = Arc::new(AtomicUsize::new(0));
    let counter = total.clone();
    let token = kernel_token(Some(&Arc::new(AtomicBool::new(false))), None)
        .expect("a token")
        .with_check(Arc::new(move || {
            counter.fetch_add(1, Ordering::Relaxed);
            false
        }));
    let uncancelled = a.boolean_until(&b, OpType::Subtract, Some(&token));
    assert!(!uncancelled.is_cancelled() && !uncancelled.is_empty());
    let total = total.load(Ordering::Relaxed);
    assert!(total >= 4, "only {total} checks in a whole boolean");

    // Fire at half of them: the boolean is stopped in the middle, and the
    // kernel makes no more than a few checks after the token fires.
    let fire_at = total / 2;
    let seen = Arc::new(AtomicUsize::new(0));
    let counter = seen.clone();
    let token = kernel_token(Some(&Arc::new(AtomicBool::new(false))), None)
        .expect("a token")
        .with_check(Arc::new(move || {
            counter.fetch_add(1, Ordering::Relaxed) + 1 >= fire_at
        }));
    let out = a.boolean_until(&b, OpType::Subtract, Some(&token));
    assert!(out.is_cancelled() && out.is_empty());
    let seen = seen.load(Ordering::Relaxed);
    assert!(
        seen < total,
        "{seen} checks before stopping, of {total} in a whole boolean"
    );
}

/// A guard whose memory probe reads over its 1 GiB limit from its
/// `fire_at`-th call on (never, with `usize::MAX`), counting its calls in
/// `calls`. Stopping at a call count, not after a sleep, makes the stop
/// land inside the boolean on any machine (a timed one let the boolean
/// finish first on a fast Mac).
fn probe_guard(calls: Arc<AtomicUsize>, fire_at: usize) -> Arc<eval::limits::Guard> {
    let probe: eval::limits::MemoryProbe = Arc::new(move || {
        if calls.fetch_add(1, Ordering::Relaxed) + 1 >= fire_at {
            2 << 30
        } else {
            0
        }
    });
    let limits = eval::limits::Limits {
        memory: Some(1 << 30),
        ..Default::default()
    };
    Arc::new(
        eval::limits::Guard::new(limits, Arc::new(AtomicBool::new(false)), None)
            .with_probe(Some(probe)),
    )
}

/// The probe calls a whole render of `src` on `renderer` makes.
fn probe_calls(renderer: &Renderer, src: &str) -> usize {
    let calls = Arc::new(AtomicUsize::new(0));
    let guard = probe_guard(calls.clone(), usize::MAX);
    let out = render(
        renderer,
        src,
        RenderOptions {
            guard: Some(guard),
            ..Default::default()
        },
    );
    assert!(out.is_ok(), "a whole render");
    let n = calls.load(Ordering::Relaxed);
    assert!(n >= 4, "only {n} probe calls in a whole render");
    n
}

/// A render cancelled inside its boolean is interrupted, and the
/// cancelled (empty) solid is not cached: the next render on the same
/// renderer gives what a fresh one does.
#[test]
fn a_cancelled_boolean_is_not_cached() {
    let full = render(&Renderer::new(), ONE_BOOLEAN, RenderOptions::default()).expect("full");
    let expected = mesh(&full.geometry);
    // The operands are cached; the flag is raised while the boolean runs.
    let r = Renderer::new();
    render(
        &r,
        "sphere(10, $fn = 160); translate([1, 1, 1]) sphere(10, $fn = 160);",
        RenderOptions::default(),
    )
    .expect("operands");
    // The same state on a second renderer gives the probe calls a whole
    // render makes; stopping at half of them lands inside the boolean.
    let counting = Renderer::new();
    render(
        &counting,
        "sphere(10, $fn = 160); translate([1, 1, 1]) sphere(10, $fn = 160);",
        RenderOptions::default(),
    )
    .expect("operands");
    let total = probe_calls(&counting, ONE_BOOLEAN);
    let out = render(
        &r,
        ONE_BOOLEAN,
        RenderOptions {
            guard: Some(probe_guard(Arc::new(AtomicUsize::new(0)), total / 2)),
            ..Default::default()
        },
    );
    assert!(out.is_err_and(|u| u.is_interrupted()));
    let again = render(&r, ONE_BOOLEAN, RenderOptions::default()).expect("again");
    assert_eq!(
        mesh(&again.geometry),
        expected,
        "nothing cancelled was cached"
    );
}

/// A memory measurement that passes the limit while the boolean runs (a
/// host's probe: the web core's counting allocator) stops it with the
/// memory limit recorded as measured, as the web core reports it.
#[test]
fn a_measured_memory_limit_stops_a_single_boolean() {
    let total = probe_calls(&Renderer::new(), ONE_BOOLEAN);
    let guard = probe_guard(Arc::new(AtomicUsize::new(0)), total / 2);
    let out = render(
        &Renderer::new(),
        ONE_BOOLEAN,
        RenderOptions {
            guard: Some(guard.clone()),
            ..Default::default()
        },
    );
    assert!(out.is_err_and(|u| u.is_interrupted()));
    let e = guard.exceeded().expect("a limit");
    assert_eq!(e.limit, eval::limits::Limit::Memory);
    assert!(e.measured);
}

/// A token that never fires changes nothing: the kernel's cancellable
/// path gives the same triangles, in the same order, as the
/// uncancellable one, at one thread and at eight.
#[test]
fn an_unfired_token_changes_nothing() {
    let src = "difference() { cube(10, center = true); \
               for (i = [0:5]) rotate([0, 0, i * 30]) cylinder(r = 1 + i / 4, h = 12, center = true, $fn = 24); \
               sphere(6.5, $fn = 40); }";
    let limited = || {
        let flag = Arc::new(AtomicBool::new(false));
        let limits = eval::limits::Limits {
            memory: Some(1 << 40),
            ..Default::default()
        };
        RenderOptions {
            interrupt: Some(flag.clone()),
            guard: Some(Arc::new(eval::limits::Guard::new(limits, flag, None))),
            ..Default::default()
        }
    };
    let plain = mesh(
        &render(&Renderer::new(), src, RenderOptions::default())
            .expect("plain")
            .geometry,
    );
    for threads in [1, 8] {
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(threads)
            .build()
            .expect("pool");
        let tokened = pool.install(|| render(&Renderer::new(), src, limited()).expect("limited"));
        assert_eq!(mesh(&tokened.geometry), plain, "{threads} threads");
    }
}
