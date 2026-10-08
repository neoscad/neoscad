//! `fillet_edges()`/`chamfer_edges()` (`--enable fillet`) at stage F0:
//! the blends are not built, so a call renders as its children's union,
//! in the full render and in the preview (`docs/fillets.md`, section 15).

use std::path::PathBuf;

use geom::{Geometry, RenderOptions, Renderer};

fn render(src: &str) -> String {
    let path = PathBuf::from("/nonexistent/test.scad");
    let mut text = src.as_bytes().to_vec();
    text.extend_from_slice(b"\n\x03\n");
    let program = lang::parse_file(path, text);
    assert!(!program.has_syntax_errors());
    let mut out = eval::Collect::default();
    let opts = eval::Options {
        extensions: eval::Extensions::NONE.with(eval::Extension::Fillet),
        ..eval::Options::default()
    };
    let ev = eval::with_stack(eval::DEFAULT_THREAD_STACK, || {
        eval::evaluate(
            &program,
            &[],
            &[],
            PathBuf::from("/nonexistent"),
            &opts,
            &mut out,
        )
    });
    let keys = eval::dump::Keys::new(&ev.root, &lang::loader::StdFs);
    let renderer = Renderer::new();
    // The preview builds too: the node is a leaf computed with geometry,
    // as `hull()` is.
    geom::csg::CsgTree::build(
        &ev.root,
        &renderer,
        &keys,
        RenderOptions::default(),
        100_000,
    )
    .expect("a preview");
    let r = renderer
        .render(&ev.root, &keys, RenderOptions::default())
        .expect("supported");
    match r.geometry {
        Some(Geometry::Manifold(m)) => format!("{:?}", m.to_polyset(&geom::color::CORNFIELD)),
        g => panic!("not a solid: {g:?}"),
    }
}

/// A call that builds nothing renders its children unchanged: one that
/// selects circles (stage F3's), one that matches nothing, one refused
/// before any boolean, and one whose arguments are wrong (a group). One
/// that builds does not.
#[test]
fn a_call_that_builds_nothing_renders_its_children_unchanged() {
    let children = "cube(10); translate([5, 5, 5]) cylinder(r = 3, h = 10);";
    let plain = render(&format!("union() {{ {children} }}"));
    for call in [
        "fillet_edges(r = 1)",
        "chamfer_edges(d = 1, edges = \"%circle\")",
        "fillet_edges(r = 1, edges = \"|x and >z and <z\")",
        "fillet_edges(r = 20, edges = \"|z and <x\")",
        "fillet_edges(r = -1)",
    ] {
        assert_eq!(plain, render(&format!("{call} {{ {children} }}")), "{call}");
    }
    assert_ne!(
        plain,
        render(&format!(
            "chamfer_edges(d = 1, edges = \"|z and <x\") {{ {children} }}"
        ))
    );
}
