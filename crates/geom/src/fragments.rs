//! How many segments a circle gets: OpenSCAD's
//! `CurveDiscretizer::getCircularSegmentCount`
//! (`src/core/CurveDiscretizer.cc`).
//!
//! The rule itself is `io::fragments`, shared with the evaluator, which
//! tessellates sketch arcs by it; these are its forms over a node's
//! [`Discretizer`].

use eval::node::Discretizer;

pub use io::fragments::GRID_FINE;

/// Segments for a full circle of radius `r`, or `None` where OpenSCAD
/// returns no value (tiny radius, infinite or NaN `$fn`); callers then use
/// 3, as every `value_or(3)` in `primitives.cc` does.
pub fn circular_segments(disc: &Discretizer, r: f64) -> Option<i32> {
    circular_segments_for_angle(disc, r, 360.0)
}

/// Segments for an arc of `angle_degrees` (`getCircularSegmentCount(r,
/// angle)`).
pub fn circular_segments_for_angle(disc: &Discretizer, r: f64, angle_degrees: f64) -> Option<i32> {
    io::fragments::circular_segments_for_angle(disc.fn_, disc.fa, disc.fs, r, angle_degrees)
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
        assert_eq!(
            circular_segments_for_angle(&d(10.0, 12.0, 2.0), 1.0, 90.0),
            Some(3)
        );
    }

    #[test]
    fn degenerate_inputs() {
        assert_eq!(circular_segments(&d(0.0, 12.0, 2.0), 0.0), None);
        assert_eq!(circular_segments(&d(f64::INFINITY, 12.0, 2.0), 1.0), None);
        assert_eq!(
            circular_segments(&d(0.0, 12.0, 2.0), f64::INFINITY),
            Some(30)
        );
    }
}
