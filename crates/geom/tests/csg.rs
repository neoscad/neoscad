//! The preview's CSG products (`geom::csg`) for small programs: which
//! leaves land in which product, where `%` and `#` objects go, and that
//! the product booleans come out the same at any thread count.

use std::path::PathBuf;

use geom::color::Color;
use geom::csg::{CsgTree, DEFAULT_TERM_LIMIT, ProductJob, Products, product_meshes};
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
                    .map(|o| mesh(o, Color([0.0, 1.0, 0.0, 1.0])))
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
