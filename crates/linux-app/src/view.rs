//! The 3D view's drawing and input, without GTK.
//!
//! How the wgpu viewport reaches the screen: it draws into a texture of
//! the widget's size in device pixels (`render::viewport::Viewport::
//! attach_texture`) and the frame is read back and handed to GTK as a
//! `GdkMemoryTexture`, which GTK composites like any image. Why not a
//! window surface (see docs/linux-app.md, "The viewport" for the rest):
//! GTK 4 gives a widget no native subwindow on either Wayland or X11, so
//! a wgpu surface would have to cover the whole toplevel or float in a
//! separate subsurface GTK does not stack or clip; and sharing GTK's GL
//! context with wgpu's GL backend ties the view to GL while GTK itself may
//! render with Vulkan or Cairo. A texture works the same under Wayland,
//! X11, Xvfb and any GSK renderer, and lets wgpu pick Vulkan (or GL)
//! independently of GTK. The cost is one copy per frame, and a frame is
//! drawn only when something changed (a model, a camera move, a resize).

use render::viewport::{Gpu, Viewport};
use std::sync::Arc;

/// A frame for GTK: RGBA, top row first, `width * 4` bytes a row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// The viewport of one window, drawing into a texture.
#[derive(Debug)]
pub struct ViewCanvas {
    pub viewport: Viewport,
}

impl ViewCanvas {
    pub fn new(gpu: Arc<Gpu>, scheme: render::ColorScheme) -> Result<ViewCanvas, String> {
        let mut viewport = Viewport::new(gpu, scheme).map_err(|e| e.to_string())?;
        viewport.attach_texture(0, 0, 1.0);
        Ok(ViewCanvas { viewport })
    }

    /// The widget is `width` by `height` logical pixels at `scale` device
    /// pixels each. Whether that is a new size.
    pub fn set_size(&mut self, width: i32, height: i32, scale: f64) -> bool {
        let (w, h) = device_size(width, height, scale);
        if (w, h) == self.viewport.size() && scale == self.viewport.scale() {
            return false;
        }
        self.viewport.resize(w, h, scale);
        true
    }

    /// A new frame if anything changed since the last one.
    pub fn frame(&mut self) -> Result<Option<Frame>, String> {
        if !self.viewport.needs_draw() {
            return Ok(None);
        }
        let image = self
            .viewport
            .read_pixels_blocking()
            .map_err(|e| e.to_string())?;
        Ok(Some(Frame {
            width: image.width,
            height: image.height,
            rgba: image.rgba,
        }))
    }

    /// The camera, as a run sees it (`$vpt`, `$vpr`, `$vpd`, `$vpf`); no
    /// "Viewall and autocenter disabled" warning, which only the command
    /// line's `--viewall` earns.
    pub fn run_camera(&self) -> eval::Camera {
        let c = self.viewport.camera();
        eval::Camera {
            vpt: c.vpt(),
            vpr: c.vpr(),
            vpd: c.viewer_distance,
            vpf: c.fov,
            auto: false,
            locked: false,
        }
    }
}

/// Logical pixels at `scale` as whole device pixels (0 for a collapsed
/// pane, which draws nothing until it grows).
pub fn device_size(width: i32, height: i32, scale: f64) -> (u32, u32) {
    let px = |v: i32| {
        let p = f64::from(v.max(0)) * scale;
        if p.is_finite() { p.round() as u32 } else { 0 }
    };
    (px(width), px(height))
}

/// What a drag in the view does: OpenSCAD's mouse (left orbits, right
/// pans, middle zooms), with Shift+left panning for touchpads, which have
/// no middle button and whose right button is a two-finger click.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Drag {
    Orbit,
    Pan,
    Zoom,
}

/// The drag for GDK mouse `button` (1 left, 2 middle, 3 right).
pub fn drag_for(button: u32, shift: bool) -> Option<Drag> {
    match (button, shift) {
        (1, false) => Some(Drag::Orbit),
        (1, true) | (3, _) => Some(Drag::Pan),
        (2, _) => Some(Drag::Zoom),
        _ => None,
    }
}

/// Apply a drag step of `dx`, `dy` logical pixels (y down).
pub fn apply_drag(viewport: &mut Viewport, drag: Drag, dx: f64, dy: f64) {
    match drag {
        Drag::Orbit => viewport.orbit(dx, dy),
        Drag::Pan => viewport.pan(dx, dy),
        // A vertical middle drag zooms as the wheel does, a notch per 12
        // pixels (OpenSCAD's `QGLView::mouseMoveEvent` zooms by `dy`).
        Drag::Zoom => zoom(viewport, -dy / 12.0),
    }
}

/// Zoom by wheel `notches` (positive: in), as the macOS view does.
pub fn zoom(viewport: &mut Viewport, notches: f64) {
    viewport.with_camera(|c| c.zoom(120.0 * notches));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_round_to_device_pixels() {
        assert_eq!(device_size(400, 300, 2.0), (800, 600));
        assert_eq!(device_size(401, 301, 1.5), (602, 452));
        assert_eq!(device_size(-5, 0, 1.0), (0, 0));
        assert_eq!(device_size(10, 10, f64::NAN), (0, 0));
    }

    #[test]
    fn buttons_map_to_openscads_drags() {
        assert_eq!(drag_for(1, false), Some(Drag::Orbit));
        assert_eq!(drag_for(1, true), Some(Drag::Pan));
        assert_eq!(drag_for(3, false), Some(Drag::Pan));
        assert_eq!(drag_for(2, false), Some(Drag::Zoom));
        assert_eq!(drag_for(8, false), None);
    }
}
