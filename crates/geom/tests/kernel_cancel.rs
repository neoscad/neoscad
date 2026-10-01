//! One kernel operation honours a cancel and the limits: the request's
//! interrupt flag and its guard reach inside a boolean
//! (`geom::manifold_geom::kernel_token`), so a single huge boolean stops
//! instead of running to its end. On wasm32 running to the end could
//! mean growing past the address space, which traps the instance.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

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

/// A cancel that lands inside one boolean stops it within a bounded time:
/// the kernel looks at the request's flag between its stages and inside
/// its long loops.
#[test]
fn a_cancel_stops_a_single_boolean() {
    let (a, b) = operands();
    let started = Instant::now();
    let full = a.boolean(&b, OpType::Subtract);
    let full_time = started.elapsed();
    assert!(!full.is_empty() && !full.is_cancelled());

    let flag = Arc::new(AtomicBool::new(false));
    let token = kernel_token(Some(&flag), None).expect("a token");
    let canceller = std::thread::spawn(move || {
        std::thread::sleep(full_time / 4);
        flag.store(true, Ordering::Relaxed);
        Instant::now()
    });
    let out = a.boolean_until(&b, OpType::Subtract, Some(&token));
    let stopped = Instant::now();
    let latency = stopped.saturating_duration_since(canceller.join().expect("canceller"));
    eprintln!("one boolean {full_time:?}; stopped {latency:?} after the cancel");
    assert!(out.is_cancelled() && out.is_empty());
    // Unchecked, it ran the remaining three quarters; checked, it runs
    // to the next check, a small part of one stage.
    assert!(
        latency < full_time / 4,
        "stopped {latency:?} after the cancel, of {full_time:?}"
    );
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
    let (a, b) = operands();
    let started = Instant::now();
    let _ = a.boolean(&b, OpType::Subtract);
    let boolean_time = started.elapsed();
    let flag = Arc::new(AtomicBool::new(false));
    let canceller = {
        let flag = flag.clone();
        std::thread::spawn(move || {
            std::thread::sleep(boolean_time / 4);
            flag.store(true, Ordering::Relaxed);
        })
    };
    let out = render(
        &r,
        ONE_BOOLEAN,
        RenderOptions {
            interrupt: Some(flag),
            ..Default::default()
        },
    );
    canceller.join().expect("canceller");
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
    let over = Arc::new(AtomicBool::new(false));
    let probe: eval::limits::MemoryProbe = {
        let over = over.clone();
        Arc::new(move || {
            if over.load(Ordering::Relaxed) {
                2 << 30
            } else {
                0
            }
        })
    };
    let flag = Arc::new(AtomicBool::new(false));
    let limits = eval::limits::Limits {
        memory: Some(1 << 30),
        ..Default::default()
    };
    let guard =
        Arc::new(eval::limits::Guard::new(limits, flag.clone(), None).with_probe(Some(probe)));
    // The boolean starts, then the "allocator" passes the limit.
    let raiser = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(50));
        over.store(true, Ordering::Relaxed);
    });
    let out = render(
        &Renderer::new(),
        ONE_BOOLEAN,
        RenderOptions {
            interrupt: Some(flag),
            guard: Some(guard.clone()),
            ..Default::default()
        },
    );
    raiser.join().expect("raiser");
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
