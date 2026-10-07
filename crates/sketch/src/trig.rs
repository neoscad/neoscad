//! The solver's only trigonometry, through the pure-Rust `libm` crate so
//! that it has the same bits on every target. The platform maths
//! libraries (macOS's, glibc's, Rust's wasm32 fallbacks) disagree in the
//! last place for some inputs, and a last-bit difference in an angle
//! constraint's cosine would move every coordinate that depends on it.

/// cos and sin of `deg` degrees. Multiples of 90° are exact (cos 90° is 0,
/// not 6.1e-17), so a right angle leaves no noise in the coordinates.
pub(crate) fn cos_sin_degrees(deg: f64) -> (f64, f64) {
    // `%` on f64 is exact (fmod has no rounding), so this is the same
    // everywhere.
    let r = deg % 360.0;
    let r = if r < 0.0 { r + 360.0 } else { r };
    if r % 90.0 == 0.0 {
        return match (r / 90.0) as u32 {
            0 | 4 => (1.0, 0.0),
            1 => (0.0, 1.0),
            2 => (-1.0, 0.0),
            _ => (0.0, -1.0),
        };
    }
    let rad = r * (core::f64::consts::PI / 180.0);
    (libm::cos(rad), libm::sin(rad))
}

/// atan2(y, x) in degrees, in (−180, 180].
pub(crate) fn atan2_degrees(y: f64, x: f64) -> f64 {
    libm::atan2(y, x) * (180.0 / core::f64::consts::PI)
}

/// cos and sin of `rad` radians.
pub(crate) fn cos_sin(rad: f64) -> (f64, f64) {
    (libm::cos(rad), libm::sin(rad))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn right_angles_are_exact() {
        assert_eq!(cos_sin_degrees(90.0), (0.0, 1.0));
        assert_eq!(cos_sin_degrees(-90.0), (0.0, -1.0));
        assert_eq!(cos_sin_degrees(540.0), (-1.0, 0.0));
        assert_eq!(cos_sin_degrees(0.0), (1.0, 0.0));
        let (c, s) = cos_sin_degrees(45.0);
        assert!((c - s).abs() < 3e-16);
        assert!((atan2_degrees(1.0, 0.0) - 90.0).abs() < 1e-13);
    }
}
