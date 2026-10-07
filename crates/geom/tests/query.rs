//! Models that use NeoSCAD's queries (`--enable query`) render the same
//! at any thread count: the queries run during evaluation, which is
//! single-threaded, and leave a tree like any other.

use std::path::{Path, PathBuf};

use geom::{RenderOptions, Renderer};

fn goldens() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../conformance/extensions/query")
}

fn stl(src: &[u8]) -> Vec<u8> {
    let path = PathBuf::from("/nonexistent/test.scad");
    let mut text = src.to_vec();
    text.extend_from_slice(b"\n\x03\n");
    let program = lang::parse_file(path, text);
    assert!(!program.has_syntax_errors(), "syntax error in test program");
    let opts = eval::Options {
        extensions: eval::Extensions::NONE
            .with(eval::Extension::Query)
            .with(eval::Extension::Sketch),
        ..eval::Options::default()
    };
    let ev = eval::with_stack(eval::DEFAULT_THREAD_STACK, || {
        eval::evaluate(
            &program,
            &[],
            &[],
            PathBuf::from("/nonexistent"),
            &opts,
            &mut eval::Collect::default(),
        )
    });
    let keys = eval::dump::Keys::new(&ev.root, &lang::loader::StdFs);
    let g = Renderer::new()
        .render(&ev.root, &keys, RenderOptions::default())
        .expect("supported")
        .geometry
        .expect("geometry");
    let ps = geom::export::as_polyset(&g, &geom::color::CORNFIELD).expect("3D");
    geom::export::stl(&ps, true, false, &mut Vec::new())
}

/// The same STL at 1, 2 and 8 threads, several times each.
#[test]
fn queries_render_the_same_at_any_thread_count() {
    let models: Vec<Vec<u8>> = ["plate.scad", "reuse.scad"]
        .iter()
        .map(|n| std::fs::read(goldens().join(n)).unwrap())
        .collect();
    let all = |models: &[Vec<u8>]| -> Vec<Vec<u8>> { models.iter().map(|m| stl(m)).collect() };
    let first = all(&models);
    for threads in [1, 2, 8] {
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(threads)
            .stack_size(eval::DEFAULT_THREAD_STACK)
            .build()
            .unwrap();
        for _ in 0..3 {
            assert!(
                pool.install(|| all(&models)) == first,
                "a query model renders differently on {threads} threads"
            );
        }
    }
}
