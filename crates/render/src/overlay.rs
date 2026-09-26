//! The view options OpenSCAD draws around the model (`GLView::paintGL`):
//! crosshairs, the axes, their scale markers with numbers in the Hershey
//! stroke font, and the small axis cross in the lower left corner. All are
//! one-pixel lines (`glLineWidth(dpi)` with an offscreen DPI of 1), built
//! here on the CPU for one camera and drawn by [`crate::gpu`].
//!
//! Lines before the model (crosshairs, axes, scales) are depth-tested and
//! write depth, so the model hides what is behind it; the small axes are
//! drawn last, over everything (`glDepthFunc(GL_ALWAYS)`). The negative
//! half of each axis, and its ticks, are stippled
//! (`glLineStipple(3, 0xAAAA)`: three pixels off, three on).

use crate::camera::{self, Camera, look_at_from_minus_y, mul, ortho, rotation, translation};
use crate::hershey::{self, Align};
use crate::scheme::ColorScheme;

/// Which of OpenSCAD's `--view` options are on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ViewOptions {
    pub axes: bool,
    pub scales: bool,
    pub edges: bool,
    pub crosshairs: bool,
}

impl ViewOptions {
    /// OpenSCAD's option names (`ViewOptions::flags`), in its order.
    pub const NAMES: [&'static str; 4] = ["axes", "crosshairs", "edges", "scales"];

    /// Switch on the option called `name`; `false` for an unknown name.
    pub fn set(&mut self, name: &str) -> bool {
        match name {
            "axes" => self.axes = true,
            "scales" => self.scales = true,
            "edges" => self.edges = true,
            "crosshairs" => self.crosshairs = true,
            _ => return false,
        }
        true
    }
}

/// The coordinates a line vertex is given in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum Space {
    /// Model coordinates, like the model.
    Model = 0,
    /// The camera's rotated frame without its translation: what
    /// `showCrosshairs` draws in, fixed at the centre of the view.
    View = 1,
    /// The small axes' own orthographic frame in the corner.
    SmallAxes = 2,
    /// Clip coordinates as given (x, y in -1..1, y up), for lines placed
    /// in pixels.
    Clip = 3,
}

/// Bytes per line vertex: position (4 x f32, `w = 0` for a point at
/// infinity), the segment's first point in the same space (4 x f32, for
/// the stipple pattern), colour (4 x f32), space and stipple flag (2 x
/// u32).
pub const LINE_VERTEX_SIZE: usize = 56;

/// One end of a line segment.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LineVertex {
    pub position: [f32; 4],
    pub start: [f32; 4],
    pub color: [f32; 4],
    pub space: Space,
    pub stipple: bool,
}

impl LineVertex {
    pub fn bytes(&self) -> [u8; LINE_VERTEX_SIZE] {
        let mut out = [0u8; LINE_VERTEX_SIZE];
        let floats = self
            .position
            .iter()
            .chain(&self.start)
            .chain(&self.color)
            .copied();
        for (i, x) in floats.enumerate() {
            out[4 * i..4 * i + 4].copy_from_slice(&x.to_le_bytes());
        }
        out[48..52].copy_from_slice(&(self.space as u32).to_le_bytes());
        out[52..56].copy_from_slice(&u32::from(self.stipple).to_le_bytes());
        out
    }
}

/// Line segments for one frame: `before` the model (depth-tested),
/// `behind` it (depth-tested against the model without writing depth: the
/// app's [`grid`]), and `after` it (drawn over everything).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Overlay {
    pub before: Vec<LineVertex>,
    pub behind: Vec<LineVertex>,
    pub after: Vec<LineVertex>,
}

/// Adds segments to a list in one space, colour and stipple.
pub struct Pen<'a> {
    pub out: &'a mut Vec<LineVertex>,
    pub space: Space,
    pub color: [f32; 4],
    pub stipple: bool,
}

impl std::fmt::Debug for Pen<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Pen").field("space", &self.space).finish()
    }
}

impl Pen<'_> {
    /// A segment between homogeneous points.
    pub fn line4(&mut self, a: [f64; 4], b: [f64; 4]) {
        let a = a.map(|x| x as f32);
        let b = b.map(|x| x as f32);
        for p in [a, b] {
            self.out.push(LineVertex {
                position: p,
                start: a,
                color: self.color,
                space: self.space,
                stipple: self.stipple,
            });
        }
    }

    pub fn line(&mut self, a: [f64; 3], b: [f64; 3]) {
        self.line4([a[0], a[1], a[2], 1.0], [b[0], b[1], b[2], 1.0]);
    }
}

/// The lines `--view` asks for, for `camera` (final, after `--viewall`).
/// `preview` leaves out the crosshairs, which OpenSCAD's preview export
/// never enables (`prepare_preview` sets only axes, scales and edges).
pub fn overlay(
    camera: &Camera,
    scheme: &ColorScheme,
    view: &ViewOptions,
    preview: bool,
) -> Overlay {
    let mut o = Overlay::default();
    let axes = scheme.axes.0;
    if view.crosshairs && !preview {
        // `showCrosshairs`: four diagonals through the view centre.
        let mut pen = Pen {
            out: &mut o.before,
            space: Space::View,
            color: scheme.crosshair.0,
            stipple: false,
        };
        let vd = camera.viewer_distance / 8.0;
        for xf in [-1.0, 1.0] {
            for yf in [-1.0, 1.0] {
                pen.line([-xf * vd, -yf * vd, -vd], [xf * vd, yf * vd, vd]);
            }
        }
    }
    if view.axes {
        // `showAxes`: from the origin to infinity along each axis.
        let mut pen = Pen {
            out: &mut o.before,
            space: Space::Model,
            color: axes,
            stipple: false,
        };
        let origin = [0.0, 0.0, 0.0, 1.0];
        for d in [
            [1.0, 0.0, 0.0, 0.0],
            [0.0, 1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0],
        ] {
            pen.line4(origin, d);
        }
        pen.stipple = true;
        for d in [
            [-1.0, 0.0, 0.0, 0.0],
            [0.0, -1.0, 0.0, 0.0],
            [0.0, 0.0, -1.0, 0.0],
        ] {
            pen.line4(origin, d);
        }
        if view.scales {
            scale_markers(&mut o.before, camera, axes);
        }
        small_axes(&mut o.after, camera, axes);
    }
    o
}

/// A grid on the ground (z = 0) plane around the point the camera looks
/// at, for the app's viewport. OpenSCAD draws no grid (`GLView::paintGL`
/// has axes, scale markers and crosshairs only); this is NeoSCAD's, so
/// exported images never include it.
///
/// The spacing is a power of ten chosen from the viewer distance, like the
/// scale markers', so it steps tenfold as the view zooms; every tenth line
/// is stronger. Lines are snapped to multiples of the spacing, so panning
/// slides the model over a fixed grid rather than dragging the grid along.
/// The lines go in [`Overlay::behind`]: drawn after the model and hidden by
/// it, but never writing depth, because a plane of depth-writing lines at
/// z = 0 would cut into the preview's depth passes wherever a face lies in
/// that plane.
pub fn grid(out: &mut Vec<LineVertex>, camera: &Camera, color: [f32; 4]) {
    let dist = camera.viewer_distance;
    if !(dist.is_finite() && dist > 0.0) {
        return;
    }
    let step = 10f64.powi((dist / 10.0).log10().floor() as i32);
    // Out to about the viewer distance each way: at OpenSCAD's 22.5 degree
    // field of view that covers the visible ground from any angle but the
    // most grazing, with at most 200 lines per direction.
    let n = (dist / step).ceil() as i64;
    let [cx, cy, _] = camera.vpt();
    let (i0, j0) = ((cx / step).round() as i64, (cy / step).round() as i64);
    let (lo_x, hi_x) = ((i0 - n) as f64 * step, (i0 + n) as f64 * step);
    let (lo_y, hi_y) = ((j0 - n) as f64 * step, (j0 + n) as f64 * step);
    let minor = [color[0], color[1], color[2], color[3] * 0.18];
    let major = [color[0], color[1], color[2], color[3] * 0.4];
    for k in -n..=n {
        for (index, horizontal) in [(i0 + k, false), (j0 + k, true)] {
            let v = index as f64 * step;
            let mut pen = Pen {
                out: &mut *out,
                space: Space::Model,
                color: if index % 10 == 0 { major } else { minor },
                stipple: false,
            };
            if horizontal {
                pen.line([lo_x, v, 0.0], [hi_x, v, 0.0]);
            } else {
                pen.line([v, lo_y, 0.0], [v, hi_y, 0.0]);
            }
        }
    }
}

/// `showScalemarkers`: ticks along each axis whose spacing changes with
/// every tenfold zoom, longer every tenth tick, with numbers there (and
/// every second tick when few are visible).
fn scale_markers(out: &mut Vec<LineVertex>, camera: &Camera, color: [f32; 4]) {
    let l = camera.viewer_distance;
    let log_l = l.log10().floor() as i32;
    let l_adjusted = 10f64.powi(log_l);
    let tick_width = l_adjusted / 10.0;
    const SIZE_DIV_SM: i32 = 60;
    let divs = (l / tick_width) as usize;
    // OpenSCAD counts ticks in `line_cnt`, which always equals `div`.
    for div in 0..divs {
        let i = div as f64 * tick_width;
        let size_div = if div > 0 && div % 10 == 0 {
            marker_value(out, i, l, SIZE_DIV_SM, color);
            SIZE_DIV_SM / 2
        } else {
            if div > 0 && div % 2 == 0 && l / l_adjusted < 3.0 {
                marker_value(out, i, l, SIZE_DIV_SM, color);
            }
            SIZE_DIV_SM
        };
        let t = l / f64::from(size_div);
        for (sign, stipple) in [(1.0, false), (-1.0, true)] {
            let mut pen = Pen {
                out,
                space: Space::Model,
                color,
                stipple,
            };
            let j = sign * i;
            pen.line([j, 0.0, 0.0], [j, -t, 0.0]);
            pen.line([0.0, j, 0.0], [-t, j, 0.0]);
            pen.line([0.0, 0.0, j], [-t, 0.0, j]);
        }
    }
}

/// `decodeMarkerValue`: the number `i` and `-i` at their ticks on all
/// three axes, in the Hershey font, lying in the axis planes.
fn marker_value(out: &mut Vec<LineVertex>, i: f64, l: f64, size_div_sm: i32, color: [f32; 4]) {
    let pos = lang::number::fmt_g(i);
    let neg = format!("-{pos}");
    let font_size = (l / f64::from(size_div_sm)) as f32;
    let baseline = font_size / 5.0;
    let prefix_offset = hershey::text_width("-", font_size) / 2.0;
    let mut pen = Pen {
        out,
        space: Space::Model,
        color,
        stipple: false,
    };
    type Plane = fn(f32, f32, f32, f32) -> [f64; 3];
    let planes: [Plane; 3] = [
        |x, y, _fh, bl| [f64::from(x), f64::from(y + bl), 0.0],
        |x, y, fh, bl| [f64::from(-y + (fh + bl)), f64::from(x), 0.0],
        |x, y, fh, bl| [f64::from(-y + (fh + bl)), 0.0, f64::from(x)],
    ];
    for plane in planes {
        for (text, x) in [(&pos, i as f32), (&neg, -(i as f32) - prefix_offset)] {
            for stroke in hershey::strokes(text, x, 0.0, Align::Center, font_size) {
                for w in stroke.windows(2) {
                    let a = plane(w[0][0], w[0][1], font_size, baseline);
                    let b = plane(w[1][0], w[1][1], font_size, baseline);
                    pen.line(a, b);
                }
            }
        }
    }
}

/// The small axes' projection (`showSmallaxes`): an orthographic view 180
/// units high, looking along +y, shifted to the lower left corner.
pub(crate) fn small_axes_projection(camera: &Camera) -> camera::Mat4 {
    let aspect = camera.aspect_ratio();
    let scale = 90.0;
    let shift = translation([-0.8, -0.8, 0.0]);
    let proj = ortho(
        -scale * aspect,
        scale * aspect,
        -scale,
        scale,
        -scale,
        scale,
    );
    mul(&mul(&shift, &proj), &look_at_from_minus_y(1.0))
}

/// The small axes' modelview: the camera's rotation alone.
pub(crate) fn small_axes_modelview(camera: &Camera) -> camera::Mat4 {
    let r = camera.object_rot;
    let m = rotation(r[0], [1.0, 0.0, 0.0]);
    let m = mul(&m, &rotation(r[1], [0.0, 1.0, 0.0]));
    mul(&m, &rotation(r[2], [0.0, 0.0, 1.0]))
}

/// `showSmallaxes`: red, green and blue axes ten units long, and x, y and
/// z drawn as letters at their projected tips, in pixels.
pub fn small_axes(out: &mut Vec<LineVertex>, camera: &Camera, color: [f32; 4]) {
    for (d, c) in [
        ([10.0, 0.0, 0.0], [1.0, 0.0, 0.0, 1.0]),
        ([0.0, 10.0, 0.0], [0.0, 1.0, 0.0, 1.0]),
        ([0.0, 0.0, 10.0], [0.0, 0.0, 1.0, 1.0]),
    ] {
        Pen {
            out,
            space: Space::SmallAxes,
            color: c,
            stipple: false,
        }
        .line([0.0; 3], d);
    }
    // `gluProject` of each label point, rounded to whole pixels.
    let m = mul(
        &small_axes_projection(camera),
        &small_axes_modelview(camera),
    );
    let (w, h) = (
        f64::from(camera.pixel_width),
        f64::from(camera.pixel_height),
    );
    let project = |p: [f64; 3]| {
        let c: [f64; 4] =
            std::array::from_fn(|r| m[r][0] * p[0] + m[r][1] * p[1] + m[r][2] * p[2] + m[r][3]);
        [
            (w * (c[0] / c[3] + 1.0) / 2.0).round(),
            (h * (c[1] / c[3] + 1.0) / 2.0).round(),
        ]
    };
    let [xl, yl, zl] = [[12.0, 0.0, 0.0], [0.0, 12.0, 0.0], [0.0, 0.0, 12.0]].map(project);
    let d = 3.0;
    let mut pen = Pen {
        out,
        space: Space::Clip,
        color,
        stipple: false,
    };
    let mut px = |a: [f64; 2], b: [f64; 2]| {
        let clip = |p: [f64; 2]| [2.0 * p[0] / w - 1.0, 2.0 * p[1] / h - 1.0, 0.0];
        pen.line(clip(a), clip(b));
    };
    let [x, y] = xl;
    px([x - d, y - d], [x + d, y + d]);
    px([x - d, y + d], [x + d, y - d]);
    let [x, y] = yl;
    px([x - d, y - d], [x + d, y + d]);
    px([x - d, y + d], [x, y]);
    let [x, y] = zl;
    px([x - d, y - d], [x + d, y - d]);
    px([x - d, y + d], [x + d, y + d]);
    px([x - d, y - d], [x + d, y + d]);
}

/// Hershey text in pixels, `(x, y)` from the lower left corner of a
/// `width` x `height` image, `size` pixels tall: labels for snapshots.
#[allow(clippy::too_many_arguments)]
pub fn pixel_text(
    out: &mut Vec<LineVertex>,
    text: &str,
    x: f64,
    y: f64,
    align: Align,
    size: f64,
    color: [f32; 4],
    (width, height): (u32, u32),
) {
    let (w, h) = (f64::from(width), f64::from(height));
    let mut pen = Pen {
        out,
        space: Space::Clip,
        color,
        stipple: false,
    };
    for stroke in hershey::strokes(text, x as f32, y as f32, align, size as f32) {
        for s in stroke.windows(2) {
            let clip = |p: [f32; 2]| {
                [
                    2.0 * f64::from(p[0]) / w - 1.0,
                    2.0 * f64::from(p[1]) / h - 1.0,
                    0.0,
                ]
            };
            pen.line(clip(s[0]), clip(s[1]));
        }
    }
}

/// The small axes' clip matrix for a 0..1 depth range, for the frame
/// uniforms.
pub(crate) fn small_axes_clip(camera: &Camera) -> camera::Mat4 {
    camera::zero_to_one(&mul(
        &small_axes_projection(camera),
        &small_axes_modelview(camera),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nothing_without_options() {
        let o = overlay(
            &Camera::default(),
            &ColorScheme::cornfield(),
            &ViewOptions::default(),
            false,
        );
        assert!(o.before.is_empty() && o.after.is_empty());
    }

    #[test]
    fn axes_and_small_axes() {
        let view = ViewOptions {
            axes: true,
            ..Default::default()
        };
        let o = overlay(&Camera::default(), &ColorScheme::cornfield(), &view, false);
        // Six half-axes; the small axes' three lines and seven label strokes.
        assert_eq!(o.before.len(), 12);
        assert_eq!(o.after.len(), 2 * (3 + 7));
        assert!(o.before[6].stipple && !o.before[0].stipple);
        assert_eq!(o.before[1].position, [1.0, 0.0, 0.0, 0.0]);
    }

    #[test]
    fn scale_ticks_follow_the_zoom() {
        let view = ViewOptions {
            axes: true,
            scales: true,
            ..Default::default()
        };
        // Distance 140: ticks every 10, 14 of them, labels at 100 (the
        // tenth) only, since 140 / 100 < 3 also labels every second tick.
        let o = overlay(&Camera::default(), &ColorScheme::cornfield(), &view, false);
        let ticks = o.before.iter().filter(|v| v.stipple).count() / 2 - 3;
        assert_eq!(ticks, 14 * 3);
    }

    #[test]
    fn crosshairs_only_in_render_mode() {
        let view = ViewOptions {
            crosshairs: true,
            ..Default::default()
        };
        let s = ColorScheme::cornfield();
        assert_eq!(
            overlay(&Camera::default(), &s, &view, false).before.len(),
            8
        );
        assert!(
            overlay(&Camera::default(), &s, &view, true)
                .before
                .is_empty()
        );
    }

    #[test]
    fn grid_lies_on_the_ground_snapped_to_its_spacing() {
        let mut camera = Camera::default();
        camera.set_vpt(13.0, -27.0, 5.0);
        let mut out = Vec::new();
        grid(&mut out, &camera, [0.0, 0.0, 0.0, 1.0]);
        // Distance 140: spacing 10, 14 lines each side of the centre
        // line, in both directions.
        assert_eq!(out.len(), 2 * 2 * (2 * 14 + 1));
        for v in &out {
            assert_eq!(v.space, Space::Model);
            assert_eq!(v.position[2], 0.0);
            assert_eq!(v.position[3], 1.0);
        }
        // Every line sits on a multiple of the spacing, whatever the centre.
        for pair in out.chunks(2) {
            let (a, b) = (pair[0].position, pair[1].position);
            let fixed = if a[0] == b[0] { a[0] } else { a[1] };
            assert_eq!(fixed % 10.0, 0.0, "{a:?} {b:?}");
        }
        // Tenfold closer, tenfold finer.
        camera.set_vpd(14.0);
        let mut fine = Vec::new();
        grid(&mut fine, &camera, [0.0; 4]);
        let first = fine[0].position;
        assert!((first[0] - first[0].round()).abs() < 1e-4);
        assert_eq!(fine.len(), out.len());
    }
}
