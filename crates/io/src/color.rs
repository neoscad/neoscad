//! Colours as OpenSCAD's `Color4f` holds them (`src/geometry/linalg.h`).

/// RGBA in `f32`, as `Color4f` stores it. A negative component means "not
/// set": `color()` with no usable argument keeps all four at -1.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Color(pub [f32; 4]);

impl Color {
    /// `Color4f(int r, int g, int b, int a = 255)`: each channel over 255 in
    /// `f32`, so converting back with [`Color::rgba_int`] reproduces the
    /// integers exactly.
    pub const fn from_u8(r: u8, g: u8, b: u8) -> Color {
        Color([r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0, 1.0])
    }

    /// `Color4f(int r, int g, int b, int a)`: each channel over 255 in
    /// `f32`, whatever the range (OFF files may hold values above 255).
    pub fn from_ints(r: i32, g: i32, b: i32, a: i32) -> Color {
        Color([r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0, a as f32 / 255.0])
    }

    /// `Color4f::isValid`: every component is set.
    pub fn is_valid(&self) -> bool {
        self.0.iter().all(|&c| c >= 0.0)
    }

    /// `getRgba(int&...)`: `static_cast<int>(c * 255.0f)` clamped to
    /// 0..=255, computed in `f32` so the truncation matches. `None` for an
    /// invalid colour, where the C++ leaves its outputs unset.
    pub fn rgba_int(&self) -> Option<[i32; 4]> {
        if !self.is_valid() {
            return None;
        }
        Some(self.0.map(|c| ((c * 255.0f32) as i32).clamp(0, 255)))
    }

    /// A total order for grouping faces by colour, like `std::map<Color4f>`
    /// (Eigen's lexicographic compare of the four floats). NaN cannot occur:
    /// the evaluator's colours come from finite numbers or parsed names.
    pub fn key(&self) -> [u32; 4] {
        self.0.map(|c| {
            // Order-preserving map of f32 to u32 (flip negatives).
            let b = c.to_bits();
            if b & 0x8000_0000 != 0 { !b } else { b | 0x8000_0000 }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_and_out_of_range() {
        assert_eq!(Color([-1.0; 4]).rgba_int(), None);
        assert_eq!(Color([1.5, 0.5, 0.0, 1.0]).rgba_int(), Some([255, 127, 0, 255]));
    }

    #[test]
    fn key_orders_like_floats() {
        let a = Color([0.1, 0.0, 0.0, 1.0]).key();
        let b = Color([0.2, 0.0, 0.0, 1.0]).key();
        let n = Color([-1.0, 0.0, 0.0, 1.0]).key();
        assert!(n < a && a < b);
    }
}
