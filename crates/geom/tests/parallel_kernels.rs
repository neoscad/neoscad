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

use geom::manifold_geom::{GlobalIds, ManifoldGeometry, OpType};
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
/// `dedupe_edges` and `split_pinched_verts` to split, and the union of 256
/// children is a `batch_boolean` of many rounds of four pairs. (Its
/// meshes are under the 100,000 halfedges at which the orbit scans go
/// parallel; the large difference and the sphere union below take that
/// path.)
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

/// One sphere of about 2,000 vertices, converted once and placed at each
/// of `centres`, as a cached subtree's solid is reused for every instance.
/// The instances share an original ID, so the output's runs of them are
/// told apart only by mesh ID. (Each call converts the sphere afresh, with
/// a new original ID, so one call's instances serve every comparison.)
fn instances(centres: &[[f64; 3]]) -> Vec<ManifoldGeometry> {
    let ev = tree("sphere(1, $fn = 64);");
    let keys = eval::dump::Keys::new(&ev.root, &lang::loader::StdFs);
    let out = Renderer::new()
        .render(&ev.root, &keys, RenderOptions::default())
        .expect("supported");
    let Some(geom::Geometry::PolySet(ps)) = out.geometry else {
        panic!("expected a mesh");
    };
    let (mut w, mut e) = (Vec::new(), Vec::new());
    let sphere = ManifoldGeometry::from_polyset(&ps, &GlobalIds, &mut w, &mut e);
    centres
        .iter()
        .map(|&[x, y, z]| {
            let mut g = sphere.clone();
            let mut m = geom::IDENTITY;
            (m[0][3], m[1][3], m[2][3]) = (x, y, z);
            g.transform(&m);
            g
        })
        .collect()
}

/// A batch of `parts` on `threads` threads, as two hashes: the kernel's
/// whole `MeshGL64` before neoscad reorders its runs (vertices, triangles,
/// runs with their original IDs and transforms, face IDs), and the OFF
/// export after.
fn batch_hashes(op: OpType, parts: &[ManifoldGeometry], threads: usize) -> (u64, u64) {
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(threads)
        .stack_size(eval::DEFAULT_THREAD_STACK)
        .build()
        .unwrap();
    let parts = parts.to_vec();
    let g = pool
        .install(|| ManifoldGeometry::batch(op, parts))
        .expect("a result");
    let gl = g.manifold.get_mesh_gl64(-1);
    let ps = g.to_polyset(&geom::color::CORNFIELD);
    let off = geom::export::off(&ps, false, &mut Vec::new());
    (fnv(format!("{gl:?}").as_bytes()), fnv(&off))
}

/// Instances of one solid combined in `batch_boolean` rounds that run
/// their pairs in parallel (each first round has well over the 10,000
/// vertices that takes) give the same mesh at any thread count, run after
/// run: not only the exported file, whose runs neoscad puts in a fixed
/// order (`canonical_mesh`), but the kernel's own output.
///
/// Each boolean draws mesh IDs from a process-wide counter, so pairs that
/// run side by side drew them in whatever order the scheduler gave, and a
/// later disjoint union in `batch_boolean` orders its operands' runs by
/// those values. manifold-rust's renumbering after each round
/// (`renumber_round_mesh_ids`, patch 0004) fixes the values' order. Three
/// batches: a union of sixteen mutually overlapping instances (every round
/// a real boolean), a union of eight in overlapping pairs ten apart
/// (upstream's case for the race), and an intersection of eight.
///
/// This passes without the renumbering too, even with every round
/// parallel, because neoscad's batches never reach that disjoint union:
/// `batch_union` first composes the operands into groups of disjoint ones,
/// and the groups' boxes overlap pairwise (an operand joins the first
/// group it overlaps nothing in), as do the unions of groups; an
/// intersection of disjoint operands is empty; and an overlapping boolean
/// puts its right operand's IDs after its left's whatever their values.
/// The test is here so that a batch path that does reach the race fails.
#[test]
fn batch_rounds_of_one_instanced_solid_agree_at_any_thread_count() {
    let ring: Vec<[f64; 3]> = (0..16)
        .map(|i| {
            let a = f64::from(i) * std::f64::consts::TAU / 16.0;
            [0.5 * a.cos(), 0.5 * a.sin(), 0.05 * f64::from(i)]
        })
        .collect();
    let pairs: Vec<[f64; 3]> = (0..8)
        .map(|i| [f64::from(i / 2) * 10.0 + f64::from(i % 2) * 0.5, 0.0, 0.0])
        .collect();
    let cases = [
        ("union of a ring", OpType::Add, &ring[..]),
        ("union of pairs", OpType::Add, &pairs[..]),
        ("intersection", OpType::Intersect, &ring[..8]),
    ];
    for (name, op, centres) in cases {
        let parts = instances(centres);
        let expected = batch_hashes(op, &parts, 1);
        for threads in [1, 2, 3, 8] {
            for rep in 0..2 {
                let (gl, off) = batch_hashes(op, &parts, threads);
                assert_eq!(
                    gl, expected.0,
                    "{name}: kernel mesh, {threads} threads, rep {rep}"
                );
                assert_eq!(
                    off, expected.1,
                    "{name}: export, {threads} threads, rep {rep}"
                );
            }
        }
    }
}
