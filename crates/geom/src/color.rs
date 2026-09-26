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

/// The render colour scheme's two face colours that reach geometry: the
/// Manifold backend paints unmarked faces with the front colour and faces
/// cut by a `difference()` with the back colour when it turns its result
/// into a mesh (`ManifoldGeometry::toPolySet`). Exports therefore carry
/// them, and a different `--colorscheme` changes exported files.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Scheme {
    pub face_front: Color,
    pub face_back: Color,
}

/// OpenSCAD's built-in default scheme, "Cornfield"
/// (`src/glview/ColorMap.cc:19,37,41`): `CGAL_FACE_FRONT_COLOR` #f9d72c
/// and `CGAL_FACE_BACK_COLOR` #9dcb51.
pub const CORNFIELD: Scheme = Scheme { face_front: Color::from_u8(0xf9, 0xd7, 0x2c), face_back: Color::from_u8(0x9d, 0xcb, 0x51) };

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scheme_colours_round_trip_to_bytes() {
        assert_eq!(CORNFIELD.face_front.rgba_int(), Some([249, 215, 44, 255]));
        assert_eq!(CORNFIELD.face_back.rgba_int(), Some([157, 203, 81, 255]));
    }

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
