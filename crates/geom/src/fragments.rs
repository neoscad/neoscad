//! How many segments a circle gets: OpenSCAD's
//! `CurveDiscretizer::getCircularSegmentCount`
//! (`src/core/CurveDiscretizer.cc`).
//!
//! The evaluator has already clamped `$fn`/`$fa`/`$fs` (negative `$fn` to 0,
//! `$fa`/`$fs` to at least 0.01) when it built the node, as the
//! `CurveDiscretizer` constructor does, so this is only the counting rule.
//! `$fe` belongs to the experimental `discretization-by-error` feature,
//! which is out of scope, so its branch is not ported.

use eval::node::Discretizer;

/// `GRID_FINE` (`src/geometry/Grid.h:20`): radii below it get no circle.
pub const GRID_FINE: f64 = 0.000_000_953_674_316_406_25;

/// Segments for a full circle of radius `r`, or `None` where OpenSCAD
/// returns no value (tiny radius, infinite or NaN `$fn`); callers then use
/// 3, as every `value_or(3)` in `primitives.cc` does.
pub fn circular_segments(disc: &Discretizer, r: f64) -> Option<i32> {
    circular_segments_for_angle(disc, r, 360.0)
}

/// Segments for an arc of `angle_degrees` (`getCircularSegmentCount(r,
/// angle)`).
pub fn circular_segments_for_angle(disc: &Discretizer, r: f64, angle_degrees: f64) -> Option<i32> {
    let fn_ = disc.fn_;
    // `r < GRID_FINE` is false for NaN, so a NaN radius falls through to the
    // arithmetic below exactly as in C++.
    if r < GRID_FINE || fn_.is_infinite() || fn_.is_nan() || angle_degrees.is_infinite() || angle_degrees.is_nan() {
        return None;
    }
    let result = if fn_ > 0.0 {
        // `$fn` is rounded up before the angle is applied, for backward
        // compatibility (the comment in CurveDiscretizer.cc).
        (if fn_ >= 3.0 { fn_ } else { 3.0 }).ceil() * angle_degrees.abs() / 360.0
    } else {
        let by_fa = 360.0 / disc.fa;
        let by_fs = r * 2.0 * std::f64::consts::PI / disc.fs;
        by_fa.min(by_fs).max(5.0).ceil() * angle_degrees.abs() / 360.0
    };
    // `std::max(1, static_cast<int>(std::ceil(result)))`: the cast saturates
    // in practice for huge values; Rust's `as` saturates by definition.
    Some((result.ceil() as i32).max(1))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(fn_: f64, fa: f64, fs: f64) -> Discretizer {
        Discretizer { fn_, fa, fs }
    }

    #[test]
    fn defaults() {
        // $fa = 12, $fs = 2: r = 1 gives max(min(30, 3.14), 5) = 5.
        assert_eq!(circular_segments(&d(0.0, 12.0, 2.0), 1.0), Some(5));
        // r = 10: min(30, 31.4) = 30.
        assert_eq!(circular_segments(&d(0.0, 12.0, 2.0), 10.0), Some(30));
        // r = 5: 2*pi*5/2 = 15.7 -> 16.
        assert_eq!(circular_segments(&d(0.0, 12.0, 2.0), 5.0), Some(16));
    }

    #[test]
    fn fn_wins_and_is_at_least_three() {
        assert_eq!(circular_segments(&d(7.0, 12.0, 2.0), 100.0), Some(7));
        assert_eq!(circular_segments(&d(1.0, 12.0, 2.0), 100.0), Some(3));
        assert_eq!(circular_segments(&d(4.5, 12.0, 2.0), 1.0), Some(5));
        assert_eq!(circular_segments_for_angle(&d(10.0, 12.0, 2.0), 1.0, 90.0), Some(3));
    }

    #[test]
    fn degenerate_inputs() {
        assert_eq!(circular_segments(&d(0.0, 12.0, 2.0), 0.0), None);
        assert_eq!(circular_segments(&d(f64::INFINITY, 12.0, 2.0), 1.0), None);
        assert_eq!(circular_segments(&d(0.0, 12.0, 2.0), f64::INFINITY), Some(30));
    }
}
