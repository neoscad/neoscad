//! The preview's CSG products (`geom::csg`) for small programs: which
//! leaves land in which product, where `%` and `#` objects go, and that
//! the product booleans come out the same at any thread count.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use geom::color::Color;
use geom::csg::{
    CsgTree, DEFAULT_TERM_LIMIT, Negative, ProductJob, Products, Stop, product_meshes,
    product_meshes_until,
};
use geom::{RenderOptions, Renderer};

fn csg(src: &str) -> CsgTree {
    let path = PathBuf::from("/nonexistent/test.scad");
    let mut text = src.as_bytes().to_vec();
    text.extend_from_slice(b"\n\x03\n");
    let program = lang::parse_file(path, text);
    assert!(!program.has_syntax_errors(), "syntax error in test program");
    let mut out = eval::Collect::default();
    let ev = eval::with_stack(eval::DEFAULT_THREAD_STACK, || {
        eval::evaluate(
            &program,
            &[],
            &[],
            PathBuf::from("/nonexistent"),
            &eval::Options {
                preview: true,
                ..Default::default()
            },
            &mut out,
        )
    });
    let keys = eval::dump::Keys::new(&ev.root, &lang::loader::StdFs);
    CsgTree::build(
        &ev.root,
        &Renderer::new(),
        &keys,
        RenderOptions::default(),
        DEFAULT_TERM_LIMIT,
    )
    .expect("supported")
}

/// Each product as (positive leaves, negative leaves).
fn shape(p: &Option<Products>) -> Vec<(usize, usize)> {
    p.as_ref().map_or(Vec::new(), |p| {
        p.products
            .iter()
            .map(|p| (p.intersections.len(), p.subtractions.len()))
            .collect()
    })
}

#[test]
fn a_highlighted_subtraction_stays_in_the_difference_and_is_drawn_again() {
    let t = csg("difference() { sphere(10); #cylinder(h=30, r=6, center=true); }");
    assert_eq!(shape(&t.root), [(1, 1)]);
    assert_eq!(shape(&t.highlights), [(1, 0)]);
    assert!(t.background.is_none());
}

#[test]
fn a_highlighted_union_member_leaves_the_union() {
    let t = csg("cube(1); #translate([3,0,0]) cube(1);");
    assert_eq!(shape(&t.root), [(1, 0)]);
    assert_eq!(shape(&t.highlights), [(1, 0)]);
}

#[test]
fn background_objects_leave_the_csg_even_inside_hull() {
    let t = csg(
        "difference() { cube(10); %sphere(3); } translate([20,0,0]) hull() { %cube(1); sphere(1); }",
    );
    // The hull is one leaf; the difference lost its background operand.
    assert_eq!(shape(&t.root), [(1, 0), (1, 0)]);
    assert_eq!(shape(&t.background), [(1, 0), (1, 0)]);
}

#[test]
fn colours_come_from_the_outermost_color() {
    let t = csg("color(\"red\") color(\"blue\") cube(1);");
    let root = t.root.expect("products");
    let leaf = &root.products[0].intersections[0].leaf;
    assert_eq!(leaf.color, Color([1.0, 0.0, 0.0, 1.0]));
}

#[test]
fn a_2d_leaf_is_a_unit_slab_and_the_box_includes_it() {
    let t = csg("square([4, 2]);");
    assert_eq!(
        t.bounding_box(false),
        Some(([0.0, 0.0, -0.5], [4.0, 2.0, 0.5]))
    );
}

#[test]
fn disjoint_intersections_are_pruned() {
    let t = csg("intersection() { cube(1); translate([5,0,0]) cube(1); } cube(2);");
    // The empty intersection leaves only the second cube to draw.
    let root = t.root.expect("products");
    let drawn: usize = root
        .products
        .iter()
        .filter(|p| p.intersections.iter().all(|o| o.leaf.mesh.is_some()))
        .count();
    assert_eq!(drawn, 1);
}

/// The product booleans run in parallel; their IDs come from ranges
/// reserved in product order, so a single thread and many give the same
/// meshes, run after run.
#[test]
fn product_meshes_are_the_same_at_any_thread_count() {
    let t = csg(
        "for (i = [0:11]) translate([i*3, 0, 0]) difference() { cube(2, center=true); color(\"red\") sphere(1.2, $fn=16); }",
    );
    let root = t.root.expect("products");
    let jobs: Vec<ProductJob> = root
        .products
        .iter()
        .map(|p| {
            let mesh = |o: &geom::csg::ChainObject, c: Color| {
                let mut ps = (**o.leaf.mesh.as_ref().expect("mesh")).clone();
                ps.transform(&o.leaf.matrix);
                ps.set_color(c);
                ps
            };
            ProductJob {
                positives: p
                    .intersections
                    .iter()
                    .map(|o| mesh(o, Color([1.0, 1.0, 0.0, 1.0])))
                    .collect(),
                negatives: p
                    .subtractions
                    .iter()
                    .map(|o| mesh(o, Color([0.0, 1.0, 0.0, 1.0])).into())
                    .collect(),
            }
        })
        .collect();
    assert_eq!(jobs.len(), 12);
    let dump = |out: Vec<Option<geom::polyset::PolySet>>| format!("{out:?}");
    let scheme = geom::color::CORNFIELD;
    let parallel = dump(product_meshes(jobs.clone(), &scheme));
    let serial = rayon::ThreadPoolBuilder::new()
        .num_threads(1)
        .build()
        .unwrap()
        .install(|| dump(product_meshes(jobs.clone(), &scheme)));
    // Manifold numbers its own IDs from a global counter; the dumps compare
    // geometry and colours, which do not depend on those numbers.
    assert_eq!(parallel, serial);
    for _ in 0..5 {
        assert_eq!(dump(product_meshes(jobs.clone(), &scheme)), parallel);
    }
}

/// The Menger sponge example (`examples/Old/example024.scad`) at depth 5.
const MENGER_5: &str = "
module menger() {
  difference() {
    cube(100, center=true);
    for (v=[[0,0,0], [0,0,90], [0,90,0]])
      rotate(v) menger_negative(side=100, maxside=100, level=5);
  }
}
module menger_negative(side=1, maxside=1, level=1) {
  l=side/3;
  cube([maxside*1.1, l, l], center=true);
  if (level > 1) {
    for (i=[-1:1], j=[-1:1])
      if (i || j)
        translate([0, i*l, j*l])
          menger_negative(side=l, maxside=maxside, level=level-1);
  }
}
difference() {
  rotate([45, atan(1/sqrt(2)), 0]) menger();
  translate([0,0,-100]) cube(200, center=true);
}
";

/// The Menger sponge at depth 3 as the preview hands it to
/// [`product_meshes`]: each negative placed, with its chain when
/// `chains`.
fn menger_jobs(chains: bool) -> Vec<ProductJob> {
    let t = csg(&MENGER_5.replace("level=5", "level=3"));
    let tint = Color([0.0, 1.0, 0.0, 1.0]);
    t.root
        .expect("products")
        .products
        .iter()
        .map(|p| ProductJob {
            positives: p
                .intersections
                .iter()
                .map(|o| {
                    let mut ps = (**o.leaf.mesh.as_ref().expect("mesh")).clone();
                    ps.transform(&o.leaf.matrix);
                    ps.set_color(Color([1.0, 1.0, 0.0, 1.0]));
                    ps
                })
                .collect(),
            negatives: p
                .subtractions
                .iter()
                .map(|o| {
                    let mut ps = (**o.leaf.mesh.as_ref().expect("mesh")).clone();
                    ps.set_color(tint);
                    Negative {
                        mesh: Arc::new(ps),
                        matrix: Some(o.leaf.matrix),
                        tint,
                        slab: false,
                        chain: if chains { o.leaf.chain.clone() } else { None },
                    }
                })
                .collect(),
        })
        .collect()
}

/// The surface area and box of a mesh, for comparing two unions of the
/// same solid computed in different orders.
fn measure(ps: &geom::polyset::PolySet) -> (f64, [f64; 3], [f64; 3]) {
    let mut area = 0.0;
    for f in &ps.faces {
        let p = |i: usize| ps.vertices[f[i] as usize];
        for k in 1..f.len().saturating_sub(1) {
            let (a, b, c) = (p(0), p(k), p(k + 1));
            let u = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
            let v = [c[0] - a[0], c[1] - a[1], c[2] - a[2]];
            let n = [
                u[1] * v[2] - u[2] * v[1],
                u[2] * v[0] - u[0] * v[2],
                u[0] * v[1] - u[1] * v[0],
            ];
            area += 0.5 * (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
        }
    }
    let mut lo = [f64::INFINITY; 3];
    let mut hi = [f64::NEG_INFINITY; 3];
    for v in &ps.vertices {
        for k in 0..3 {
            lo[k] = lo[k].min(v[k]);
            hi[k] = hi[k].max(v[k]);
        }
    }
    (area, lo, hi)
}

/// The Menger sponge's negatives are copies of one subtree: the preview
/// unions each copy once and moves it, as a render reuses its cache. The
/// mesh is the same at any thread count, and the same solid as the flat
/// union of every negative (whose triangles differ: the unions run in
/// another order).
#[test]
fn repeated_subtrees_share_one_union_at_any_thread_count() {
    let shared = menger_jobs(true);
    assert_eq!(shared.len(), 1);
    assert!(geom::csg::shares_subtrees(&shared[0].negatives));
    assert!(!geom::csg::shares_subtrees(
        &menger_jobs(false)[0].negatives
    ));
    let scheme = geom::color::CORNFIELD;
    let dump = |out: &[Option<geom::polyset::PolySet>]| format!("{out:?}");
    let run = |threads: usize| {
        rayon::ThreadPoolBuilder::new()
            .num_threads(threads)
            .build()
            .unwrap()
            .install(|| product_meshes(menger_jobs(true), &scheme))
    };
    let one = run(1);
    assert_eq!(dump(&run(8)), dump(&one), "1 and 8 threads");
    let flat = product_meshes(menger_jobs(false), &scheme);
    let (a, lo, hi) = measure(one[0].as_ref().expect("a solid"));
    let (fa, flo, fhi) = measure(flat[0].as_ref().expect("a solid"));
    assert!(
        (a - fa).abs() < 1e-6 * fa,
        "area {a} against the flat union's {fa}"
    );
    for k in 0..3 {
        assert!((lo[k] - flo[k]).abs() < 1e-9 && (hi[k] - fhi[k]).abs() < 1e-9);
    }
}

/// A subtree repeated with a leaf pruned from one copy is not the same
/// subtree: only the copies with the same leaves share a union.
#[test]
fn copies_with_different_leaves_do_not_share() {
    let jobs = menger_jobs(true);
    let mut negatives = jobs[0].negatives.clone();
    // Drop one leaf of one copy (the box pruning a normaliser can do).
    negatives.remove(5);
    let scheme = geom::color::CORNFIELD;
    let pruned = ProductJob {
        positives: jobs[0].positives.clone(),
        negatives: negatives.clone(),
    };
    let flat = ProductJob {
        positives: jobs[0].positives.clone(),
        negatives: negatives
            .into_iter()
            .map(|n| geom::csg::Negative { chain: None, ..n })
            .collect(),
    };
    let s = product_meshes(vec![pruned], &scheme);
    let f = product_meshes(vec![flat], &scheme);
    let (a, ..) = measure(s[0].as_ref().expect("a solid"));
    let (fa, ..) = measure(f[0].as_ref().expect("a solid"));
    assert!(
        (a - fa).abs() < 1e-6 * fa,
        "area {a} against the flat union's {fa}"
    );
}

/// Normalising a difference of one solid and thousands of holes makes a
/// chain of thousands of operations; the web demo's preview of this model
/// overflowed V8's stack (about 1 MB) walking it recursively. Building the
/// products, and dropping them, must not depend on the chain's length, so
/// this runs on a stack far smaller than the one that overflowed.
#[test]
fn a_long_normalised_chain_needs_no_deep_stack() {
    let products = std::thread::Builder::new()
        .stack_size(256 << 10)
        .spawn(|| {
            let t = csg(MENGER_5);
            let out = (shape(&t.root), t.booleans, t.messages.clone());
            drop(t);
            out
        })
        .unwrap()
        .join()
        .expect("no stack overflow");
    let (products, booleans, messages) = products;
    // OpenSCAD 2026.09.23: "Normalized CSG tree has 14045 elements".
    assert_eq!(products, [(1, 14044)]);
    // Its boolean is past what a preview computes: thrown together.
    assert!(!booleans);
    let texts: Vec<&str> = messages.iter().map(|m| m.text.as_str()).collect();
    assert_eq!(
        texts,
        [
            "The CSG products have 14045 elements to combine, more than the 10000 a preview \
          computes; drawing them thrown together. Render to see the result."
        ]
    );
}

/// Past the normalisation limit OpenSCAD abandons the term, and warns of
/// the abort before the empty tree it leaves.
#[test]
fn a_term_past_the_limit_is_abandoned_with_openscads_warnings() {
    let path = PathBuf::from("/nonexistent/test.scad");
    let src =
        "difference() { cube(10); for (i = [0:20]) translate([i/3, 0, 0]) sphere(1); }\n\x03\n";
    let program = lang::parse_file(path, src.as_bytes().to_vec());
    let mut out = eval::Collect::default();
    let ev = eval::with_stack(eval::DEFAULT_THREAD_STACK, || {
        eval::evaluate(
            &program,
            &[],
            &[],
            PathBuf::from("/nonexistent"),
            &eval::Options::default(),
            &mut out,
        )
    });
    let keys = eval::dump::Keys::new(&ev.root, &lang::loader::StdFs);
    let t = CsgTree::build(
        &ev.root,
        &Renderer::new(),
        &keys,
        RenderOptions::default(),
        5,
    )
    .expect("supported");
    assert!(t.root.is_none());
    let texts: Vec<&str> = t.messages.iter().map(|m| m.text.as_str()).collect();
    assert_eq!(
        texts,
        [
            "Normalized tree is growing past 5 elements. Aborting normalization.\n",
            "CSG normalization resulted in an empty tree",
        ]
    );
}

/// A product whose union of negatives takes many kernel operations: a
/// slab minus a 20 x 20 grid of overlapping cubes (each overlaps its
/// neighbours, so every union does real work).
fn slow_job() -> ProductJob {
    let mut negatives = Vec::new();
    for i in 0..20 {
        for j in 0..20 {
            let mut c = geom::primitives::cube([1.3, 1.3, 1.3], false);
            let z = 0.1 * f64::from((i + j) % 3);
            c.transform(&[
                [1.0, 0.0, 0.0, f64::from(i)],
                [0.0, 1.0, 0.0, f64::from(j)],
                [0.0, 0.0, 1.0, z],
                [0.0, 0.0, 0.0, 1.0],
            ]);
            negatives.push(c.into());
        }
    }
    ProductJob {
        positives: vec![geom::primitives::cube([20.0, 20.0, 1.0], false)],
        negatives,
    }
}

fn guard(limits: eval::limits::Limits, clock: Option<eval::limits::Clock>) -> Stop {
    let flag = Arc::new(AtomicBool::new(false));
    Stop {
        interrupt: Some(flag.clone()),
        guard: Some(Arc::new(eval::limits::Guard::new(limits, flag, clock))),
    }
}

/// A cancel that arrives while the products' booleans run stops them at
/// the next kernel operation, not minutes later (the web demo's Menger
/// preview at depth 5 once ran 235 s past its 60 s limit).
#[test]
fn a_cancelled_preview_stops_between_booleans() {
    let stop = Stop {
        interrupt: Some(Arc::new(AtomicBool::new(false))),
        guard: None,
    };
    let flag = stop.interrupt.clone().expect("flag");
    let scheme = geom::color::CORNFIELD;
    let started = Instant::now();
    product_meshes_until(vec![slow_job(); 8], &scheme, &stop).expect("not stopped");
    let full = started.elapsed();
    let started = Instant::now();
    let canceller = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(20));
        flag.store(true, Ordering::Relaxed);
    });
    let out = product_meshes_until(vec![slow_job(); 8], &scheme, &stop);
    canceller.join().expect("canceller");
    let took = started.elapsed();
    eprintln!("uncancelled {full:?}, cancelled after 20 ms: {took:?}");
    assert!(out.is_err_and(|u| u.is_interrupted()));
    assert!(stop.exceeded().is_none(), "a cancel is not a limit");
    // A stopped run waits only for the kernel operations in flight.
    assert!(took < full / 2, "took {took:?} of {full:?}");
}

/// Out of time mid-way (a clock that advances 100 ms per reading, under a
/// one-second limit): the preview stops with the time limit recorded,
/// as the render stage reports it.
#[test]
fn a_preview_past_its_time_limit_stops_with_the_limit() {
    let ticks = Arc::new(AtomicU64::new(0));
    let clock: eval::limits::Clock =
        Arc::new(move || ticks.fetch_add(1, Ordering::Relaxed) as f64 * 100.0);
    let limits = eval::limits::Limits {
        time: Some(1.0),
        ..Default::default()
    };
    let stop = guard(limits, Some(clock));
    let out = product_meshes_until(vec![slow_job()], &geom::color::CORNFIELD, &stop);
    assert!(out.is_err_and(|u| u.is_interrupted()));
    let e = stop.exceeded().expect("a limit");
    assert_eq!(e.limit, eval::limits::Limit::Time);
}

/// The leaves and partial unions count against the memory limit while
/// they are alive, and are credited back however the preview ends.
#[test]
fn a_preview_past_its_memory_limit_stops_and_credits_what_it_held() {
    let limits = eval::limits::Limits {
        memory: Some(1 << 20),
        ..Default::default()
    };
    let stop = guard(limits, None);
    let out = product_meshes_until(vec![slow_job()], &geom::color::CORNFIELD, &stop);
    assert!(out.is_err_and(|u| u.is_interrupted()));
    let g = stop.guard.as_ref().expect("guard");
    assert_eq!(
        g.exceeded().expect("a limit").limit,
        eval::limits::Limit::Memory
    );
    assert_eq!(g.geometry_bytes(), 0);

    // Under a limit it fits, the meshes are the unlimited ones and every
    // byte is credited back.
    let limits = eval::limits::Limits {
        memory: Some(1 << 30),
        time: Some(1e6),
        ..Default::default()
    };
    let stop = guard(limits, Some(Arc::new(|| 0.0)));
    let scheme = geom::color::CORNFIELD;
    let limited = product_meshes_until(vec![slow_job()], &scheme, &stop).expect("fits");
    assert_eq!(stop.guard.as_ref().expect("guard").geometry_bytes(), 0);
    assert_eq!(
        format!("{limited:?}"),
        format!("{:?}", product_meshes(vec![slow_job()], &scheme))
    );
}
