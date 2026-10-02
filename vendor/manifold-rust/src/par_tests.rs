// par_tests.rs — the parallel sites must give the sequential output, on any
// thread count.

use crate::linalg::Vec3;
use crate::manifold::Manifold;

/// FNV-1a over the output mesh, less `run_original_id`, which comes from the
/// process-wide ID counter.
fn fingerprint(m: &Manifold) -> u64 {
    let gl = m.get_mesh_gl64(-1);
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let mut eat = |bytes: &[u8]| {
        for &b in bytes {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x0100_0000_01b3);
        }
    };
    eat(&gl.num_prop.to_le_bytes());
    for x in &gl.vert_properties {
        eat(&x.to_bits().to_le_bytes());
    }
    for list in [
        &gl.tri_verts,
        &gl.merge_from_vert,
        &gl.merge_to_vert,
        &gl.run_index,
        &gl.face_id,
    ] {
        eat(&(list.len() as u64).to_le_bytes());
        for x in list {
            eat(&x.to_le_bytes());
        }
    }
    h
}

/// `f`'s fingerprint, on `threads` threads when the `parallel` feature is on.
fn run_on(threads: usize, f: &(dyn Fn() -> Manifold + Sync)) -> u64 {
    #[cfg(feature = "parallel")]
    {
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(threads)
            .build()
            .unwrap();
        pool.install(|| fingerprint(&f()))
    }
    #[cfg(not(feature = "parallel"))]
    {
        let _ = threads;
        fingerprint(&f())
    }
}

/// Pins a union of 500 cubes touching along edges and corners, which leaves
/// duplicate edges and pinched verts. Integer coordinates, so the hash holds
/// on every platform.
#[test]
fn checkerboard_union_keeps_the_sequential_output() {
    let model = || {
        let cube = Manifold::cube(Vec3::splat(1.0), false);
        let mut result = Manifold::empty();
        for x in 0..10 {
            for y in 0..10 {
                for z in 0..10 {
                    if (x + y + z) % 2 == 0 {
                        let at = Vec3::new(f64::from(x), f64::from(y), f64::from(z));
                        result = result.union(&cube.translate(at));
                    }
                }
            }
        }
        result
    };
    for threads in [1, 8] {
        assert_eq!(
            run_on(threads, &model),
            0x1d97_1ec0_be2b_3cfb,
            "{threads} threads"
        );
    }
}

/// A difference large enough for every parallel path must match on 1 and 8
/// threads. It goes through libm's `sin` and `cos`, so no hash is pinned.
#[cfg(feature = "parallel")]
#[test]
fn large_difference_is_identical_at_any_thread_count() {
    let model = || {
        let mut cutters = Vec::new();
        for i in -3..=3 {
            for j in -3..=3 {
                let c = Manifold::cylinder_centered(50.0, 2.0, 2.0, 64, true)
                    .rotate(f64::from(i) * 9.0, f64::from(j) * 7.0, 0.0)
                    .translate(Vec3::new(f64::from(i) * 5.5, f64::from(j) * 5.5, 0.0));
                cutters.push(c);
            }
        }
        let cutters = Manifold::batch_boolean(&cutters, crate::types::OpType::Add);
        let result = Manifold::sphere(20.0, 224).difference(&cutters);
        assert!(
            3 * result.num_tri() > 100_000,
            "{} triangles",
            result.num_tri()
        );
        result
    };
    assert_eq!(run_on(1, &model), run_on(8, &model));
}
