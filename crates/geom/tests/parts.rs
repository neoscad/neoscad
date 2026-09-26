//! neoscad's `part()`: each rendered face knows its part, through booleans,
//! colours and cached copies, and the attribution is the same on every run
//! whatever the thread scheduling.

use std::collections::BTreeMap;
use std::path::PathBuf;

use geom::manifold_geom::ManifoldGeometry;
use geom::{Geometry, RenderOptions, Renderer};

fn solid(src: &str) -> ManifoldGeometry {
    let path = PathBuf::from("/nonexistent/test.scad");
    let mut text = src.as_bytes().to_vec();
    text.extend_from_slice(b"\n\x03\n");
    let program = lang::parse_file(path, text);
    assert!(!program.has_syntax_errors());
    let mut out = eval::Collect::default();
    let opts = eval::Options {
        parts: true,
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
    let r = Renderer::new()
        .render(&ev.root, &keys, RenderOptions::default())
        .expect("supported");
    match r.geometry {
        Some(Geometry::Manifold(m)) => (*m).clone(),
        g => panic!("not a solid: {g:?}"),
    }
}

/// Triangle count per part (`-` for none), and the triangles in order.
fn faces(m: &ManifoldGeometry) -> (BTreeMap<String, usize>, Vec<String>) {
    let (ps, ids) = m.to_polyset_with_ids(&geom::color::CORNFIELD);
    let mut count = BTreeMap::new();
    let mut order = Vec::new();
    for (f, id) in ps.faces.iter().zip(ids) {
        let name = m.part_of(id).map_or("-".to_string(), |n| n.to_string());
        *count.entry(name.clone()).or_insert(0) += 1;
        order.push(format!("{name} {f:?}"));
    }
    (count, order)
}

#[test]
fn copies_of_one_cached_solid_stay_separate_parts() {
    // `m()` is one cached union under both parts.
    let src = "module m() union() { cube(2); translate([1,1,1]) cube(2); }\n\
               part(\"a\") m();\n\
               part(\"b\") translate([10,0,0]) m();";
    let (count, _) = faces(&solid(src));
    assert_eq!(count.len(), 2, "{count:?}");
    assert_eq!(count["a"], count["b"], "{count:?}");
}

#[test]
fn parts_survive_booleans_and_colours() {
    // Nested parts, a colour over two parts, and a cut by an object of no
    // part, whose faces belong to the part it cut.
    let src = "color(\"red\") {\n\
                 part(\"body\") difference() { cube(10); translate([5,5,5]) cube(10); }\n\
                 part(\"arm\") { translate([10,0,0]) cube([10,2,2]); part(\"tip\") translate([20,0,0]) cube(2); }\n\
               }\n\
               difference() { part(\"base\") translate([0,0,-5]) cube([10,10,5]); translate([2,2,-6]) cube(2); }";
    let m = solid(src);
    let (count, _) = faces(&m);
    let names: Vec<&str> = count.keys().map(String::as_str).collect();
    assert_eq!(names, ["arm", "arm.tip", "base", "body"], "{count:?}");
    // Every face is red but the base's.
    let ps = m.to_polyset(&geom::color::CORNFIELD);
    let (_, ids) = m.to_polyset_with_ids(&geom::color::CORNFIELD);
    for (i, id) in ids.iter().enumerate() {
        let red = ps.colors[ps.color_indices[i] as usize].0 == [1.0, 0.0, 0.0, 1.0];
        assert_eq!(red, m.part_of(*id).map(|n| &**n) != Some("base"));
    }
}

#[test]
fn attribution_is_the_same_on_every_run() {
    let src = "module m() difference() { cube(2, center=true); sphere(1.2, $fn=16); }\n\
               for (i = [0:5]) part(str(\"p\", i)) translate([i*3,0,0]) m();\n\
               part(\"all\") for (j = [0:3]) translate([0,5+j*3,0]) m();";
    let (count, first) = faces(&solid(src));
    assert_eq!(count.len(), 7, "{count:?}");
    for _ in 0..10 {
        assert!(
            faces(&solid(src)).1 == first,
            "attribution differs between runs"
        );
    }
}
