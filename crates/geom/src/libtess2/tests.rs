//! Faces whose triangles were taken from OpenSCAD's own code: libtess2's C
//! sources from the reference checkout with `GeometryUtils.cc`'s
//! `tessellatePolygonWithHoles` and `PolySetUtils::tessellate_faces`
//! around them, built with Apple clang `-O3` (the scratch oracle described
//! in `docs/followups.md`). The degenerate ones are the cases where libtess2
//! merges, splits or drops what it is given, and where OpenSCAD's clean-up
//! (the index clean-up before, the flip test and `triangulateLoops` after)
//! decides the result.

use super::Tessellator;
use crate::polyset::PolySet;

struct Case {
    name: &'static str,
    verts: &'static [[f64; 3]],
    faces: &'static [&'static [u32]],
    /// Vertices left after `tessellate_faces` drops unused ones.
    nverts: usize,
    /// Triangles, in OpenSCAD's order, over the vertices left.
    tris: &'static [[u32; 3]],
}

const CASES: &[Case] = &[
    Case {
        name: "collinear_midpoints",
        verts: &[
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [2.0, 0.0, 0.0],
            [2.0, 1.0, 0.0],
            [1.0, 1.0, 0.0],
            [0.0, 1.0, 0.0],
        ],
        faces: &[&[0, 1, 2, 3, 4, 5]],
        nverts: 6,
        tris: &[[4, 2, 3], [4, 1, 2], [5, 1, 4], [1, 5, 0]],
    },
    Case {
        name: "all_collinear",
        verts: &[
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [2.0, 0.0, 0.0],
            [3.0, 0.0, 0.0],
        ],
        faces: &[&[0, 1, 2, 3]],
        nverts: 4,
        tris: &[[3, 0, 1], [3, 1, 2]],
    },
    Case {
        name: "zero_area_back_and_forth",
        verts: &[
            [0.0, 0.0, 0.0],
            [2.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [3.0, 0.0, 0.0],
        ],
        faces: &[&[0, 1, 2, 3]],
        nverts: 4,
        tris: &[[3, 0, 1], [3, 1, 2]],
    },
    Case {
        name: "repeated_index",
        verts: &[
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [1.0, 1.0, 0.0],
            [0.0, 1.0, 0.0],
        ],
        faces: &[&[0, 1, 1, 2, 3]],
        nverts: 4,
        tris: &[[3, 1, 2], [1, 3, 0]],
    },
    Case {
        name: "null_ear",
        verts: &[
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [1.0, 1.0, 0.0],
            [0.0, 1.0, 0.0],
            [2.0, 2.0, 0.0],
        ],
        faces: &[&[0, 1, 4, 1, 2, 3]],
        nverts: 5,
        tris: &[[3, 1, 2], [1, 3, 0]],
    },
    Case {
        name: "pinched_duplicate_coords",
        verts: &[
            [0.0, 0.0, 0.0],
            [2.0, 0.0, 0.0],
            [1.0, 1.0, 0.0],
            [2.0, 2.0, 0.0],
            [0.0, 2.0, 0.0],
            [1.0, 1.0, 0.0],
        ],
        faces: &[&[0, 1, 2, 3, 4, 5]],
        nverts: 6,
        tris: &[[3, 4, 5], [1, 5, 0], [5, 1, 2], [5, 2, 3]],
    },
    Case {
        name: "bowtie",
        verts: &[
            [0.0, 0.0, 0.0],
            [1.0, 1.0, 0.0],
            [1.0, 0.0, 0.0],
            [0.0, 1.0, 0.0],
        ],
        faces: &[&[0, 1, 2, 3]],
        nverts: 4,
        tris: &[[3, 0, 1], [3, 1, 2]],
    },
    Case {
        name: "pentagram",
        verts: &[
            [0.955336489125606, 0.29552020666133955, 0.0],
            [0.014158792244151968, 0.9998997592769922, 0.0],
            [-0.9465858742790716, 0.32245182991467986, 0.0],
            [-0.5991810358191534, -0.8006135686551199, 0.0],
            [0.5762716287284666, -0.8172582271978914, 0.0],
        ],
        faces: &[&[0, 2, 4, 1, 3]],
        nverts: 5,
        tris: &[[3, 0, 2], [3, 2, 4], [3, 4, 1]],
    },
    Case {
        name: "concave_l",
        verts: &[
            [0.0, 0.0, 0.0],
            [2.0, 0.0, 0.0],
            [2.0, 1.0, 0.0],
            [1.0, 1.0, 0.0],
            [1.0, 2.0, 0.0],
            [0.0, 2.0, 0.0],
        ],
        faces: &[&[0, 1, 2, 3, 4, 5]],
        nverts: 6,
        tris: &[[4, 5, 3], [3, 1, 2], [0, 3, 5], [3, 0, 1]],
    },
    Case {
        name: "nonplanar_quad",
        verts: &[
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.3],
            [1.1, 1.0, -0.2],
            [0.0, 0.9, 0.1],
        ],
        faces: &[&[0, 1, 2, 3]],
        nverts: 4,
        tris: &[[3, 1, 2], [1, 3, 0]],
    },
    Case {
        name: "regular_12gon",
        verts: &[
            [13.0, -4.0, 5.0],
            [12.598076211353316, -2.5, 5.0],
            [11.5, -1.401923788646684, 5.0],
            [10.0, -1.0, 5.0],
            [8.5, -1.401923788646684, 5.0],
            [7.401923788646684, -2.5, 5.0],
            [7.0, -3.9999999999999996, 5.0],
            [7.401923788646684, -5.499999999999999, 5.0],
            [8.499999999999998, -6.598076211353315, 5.0],
            [10.0, -7.0, 5.0],
            [11.5, -6.598076211353316, 5.0],
            [12.598076211353316, -5.500000000000002, 5.0],
        ],
        faces: &[&[0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11]],
        nverts: 12,
        tris: &[
            [1, 11, 0],
            [2, 11, 1],
            [2, 10, 11],
            [3, 10, 2],
            [3, 9, 10],
            [4, 9, 3],
            [4, 8, 9],
            [5, 8, 4],
            [5, 7, 8],
            [7, 5, 6],
        ],
    },
    Case {
        name: "thin_rhombus_flips",
        verts: &[
            [0.0, 0.0, 0.0],
            [4.0, 0.5, 0.0],
            [8.0, 0.0, 0.0],
            [4.0, -0.5, 0.0],
        ],
        faces: &[&[0, 3, 2, 1]],
        nverts: 4,
        tris: &[[1, 3, 2], [3, 1, 0]],
    },
    Case {
        name: "rotated_ellipse_9gon",
        verts: &[
            [3.950066710984226, 1.410499067523344, 2.837236978694042],
            [2.609614114795233, 0.6999593004398136, 2.7580370622404837],
            [
                0.04809407166325291,
                -0.33809920250061076,
                1.3883209521969846,
            ],
            [
                -2.5359297221060317,
                -1.2179573310369158,
                -0.6310059608481868,
            ],
            [
                -3.9333638151824126,
                -1.5279196882930912,
                -2.3550781719623943,
            ],
            [-3.4903332646654683, -1.122951442461091, -2.977183132637],
            [-1.414136988878195, -0.19254173648642858, -2.206231017845856],
            [1.3237497003871788, 0.8279603878532771, -0.40295889027808973],
            [3.4422391930022154, 1.461050644961702, 1.588862180440015],
        ],
        faces: &[&[0, 1, 2, 3, 4, 5, 6, 7, 8]],
        nverts: 9,
        tris: &[
            [1, 8, 0],
            [2, 8, 1],
            [2, 7, 8],
            [3, 7, 2],
            [3, 6, 7],
            [4, 6, 3],
            [6, 4, 5],
        ],
    },
    Case {
        name: "two_faces_shared",
        verts: &[
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [1.0, 1.0, 0.0],
            [0.0, 1.0, 0.0],
            [2.0, 0.0, 0.0],
            [2.0, 1.0, 0.0],
        ],
        faces: &[&[0, 1, 2, 3], &[1, 4, 5, 2]],
        nverts: 6,
        tris: &[[3, 1, 2], [1, 3, 0], [2, 4, 5], [4, 2, 1]],
    },
];

fn polyset(c: &Case) -> PolySet {
    PolySet {
        vertices: c.verts.to_vec(),
        faces: c.faces.iter().map(|f| f.to_vec()).collect(),
        ..Default::default()
    }
}

#[test]
fn faces_tessellate_as_openscad_does() {
    for c in CASES {
        let t = polyset(c).tessellate(&mut Vec::new());
        let tris: Vec<[u32; 3]> = t.faces.iter().map(|f| [f[0], f[1], f[2]]).collect();
        assert_eq!(t.vertices.len(), c.nverts, "{}: vertices", c.name);
        assert_eq!(tris, c.tris, "{}: triangles", c.name);
    }
}

/// The convex fast path (`convex.rs`) must give the full sweep's triangles
/// on every face, whichever of its routes the face takes.
#[test]
fn the_fast_path_agrees_with_the_full_sweep() {
    for c in CASES {
        let verts: Vec<[f32; 3]> = c.verts.iter().map(|v| v.map(|x| x as f32)).collect();
        for face in c.faces {
            let mut fast = Tessellator::new();
            let mut full = Tessellator::new();
            full.set_fast_paths(false);
            let (mut a, mut b) = (Vec::new(), Vec::new());
            fast.tessellate_polygon(&verts, face, &mut a);
            full.tessellate_polygon(&verts, face, &mut b);
            assert_eq!(a, b, "{}", c.name);
        }
    }
}

/// A polygon whose mesh breaks (where upstream would follow a null link)
/// yields nothing, and leaves nothing behind: the next polygon comes out
/// as from a fresh tessellator. The broken flag of the dictionary arena
/// once survived into the next polygon and emptied it.
///
/// The polygon below breaks the mesh only where multiply-adds fuse
/// (`geom::fma`: aarch64). Rounded twice, as on x86_64 and wasm32, it
/// tessellates into eight triangles, and no breaking input is known
/// there: a search of 3 million random polygons (grids of several steps,
/// tilted planes, several contours, scales from 1e-30 to 3e7) under
/// Rosetta found none. So the whole path is checked on aarch64, and on
/// every platform the flag is also set by hand before the next polygon.
#[test]
fn a_broken_polygon_does_not_affect_the_next() {
    let broken: [[f32; 3]; 10] = [
        [-0.04, 0.0, 0.01],
        [-0.03, 0.0, 0.04],
        [0.0, 0.0, 0.0],
        [-0.03, 0.0, 0.01],
        [0.0, 0.0, 0.01],
        [-0.03, 0.0, 0.01],
        [-0.04, 0.0, 0.0],
        [-0.02, 0.0, 0.02],
        [-0.04, 0.0, 0.0],
        [0.0, 0.0, 0.04],
    ];
    let next: [[f32; 3]; 8] = [
        [1.6208199, 0.0050529586, 1.248955],
        [2.0275538, -0.49888518, -0.27562904],
        [1.273268, -0.26171252, -1.601791],
        [-0.2775204, -0.42628258, -2.0272958],
        [-1.7083656, -0.23036602, -1.1262473],
        [-2.013338, 0.31373334, 0.36526147],
        [-1.1286497, -0.25833455, 1.7067794],
        [0.31682545, -0.3931642, 2.021526],
    ];
    let face = |n: u32| (0..n).collect::<Vec<u32>>();
    for fast in [true, false] {
        let mut t = Tessellator::new();
        t.set_fast_paths(fast);
        let mut out = Vec::new();
        t.tessellate_polygon(&broken, &face(10), &mut out);
        if cfg!(target_arch = "aarch64") {
            assert!(out.is_empty(), "the broken polygon produced {out:?}");
        }
        out.clear();
        t.tessellate_polygon(&next, &face(8), &mut out);
        let mut fresh = Tessellator::new();
        fresh.set_fast_paths(fast);
        let mut want = Vec::new();
        fresh.tessellate_polygon(&next, &face(8), &mut want);
        assert!(!want.is_empty());
        assert_eq!(out, want);
    }
    // The flag a break leaves set, set by hand: the next polygon starts
    // clean and comes out as from a fresh tessellator.
    let run = |poisoned: bool| {
        let mut t = super::Tess::default();
        if poisoned {
            t.broken.set(true);
        }
        t.begin(&next, &[8]);
        assert!(t.tesselate(), "poisoned: {poisoned}");
        assert!(!t.failed(), "poisoned: {poisoned}");
        (t.elements.clone(), t.vertex_indices.clone())
    };
    assert_eq!(run(true), run(false));
}

/// Holes: a contour inside the outline and winding against it (as a Nef
/// facet's holes do) is cut out, holes that clean down to fewer than
/// three points are dropped, and an outline that does leaves nothing, as
/// in `tessellatePolygonWithHoles`. With every hole gone the outline
/// comes out as a lone face would. (A hole winding with the outline is
/// cut out by libtess2 too, but upstream's repair then flips the
/// triangles along it and refills the hole from its edges; that is kept.)
#[test]
fn holes_are_cut_and_collapsed_ones_dropped() {
    let verts: [[f32; 3]; 8] = [
        [0.0, 0.0, 0.0],
        [4.0, 0.0, 0.0],
        [4.0, 4.0, 0.0],
        [0.0, 4.0, 0.0],
        [1.0, 1.0, 0.0],
        [1.0, 3.0, 0.0],
        [3.0, 3.0, 0.0],
        [3.0, 1.0, 0.0],
    ];
    let area = |tris: &[[u32; 3]]| -> f32 {
        tris.iter()
            .map(|t| {
                let [a, b, c] = t.map(|i| verts[i as usize]);
                ((b[0] - a[0]) * (c[1] - a[1]) - (c[0] - a[0]) * (b[1] - a[1])) / 2.0
            })
            .sum()
    };
    let outline = vec![0, 1, 2, 3];
    let mut out = Vec::new();
    Tessellator::new().tessellate_polygon_with_holes(
        &verts,
        &[outline.clone(), vec![4, 5, 6, 7]],
        &mut out,
    );
    assert_eq!(out.len(), 8, "{out:?}");
    assert_eq!(area(&out), 12.0);
    let mut lone = Vec::new();
    Tessellator::new().tessellate_polygon(&verts, &outline, &mut lone);
    out.clear();
    // A hole of one repeated point cleans away.
    Tessellator::new().tessellate_polygon_with_holes(
        &verts,
        &[outline.clone(), vec![4, 4, 4]],
        &mut out,
    );
    assert_eq!(out, lone);
    out.clear();
    Tessellator::new().tessellate_polygon_with_holes(
        &verts,
        &[vec![0, 1, 0], vec![4, 5, 6, 7]],
        &mut out,
    );
    assert!(out.is_empty(), "{out:?}");
}
