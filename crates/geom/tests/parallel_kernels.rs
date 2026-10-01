//! The vendored manifold-rust runs parts of its boolean pipeline in
//! parallel (see `vendor/README.md`, "The parallel boolean patch"): the
//! pairs of each `batch_boolean` round, the edge-flag and vertex-orbit scans
//! of the topology cleanup, the edge-map sort of the result assembly, the
//! triangle writes of `face2tri`, and the sorts of `sort_geometry` and
//! `intersect12`. Each is written to give the sequential result exactly.
//! These models are big enough to take every one of those parallel paths,
//! and must export the same bytes at any thread count, and the bytes they
//! exported before the patch (the pinned hashes were taken with the
//! unpatched vendored copy).

use std::path::PathBuf;

use geom::{RenderOptions, Renderer};

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

/// The model rendered and exported as OFF.
fn off(src: &str) -> Vec<u8> {
    let ev = tree(src);
    let keys = eval::dump::Keys::new(&ev.root, &lang::loader::StdFs);
    let out = Renderer::new()
        .render(&ev.root, &keys, RenderOptions::default())
        .expect("supported");
    let g = out.geometry.expect("geometry");
    let ps = geom::export::as_polyset(&g, &geom::color::CORNFIELD).expect("3D");
    geom::export::off(&ps, false, &mut Vec::new())
}

/// FNV-1a, to pin an export without storing it.
fn fnv(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}

/// `src` exports the pinned bytes on 1 and 8 threads. (Each render of the
/// larger model takes seconds in a debug build, so two thread counts.)
fn check(src: &str, pinned: u64) {
    for threads in [1, 8] {
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(threads)
            .stack_size(eval::DEFAULT_THREAD_STACK)
            .build()
            .unwrap();
        let bytes = pool.install(|| off(src));
        assert_eq!(
            fnv(&bytes),
            pinned,
            "export on {threads} threads differs from the pinned one"
        );
    }
}

/// A 3D checkerboard of cubes that touch only along edges and at corners,
/// unioned. Every boolean leaves duplicate edges and pinched vertices for
/// `dedupe_edges` and `split_pinched_verts` to split (over 10,000
/// halfedges, their parallel orbit scans), and the union of 256 children
/// is a `batch_boolean` of many rounds of four pairs.
#[test]
fn checkerboard_union_is_unchanged_at_any_thread_count() {
    check(
        "for (x = [0:7], y = [0:7], z = [0:7]) if ((x + y + z) % 2 == 0)
    translate([x, y, z]) cube(1);",
        0x1492_de8d_3fc5_045b,
    );
}

/// One large difference: the result has well over 100,000 halfedges, so
/// the edge-flag scans, the edge-map sort, the triangle writes and the
/// sorts all take their parallel paths.
#[test]
fn large_difference_is_unchanged_at_any_thread_count() {
    check(
        "difference() {
    sphere(20, $fn = 240);
    for (i = [-3:3], j = [-3:3]) translate([i * 5.5, j * 5.5, 0])
        rotate([i * 9, j * 7, 0]) cylinder(r = 2, h = 50, center = true, $fn = 96);
}",
        0x5812_d2aa_7f40_2588,
    );
}

/// `src` exports the same bytes on 1 and 8 threads. For models whose bytes
/// differ by platform (spheres: `sin` and `cos` round differently in each
/// platform's libm), where one pinned hash can't hold everywhere.
fn check_agree(src: &str) {
    let export = |threads| {
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(threads)
            .stack_size(eval::DEFAULT_THREAD_STACK)
            .build()
            .unwrap();
        fnv(&pool.install(|| off(src)))
    };
    assert_eq!(
        export(1),
        export(8),
        "1 and 8 threads export different bytes"
    );
}

/// A union of 32 overlapping spheres of about 1,150 vertices each: its
/// first `batch_boolean` rounds are under the 10,000 vertices a round
/// needs to run its pairs in parallel and its later rounds over, so both
/// paths run, and must give the same bytes. (The checkerboard's rounds
/// are all under it.)
#[test]
fn batch_rounds_above_and_below_the_parallel_threshold_agree() {
    check_agree("for (i = [0:31]) translate([i * 1.5, (i % 4) * 1.5, 0]) sphere(2, $fn = 48);");
}
