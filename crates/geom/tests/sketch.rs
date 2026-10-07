//! Constrained sketches (`--enable sketch`) in the renderer: a sketch node
//! is the polygon it holds (same geometry, same cache key), a sketch
//! circle is `circle()`'s polygon, and the result is the same at any
//! thread count.

use std::path::{Path, PathBuf};

use eval::node::NodeKind;
use geom::{Geometry, RenderOptions, Renderer};

fn goldens() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../conformance/extensions/sketch")
}

fn tree(src: &[u8]) -> eval::Evaluation {
    let path = PathBuf::from("/nonexistent/test.scad");
    let mut text = src.to_vec();
    text.extend_from_slice(b"\n\x03\n");
    let program = lang::parse_file(path, text);
    assert!(!program.has_syntax_errors(), "syntax error in test program");
    let mut out = eval::Collect::default();
    let opts = eval::Options {
        extensions: eval::Extensions::NONE.with(eval::Extension::Sketch),
        ..eval::Options::default()
    };
    eval::with_stack(eval::DEFAULT_THREAD_STACK, || {
        eval::evaluate(
            &program,
            &[],
            &[],
            PathBuf::from("/nonexistent"),
            &opts,
            &mut out,
        )
    })
}

fn render(root: &eval::Node) -> Option<Geometry> {
    let keys = eval::dump::Keys::new(root, &lang::loader::StdFs);
    Renderer::new()
        .render(root, &keys, RenderOptions::default())
        .expect("supported")
        .geometry
}

fn stl(root: &eval::Node) -> Vec<u8> {
    let g = render(root).expect("geometry");
    let ps = geom::export::as_polyset(&g, &geom::color::CORNFIELD).expect("3D");
    geom::export::stl(&ps, true, false, &mut Vec::new())
}

/// Every sketch node of the tree turned into the `polygon` node it holds.
fn as_polygons(n: &mut eval::Node) -> usize {
    let mut count = 0;
    let mut stack = vec![n];
    while let Some(n) = stack.pop() {
        if let NodeKind::Sketch(s) = &n.kind {
            n.kind = NodeKind::Polygon {
                points: s.points.clone(),
                paths: s.paths.clone(),
                convexity: s.convexity,
            };
            count += 1;
        }
        stack.extend(n.children.iter_mut());
    }
    count
}

/// A sketch renders exactly as the polygon it holds, and keys the same, so
/// the two share cache entries (section 4.4).
#[test]
fn a_sketch_is_its_polygon() {
    for name in ["gusset.scad", "slot.scad", "holes.scad"] {
        let src = std::fs::read(goldens().join(name)).unwrap();
        let ev = tree(&src);
        let mut plain = ev.root.clone();
        assert!(as_polygons(&mut plain) > 0, "{name}");
        let (k1, k2) = (
            eval::dump::Keys::new(&ev.root, &lang::loader::StdFs),
            eval::dump::Keys::new(&plain, &lang::loader::StdFs),
        );
        assert_eq!(k1.get(&ev.root), k2.get(&plain), "{name}: keys differ");
        let (a, b) = (render(&ev.root), render(&plain));
        // A mesh's OFF, or a 2D shape's outlines as they print (holes.scad
        // is 2D).
        let text = |g: Option<Geometry>| {
            let g = g.expect("geometry");
            match geom::export::as_polyset(&g, &geom::color::CORNFIELD) {
                Some(ps) => geom::export::off(&ps, false, &mut Vec::new()),
                None => format!("{g:?}").into_bytes(),
            }
        };
        assert!(text(a) == text(b), "{name}: geometry differs");
    }
}

/// A sketch circle is `circle()` at the same `$fn`, `$fa`, `$fs`: the
/// same vertices, so the difference of the two is empty.
#[test]
fn a_sketch_circle_is_circle() {
    for disc in [
        "",
        "$fn = 5",
        "$fn = 64",
        "$fa = 5, $fs = 0.5",
        "$fs = 0.01",
    ] {
        let sep = if disc.is_empty() { "" } else { ", " };
        let src = format!(
            "difference() {{\n\
               circle(r = 7{sep}{disc});\n\
               sketch({disc}) {{ c = circle([0, 0], r = 7); fix(c.center); }}\n\
             }}\n\
             difference() {{\n\
               sketch({disc}) {{ c = circle([0, 0], r = 7); fix(c.center); }}\n\
               circle(r = 7{sep}{disc});\n\
             }}"
        );
        let ev = tree(src.as_bytes());
        for d in &ev.root.children {
            assert!(
                render(d).is_none_or(|g| g.is_empty()),
                "{disc}: the circles differ"
            );
        }
        // And the same vertex count: the circle's own polygon is the
        // sketch's points.
        let NodeKind::Sketch(s) = &ev.root.children[0].children[1].kind else {
            panic!("a sketch node")
        };
        let n = geom::fragments::circular_segments(
            &eval::node::Discretizer {
                fn_: if disc.contains("$fn = 5") {
                    5.0
                } else if disc.contains("$fn = 64") {
                    64.0
                } else {
                    0.0
                },
                fa: if disc.contains("$fa = 5") { 5.0 } else { 12.0 },
                fs: if disc.contains("$fs = 0.5") {
                    0.5
                } else if disc.contains("$fs = 0.01") {
                    0.01
                } else {
                    2.0
                },
            },
            7.0,
        )
        .unwrap();
        assert_eq!(s.points.len(), n as usize, "{disc}");
    }
}

/// The same STL at 1, 2 and 8 threads, several times each: the solve is
/// single-threaded and deterministic, and the polygon renders like any.
#[test]
fn sketches_render_the_same_at_any_thread_count() {
    let models: Vec<Vec<u8>> = ["gusset.scad", "slot.scad"]
        .iter()
        .map(|n| std::fs::read(goldens().join(n)).unwrap())
        .collect();
    let all = |models: &[Vec<u8>]| -> Vec<Vec<u8>> {
        models.iter().map(|m| stl(&tree(m).root)).collect()
    };
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
                "a sketch renders differently on {threads} threads"
            );
        }
    }
}
