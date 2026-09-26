//! The render colour scheme's face colours. [`Color`] itself lives in the
//! `io` crate, which readers and writers share with this one.

pub use io::Color;

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
pub const CORNFIELD: Scheme = Scheme {
    face_front: Color::from_u8(0xf9, 0xd7, 0x2c),
    face_back: Color::from_u8(0x9d, 0xcb, 0x51),
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scheme_colours_round_trip_to_bytes() {
        assert_eq!(CORNFIELD.face_front.rgba_int(), Some([249, 215, 44, 255]));
        assert_eq!(CORNFIELD.face_back.rgba_int(), Some([157, 203, 81, 255]));
    }
}
