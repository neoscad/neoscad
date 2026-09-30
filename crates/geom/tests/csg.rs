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
