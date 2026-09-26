//! The union of two touching parts of a BOSL2 `cubetruss` came out 7.3 mm³
//! too large: after the boolean, manifold-rust's edge-collapse cleanup slid
//! a vertex off the crease between two faces and filled a concave corner
//! (C++ Manifold 3.5.2 does the same on these meshes). The fix is the
//! patch in `vendor/manifold-rust` (see `vendor/README.md`). These are the
//! two operands, cut down to the region around that corner, with every
//! coordinate exact (`{:?}` of the f64): a header `3 <verts> <tris>
//! <tolerance> 0`, then vertex lines, then triangle lines.

use manifold_rust::manifold::Manifold;
use manifold_rust::types::{MeshGL64, OpType};

fn mesh(text: &str) -> Manifold {
    let mut lines = text.lines();
    let head: Vec<&str> = lines.next().expect("header").split(' ').collect();
    let nv: usize = head[1].parse().expect("vertex count");
    let nt: usize = head[2].parse().expect("triangle count");
    let mut m = MeshGL64 {
        num_prop: 3,
        tolerance: head[3].parse().expect("tolerance"),
        ..Default::default()
    };
    for l in lines.by_ref().take(nv) {
        m.vert_properties
            .extend(l.split(' ').map(|x| x.parse::<f64>().expect("coordinate")));
    }
    for l in lines.take(nt) {
        m.tri_verts
            .extend(l.split(' ').map(|x| x.parse::<u64>().expect("index")));
    }
    Manifold::from_mesh_gl64(&m)
}

#[test]
fn union_keeps_concave_corner() {
    let a = mesh(include_str!("data/collapse-crease-a.txt"));
    let b = mesh(include_str!("data/collapse-crease-b.txt"));
    let expected = a.volume() + b.volume() - a.boolean(&b, OpType::Intersect).volume();
    for (x, y) in [(&a, &b), (&b, &a)] {
        let v = x.boolean(y, OpType::Add).volume();
        // Unpatched, a + b is 328.29 (C++ Manifold 3.5.2 gives the same)
        // against 314.49.
        assert!(
            (v - expected).abs() < 1e-9 * expected,
            "union volume {v}, expected {expected}"
        );
    }
}
