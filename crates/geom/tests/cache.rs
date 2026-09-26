//! The geometry cache across renders, as a long-lived host uses it: the
//! messages a fresh render prints must come back from a warm cache, a
//! stale render must stop when its interrupt flag is set, and the budget
//! must hold.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

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

/// The messages of one render, with their lines.
fn messages(r: &Renderer, src: &str, epoch: u64) -> Vec<String> {
    let ev = tree(src);
    let keys = eval::dump::Keys::new(&ev.root, &lang::loader::StdFs);
    let out = r
        .render(
            &ev.root,
            &keys,
            RenderOptions {
                replay: Some(epoch),
                ..Default::default()
            },
        )
        .expect("supported");
    out.messages
        .iter()
        .map(|m| format!("{}@{}", m.text, m.loc.as_ref().map_or(0, |l| l.line)))
        .collect()
}

/// Mixing 2D and 3D warns inside a union; a translated copy of the same
/// union is not first, so it is silent in a fresh render.
const WARNS: &str = "module m() union() { cube(1); square(1); }\nm();\ntranslate([5,0,0]) m();";

#[test]
fn warm_renders_print_what_a_fresh_render_prints() {
    let fresh = messages(&Renderer::new(), WARNS, 1);
    assert!(!fresh.is_empty(), "the model warns");
    let r = Renderer::new();
    assert_eq!(messages(&r, WARNS, 1), fresh);
    // The same sources: replayed from the cache.
    let before = r.stats();
    assert_eq!(messages(&r, WARNS, 1), fresh);
    assert_eq!(r.stats().misses, before.misses, "nothing recomputed");
    // Other sources (an edit elsewhere moved the lines): the nodes with
    // messages are computed again, so their locations are current.
    let moved = format!("\n\n{WARNS}");
    let fresh_moved = messages(&Renderer::new(), &moved, 2);
    assert_ne!(fresh_moved, fresh, "lines moved");
    assert_eq!(messages(&r, &moved, 2), fresh_moved);
}

#[test]
fn a_node_that_becomes_first_prints_its_messages() {
    // First the warning union appears twice; then the first copy is gone,
    // so the (unchanged) second one is first and must warn.
    let r = Renderer::new();
    messages(&r, WARNS, 1);
    let second_only = "module m() union() { cube(1); square(1); }\ntranslate([5,0,0]) m();";
    assert_eq!(
        messages(&r, second_only, 1),
        messages(&Renderer::new(), second_only, 1)
    );
}

#[test]
fn an_interrupted_render_stops_and_keeps_the_cache_usable() {
    let ev = tree("difference() { cube(10); sphere(6); }");
    let keys = eval::dump::Keys::new(&ev.root, &lang::loader::StdFs);
    let r = Renderer::new();
    let flag = Arc::new(AtomicBool::new(true));
    let err = r
        .render(
            &ev.root,
            &keys,
            RenderOptions {
                interrupt: Some(flag),
                ..Default::default()
            },
        )
        .unwrap_err();
    assert!(err.is_interrupted());
    let ok = r.render(&ev.root, &keys, RenderOptions::default()).unwrap();
    assert!(ok.geometry.is_some_and(|g| !g.is_empty()));
}

#[test]
fn the_budget_bounds_the_cache() {
    let r = Renderer::with_budget(1);
    messages(&r, "for (i = [0:9]) translate([i * 3, 0, 0]) sphere(1);", 0);
    let s = r.stats();
    assert_eq!(s.budget, 1);
    assert_eq!(s.entries, 1, "only the newest entry stays over budget");
    assert!(s.evictions > 0);
    r.set_budget(geom::CACHE_BUDGET);
    assert_eq!(r.stats().budget, geom::CACHE_BUDGET);
}
