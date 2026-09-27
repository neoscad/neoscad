//! The speed patches in `vendor/` (see `vendor/README.md`) must not change
//! a single output byte. Each test here pins behaviour the patched code has
//! to reproduce exactly, so a later edit to either patch that changes the
//! result fails here rather than as a drifted export.

use manifold_rust::linalg::Vec2;
use manifold_rust::polygon::triangulate;

/// FNV-1a over the triangle list, so the test pins both the triangles and
/// their order (the order reaches the exported files).
fn fnv(tris: &[manifold_rust::linalg::IVec3]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for t in tris {
        for c in [t.x, t.y, t.z] {
            for b in c.to_le_bytes() {
                h ^= u64::from(b);
                h = h.wrapping_mul(0x0100_0000_01b3);
            }
        }
    }
    h
}

/// manifold-rust's ear clipper keyholes each hole into the outer ring by
/// walking every outer ring twice per hole; the patch walks the rings in
/// place instead of collecting them, and must pick the same bridges. A
/// 24x24 grid of octagonal holes (exact coordinates, so the pinned hash
/// does not depend on a platform's `sin`/`cos`), with every other row
/// shifted so the bridges run between holes rather than straight to the
/// outer edge. The hash was taken with the unpatched vendored copy.
#[test]
fn earclip_many_holes_is_unchanged() {
    let n = 24;
    let size = 3.0 * f64::from(n) + 3.0;
    let mut polys = vec![vec![
        Vec2::new(0.0, 0.0),
        Vec2::new(size, 0.0),
        Vec2::new(size, size),
        Vec2::new(0.0, size),
    ]];
    // Clockwise, so each ring is a hole.
    let octagon = [
        (1.0, -0.5),
        (0.5, -1.0),
        (-0.5, -1.0),
        (-1.0, -0.5),
        (-1.0, 0.5),
        (-0.5, 1.0),
        (0.5, 1.0),
        (1.0, 0.5),
    ];
    for i in 0..n {
        for j in 0..n {
            let cx = 3.0 * f64::from(i) + 2.0 + if j % 2 == 1 { 0.75 } else { 0.0 };
            let cy = 3.0 * f64::from(j) + 2.0;
            polys.push(
                octagon
                    .iter()
                    .map(|&(x, y)| Vec2::new(cx + x, cy + y))
                    .collect(),
            );
        }
    }
    let tris = triangulate(&polys, 1e-9, true);
    // Every vertex is kept: 4 + 8n² verts and n² holes give v + 2h - 2 triangles.
    let verts = 4 + 8 * n * n;
    assert_eq!(tris.len() as i32, verts + 2 * n * n - 2);
    assert_eq!(fnv(&tris), 0xb444_9c9b_cd61_b83e, "hash {:#x}", fnv(&tris));
}

/// clipper2-rust's `nearbyint_f64` as released in 1.2.0, before the patch
/// replaced it with `round_ties_even`.
fn nearbyint_unpatched(x: f64) -> f64 {
    let trunc = x.trunc();
    let frac = x - trunc;
    if frac.abs() < 0.5 {
        trunc
    } else if frac.abs() > 0.5 {
        trunc + frac.signum()
    } else if trunc % 2.0 == 0.0 {
        trunc
    } else {
        trunc + frac.signum()
    }
}

fn same(x: f64) {
    let old = nearbyint_unpatched(x);
    let new = clipper2_rust::core::nearbyint_f64(x);
    if old.is_nan() {
        assert!(new.is_nan(), "{x:e}: {new:e} instead of NaN");
    } else {
        assert_eq!(
            old.to_bits(),
            new.to_bits(),
            "{x:e}: {new:e} instead of {old:e}"
        );
    }
}

/// The patched rounding must agree with the old one bit for bit, the sign
/// of zero included, and keep turning ±∞ into NaN: `top_x` casts the result
/// to `i64`, where NaN gives 0 but ∞ would give `i64::MAX` and overflow.
#[test]
fn clipper_rounding_is_unchanged() {
    for x in [
        0.0,
        -0.0,
        0.5,
        -0.5,
        1.5,
        -1.5,
        2.5,
        -2.5,
        0.499_999_999_999_999_94,
        -0.499_999_999_999_999_94,
        4_503_599_627_370_495.5,
        -4_503_599_627_370_495.5,
        4_503_599_627_370_496.0,
        9_007_199_254_740_993.0,
        f64::MIN_POSITIVE,
        -f64::MIN_POSITIVE,
        5e-324,
        f64::MAX,
        f64::MIN,
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::NAN,
    ] {
        same(x);
    }
    // Every half-integer and its neighbours over a range of i64 coordinates.
    for i in -100_000i64..100_000 {
        let h = i as f64 + 0.5;
        for x in [h, h.next_up(), h.next_down(), i as f64] {
            same(x);
        }
    }
    // Arbitrary bit patterns, from a fixed xorshift seed so the run is
    // reproducible: all exponents, both signs, NaNs and infinities.
    let mut s: u64 = 0x9e37_79b9_7f4a_7c15;
    for _ in 0..2_000_000 {
        s ^= s << 13;
        s ^= s >> 7;
        s ^= s << 17;
        same(f64::from_bits(s));
        // And values in Clipper's working range, with fractional parts.
        same((s >> 11) as f64 / 2048.0 - 2.0e12);
    }
}
