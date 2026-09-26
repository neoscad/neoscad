//! Glyph outlines as OpenSCAD's `FreetypeRenderer` and `DrawingCallback`
//! produce them.
//!
//! OpenSCAD loads each glyph with `FT_Load_Glyph(face, g, FT_LOAD_DEFAULT)`
//! (`FreetypeRenderer.cc:432`): hinted, at about 2170 pixels per em, by the
//! TrueType interpreter when the font has hinting programs and by the
//! autohinter when it has none. skrifa's hinting engines port FreeType's
//! and are asked for the same thing (`HintingOptions::default()`,
//! `Engine::AutoFallback`, `FT_LOAD_TARGET_NORMAL`), and its FreeType path
//! style starts contours and splits implied on-curve points with FreeType's
//! integer arithmetic, so the points match `FT_Outline_Decompose`'s.
//!
//! `DrawingCallback` flattens each quadratic or cubic segment into `fn`
//! equal parameter steps (`DrawingCallback.cc:107-125`), where `fn` is
//! `max($fn-style segments / 8 + 1, 2)` for a circle of radius `size`
//! (`FreetypeRenderer::Params::detect_properties`). Each contour becomes
//! an outline and each glyph a polygon; the caller unions the glyphs with
//! the non-zero rule, so contour direction (and overlap, as in
//! `text-font-overlap.scad`) is resolved there.
//!
//! The flattened points do not depend on `size`: DrawingCallback computes
//! `size * (v + offset + advance)` from the unscaled point `v`, so the
//! cache below is keyed by glyph and segment count only, and one entry
//! serves every size, position and repetition of a glyph.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use skrifa::instance::LocationRef;
use skrifa::outline::{DrawSettings, OutlinePen};
use skrifa::raw::FontRef;
use skrifa::{GlyphId, MetadataProvider};

use crate::shape::{FaceState, SCALE};

/// A control box in 26.6 units (`FT_BBox`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CBox {
    pub x_min: i64,
    pub y_min: i64,
    pub x_max: i64,
    pub y_max: i64,
}

impl CBox {
    /// `FT_Glyph_Get_CBox(..., FT_GLYPH_BBOX_GRIDFIT)`.
    pub fn grid_fit(self) -> CBox {
        CBox {
            x_min: self.x_min & !63,
            y_min: self.y_min & !63,
            x_max: (self.x_max + 63) & !63,
            y_max: (self.y_max + 63) & !63,
        }
    }
}

/// One path command in 26.6 units.
#[derive(Debug, Clone, Copy)]
enum Cmd {
    Move([i64; 2]),
    Line([i64; 2]),
    Quad([i64; 2], [i64; 2]),
    Cubic([i64; 2], [i64; 2], [i64; 2]),
}

/// A hinted glyph: its path and control box.
#[derive(Debug, Default)]
pub struct Glyph {
    cmds: Vec<Cmd>,
    pub cbox: Option<CBox>,
}

/// A glyph flattened with a segment count: contours of unscaled points
/// (26.6 values divided by 1e5, as `get_scaled_vector` does).
pub type Flat = Arc<Vec<Vec<[f64; 2]>>>;

/// Per-face caches of hinted glyphs and their flattenings.
#[derive(Debug, Default)]
pub struct Cache {
    glyphs: Mutex<HashMap<u32, Arc<Glyph>>>,
    flat: Mutex<HashMap<(u32, u32), Flat>>,
}

#[derive(Default)]
struct PathPen {
    cmds: Vec<Cmd>,
    cbox: Option<CBox>,
}

fn fixed(x: f32) -> [i64; 1] {
    // skrifa reports 26.6 values as `bits / 64` in an f32, which is exact
    // for every coordinate a 2170 ppem outline can have.
    [(f64::from(x) * 64.0).round() as i64]
}

impl PathPen {
    fn pt(&mut self, x: f32, y: f32) -> [i64; 2] {
        let p = [fixed(x)[0], fixed(y)[0]];
        let b = self.cbox.get_or_insert(CBox {
            x_min: p[0],
            y_min: p[1],
            x_max: p[0],
            y_max: p[1],
        });
        b.x_min = b.x_min.min(p[0]);
        b.y_min = b.y_min.min(p[1]);
        b.x_max = b.x_max.max(p[0]);
        b.y_max = b.y_max.max(p[1]);
        p
    }
}

impl OutlinePen for PathPen {
    fn move_to(&mut self, x: f32, y: f32) {
        let p = self.pt(x, y);
        self.cmds.push(Cmd::Move(p));
    }
    fn line_to(&mut self, x: f32, y: f32) {
        let p = self.pt(x, y);
        self.cmds.push(Cmd::Line(p));
    }
    fn quad_to(&mut self, cx0: f32, cy0: f32, x: f32, y: f32) {
        let c = self.pt(cx0, cy0);
        let p = self.pt(x, y);
        self.cmds.push(Cmd::Quad(c, p));
    }
    fn curve_to(&mut self, cx0: f32, cy0: f32, cx1: f32, cy1: f32, x: f32, y: f32) {
        let c0 = self.pt(cx0, cy0);
        let c1 = self.pt(cx1, cy1);
        let p = self.pt(x, y);
        self.cmds.push(Cmd::Cubic(c0, c1, p));
    }
    // FreeType ends a contour whose last point is on the curve with a
    // `line_to` back to its start; the repeated start vertex is dropped
    // by the union the caller performs, so nothing is added here.
    fn close(&mut self) {}
}

impl Cache {
    /// The hinted glyph, loaded once per face. `None` when the glyph cannot
    /// be loaded ("Could not load glyph").
    pub fn glyph(&self, font: &FontRef<'_>, state: &FaceState, g: u32) -> Option<Arc<Glyph>> {
        if let Some(x) = self.glyphs.lock().expect("glyph cache").get(&g) {
            return Some(x.clone());
        }
        let outlines = font.outline_glyphs();
        let mut pen = PathPen::default();
        match outlines.get(GlyphId::new(g)) {
            Some(glyph) => {
                let settings = match &state.hinting {
                    Some(h) if h.is_enabled() => DrawSettings::hinted(h, false),
                    // A font's `prep` program can switch its instructions
                    // off at this size (Liberation does). FreeType then
                    // still loads at the hinted size, whose ppem is rounded
                    // to an integer for fonts that ask for it (`head` flag
                    // bit 3), while skrifa would fall back to the requested
                    // size: the rounded one is what the nightly shows
                    // (Liberation Sans `l` is 21564 units wide, not 21566).
                    _ => DrawSettings::unhinted(state.hinted_size, LocationRef::default()),
                };
                glyph.draw(settings, &mut pen).ok()?;
            }
            // An empty glyph (a space) loads with an empty outline.
            None if g < u32::from(font_glyph_count(font)) => {}
            None => return None,
        }
        let out = Arc::new(Glyph {
            cmds: pen.cmds,
            cbox: pen.cbox,
        });
        self.glyphs
            .lock()
            .expect("glyph cache")
            .insert(g, out.clone());
        Some(out)
    }

    /// The glyph flattened with `fn` steps per curve.
    pub fn flat(&self, glyph: &Glyph, g: u32, fn_: u32) -> Flat {
        if let Some(x) = self.flat.lock().expect("flat cache").get(&(g, fn_)) {
            return x.clone();
        }
        let out = Arc::new(flatten(glyph, fn_));
        self.flat
            .lock()
            .expect("flat cache")
            .insert((g, fn_), out.clone());
        out
    }
}

fn font_glyph_count(font: &FontRef<'_>) -> u16 {
    use skrifa::raw::TableProvider;
    font.maxp().map_or(0, |m| m.num_glyphs())
}

fn v(p: [i64; 2]) -> [f64; 2] {
    [p[0] as f64 / SCALE, p[1] as f64 / SCALE]
}

/// `DrawingCallback`'s `move_to`, `line_to` and both `curve_to`s. The
/// powers go through `powf` as the C++ goes through `std::pow`: the
/// compiler turns squares into products in both, and cubes stay calls to
/// the same libm, so the points agree to the last bit.
fn flatten(glyph: &Glyph, fn_: u32) -> Vec<Vec<[f64; 2]>> {
    let mut out: Vec<Vec<[f64; 2]>> = Vec::new();
    let mut cur: Vec<[f64; 2]> = Vec::new();
    let mut pen = [0.0, 0.0];
    let step = 1.0 / f64::from(fn_);
    for cmd in &glyph.cmds {
        match *cmd {
            Cmd::Move(p) => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
                pen = v(p);
                cur.push(pen);
            }
            Cmd::Line(p) => {
                pen = v(p);
                cur.push(pen);
            }
            Cmd::Quad(c, p) => {
                let (c1, to) = (v(c), v(p));
                for idx in 1..=fn_ {
                    let a = f64::from(idx) * step;
                    let b = 1.0 - a;
                    // pen * pow(1 - a, 2) + c1 * 2 * pow(1 - a, 1) * a + to * pow(a, 2)
                    let f = |k: usize| {
                        pen[k] * b.powf(2.0) + c1[k] * 2.0 * b.powf(1.0) * a + to[k] * a.powf(2.0)
                    };
                    cur.push([f(0), f(1)]);
                }
                pen = to;
            }
            Cmd::Cubic(c0, c1, p) => {
                let (c1_, c2, to) = (v(c0), v(c1), v(p));
                for idx in 1..=fn_ {
                    let a = f64::from(idx) * step;
                    let b = 1.0 - a;
                    let f = |k: usize| {
                        pen[k] * b.powf(3.0)
                            + c1_[k] * 3.0 * b.powf(2.0) * a
                            + c2[k] * 3.0 * b.powf(1.0) * a.powf(2.0)
                            + to[k] * a.powf(3.0)
                    };
                    cur.push([f(0), f(1)]);
                }
                pen = to;
            }
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}
