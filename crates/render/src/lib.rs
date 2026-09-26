//! NeoSCAD's renderer: OpenSCAD's images (`-o x.png`: render mode, the
//! OpenCSG and throwntogether previews, the `--view` options) drawn with
//! wgpu, and `neoscad snapshot`'s contact sheets.
//!
//! OpenSCAD's own renderer is fixed-function OpenGL (`src/glview`). This
//! crate reproduces what it draws, not how: the same camera maths
//! ([`camera`], a port of `Camera` and `GLView::setupCamera`), the same
//! colour schemes ([`scheme`], OpenSCAD's JSON files compiled in), the same
//! meshes and colours ([`scene`], after `PolySetRenderer` and `VBOBuilder`)
//! and the same lighting (`shader.wgsl`, OpenSCAD's two-light
//! fixed-function set-up written out as a shader).
//!
//! The layers are kept apart so that the macOS app (a `CAMetalLayer`
//! surface), the web app (a WebGPU canvas) and the offscreen exporter share
//! everything but the target:
//!
//! - [`camera`], [`scheme`] and [`scene`] are plain Rust with no GPU types:
//!   they build on every target, and a scene is built once per model.
//! - [`gpu`] (feature `gpu`) uploads a scene and records a frame into any
//!   colour view the caller owns.
//! - [`offscreen`] (feature `gpu`) is one such caller: a texture read back
//!   into memory, which [`encode_png`] turns into OpenSCAD's PNG.
//! - [`preview`] turns OpenSCAD's preview model (CSG products,
//!   `geom::csg`) into a scene: the OpenCSG preview, with `%` and `#`
//!   objects, and the throwntogether view.
//! - [`overlay`] builds the `--view` options' lines (axes, scale markers
//!   with [`hershey`] numbers, crosshairs) for a camera; edges are a
//!   shader mode.
//! - [`snapshot`] lays out and annotates contact sheets.

pub mod camera;
pub mod hershey;
pub mod overlay;
pub mod preview;
pub mod scene;
pub mod scheme;
pub mod snapshot;

#[cfg(feature = "gpu")]
pub mod gpu;
#[cfg(feature = "gpu")]
pub mod offscreen;

pub use camera::{Camera, Projection};
pub use overlay::{Overlay, ViewOptions};
pub use preview::Previewer;
pub use scene::Scene;
pub use scheme::ColorScheme;

/// A rendered image: `width * height` pixels, RGBA, top row first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Image {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// `setupCamera` in `export_png.cc`: with `--viewall`, fit the scene's
/// bounding box (`Camera::viewAll`, which also centres on it with
/// `--autocenter`). Without `--viewall` the camera is used as it is, so
/// `--autocenter` alone does nothing, as in OpenSCAD.
pub fn fit_camera(camera: &mut Camera, scene: &Scene) {
    if camera.viewall {
        camera.view_all(scene.bounding_box());
    }
}

/// An RGBA image (top row first) as the PNG OpenSCAD writes: 8-bit RGB,
/// the alpha channel dropped (`write_png` in `imageutils-lodepng.cc`
/// forces `LCT_RGB`; OpenSCAD also clears the framebuffer's alpha to
/// opaque before reading it, so nothing is lost). The encoding is
/// deterministic: the same pixels always give the same bytes.
pub fn encode_png(width: u32, height: u32, rgba: &[u8]) -> Vec<u8> {
    let mut rgb = Vec::with_capacity(rgba.len() / 4 * 3);
    for p in rgba.as_chunks::<4>().0 {
        rgb.extend_from_slice(&p[..3]);
    }
    let mut out = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut out, width, height);
        encoder.set_color(png::ColorType::Rgb);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder
            .write_header()
            .expect("writing a PNG header to memory cannot fail");
        writer
            .write_image_data(&rgb)
            .expect("the pixel buffer matches the image size");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn png_is_rgb_and_round_trips() {
        let rgba = [1, 2, 3, 255, 4, 5, 6, 0];
        let data = encode_png(2, 1, &rgba);
        let decoder = png::Decoder::new(std::io::Cursor::new(&data));
        let mut reader = decoder.read_info().unwrap();
        let mut buf = vec![0; reader.output_buffer_size().unwrap()];
        let info = reader.next_frame(&mut buf).unwrap();
        assert_eq!(info.color_type, png::ColorType::Rgb);
        assert_eq!(&buf[..info.buffer_size()], &[1, 2, 3, 4, 5, 6]);
        assert_eq!(encode_png(2, 1, &rgba), data, "deterministic");
    }
}
