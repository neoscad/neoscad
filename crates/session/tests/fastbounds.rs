//! The fast path for `child_bounds()` (`geom::fastbounds`) against the
//! render it replaces: generated models (primitives, transforms, unions,
//! hulls, extrusions, modifiers, and the operations it must leave to the
//! renderer) are rendered subtree by subtree, and wherever the fast path
//! gives a box it must be the rendered box bit for bit
//! (`docs/language-extensions.md`, sections 9 item 8 and 11.6).
//!
//! Bounded: the generator caps depth, children and `$fn`; every render
//! runs under `Limits::AGENT`; and a watcher aborts the process past 2 GB
//! resident.

use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Once};
use std::time::Duration;

use eval::oracle::Bounds;
use geom::{RenderOptions, Renderer};

fn watch_memory() {
    static START: Once = Once::new();
    START.call_once(|| {
        std::thread::spawn(|| {
            loop {
                let mb = rss_mb();
                if mb > 2048 {
                    eprintln!("fastbounds: {mb} MB resident, over the 2 GB guard; aborting");
                    std::process::abort();
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        });
    });
}

fn rss_mb() -> u64 {
    std::process::Command::new("ps")
        .args(["-o", "rss=", "-p", &std::process::id().to_string()])
        .output()
        .ok()
        .and_then(|o| {
            String::from_utf8_lossy(&o.stdout)
                .trim()
                .parse::<u64>()
                .ok()
        })
        .map_or(0, |kb| kb / 1024)
}

/// splitmix64: the same models on every run and platform.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }

    /// A number as models write them: integers, decimals that do not
    /// round-trip exactly in binary, and thirds.
    fn num(&mut self, lo: f64, hi: f64) -> String {
        let t = (self.below(10_000) as f64) / 10_000.0;
        let x = lo + (hi - lo) * t;
        match self.below(4) {
            0 => format!("{}", x.round()),
            1 => format!("{:.1}", x),
            2 => format!("{:.3}", x),
            _ => format!("{} / 3", (3.0 * x).round()),
        }
    }

    fn angle(&mut self) -> String {
        match self.below(3) {
            0 => format!("{}", 90 * self.below(4)),
            1 => self.num(-180.0, 180.0),
            _ => format!("{}", 15 * self.below(24)),
        }
    }

    fn fn_(&mut self) -> u64 {
        [3, 5, 8, 12, 16, 24][self.below(6) as usize]
    }
}

fn shape2(r: &mut Rng, depth: u32) -> String {
    let leaf = |r: &mut Rng| match r.below(4) {
        0 => format!(
            "square([{}, {}], center = {});",
            r.num(0.5, 9.0),
            r.num(0.5, 9.0),
            r.below(2) == 1
        ),
        1 => format!("circle(r = {}, $fn = {});", r.num(0.3, 6.0), r.fn_()),
        2 => {
            // A star, which Clipper sanitizes (an unsanitized leaf).
            let n = 3 + r.below(5);
            let pts: Vec<String> = (0..2 * n)
                .map(|i| {
                    let rad = if i % 2 == 0 {
                        r.num(3.0, 6.0)
                    } else {
                        r.num(0.5, 2.5)
                    };
                    format!("[{rad} * cos({i} * 180 / {n}), {rad} * sin({i} * 180 / {n})]")
                })
                .collect();
            format!("polygon([{}]);", pts.join(", "))
        }
        _ => format!("square({});", r.num(0.5, 4.0)),
    };
    if depth == 0 {
        return leaf(r);
    }
    match r.below(9) {
        0 | 1 => leaf(r),
        2 => format!(
            "translate([{}, {}]) {}",
            r.num(-9.0, 9.0),
            r.num(-9.0, 9.0),
            shape2(r, depth - 1)
        ),
        3 => format!("rotate({}) {}", r.angle(), shape2(r, depth - 1)),
        4 => format!(
            "scale([{}, {}]) {}",
            if r.below(4) == 0 {
                "-1".into()
            } else {
                r.num(0.2, 3.0)
            },
            r.num(0.2, 3.0),
            shape2(r, depth - 1)
        ),
        5 => format!(
            "union() {{ {} {} }}",
            shape2(r, depth - 1),
            shape2(r, depth - 1)
        ),
        6 => format!(
            "hull() {{ {} {} }}",
            shape2(r, depth - 1),
            shape2(r, depth - 1)
        ),
        7 => format!(
            "offset(r = {}, $fn = 8) {}",
            r.num(0.2, 1.0),
            shape2(r, depth - 1)
        ),
        _ => format!("mirror([1, 1]) {}", shape2(r, depth - 1)),
    }
}

fn shape3(r: &mut Rng, depth: u32) -> String {
    let leaf = |r: &mut Rng| match r.below(6) {
        0 => format!(
            "cube([{}, {}, {}], center = {});",
            r.num(0.5, 9.0),
            r.num(0.5, 9.0),
            r.num(0.5, 9.0),
            r.below(2) == 1
        ),
        1 => format!("sphere(r = {}, $fn = {});", r.num(0.3, 6.0), r.fn_()),
        2 => format!(
            "cylinder(h = {}, r1 = {}, r2 = {}, center = {}, $fn = {});",
            r.num(0.5, 9.0),
            r.num(0.0, 4.0),
            r.num(0.3, 4.0),
            r.below(2) == 1,
            r.fn_()
        ),
        3 => {
            // A tetrahedron, sometimes missing a face (open).
            let p: Vec<String> = (0..4)
                .map(|_| {
                    format!(
                        "[{}, {}, {}]",
                        r.num(-5.0, 5.0),
                        r.num(-5.0, 5.0),
                        r.num(-5.0, 5.0)
                    )
                })
                .collect();
            let faces = if r.below(3) == 0 {
                "[[0, 1, 2], [0, 3, 1], [1, 3, 2]]"
            } else {
                "[[0, 1, 2], [0, 3, 1], [1, 3, 2], [0, 2, 3]]"
            };
            format!("polyhedron([{}], {faces});", p.join(", "))
        }
        4 => format!(
            "linear_extrude(height = {}, twist = {}, scale = {}, slices = 3) {}",
            r.num(0.5, 6.0),
            if r.below(2) == 0 {
                "0".into()
            } else {
                r.num(-90.0, 90.0)
            },
            r.num(0.3, 2.0),
            shape2(r, 1)
        ),
        _ => format!(
            "rotate_extrude(angle = {}, $fn = {}) translate([{}, 0]) square([{}, {}]);",
            if r.below(2) == 0 {
                "360".into()
            } else {
                r.num(30.0, 300.0)
            },
            r.fn_(),
            r.num(0.0, 5.0),
            r.num(0.5, 3.0),
            r.num(0.5, 3.0)
        ),
    };
    if depth == 0 {
        return leaf(r);
    }
    let kid = |r: &mut Rng| {
        let s = shape3(r, depth - 1);
        match r.below(12) {
            0 => format!("%{s}"),
            1 => format!("#{s}"),
            _ => s,
        }
    };
    match r.below(15) {
        0 | 1 => leaf(r),
        2 => format!(
            "translate([{}, {}, {}]) {}",
            r.num(-9.0, 9.0),
            r.num(-9.0, 9.0),
            r.num(-9.0, 9.0),
            kid(r)
        ),
        3 => format!(
            "rotate([{}, {}, {}]) {}",
            r.angle(),
            r.angle(),
            r.angle(),
            kid(r)
        ),
        4 => format!(
            "scale([{}, {}, {}]) {}",
            if r.below(4) == 0 {
                "-1".into()
            } else {
                r.num(0.2, 3.0)
            },
            r.num(0.2, 3.0),
            r.num(0.2, 3.0),
            kid(r)
        ),
        5 => {
            let n = 2 + r.below(2);
            let kids: Vec<String> = (0..n).map(|_| kid(r)).collect();
            format!("union() {{ {} }}", kids.join(" "))
        }
        6 => {
            let kids: Vec<String> = (0..2).map(|_| kid(r)).collect();
            format!("hull() {{ {} }}", kids.join(" "))
        }
        7 => format!("color(\"red\") {{ {} {} }}", kid(r), kid(r)),
        8 => format!(
            "multmatrix([[{}, {}, 0, {}], [{}, 1, {}, 0], [0, {}, 1, {}], [0, 0, 0, 1]]) {}",
            r.num(0.5, 2.0),
            r.num(-1.0, 1.0),
            r.num(-5.0, 5.0),
            r.num(-1.0, 1.0),
            r.num(-1.0, 1.0),
            r.num(-1.0, 1.0),
            r.num(-5.0, 5.0),
            kid(r)
        ),
        9 => format!("difference() {{ {} {} }}", kid(r), kid(r)),
        10 => format!("intersection() {{ {} {} }}", kid(r), kid(r)),
        11 => format!("g() {{ {} {} }}", kid(r), kid(r)),
        12 => format!("render() {}", kid(r)),
        13 => format!(
            "scale({}) {}",
            ["0.001", "1000", "1e-5"][r.below(3) as usize],
            kid(r)
        ),
        _ => format!("mirror([0, 1, 1]) {}", kid(r)),
    }
}

fn model(seed: u64) -> String {
    let mut r = Rng(seed);
    let body = if r.below(4) == 0 {
        shape2(&mut r, 3)
    } else {
        shape3(&mut r, 3)
    };
    format!("module g() children();\n{body}\n")
}

fn root(src: &str) -> eval::Node {
    let path = PathBuf::from("/nonexistent/test.scad");
    let mut text = src.as_bytes().to_vec();
    text.extend_from_slice(b"\n\x03\n");
    let program = lang::parse_file(path, text);
    assert!(!program.has_syntax_errors(), "syntax error in:\n{src}");
    eval::with_stack(eval::DEFAULT_THREAD_STACK, || {
        eval::evaluate(
            &program,
            &[],
            &[],
            PathBuf::from("/nonexistent"),
            &eval::Options::default(),
            &mut eval::Collect::default(),
        )
    })
    .root
}

fn guard() -> Arc<eval::limits::Guard> {
    Arc::new(eval::limits::Guard::new(
        eval::limits::Limits::AGENT,
        Arc::new(AtomicBool::new(false)),
        None,
    ))
}

/// Every node of `n`'s subtree, in pre-order.
fn nodes(n: &eval::Node) -> Vec<&eval::Node> {
    let mut out = Vec::new();
    let mut stack = vec![n];
    while let Some(n) = stack.pop() {
        out.push(n);
        stack.extend(n.children.iter().rev());
    }
    out
}

fn same(a: &Bounds, b: &Bounds) -> bool {
    let bits2 = |v: &[f64; 2]| v.map(f64::to_bits);
    let bits3 = |v: &[f64; 3]| v.map(f64::to_bits);
    match (a, b) {
        (Bounds::Empty, Bounds::Empty) => true,
        (Bounds::Flat { min: a0, max: a1 }, Bounds::Flat { min: b0, max: b1 }) => {
            bits2(a0) == bits2(b0) && bits2(a1) == bits2(b1)
        }
        (Bounds::Solid { min: a0, max: a1 }, Bounds::Solid { min: b0, max: b1 }) => {
            bits3(a0) == bits3(b0) && bits3(a1) == bits3(b1)
        }
        _ => false,
    }
}

/// Models per run; `NEOSCAD_FASTBOUNDS_MODELS` asks for a longer search.
fn models() -> u64 {
    std::env::var("NEOSCAD_FASTBOUNDS_MODELS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(300)
}

#[test]
fn the_fast_path_gives_the_rendered_box_wherever_it_answers() {
    watch_memory();
    let renderer = Renderer::new();
    let g = guard();
    let (mut answered, mut declined) = (0usize, 0usize);
    let mut failures = Vec::new();
    for seed in 0..models() {
        let src = model(seed);
        let top = root(&src);
        let keys = eval::dump::Keys::new(&top, &lang::loader::StdFs);
        for n in nodes(&top) {
            let Some(fast) = geom::fastbounds::bounds(n, Some(&g)) else {
                declined += 1;
                continue;
            };
            answered += 1;
            let opts = RenderOptions {
                guard: Some(g.clone()),
                ..RenderOptions::default()
            };
            let rendered = renderer.render(n, &keys, opts).expect("supported");
            let want = session::oracle::facts(rendered.geometry.as_ref()).bounds();
            if !same(&fast, &want) {
                failures.push(format!(
                    "seed {seed}, node {} ({:?}):\n  fast     {fast:?}\n  rendered {want:?}\n{src}",
                    n.index,
                    std::mem::discriminant(&n.kind)
                ));
            }
        }
        assert!(
            g.exceeded().is_none(),
            "a limit stopped seed {seed}:\n{src}"
        );
    }
    assert!(
        failures.is_empty(),
        "{} of {answered} fast answers differ from the render:\n{}",
        failures.len(),
        failures.join("\n")
    );
    // The fast path must actually be taken on most of these models, and
    // decline on the operations it leaves to the renderer.
    assert!(
        answered > 1000 && declined > 100,
        "{answered} answered, {declined} declined"
    );
    eprintln!("fastbounds: {answered} subtrees answered, {declined} declined");
}

/// A query model answers the same with the fast path as with a render:
/// the whole pipeline, through the session's oracle.
#[test]
fn child_bounds_reads_the_same_through_the_fast_path() {
    let fs = Arc::new(lang::vfs::MemFs::new());
    let src = b"module b() { echo(child_bounds(0)); children(0); }\n\
        b() rotate([10, 20, 30]) union() { cube([1, 2, 3]); translate([4, 0, 0]) sphere(1.5, $fn = 12); }\n\
        b() difference() { cube(5); sphere(2); }\n\
        b() translate([1, 1]) circle(2, $fn = 7);\n";
    fs.insert("/doc/m.scad", src.to_vec());
    let mut cfg = session::Config::new(fs, lang::loader::LibraryPath(Vec::new()));
    cfg.work_dir = PathBuf::from("/doc");
    let s = session::Session::new(cfg);
    let mut run = session::Run::new("m.scad");
    run.extensions = eval::Extensions::NONE.with(eval::Extension::Query);
    let r = s.evaluate(&run, false).unwrap();
    let echo = r.log.echo();
    assert_eq!(echo.len(), 3, "{echo:?}");
    // The difference is rendered; its box is the cube's.
    assert_eq!(echo[1], "ECHO: [[0, 0, 0], [5, 5, 5]]");
    assert!(echo[2].starts_with("ECHO: [[-0.80"), "{}", echo[2]);
}
