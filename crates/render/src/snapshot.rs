//! Contact sheets for `neoscad snapshot`: one PNG with a model drawn from
//! several standard views, each panel with a scale grid, an axis cross, a
//! caption, and (optionally) the bounding box's dimensions in mm. Made for
//! agents: a vision model reading one compact image learns the shape, its
//! orientation and its size, without a GUI.
//!
//! Every panel is an orthographic view fitted to the model's bounding
//! sphere, so all panels share one scale and the grid step printed in each
//! applies to all. The views are OpenSCAD's own presets
//! (`MainWindow::on_viewAction*`). The grid lies in a plane just behind the
//! model and is depth-tested, so the model hides it; lines through the
//! origin are tinted in the axis colours (x red, y green, z blue).
//!
//! Text is OpenSCAD's Hershey stroke font drawn into the pixels on the
//! CPU, one pixel wide and doubled for weight, so the sheet needs no font
//! file and the same input gives the same bytes.

use crate::camera::{self, BoundingBox, Camera, Projection};
use crate::hershey::{self, Align};
use crate::overlay::{self, LineVertex, Overlay, Pen, Space};

/// An axis-aligned view's screen axes (world axis and sign, right then
/// up) and its depth axis (and whether the box's far side is its maximum).
type Frame = ([(usize, f64); 2], (usize, bool));

/// A standard view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    Iso,
    Front,
    Back,
    Left,
    Right,
    Top,
    Bottom,
}

impl View {
    pub const ALL: [View; 7] = [
        View::Iso,
        View::Front,
        View::Back,
        View::Left,
        View::Right,
        View::Top,
        View::Bottom,
    ];

    pub fn parse(s: &str) -> Option<View> {
        View::ALL.into_iter().find(|v| v.name() == s)
    }

    pub fn name(self) -> &'static str {
        match self {
            View::Iso => "iso",
            View::Front => "front",
            View::Back => "back",
            View::Left => "left",
            View::Right => "right",
            View::Top => "top",
            View::Bottom => "bottom",
        }
    }

    /// OpenSCAD's `object_rot` for the view (`MainWindow.cc`,
    /// `on_viewAction*`; the diagonal view is the default camera).
    pub fn object_rot(self) -> [f64; 3] {
        match self {
            View::Iso => [35.0, 0.0, 335.0],
            View::Front => [0.0, 0.0, 0.0],
            View::Back => [0.0, 0.0, 180.0],
            View::Left => [0.0, 0.0, 90.0],
            View::Right => [0.0, 0.0, 270.0],
            View::Top => [90.0, 0.0, 0.0],
            View::Bottom => [270.0, 0.0, 0.0],
        }
    }

    /// For an axis-aligned view: the world axes pointing right and up on
    /// screen (index and sign), and the depth axis with the side of the
    /// box that is farthest from the camera (`true`: its maximum).
    fn frame(self) -> Option<Frame> {
        Some(match self {
            View::Iso => return None,
            View::Front => ([(0, 1.0), (2, 1.0)], (1, true)),
            View::Back => ([(0, -1.0), (2, 1.0)], (1, false)),
            View::Left => ([(1, -1.0), (2, 1.0)], (0, true)),
            View::Right => ([(1, 1.0), (2, 1.0)], (0, false)),
            View::Top => ([(0, 1.0), (1, 1.0)], (2, false)),
            View::Bottom => ([(0, 1.0), (1, -1.0)], (2, true)),
        })
    }

    /// The caption: the view and which way the axes point.
    fn caption(self) -> (String, String) {
        let title = self.name().to_uppercase();
        let sub = match self.frame() {
            None => "x red, y green, z blue".to_string(),
            Some(([(h, hs), (v, vs)], _)) => {
                let n = |i: usize, s: f64| {
                    format!("{}{}", if s < 0.0 { "-" } else { "+" }, ["x", "y", "z"][i])
                };
                format!("{} right, {} up", n(h, hs), n(v, vs))
            }
        };
        (title, sub)
    }
}

/// What a sheet shows besides the model.
#[derive(Debug, Clone, PartialEq)]
pub struct Sheet {
    pub views: Vec<View>,
    /// The whole sheet, in pixels.
    pub width: u32,
    pub height: u32,
    /// Annotate the bounding box's size in each panel.
    pub dims: bool,
    /// Header lines: the first larger (the model's name), the rest small.
    pub header: Vec<String>,
    /// Colour keys shown in the header (diffs, parts, issues).
    pub legend: Vec<([f32; 4], String)>,
    /// Numbered points drawn over every panel (`snapshot --issues`).
    pub markers: Vec<Marker>,
    /// How the panels are lit: [`crate::Lighting::Headlight`] for agents'
    /// sheets, so faces turned away from OpenSCAD's fixed light stay
    /// legible.
    pub lighting: crate::Lighting,
}

/// A labelled point in model coordinates, drawn as a numbered disc over
/// the panels. It is drawn whether or not the model hides the point from
/// that view, so a problem at the back is still pointed at.
#[derive(Debug, Clone, PartialEq)]
pub struct Marker {
    pub point: [f64; 3],
    pub label: String,
    pub color: [u8; 3],
}

/// Where each panel goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Layout {
    header: u32,
    cols: u32,
    rows: u32,
    panel_w: u32,
    panel_h: u32,
}

const HEADER: u32 = 44;
const INK: [u8; 3] = [32, 32, 32];
const MUTED: [u8; 3] = [96, 96, 88];
const SEPARATOR: [u8; 3] = [176, 176, 160];

impl Sheet {
    fn layout(&self) -> Layout {
        let n = self.views.len().max(1) as u32;
        let cols = (n as f64).sqrt().ceil() as u32;
        let rows = n.div_ceil(cols);
        let header = HEADER.min(self.height / 4);
        Layout {
            header,
            cols,
            rows,
            panel_w: (self.width / cols).max(1),
            panel_h: ((self.height - header) / rows).max(1),
        }
    }

    /// Each panel's camera (orthographic, fitted to `bbox` with room for
    /// the annotations) and its grid and axis lines.
    pub fn views(&self, bbox: BoundingBox, scheme: &crate::ColorScheme) -> Vec<(Camera, Overlay)> {
        let l = self.layout();
        self.views
            .iter()
            .map(|&view| {
                let mut cam = Camera {
                    projection: Projection::Orthogonal,
                    autocenter: true,
                    viewall: true,
                    pixel_width: l.panel_w,
                    pixel_height: l.panel_h,
                    ..Camera::default()
                };
                cam.object_rot = view.object_rot();
                cam.view_all(bbox);
                if bbox.is_some() {
                    // Room around the model for the dimension lines.
                    cam.viewer_distance *= 1.3;
                }
                let mut o = Overlay::default();
                if let Some(b) = bbox {
                    grid(&mut o.before, &cam, view, b);
                }
                overlay::small_axes(&mut o.after, &cam, scheme.axes.0);
                (cam, o)
            })
            .collect()
    }

    /// The sheet from the panels' images (in [`Sheet::views`] order):
    /// panels placed in a grid under the header, with their captions, grid
    /// step, dimensions and separators, and the header drawn on top.
    pub fn compose(
        &self,
        panels: &[crate::Image],
        cameras: &[Camera],
        bbox: BoundingBox,
        background: [u8; 3],
    ) -> crate::Image {
        let l = self.layout();
        let mut c = Canvas::new(self.width, self.height, background);
        for (i, ((img, cam), view)) in panels.iter().zip(cameras).zip(&self.views).enumerate() {
            let (x0, y0) = (
                (i as u32 % l.cols) * l.panel_w,
                l.header + (i as u32 / l.cols) * l.panel_h,
            );
            c.blit(img, x0, y0);
            let (title, sub) = view.caption();
            c.text(
                &title,
                x0 as f64 + 8.0,
                y0 as f64 + 20.0,
                16.0,
                Align::Left,
                INK,
                true,
            );
            c.text(
                &sub,
                x0 as f64 + 8.0,
                y0 as f64 + 38.0,
                12.0,
                Align::Left,
                MUTED,
                false,
            );
            if let Some(b) = bbox {
                let step = grid_step(cam);
                c.text(
                    &format!("grid {} mm", number(step)),
                    (x0 + l.panel_w) as f64 - 8.0,
                    (y0 + l.panel_h) as f64 - 8.0,
                    12.0,
                    Align::Right,
                    MUTED,
                    false,
                );
                if self.dims {
                    dimensions(&mut c, cam, *view, b, (x0, y0));
                }
            }
            // Markers that would cover one another (two findings at one
            // point, or seen end on) step aside to the nearest free spot
            // around their point, so every number shows and stays close to
            // what it marks; with no free spot near, it is drawn on top.
            let mut placed: Vec<(f64, f64)> = Vec::new();
            for m in &self.markers {
                let [px, py] = to_pixel(cam, m.point);
                let (px, py) = (px + x0 as f64, py + y0 as f64);
                let free = |x: f64, y: f64, placed: &[(f64, f64)]| {
                    placed
                        .iter()
                        .all(|(qx, qy)| (qx - x).abs() >= 19.0 || (qy - y).abs() >= 19.0)
                };
                const STEPS: [(f64, f64); 8] = [
                    (1.0, 0.0),
                    (0.0, 1.0),
                    (-1.0, 0.0),
                    (0.0, -1.0),
                    (1.0, 1.0),
                    (-1.0, 1.0),
                    (-1.0, -1.0),
                    (1.0, -1.0),
                ];
                let (px, py) = std::iter::once((px, py))
                    .chain((1..=2).flat_map(|k| {
                        STEPS.iter().map(move |(dx, dy)| {
                            (px + dx * 20.0 * k as f64, py + dy * 20.0 * k as f64)
                        })
                    }))
                    .find(|&(x, y)| free(x, y, &placed))
                    .unwrap_or((px, py));
                placed.push((px, py));
                let inside = px >= x0 as f64
                    && py >= y0 as f64
                    && px < (x0 + l.panel_w) as f64
                    && py < (y0 + l.panel_h) as f64;
                if inside {
                    c.marker(px, py, &m.label, m.color);
                }
            }
        }
        // Separators between panels and under the header.
        for col in 1..l.cols {
            let x = col * l.panel_w;
            c.line(
                x as f64,
                l.header as f64,
                x as f64,
                self.height as f64,
                SEPARATOR,
            );
        }
        for row in 1..l.rows {
            let y = l.header + row * l.panel_h;
            c.line(0.0, y as f64, self.width as f64, y as f64, SEPARATOR);
        }
        c.line(
            0.0,
            l.header as f64,
            self.width as f64,
            l.header as f64,
            SEPARATOR,
        );
        let mut y = 20.0;
        for (k, line) in self.header.iter().enumerate() {
            let (size, color, bold) = if k == 0 {
                (16.0, INK, true)
            } else {
                (12.0, MUTED, false)
            };
            c.text(line, 8.0, y, size, Align::Left, color, bold);
            y += 18.0;
        }
        // The legend, right-aligned: a swatch and a label per entry.
        let mut x = self.width as f64 - 8.0;
        for (color, label) in self.legend.iter().rev() {
            let w = f64::from(hershey::text_width(label, 13.0));
            c.text(label, x, 26.0, 13.0, Align::Right, INK, false);
            x -= w + 6.0;
            let rgb = color.map(|v| (v.clamp(0.0, 1.0) * 255.0).round() as u8);
            c.fill(x - 12.0, 14.0, 12.0, 12.0, [rgb[0], rgb[1], rgb[2]]);
            x -= 24.0;
        }
        c.into_pixels()
    }
}

/// `x` with at most two decimals and no trailing zeros.
pub fn number(x: f64) -> String {
    let s = format!("{x:.2}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s == "-0" { "0".into() } else { s.to_string() }
}

/// A round step (1, 2 or 5 times a power of ten) giving about ten cells
/// over the panel's height (between about 7 and 15).
fn grid_step(cam: &Camera) -> f64 {
    let half_h = cam.viewer_distance * io::trig::tan_degrees(cam.fov / 2.0);
    let raw = 2.0 * half_h / 10.0;
    let p = 10f64.powf(raw.log10().floor());
    let m = raw / p;
    // The nearest of 1, 2, 5 and 10 (on a log scale, roughly).
    let m = if m < 1.5 {
        1.0
    } else if m < 3.5 {
        2.0
    } else if m < 7.5 {
        5.0
    } else {
        10.0
    };
    m * p
}

/// The grid for one panel: in the plane behind the box for an
/// axis-aligned view (filling the panel), or on the floor under it for
/// the iso view (the footprint and a cell around it).
fn grid(out: &mut Vec<LineVertex>, cam: &Camera, view: View, (lo, hi): ([f64; 3], [f64; 3])) {
    let step = grid_step(cam);
    let size: [f64; 3] = std::array::from_fn(|k| hi[k] - lo[k]);
    let eps = 0.002 * size.iter().copied().fold(0.0, f64::max).max(step);
    let half_h = cam.viewer_distance * io::trig::tan_degrees(cam.fov / 2.0);
    let half_w = half_h * cam.aspect_ratio();
    let centre = cam.vpt();
    let (h, v, plane) = match view.frame() {
        Some(([(h, _), (v, _)], (d, far_max))) => {
            let at = if far_max { hi[d] + eps } else { lo[d] - eps };
            let range = |axis: usize, half: f64| (centre[axis] - half, centre[axis] + half);
            (
                range_of(h, range(h, half_w)),
                range_of(v, range(v, half_h)),
                (d, at),
            )
        }
        None => (
            range_of(0, (lo[0] - step, hi[0] + step)),
            range_of(1, (lo[1] - step, hi[1] + step)),
            (2, lo[2] - eps),
        ),
    };
    let (d, at) = plane;
    let point = |a: (usize, f64), b: (usize, f64)| {
        let mut p = [0.0; 3];
        p[a.0] = a.1;
        p[b.0] = b.1;
        p[d] = at;
        p
    };
    let axis_color = |axis: usize| match axis {
        0 => [0.85, 0.45, 0.45, 1.0],
        1 => [0.45, 0.72, 0.45, 1.0],
        _ => [0.45, 0.55, 0.85, 1.0],
    };
    // Lines across `across` at each multiple of the step along `along`.
    for (along, across) in [(h, v), (v, h)] {
        let (axis, a0, a1) = along;
        let first = (a0 / step).ceil() as i64;
        let last = (a1 / step).floor() as i64;
        for k in first..=last {
            let c = k as f64 * step;
            let color = if k == 0 {
                // The line where `axis` is 0 runs along the other axis.
                axis_color(across.0)
            } else if k % 5 == 0 {
                [0.72, 0.72, 0.64, 1.0]
            } else {
                [0.86, 0.86, 0.78, 1.0]
            };
            Pen {
                out,
                space: Space::Model,
                color,
                stipple: false,
            }
            .line(
                point((axis, c), (across.0, across.1)),
                point((axis, c), (across.0, across.2)),
            );
        }
    }
}

fn range_of(axis: usize, (a, b): (f64, f64)) -> (usize, f64, f64) {
    (axis, a.min(b), a.max(b))
}

/// Model point to panel pixel (x right, y down).
fn to_pixel(cam: &Camera, p: [f64; 3]) -> [f64; 2] {
    let gl = cam.gl_matrices();
    let m = camera::mul(&gl.projection, &gl.modelview);
    let c: [f64; 4] =
        std::array::from_fn(|r| m[r][0] * p[0] + m[r][1] * p[1] + m[r][2] * p[2] + m[r][3]);
    [
        (c[0] / c[3] + 1.0) / 2.0 * f64::from(cam.pixel_width),
        (1.0 - c[1] / c[3]) / 2.0 * f64::from(cam.pixel_height),
    ]
}

/// Dimension lines for an axis-aligned panel (width under the box, height
/// to its left), or the three sizes as text for the iso panel.
fn dimensions(
    c: &mut Canvas,
    cam: &Camera,
    view: View,
    (lo, hi): ([f64; 3], [f64; 3]),
    (x0, y0): (u32, u32),
) {
    let size: [f64; 3] = std::array::from_fn(|k| hi[k] - lo[k]);
    let (ox, oy) = (f64::from(x0), f64::from(y0));
    let Some(([(h, _), (v, _)], _)) = view.frame() else {
        let text = format!(
            "{} x {} x {} mm",
            number(size[0]),
            number(size[1]),
            number(size[2])
        );
        c.text(
            &text,
            ox + f64::from(cam.pixel_width) / 2.0,
            oy + f64::from(cam.pixel_height) - 12.0,
            14.0,
            Align::Center,
            INK,
            false,
        );
        return;
    };
    let mid: [f64; 3] = std::array::from_fn(|k| (lo[k] + hi[k]) / 2.0);
    let corner = |hv: f64, vv: f64| {
        let mut p = mid;
        p[h] = hv;
        p[v] = vv;
        to_pixel(cam, p)
    };
    // The four projected corners of the box's silhouette.
    let pts = [
        corner(lo[h], lo[v]),
        corner(hi[h], lo[v]),
        corner(lo[h], hi[v]),
        corner(hi[h], hi[v]),
    ];
    let (min_x, max_x) = pts
        .iter()
        .fold((f64::MAX, f64::MIN), |a, p| (a.0.min(p[0]), a.1.max(p[0])));
    let (min_y, max_y) = pts
        .iter()
        .fold((f64::MAX, f64::MIN), |a, p| (a.0.min(p[1]), a.1.max(p[1])));
    let gap = 14.0;
    // Width, under the box.
    let y = oy + max_y + gap;
    let (a, b) = (ox + min_x, ox + max_x);
    c.line(a, oy + max_y + 3.0, a, y + 4.0, INK);
    c.line(b, oy + max_y + 3.0, b, y + 4.0, INK);
    c.line(a, y, b, y, INK);
    c.line(a, y, a + 5.0, y - 3.0, INK);
    c.line(a, y, a + 5.0, y + 3.0, INK);
    c.line(b, y, b - 5.0, y - 3.0, INK);
    c.line(b, y, b - 5.0, y + 3.0, INK);
    c.text(
        &number(size[h]),
        (a + b) / 2.0,
        y + 17.0,
        14.0,
        Align::Center,
        INK,
        false,
    );
    // Height, left of the box.
    let x = ox + min_x - gap;
    let (a, b) = (oy + min_y, oy + max_y);
    c.line(ox + min_x - 3.0, a, x - 4.0, a, INK);
    c.line(ox + min_x - 3.0, b, x - 4.0, b, INK);
    c.line(x, a, x, b, INK);
    c.line(x, a, x - 3.0, a + 5.0, INK);
    c.line(x, a, x + 3.0, a + 5.0, INK);
    c.line(x, b, x - 3.0, b - 5.0, INK);
    c.line(x, b, x + 3.0, b - 5.0, INK);
    c.text(
        &number(size[v]),
        x - 6.0,
        (a + b) / 2.0 + 6.0,
        14.0,
        Align::Right,
        INK,
        false,
    );
}

/// An RGB image being drawn on the CPU.
struct Canvas {
    width: u32,
    height: u32,
    rgb: Vec<[u8; 3]>,
    background: [u8; 3],
}

impl Canvas {
    fn new(width: u32, height: u32, background: [u8; 3]) -> Canvas {
        Canvas {
            width,
            height,
            rgb: vec![background; (width * height) as usize],
            background,
        }
    }

    fn blit(&mut self, img: &crate::Image, x0: u32, y0: u32) {
        for y in 0..img.height.min(self.height.saturating_sub(y0)) {
            for x in 0..img.width.min(self.width.saturating_sub(x0)) {
                let s = ((y * img.width + x) * 4) as usize;
                let d = ((y0 + y) * self.width + x0 + x) as usize;
                self.rgb[d] = [img.rgba[s], img.rgba[s + 1], img.rgba[s + 2]];
            }
        }
    }

    fn put(&mut self, x: i64, y: i64, c: [u8; 3]) {
        if x >= 0 && y >= 0 && (x as u32) < self.width && (y as u32) < self.height {
            self.rgb[(y as u32 * self.width + x as u32) as usize] = c;
        }
    }

    /// A one-pixel line (Bresenham), end points rounded.
    fn line(&mut self, x0: f64, y0: f64, x1: f64, y1: f64, c: [u8; 3]) {
        let (mut x, mut y) = (x0.round() as i64, y0.round() as i64);
        let (x1, y1) = (x1.round() as i64, y1.round() as i64);
        let (dx, dy) = ((x1 - x).abs(), -(y1 - y).abs());
        let (sx, sy) = (if x < x1 { 1 } else { -1 }, if y < y1 { 1 } else { -1 });
        let mut err = dx + dy;
        loop {
            self.put(x, y, c);
            if x == x1 && y == y1 {
                break;
            }
            let e2 = 2 * err;
            if e2 >= dy {
                err += dy;
                x += sx;
            }
            if e2 <= dx {
                err += dx;
                y += sy;
            }
        }
    }

    fn fill(&mut self, x: f64, y: f64, w: f64, h: f64, c: [u8; 3]) {
        for yy in y.round() as i64..(y + h).round() as i64 {
            for xx in x.round() as i64..(x + w).round() as i64 {
                self.put(xx, yy, c);
            }
        }
    }

    /// Hershey text with its baseline at `y` (pixels, y down); `bold`
    /// draws each stroke twice, a pixel apart. The text's box is cleared to
    /// the background first, so grid lines and the model do not run
    /// through it.
    #[allow(clippy::too_many_arguments)]
    fn text(&mut self, s: &str, x: f64, y: f64, size: f64, align: Align, c: [u8; 3], bold: bool) {
        let w = f64::from(hershey::text_width(s, size as f32));
        let left = match align {
            Align::Left => x,
            Align::Center => x - w / 2.0,
            Align::Right => x - w,
        };
        // Glyphs reach about 0.9 of `size` above the baseline and 0.3
        // below it (descenders).
        let bg = self.background;
        self.fill(left - 2.0, y - 0.95 * size, w + 5.0, 1.3 * size, bg);
        for stroke in hershey::strokes(s, 0.0, 0.0, align, size as f32) {
            for w in stroke.windows(2) {
                let p = |q: [f32; 2]| (x + f64::from(q[0]), y - f64::from(q[1]));
                let (a, b) = (p(w[0]), p(w[1]));
                self.line(a.0, a.1, b.0, b.1, c);
                if bold {
                    self.line(a.0 + 1.0, a.1, b.0 + 1.0, b.1, c);
                }
            }
        }
    }

    /// A numbered disc centred at (`x`, `y`): the colour with a white rim
    /// and a white label, so it reads on the model and on the background.
    fn marker(&mut self, x: f64, y: f64, label: &str, c: [u8; 3]) {
        const R: f64 = 9.0;
        let white = [255, 255, 255];
        for yy in (y - R - 1.0).floor() as i64..=(y + R + 1.0).ceil() as i64 {
            for xx in (x - R - 1.0).floor() as i64..=(x + R + 1.0).ceil() as i64 {
                let d = ((xx as f64 - x).powi(2) + (yy as f64 - y).powi(2)).sqrt();
                if d <= R - 1.0 {
                    self.put(xx, yy, c);
                } else if d <= R + 0.5 {
                    self.put(xx, yy, white);
                }
            }
        }
        let size = if label.len() > 2 { 9.0 } else { 11.0 };
        let base = y + size * 0.45;
        for stroke in hershey::strokes(label, 0.0, 0.0, Align::Center, size as f32) {
            for w in stroke.windows(2) {
                let p = |q: [f32; 2]| (x + f64::from(q[0]), base - f64::from(q[1]));
                let (a, b) = (p(w[0]), p(w[1]));
                self.line(a.0, a.1, b.0, b.1, white);
                self.line(a.0 + 1.0, a.1, b.0 + 1.0, b.1, white);
            }
        }
    }

    fn into_pixels(self) -> crate::Image {
        let mut rgba = Vec::with_capacity(self.rgb.len() * 4);
        for p in self.rgb {
            rgba.extend_from_slice(&[p[0], p[1], p[2], 255]);
        }
        crate::Image {
            width: self.width,
            height: self.height,
            rgba,
        }
    }
}

/// Draw `scene` as a sheet: the panels on the GPU, the rest on the CPU.
#[cfg(feature = "gpu")]
pub async fn draw(
    gpu: &crate::offscreen::Offscreen,
    scene: &crate::Scene,
    scheme: &crate::ColorScheme,
    sheet: &Sheet,
) -> Result<crate::Image, crate::offscreen::Error> {
    let bbox = scene.bounding_box();
    let views = sheet.views(bbox, scheme);
    let images = gpu
        .render_views_lit(scene, &views, scheme, false, sheet.lighting)
        .await?;
    let cameras: Vec<Camera> = views.iter().map(|(c, _)| *c).collect();
    let bg = scheme.background.0.map(|v| (v * 255.0).round() as u8);
    Ok(sheet.compose(&images, &cameras, bbox, [bg[0], bg[1], bg[2]]))
}

/// [`draw`], blocking until the panels are read back.
#[cfg(all(feature = "gpu", not(target_arch = "wasm32")))]
pub fn draw_blocking(
    gpu: &crate::offscreen::Offscreen,
    scene: &crate::Scene,
    scheme: &crate::ColorScheme,
    sheet: &Sheet,
) -> Result<crate::Image, crate::offscreen::Error> {
    pollster::block_on(draw(gpu, scene, scheme, sheet))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn views_parse_and_captions_name_the_axes() {
        assert_eq!(View::parse("front"), Some(View::Front));
        assert_eq!(View::parse("nope"), None);
        assert_eq!(View::Right.caption().1, "+y right, +z up");
        assert_eq!(View::Back.caption().1, "-x right, +z up");
    }

    #[test]
    fn numbers_are_short() {
        assert_eq!(number(20.0), "20");
        assert_eq!(number(12.5), "12.5");
        assert_eq!(number(0.333333), "0.33");
        assert_eq!(number(-0.001), "0");
    }

    #[test]
    fn layout_is_a_grid_under_the_header() {
        let s = Sheet {
            views: vec![View::Iso, View::Front, View::Top, View::Right],
            width: 1024,
            height: 1024,
            dims: false,
            header: vec![],
            legend: vec![],
            markers: vec![],
            lighting: crate::Lighting::Headlight,
        };
        let l = s.layout();
        assert_eq!((l.cols, l.rows, l.panel_w, l.panel_h), (2, 2, 512, 490));
    }

    #[test]
    fn grid_steps_are_round() {
        let mut cam = Camera {
            projection: Projection::Orthogonal,
            ..Camera::default()
        };
        cam.viewer_distance = 100.0;
        // The panel is 2 * 100 * tan(11.25) = 39.8 high: step 5.
        assert_eq!(grid_step(&cam), 5.0);
    }

    #[test]
    fn the_right_view_puts_y_to_the_right() {
        let mut cam = Camera {
            projection: Projection::Orthogonal,
            ..Camera::default()
        };
        cam.object_rot = View::Right.object_rot();
        let a = to_pixel(&cam, [0.0, 0.0, 0.0]);
        let b = to_pixel(&cam, [0.0, 10.0, 0.0]);
        assert!(b[0] > a[0] + 1.0 && (b[1] - a[1]).abs() < 1e-9);
    }
}
