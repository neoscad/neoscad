//! How many segments a circle gets: OpenSCAD's
//! `CurveDiscretizer::getCircularSegmentCount`
//! (`src/core/CurveDiscretizer.cc`).
//!
//! The rule lives here, below both the evaluator and `geom`, so that a
//! sketch's arcs and circles (tessellated by the evaluator) and `circle()`
//! (tessellated by `geom`) count segments by one rule: a sketch circle must
//! have exactly the vertices of `circle(r)`, and two copies of the rule
//! would drift (docs/language-extensions.md, section 8). `geom::fragments`
//! re-exports it over its node type.
//!
//! The caller has already clamped `$fn`/`$fa`/`$fs` (negative `$fn` to 0,
//! `$fa`/`$fs` to at least 0.01) as the `CurveDiscretizer` constructor
//! does, so this is only the counting rule. `$fe` belongs to the
//! experimental `discretization-by-error` feature, which is out of scope,
//! so its branch is not ported.

/// `GRID_FINE` (`src/geometry/Grid.h:20`): radii below it get no circle.
pub const GRID_FINE: f64 = 0.000_000_953_674_316_406_25;

/// Segments for an arc of `angle_degrees` of radius `r`, under `$fn`,
/// `$fa` and `$fs` (`getCircularSegmentCount(r, angle)`), or `None` where
/// OpenSCAD returns no value (tiny radius, infinite or NaN `$fn` or angle).
pub fn circular_segments_for_angle(
    fn_: f64,
    fa: f64,
    fs: f64,
    r: f64,
    angle_degrees: f64,
) -> Option<i32> {
    // `r < GRID_FINE` is false for NaN, so a NaN radius falls through to the
    // arithmetic below exactly as in C++.
    if r < GRID_FINE
        || fn_.is_infinite()
        || fn_.is_nan()
        || angle_degrees.is_infinite()
        || angle_degrees.is_nan()
    {
        return None;
    }
    let result = if fn_ > 0.0 {
        // `$fn` is rounded up before the angle is applied, for backward
        // compatibility (the comment in CurveDiscretizer.cc).
        (if fn_ >= 3.0 { fn_ } else { 3.0 }).ceil() * angle_degrees.abs() / 360.0
    } else {
        let by_fa = 360.0 / fa;
        let by_fs = r * 2.0 * std::f64::consts::PI / fs;
        by_fa.min(by_fs).max(5.0).ceil() * angle_degrees.abs() / 360.0
    };
    // `std::max(1, static_cast<int>(std::ceil(result)))`: the cast saturates
    // in practice for huge values; Rust's `as` saturates by definition.
    Some((result.ceil() as i32).max(1))
}
