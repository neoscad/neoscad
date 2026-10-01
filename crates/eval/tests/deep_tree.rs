//! The walks over an evaluated node tree, on a tree far deeper than a
//! small stack could recurse through.
//!
//! A recursive module makes a node tree as deep as the evaluator allows
//! (65,507 levels natively). Copying, comparing, freeing, dumping and
//! keying that tree all happen after the evaluation succeeded, and each of
//! them used to recurse once per level: the tree could be built and then
//! overflow the stack on its way out. The trees here are built directly,
//! not through the evaluator, and walked on a thread with [`SMALL_STACK`].
//!
//! Every tree here is a few tens of MB at most; a watchdog aborts the
//! process past 2 GB resident all the same.

use std::path::Path;
use std::sync::Once;
use std::time::Duration;

use eval::dump::{Keys, csg};
use eval::node::{Node, NodeKind, Origin};
use lang::loader::StdFs;
use lang::source::Span;

/// Far less than any of the recursive walks needed for [`DEPTH`] levels
/// (the derived `Drop` alone took tens of bytes per level), and enough for
/// the iterative ones in a debug build.
const SMALL_STACK: usize = 128 << 10;

/// Deeper than the evaluator's native module limit.
const DEPTH: usize = 100_000;

fn watch_memory() {
    static START: Once = Once::new();
    START.call_once(|| {
        std::thread::spawn(|| {
            loop {
                let mb = rss_mb();
                if mb > 2048 {
                    eprintln!("deep_tree: {mb} MB resident, over the 2 GB guard; aborting");
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

fn on_small_stack<T: Send>(f: impl FnOnce() -> T + Send) -> T {
    watch_memory();
    std::thread::scope(|s| {
        std::thread::Builder::new()
            .stack_size(SMALL_STACK)
            .spawn_scoped(s, f)
            .expect("a thread")
            .join()
            .expect("no overflow or panic")
    })
}

fn node(kind: NodeKind, index: usize, children: Vec<Node>) -> Node {
    Node {
        kind,
        children,
        origin: Some(Box::new(Origin {
            name: "module m".into(),
            unit: 0,
            span: Span::default(),
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

fn cube(index: usize) -> Node {
    node(
        NodeKind::Cube {
            size: [1.0; 3],
            center: false,
        },
        index,
        Vec::new(),
    )
}

/// The root over `depth` nested groups with a cube at the bottom, as
/// `module m(n) { if (n > 0) m(n - 1); else cube(1); }` makes. Indices
/// are in creation (pre-)order.
fn chain(depth: usize) -> Node {
    let mut n = cube(depth + 1);
    for i in (1..=depth).rev() {
        n = group(i, vec![n]);
    }
    let mut root = node(NodeKind::Root, 0, vec![n]);
    root.origin = None;
    root
}

/// The root over `depth` levels that each hold a cube beside the next
/// level, as `module m(n) { cube(1); if (n > 0) m(n - 1); }` makes: a
/// branch point at every level, which a parallel walk splits at.
fn comb(depth: usize) -> Node {
    let mut n = cube(2 * depth + 1);
    for i in (1..=depth).rev() {
        n = group(2 * i - 1, vec![cube(2 * i), n]);
    }
    let mut root = node(NodeKind::Root, 0, vec![n]);
    root.origin = None;
    root
}

/// The node at `depth` levels below `root`, following the last child.
fn descend(root: &Node, depth: usize) -> &Node {
    let mut n = root;
    for _ in 0..depth {
        n = n.children.last().expect("a child");
    }
    n
}

#[test]
fn a_deep_tree_clones_compares_and_drops_on_a_small_stack() {
    for tree in [chain(DEPTH), comb(DEPTH)] {
        on_small_stack(|| {
            let copy = tree.clone();
            assert!(copy == tree);
            let mut other = tree.clone();
            // A difference at the very bottom.
            let mut n = &mut other;
            while let Some(c) = n.children.last_mut() {
                n = c;
            }
            n.index += 1;
            assert!(other != tree);
            drop(copy);
            drop(other);
        });
        // The tree itself is dropped on a small stack too.
        on_small_stack(move || drop(tree));
    }
}

#[test]
fn a_deep_tree_finds_its_root_tag_on_a_small_stack() {
    let mut tree = chain(DEPTH);
    let mut n = &mut tree;
    while let Some(c) = n.children.last_mut() {
        n = c;
    }
    n.origin.as_mut().expect("an origin").tag_root = true;
    let index = on_small_stack(|| {
        let (found, next) = tree.find_root_tag();
        assert!(next.is_none());
        found.expect("the tagged cube").index
    });
    assert_eq!(index, DEPTH + 1);
}

#[test]
fn a_deep_tree_is_keyed_on_a_small_stack() {
    for tree in [chain(DEPTH), comb(DEPTH)] {
        on_small_stack(|| {
            let keys = Keys::new(&tree, &StdFs);
            // Every group with one content child is transparent, so the
            // chain's groups all share the cube's key; the comb's do not.
            let bottom = descend(&tree, DEPTH + 1);
            let top = descend(&tree, 1);
            if tree.children[0].children.len() == 1 {
                assert_eq!(keys.get(top), keys.get(bottom));
            } else {
                assert_ne!(keys.get(top), keys.get(bottom));
            }
        });
    }
}

/// The comb's keys split across the pool at its branch points; they must
/// be the ones a single thread computes, at every node.
#[test]
fn deep_keys_are_the_same_at_any_thread_count() {
    let tree = comb(20_000);
    let keys = |threads: usize| {
        // The global pool's threads have Rust's default 2 MiB; the
        // parallel splits nest at most `PARALLEL_MAX_NESTING` deep in it.
        rayon::ThreadPoolBuilder::new()
            .num_threads(threads)
            .stack_size(2 << 20)
            .build()
            .expect("a pool")
            .install(|| Keys::new(&tree, &StdFs))
    };
    let (one, eight) = (keys(1), keys(8));
    let mut n = &tree;
    loop {
        assert_eq!(one.get(n), eight.get(n));
        for c in &n.children {
            assert_eq!(one.get(c), eight.get(c));
        }
        match n.children.last() {
            Some(c) => n = c,
            None => break,
        }
    }
}

/// The `.csg` dump writes a tab per level on every line, so its size grows
/// with the square of the depth; a few thousand levels are enough to need
/// far more than [`SMALL_STACK`] recursively.
#[test]
fn a_deep_tree_dumps_on_a_small_stack() {
    const LEVELS: usize = 5_000;
    let tree = chain(LEVELS);
    let text = on_small_stack(|| csg(&tree, Path::new("/"), &StdFs));
    let mut expected = String::new();
    for d in 0..LEVELS {
        expected.push_str(&"\t".repeat(d));
        expected.push_str("group() {\n");
    }
    expected.push_str(&"\t".repeat(LEVELS));
    expected.push_str("cube(size = [1, 1, 1], center = false);\n");
    for d in (0..LEVELS).rev() {
        expected.push_str(&"\t".repeat(d));
        expected.push_str("}\n");
    }
    expected.push('\n');
    assert!(text == expected, "the dump of the chain differs");
}
