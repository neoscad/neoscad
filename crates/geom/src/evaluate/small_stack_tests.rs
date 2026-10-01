//! The render walk on a stack far smaller than a deep tree's recursion
//! needed. Safari's worker overflowed rendering the web demo's BOSL2 gear
//! (a tree 77 levels deep) because the walk recursed once per level, and
//! JavaScriptCore's frames for it are much larger than V8's; natively the
//! gear needed between 128 and 256 KiB. These walk trees as wasm32 does,
//! every node on the calling thread, on a thread with [`SMALL_STACK`], and
//! check the result is the one [`Renderer::render`] gives.

use std::path::{Path, PathBuf};

use super::*;

/// Smaller than the old recursive walk needed for the gear in a release
/// build, and still enough for the kernels in a debug one.
const SMALL_STACK: usize = 96 << 10;

fn evaluate(path: &Path, libs: &lang::loader::LibraryPath, text: Vec<u8>) -> eval::Evaluation {
    let fs = lang::loader::StdFs;
    let suffix = b"\n\x03\n";
    let mut text = text;
    text.extend_from_slice(suffix);
    let program = lang::parse_program(path.to_path_buf(), text, &fs, libs);
    assert!(!program.has_syntax_errors(), "{}", path.display());
    let deps = lang::deps::load_dependencies(&program, suffix, &fs, libs);
    let uses = lang::deps::resolve_uses(&program, &fs, libs);
    let libraries: Vec<eval::Library<'_>> = deps
        .iter()
        .map(|l| eval::Library {
            path: &l.path,
            program: l.program.as_ref(),
            uses: &l.uses,
        })
        .collect();
    let dir = path.parent().expect("a directory").to_path_buf();
    let mut out = eval::Collect::default();
    let ev = eval::with_stack(eval::DEFAULT_THREAD_STACK, || {
        eval::evaluate(
            &program,
            &uses,
            &libraries,
            dir,
            &eval::Options::default(),
            &mut out,
        )
    });
    assert!(!ev.aborted, "{} stopped evaluating", path.display());
    ev
}

fn off(g: Option<&Geometry>) -> Vec<u8> {
    let ps = crate::export::as_polyset(g.expect("geometry"), &crate::color::CORNFIELD)
        .expect("a 3D result");
    crate::export::off(&ps, false, &mut Vec::new())
}

/// `root` walked serially on a thread with [`SMALL_STACK`], and as
/// [`Renderer::render`] renders it.
fn walk_small(root: &Node) -> (Vec<u8>, Vec<u8>) {
    let opts = RenderOptions::default();
    let small = std::thread::scope(|s| {
        std::thread::Builder::new()
            .stack_size(SMALL_STACK)
            .spawn_scoped(s, || {
                // The cache keys are another walk over the tree, iterative
                // too, so they are computed on the small stack as well.
                let keys = eval::dump::Keys::new(root, &lang::loader::StdFs);
                let r = Renderer::new();
                let mut ctx = r.prepare(&[root], &keys, &opts);
                ctx.parallel = false;
                let out = ctx.node(root).expect("supported");
                off(out.geom.as_ref())
            })
            .expect("a thread")
            .join()
            .expect("no panic")
    });
    let keys = eval::dump::Keys::new(root, &lang::loader::StdFs);
    let full = Renderer::new()
        .render(root, &keys, RenderOptions::default())
        .expect("supported");
    (small, off(full.geometry.as_ref()))
}

/// A chain of 2,000 nodes, which the recursive walk needed megabytes of
/// stack for.
#[test]
fn a_deep_chain_walks_on_a_small_stack() {
    let src = "module m(n) { if (n > 0) translate([0, 0, 0.001]) m(n - 1); else cube(1); }\n\
               m(1000);\n";
    let path = PathBuf::from("/nonexistent/deep.scad");
    let ev = evaluate(
        &path,
        &lang::loader::LibraryPath::default(),
        src.as_bytes().to_vec(),
    );
    let (small, full) = walk_small(&ev.root);
    assert!(small == full, "the walk on a small stack differs");
}

/// The web demo's two BOSL2 examples, when BOSL2 is checked out at
/// `.reference/BOSL2` (CI has no checkout, and skips them).
#[test]
fn the_bosl2_examples_walk_on_a_small_stack() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let reference = root.join(".reference");
    if !reference.join("BOSL2/std.scad").is_file() {
        eprintln!("skipped: no .reference/BOSL2");
        return;
    }
    let libs = lang::loader::LibraryPath(vec![reference]);
    for name in ["helical-gear.scad", "gearbox.scad"] {
        let path = root.join("web/examples").join(name);
        let text = std::fs::read(&path).expect("the example");
        let ev = evaluate(&path, &libs, text);
        let (small, full) = walk_small(&ev.root);
        assert!(small == full, "{name}: the walk on a small stack differs");
    }
}

fn node(kind: NodeKind, index: usize, children: Vec<Node>) -> Node {
    Node {
        kind,
        children,
        origin: Some(Box::new(eval::node::Origin {
            name: "module m".into(),
            unit: 0,
            span: lang::source::Span::default(),
            line: 1,
            tag_root: false,
            tag_highlight: false,
            tag_background: false,
        })),
        index,
    }
}

fn group(index: usize, children: Vec<Node>) -> Node {
    node(
        NodeKind::Group {
            name: Some("module m".into()),
        },
        index,
        children,
    )
}

/// The root over `depth` levels built directly, each a group holding an
/// empty group beside the next level when `branch`, a cube at the bottom.
/// With `branch` every level is a node with two children, where the
/// parallel walk splits.
fn deep(depth: usize, branch: bool) -> Node {
    let cube = NodeKind::Cube {
        size: [1.0; 3],
        center: false,
    };
    let mut n = node(cube, 2 * depth + 1, Vec::new());
    for i in (1..=depth).rev() {
        n = match branch {
            true => group(2 * i - 1, vec![group(2 * i, Vec::new()), n]),
            false => group(2 * i - 1, vec![n]),
        };
    }
    let mut root = node(NodeKind::Root, 0, vec![n]);
    root.origin = None;
    root
}

/// Deeper than the evaluator's native module limit (65,507), built
/// directly. A chain renders on a small stack; a tree that branches at
/// every level renders serially on one and in parallel on the pool, where
/// the splits used to nest once per level, to the same result.
#[test]
fn a_100k_level_tree_renders_on_a_small_stack() {
    for branch in [false, true] {
        let root = deep(100_000, branch);
        let (small, full) = walk_small(&root);
        assert!(
            small == full,
            "branch {branch}: the walk on a small stack differs"
        );
        assert!(!small.is_empty());
    }
}

/// The preview's walk over the same deep chain, on a small stack.
#[test]
fn a_100k_level_tree_previews_on_a_small_stack() {
    let root = deep(100_000, false);
    std::thread::scope(|s| {
        std::thread::Builder::new()
            .stack_size(SMALL_STACK)
            .spawn_scoped(s, || {
                let keys = eval::dump::Keys::new(&root, &lang::loader::StdFs);
                let r = Renderer::new();
                let tree = crate::csg::CsgTree::build(
                    &root,
                    &r,
                    &keys,
                    RenderOptions::default(),
                    crate::csg::DEFAULT_TERM_LIMIT,
                )
                .expect("supported");
                assert_eq!(tree.root.expect("products").len(), 1);
            })
            .expect("a thread")
            .join()
            .expect("no overflow or panic")
    });
}
