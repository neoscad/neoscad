//! Shaping with harfrust, set up to give the positions OpenSCAD gets from
//! HarfBuzz on a FreeType face (`hb_ft_font_create`, HarfBuzz 11.4.1 in
//! the nightly, `src/hb-ft.cc`).
//!
//! OpenSCAD sizes the face with `FT_Set_Char_Size(face, 0, 1e5, 100, 100)`
//! (`FreetypeRenderer.cc:302`): 1e5 26.6 points at 100 dpi, so one em is
//! `(1e5 * 100 + 36) / 72 = 138889` 26.6 units, about 2170 pixels. Every
//! position below is in those units; OpenSCAD divides them by 1e5 and
//! multiplies by `size`, which is where the 100/72 factor between `size`
//! and the em comes from (issue #4304).
//!
//! `hb_ft` answers HarfBuzz's metric queries from FreeType with hinting
//! off, and those answers are reproduced exactly through harfrust's
//! [`FontFuncs`], because they are not what HarfBuzz would compute from the
//! tables itself:
//!
//! - the font scale is FreeType's `x_scale * upem`, rounded (`hb_ft_font_changed`);
//! - advances are FreeType's 16.16 advances rounded to 26.6 *through a
//!   `float`* (`hb_ft_get_glyph_h_advances`), which can differ by one unit
//!   from HarfBuzz's own scaling;
//! - vertical advances and origins come from FreeType's synthesized
//!   vertical metrics for fonts without `vmtx` (`compute_glyph_metrics` in
//!   FreeType's `ttgload.c`), which vertical text (`direction="ttb"`)
//!   depends on;
//! - characters map through the charmap `FontCache` selects, including the
//!   symbol-font rules.

use std::sync::{Arc, Mutex};

use harfrust::font::{BuiltinFontFuncs, FontFuncs};
use harfrust::{
    Direction, Feature, GlyphExtents, GlyphId, Language, Script, ShapeOptions, ShaperData, Tag,
    UnicodeBuffer,
};
use skrifa::instance::{LocationRef, Size};
use skrifa::outline::{DrawSettings, HintingInstance, HintingOptions, OutlinePen};
use skrifa::raw::tables::cmap::{CmapSubtable, PlatformId};
use skrifa::raw::{FontRef, TableProvider};
use skrifa::{GlyphId as SkGlyphId, MetadataProvider};

use crate::fontdb::Face;
use crate::outline::{CBox, Cache};

/// FreeType's character size in 26.6 points (`FreetypeRenderer::scale`).
pub const SCALE: f64 = 1e5;

/// The requested 26.6 pixel size: `FT_REQUEST_HEIGHT` for 1e5 points at
/// 100 dpi, `(1e5 * 100 + 36) / 72`.
const PIXEL_SIZE: i64 = (100_000 * 100 + 36) / 72;

// FreeType's fixed-point helpers (`ftcalc.c`), on 64-bit longs.

pub(crate) fn ft_mul_fix(a: i64, b: i64) -> i64 {
    let ab = a * b;
    (ab + 0x8000 - i64::from(ab < 0)) >> 16
}

pub(crate) fn ft_div_fix(a: i64, b: i64) -> i64 {
    let neg = (a < 0) != (b < 0);
    let (a, b) = (a.abs(), b.abs());
    let q = if b == 0 {
        0x7FFF_FFFF
    } else {
        ((a << 16) + (b >> 1)) / b
    };
    if neg { -q } else { q }
}

pub(crate) fn ft_mul_div(a: i64, b: i64, c: i64) -> i64 {
    let neg = ((a < 0) != (b < 0)) != (c < 0);
    let (a, b, c) = (a.abs(), b.abs(), c.abs());
    let d = if c > 0 {
        (a * b + (c >> 1)) / c
    } else {
        0x7FFF_FFFF
    };
    if neg { -d } else { d }
}

/// The character map FreeType ends up with after `FontCache` selects one.
#[derive(Debug, Clone, Copy)]
struct Charmap {
    index: u16,
    /// A Microsoft symbol map (3,0): HarfBuzz retries unmapped characters
    /// below U+0100 in the U+F000 page.
    symbol: bool,
}

/// Per-face state, built on first use and kept for the process.
pub struct FaceState {
    upem: i64,
    /// FreeType's unhinted 16.16 scales (`size->metrics.x_scale`).
    x_scale: i64,
    y_scale: i64,
    /// HarfBuzz's font scale.
    hb_scale: (i32, i32),
    charmap: Option<Charmap>,
    /// `FontCache::is_windows_symbol_font`: text below U+0100 is moved to
    /// U+F000 before shaping.
    pub windows_symbol: bool,
    /// The TrueType interpreter or autohinter at FreeType's size, as
    /// `FT_LOAD_DEFAULT` would pick.
    pub hinting: Option<HintingInstance>,
    /// The size FreeType loads hinted glyphs at: its ppem rounded to an
    /// integer when the font sets `head` flag bit 3 (`tt_size_reset`).
    pub hinted_size: Size,
    shaper_data: ShaperData,
    /// Flattened glyph outlines, see [`Cache`].
    pub cache: Cache,
    /// Unhinted metrics by glyph.
    metrics: Mutex<std::collections::HashMap<u32, Arc<UnhintedMetrics>>>,
}

impl std::fmt::Debug for FaceState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FaceState")
            .field("upem", &self.upem)
            .field("x_scale", &self.x_scale)
            .finish()
    }
}

/// `FT_Load_Glyph(..., FT_LOAD_NO_HINTING)` glyph metrics, in 26.6.
#[derive(Debug, Clone, Copy)]
struct UnhintedMetrics {
    cbox: CBox,
    hori_advance: i64,
    vert_bearing_y: i64,
}

impl FaceState {
    /// Font units in 26.6 at FreeType's size: `FT_MulFix(units,
    /// size->metrics.y_scale)`. The unhinted scale, which FreeType keeps in
    /// `size->metrics` even for a font whose hinting rounds the ppem.
    pub(crate) fn scale_y(&self, units: i64) -> i64 {
        ft_mul_fix(units, self.y_scale)
    }

    pub fn new(font: &FontRef<'_>) -> FaceState {
        let upem = i64::from(font.head().map_or(1000, |h| h.units_per_em())).max(1);
        let x_scale = ft_div_fix(PIXEL_SIZE, upem);
        let y_scale = x_scale;
        // `hb_ft_font_changed`.
        let hb = ((x_scale as u64 * upem as u64 + (1 << 15)) >> 16) as i32;
        let charmap = select_charmap(font);
        let windows_symbol = charmap.is_some_and(|c| c.symbol && first_char(font, c) >= 0xF000);
        let outlines = font.outline_glyphs();
        let size = Size::new((PIXEL_SIZE as f64 / 64.0) as f32);
        let hinting = HintingInstance::new(
            &outlines,
            size,
            LocationRef::default(),
            HintingOptions::default(),
        )
        .ok();
        let force_integer = font.head().is_ok_and(|h| h.flags().bits() & 8 != 0);
        let hinted_size = if force_integer {
            Size::new(((PIXEL_SIZE + 32) >> 6) as f32)
        } else {
            size
        };
        FaceState {
            hinted_size,
            upem,
            x_scale,
            y_scale,
            hb_scale: (hb, hb),
            charmap,
            windows_symbol,
            hinting,
            shaper_data: ShaperData::new(font),
            cache: Cache::default(),
            metrics: Mutex::default(),
        }
    }

    fn map(&self, font: &FontRef<'_>, c: u32) -> Option<u32> {
        let cm = self.charmap?;
        let cmap = font.cmap().ok()?;
        let sub = cmap.subtable(cm.index).ok()?;
        let g = sub.map_codepoint(c).map(|g| g.to_u32()).filter(|&g| g != 0);
        match g {
            Some(g) => Some(g),
            // `hb_ft_get_nominal_glyph`, font page "none".
            None if cm.symbol && c <= 0xFF => sub
                .map_codepoint(0xF000 + c)
                .map(|g| g.to_u32())
                .filter(|&g| g != 0),
            None => None,
        }
    }

    /// `FT_Get_Advance(..., FT_LOAD_NO_HINTING)`: the font-unit advance
    /// scaled to 16.16 (`ft_face_scale_advances_`).
    fn ft_advance(&self, font: &FontRef<'_>, g: u32, vertical: bool) -> i64 {
        if vertical {
            ft_mul_div(vertical_advance_units(font, g), self.y_scale, 64)
        } else {
            let aw = font
                .hmtx()
                .ok()
                .and_then(|h| h.advance(SkGlyphId::new(g)))
                .unwrap_or(0);
            ft_mul_div(i64::from(aw), self.x_scale, 64)
        }
    }

    fn unhinted(&self, font: &FontRef<'_>, g: u32) -> Arc<UnhintedMetrics> {
        if let Some(m) = self
            .metrics
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&g)
        {
            return m.clone();
        }
        let m = Arc::new(self.compute_unhinted(font, g));
        self.metrics
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(g, m.clone());
        m
    }

    /// `compute_glyph_metrics` for an unhinted load at the unrounded scale.
    fn compute_unhinted(&self, font: &FontRef<'_>, g: u32) -> UnhintedMetrics {
        let gid = SkGlyphId::new(g);
        let mut pen = BoxPen::default();
        if let Some(glyph) = font.outline_glyphs().get(gid) {
            let size = Size::new((PIXEL_SIZE as f64 / 64.0) as f32);
            let _ = glyph.draw(
                DrawSettings::unhinted(size, LocationRef::default()),
                &mut pen,
            );
        }
        let cbox = pen.cbox.unwrap_or_default();
        let (aw, lsb) = font.hmtx().ok().map_or((0, 0), |h| {
            (
                i64::from(h.advance(gid).unwrap_or(0)),
                i64::from(h.side_bearing(gid).unwrap_or(0)),
            )
        });
        // The phantom points: `pp1.x = xMin - lsb`, `pp2.x = pp1.x + aw` in
        // font units (the glyph header's xMin), each scaled on its own.
        let (x_min, y_max) = glyph_header_bounds(font, gid);
        let pp1 = x_min - lsb;
        let hori_advance = ft_mul_fix(pp1 + aw, self.x_scale) - ft_mul_fix(pp1, self.x_scale);
        let top = if let Some((tsb, _)) = vmtx_metrics(font, gid) {
            let pp3 = ft_mul_fix(y_max + tsb, self.y_scale);
            i64::from(ft_div_fix(pp3 - cbox.y_max, self.y_scale) as i16)
        } else {
            let height = ft_div_fix(cbox.y_max - cbox.y_min, self.y_scale) as i16;
            let advance = synthesized_vertical_advance(font);
            // C integer division truncates toward zero.
            (advance - i64::from(height)) / 2
        };
        UnhintedMetrics {
            cbox,
            hori_advance,
            vert_bearing_y: ft_mul_fix(top, self.y_scale),
        }
    }
}

/// The glyph header's `xMin` and `yMax` in font units (0 for an empty
/// glyph), as FreeType's TrueType loader reads them.
fn glyph_header_bounds(font: &FontRef<'_>, gid: SkGlyphId) -> (i64, i64) {
    let (Ok(loca), Ok(glyf)) = (font.loca(None), font.glyf()) else {
        return (0, 0);
    };
    match loca.get_glyf(gid, &glyf) {
        Ok(Some(g)) => (i64::from(g.x_min()), i64::from(g.y_max())),
        _ => (0, 0),
    }
}

fn vmtx_metrics(font: &FontRef<'_>, gid: SkGlyphId) -> Option<(i64, i64)> {
    font.vhea().ok()?;
    let vmtx = font.vmtx().ok()?;
    Some((
        i64::from(vmtx.side_bearing(gid)?),
        i64::from(vmtx.advance(gid)?),
    ))
}

/// `TT_Get_VMetrics` without `vmtx`: the OS/2 typographic line height, or
/// the `hhea` one.
fn synthesized_vertical_advance(font: &FontRef<'_>) -> i64 {
    if let Ok(os2) = font.os2() {
        (i64::from(os2.s_typo_ascender()) - i64::from(os2.s_typo_descender())).abs()
    } else if let Ok(hhea) = font.hhea() {
        (i64::from(hhea.ascender().to_i16()) - i64::from(hhea.descender().to_i16())).abs()
    } else {
        0
    }
}

fn vertical_advance_units(font: &FontRef<'_>, g: u32) -> i64 {
    match vmtx_metrics(font, SkGlyphId::new(g)) {
        Some((_, ah)) => ah,
        None => synthesized_vertical_advance(font),
    }
}

/// FreeType's `FT_Select_Charmap(face, FT_ENCODING_UNICODE)` (a UCS-4 map
/// first, then any Unicode map, searching from the last), then the
/// fallbacks `FontCache::find_face_fontconfig` tries in order.
fn select_charmap(font: &FontRef<'_>) -> Option<Charmap> {
    let cmap = font.cmap().ok()?;
    let recs: Vec<(u16, u16, u16)> = cmap
        .encoding_records()
        .iter()
        .enumerate()
        .filter(|(i, _)| {
            // Format 14 holds variation sequences, not a character map.
            !matches!(cmap.subtable(*i as u16), Ok(CmapSubtable::Format14(_)))
        })
        .map(|(i, r)| {
            let p = match r.platform_id() {
                PlatformId::Unicode => 0,
                PlatformId::Macintosh => 1,
                PlatformId::ISO => 2,
                PlatformId::Windows => 3,
                PlatformId::Custom => 4,
                _ => 5,
            };
            (i as u16, p, r.encoding_id())
        })
        .collect();
    let is_unicode =
        |p: u16, e: u16| p == 0 || (p == 3 && (e == 1 || e == 10)) || (p == 2 && e == 1);
    let pick = |i: u16, p: u16, e: u16| Charmap {
        index: i,
        symbol: p == 3 && e == 0,
    };
    for &(i, p, e) in recs.iter().rev() {
        if is_unicode(p, e) && ((p == 3 && e == 10) || (p == 0 && e == 4)) {
            return Some(pick(i, p, e));
        }
    }
    for &(i, p, e) in recs.iter().rev() {
        if is_unicode(p, e) {
            return Some(pick(i, p, e));
        }
    }
    // `try_charmap`: (3,1), (2,1), (0,any), (3,0), (1,0), (2,2), (2,0).
    for (pp, pe) in [
        (3, Some(1)),
        (2, Some(1)),
        (0, None),
        (3, Some(0)),
        (1, Some(0)),
        (2, Some(2)),
        (2, Some(0)),
    ] {
        if let Some(&(i, p, e)) = recs
            .iter()
            .find(|&&(_, p, e)| p == pp && pe.is_none_or(|x| x == e))
        {
            return Some(pick(i, p, e));
        }
    }
    None
}

/// `FT_Get_First_Char`: the lowest character code with a glyph.
fn first_char(font: &FontRef<'_>, cm: Charmap) -> u32 {
    let Ok(cmap) = font.cmap() else { return 0 };
    let Ok(sub) = cmap.subtable(cm.index) else {
        return 0;
    };
    sub.iter()
        .filter(|(_, g)| g.to_u32() != 0)
        .map(|(c, _)| c)
        .min()
        .unwrap_or(0)
}

/// A pen that only records the control box of what it is given.
#[derive(Default)]
struct BoxPen {
    cbox: Option<CBox>,
}

impl BoxPen {
    fn add(&mut self, x: f32, y: f32) {
        let (x, y) = ((f64::from(x) * 64.0) as i64, (f64::from(y) * 64.0) as i64);
        let b = self.cbox.get_or_insert(CBox {
            x_min: x,
            y_min: y,
            x_max: x,
            y_max: y,
        });
        b.x_min = b.x_min.min(x);
        b.y_min = b.y_min.min(y);
        b.x_max = b.x_max.max(x);
        b.y_max = b.y_max.max(y);
    }
}

impl OutlinePen for BoxPen {
    fn move_to(&mut self, x: f32, y: f32) {
        self.add(x, y);
    }
    fn line_to(&mut self, x: f32, y: f32) {
        self.add(x, y);
    }
    fn quad_to(&mut self, cx0: f32, cy0: f32, x: f32, y: f32) {
        self.add(cx0, cy0);
        self.add(x, y);
    }
    fn curve_to(&mut self, cx0: f32, cy0: f32, cx1: f32, cy1: f32, x: f32, y: f32) {
        self.add(cx0, cy0);
        self.add(cx1, cy1);
        self.add(x, y);
    }
    fn close(&mut self) {}
}

/// harfrust's metric callbacks answering as `hb_ft` does.
struct FtFuncs<'a> {
    state: &'a FaceState,
    font: &'a FontRef<'a>,
}

impl FontFuncs for FtFuncs<'_> {
    fn nominal_glyph(&mut self, _: &BuiltinFontFuncs, c: u32) -> Option<GlyphId> {
        self.state.map(self.font, c).map(GlyphId::new)
    }

    fn advance_width(&mut self, _: &BuiltinFontFuncs, glyph: GlyphId) -> i32 {
        // `v = (int) (v * x_mult + (1<<9)) >> 10`, with `x_mult` a float:
        // the 16.16 advance is rounded to a float before it is rounded
        // to 26.6.
        let v = self
            .state
            .ft_advance(self.font, glyph.to_u32(), false)
            .abs();
        (((v as f32) * 1.0f32 + 512.0f32) as i32) >> 10
    }

    fn advance_height(&mut self, _: &BuiltinFontFuncs, glyph: GlyphId) -> i32 {
        // "FreeType's vertical metrics grows downward": `(-v + (1<<9)) >> 10`.
        let v = self.state.ft_advance(self.font, glyph.to_u32(), true);
        ((-v + 512) >> 10) as i32
    }

    fn vertical_origin(&mut self, _: &BuiltinFontFuncs, glyph: GlyphId) -> (i32, i32) {
        let m = self.state.unhinted(self.font, glyph.to_u32());
        // horiBearingX - vertBearingX, where vertBearingX = horiBearingX -
        // horiAdvance / 2; horiBearingY + vertBearingY.
        let x = m.cbox.x_min - (m.cbox.x_min - m.hori_advance / 2);
        let y = m.cbox.y_max + m.vert_bearing_y;
        (x as i32, y as i32)
    }

    fn extents(&mut self, _: &BuiltinFontFuncs, glyph: GlyphId) -> Option<GlyphExtents> {
        let m = self.state.unhinted(self.font, glyph.to_u32());
        let x1 = m.cbox.x_min as f32;
        let y1 = m.cbox.y_max as f32;
        let x2 = x1 + (m.cbox.x_max - m.cbox.x_min) as f32;
        let y2 = y1 - (m.cbox.y_max - m.cbox.y_min) as f32;
        let x_bearing = x1.round() as i32;
        let y_bearing = y1.round() as i32;
        Some(GlyphExtents {
            x_bearing,
            y_bearing,
            width: x2.round() as i32 - x_bearing,
            height: y2.round() as i32 - y_bearing,
        })
    }
}

/// One shaped glyph, positions in 26.6 at FreeType's size.
#[derive(Debug, Clone, Copy)]
pub struct Shaped {
    pub glyph: u32,
    pub x_advance: i32,
    pub y_advance: i32,
    pub x_offset: i32,
    pub y_offset: i32,
}

/// `hb_tag_from_string`: up to four bytes before a NUL, space padded.
fn tag_from_string(s: &str) -> Option<Tag> {
    let b: Vec<u8> = s.bytes().take_while(|&c| c != 0).take(4).collect();
    if b.is_empty() {
        return None;
    }
    let mut t = [b' '; 4];
    t[..b.len()].copy_from_slice(&b);
    Some(Tag::new(&t))
}

/// Shape `text` as `FreetypeRenderer::ShapeResults` does, with the
/// direction, script and language the node resolved.
pub fn shape(
    face: &Face,
    state: &FaceState,
    text: &str,
    direction: &str,
    script: &str,
    language: &str,
    features: &[String],
) -> (Vec<Shaped>, bool) {
    let Some(font) = face.font() else {
        return (Vec::new(), true);
    };
    let mut buf = UnicodeBuffer::new();
    // HarfBuzz receives the text as a C string.
    let text = text.split('\0').next().unwrap_or("");
    if state.windows_symbol {
        for (i, c) in text.chars().enumerate() {
            let c = c as u32;
            let c = if c < 0x100 { 0xF000 + c } else { c };
            buf.add(char::from_u32(c).unwrap_or('\u{FFFD}'), i as u32);
        }
    } else {
        buf.push_str(text);
    }
    let dir: Direction = direction.parse().unwrap_or(Direction::LeftToRight);
    buf.set_direction(dir);
    if let Some(s) = tag_from_string(script).and_then(Script::from_iso15924_tag) {
        buf.set_script(s);
    }
    if let Ok(l) = language.parse::<Language>() {
        buf.set_language(l);
    }
    let feats: Vec<Feature> = features.iter().filter_map(|f| f.parse().ok()).collect();
    let shaper = state.shaper_data.shaper(&font).build();
    let mut funcs = FtFuncs { state, font: &font };
    let out = shaper.shape(
        buf,
        ShapeOptions::new()
            .scale_separate(Some(state.hb_scale))
            .features(&feats)
            .font_funcs(Some(&mut funcs)),
    );
    let glyphs = out
        .glyph_infos()
        .iter()
        .zip(out.glyph_positions())
        .map(|(i, p)| Shaped {
            glyph: i.glyph_id,
            x_advance: p.x_advance,
            y_advance: p.y_advance,
            x_offset: p.x_offset,
            y_offset: p.y_offset,
        })
        .collect();
    let horizontal = matches!(dir, Direction::LeftToRight | Direction::RightToLeft);
    (glyphs, horizontal)
}

/// The face-wide values FreeType fills in when it opens a face
/// (`sfnt_load_face` in `sfobjs.c`), in font units.
pub(crate) struct FaceMetrics {
    pub ascender: i64,
    pub descender: i64,
    pub height: i64,
    pub y_min: i64,
    pub y_max: i64,
    pub family: String,
    pub style: String,
}

/// `FT_Face`'s `ascender`, `descender`, `height`, `bbox` and names: the
/// `hhea` metrics, or `OS/2`'s when `hhea` has none; `head`'s box; the
/// names as `sfnt_load_face` picks them.
pub(crate) fn face_metrics(font: &FontRef<'_>) -> FaceMetrics {
    let (mut ascender, mut descender, mut height) = (0, 0, 0);
    if let Ok(h) = font.hhea() {
        ascender = i64::from(h.ascender().to_i16());
        descender = i64::from(h.descender().to_i16());
        height = ascender - descender + i64::from(h.line_gap().to_i16());
    }
    if ascender == 0
        && descender == 0
        && let Ok(os2) = font.os2()
    {
        if os2.s_typo_ascender() != 0 || os2.s_typo_descender() != 0 {
            ascender = i64::from(os2.s_typo_ascender());
            descender = i64::from(os2.s_typo_descender());
            height = ascender - descender + i64::from(os2.s_typo_line_gap());
        } else {
            // FreeType casts both to `FT_Short` first.
            ascender = i64::from(os2.us_win_ascent() as i16);
            descender = -i64::from(os2.us_win_descent() as i16);
            height = ascender - descender;
        }
    }
    let (y_min, y_max) = font
        .head()
        .map_or((0, 0), |h| (i64::from(h.y_min()), i64::from(h.y_max())));
    // A WWS font (`fsSelection` bit 8) names its family by the WWS names
    // first; otherwise the typographic names come first.
    let wws = font
        .os2()
        .is_ok_and(|o| o.fs_selection().bits() & (1 << 8) != 0);
    let pick = |ids: &[u16]| ids.iter().find_map(|&id| face_name(font, id));
    let (family, style) = if wws {
        (pick(&[21, 16, 1]), pick(&[22, 17, 2]))
    } else {
        (pick(&[16, 1]), pick(&[17, 2]))
    };
    FaceMetrics {
        ascender,
        descender,
        height,
        y_min,
        y_max,
        family: family.unwrap_or_default(),
        style: style.unwrap_or_default(),
    }
}

/// `tt_face_get_name`: an English Windows name if there is one (else any
/// Windows name in a Unicode or symbol encoding, unless an Apple English
/// one exists), then an Apple name (English, then Roman), then a Unicode
/// platform one; FreeType turns it into ASCII, every character outside
/// 32..=127 becoming `?`, and stops at a NUL.
fn face_name(font: &FontRef<'_>, id: u16) -> Option<String> {
    let name = font.name().ok()?;
    let data = name.string_data();
    let (mut win, mut is_english) = (None, false);
    let (mut apple_english, mut apple_roman, mut unicode) = (None, None, None);
    for rec in name.name_record() {
        if rec.name_id().to_u16() != id || rec.length() == 0 {
            continue;
        }
        let english = rec.language_id() & 0x3FF == 0x009;
        match rec.platform_id() {
            0 | 2 => unicode = Some(rec),
            1 => {
                if rec.language_id() == 0 {
                    apple_english = Some(rec);
                } else if rec.encoding_id() == 0 {
                    apple_roman = Some(rec);
                }
            }
            3 if (win.is_none() || english) && matches!(rec.encoding_id(), 0 | 1 | 10) => {
                is_english = english;
                win = Some(rec);
            }
            _ => {}
        }
    }
    let apple = apple_english.or(apple_roman);
    let rec = match win {
        Some(w) if !(apple.is_some() && !is_english) => w,
        _ => apple.or(unicode)?,
    };
    let s = rec.string(data).ok()?;
    Some(
        s.chars()
            .take_while(|&c| c != '\0')
            .map(|c| {
                if (' '..='\u{7f}').contains(&c) {
                    c
                } else {
                    '?'
                }
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_point() {
        // 1e5 points at 100 dpi over a 2048 unit em.
        assert_eq!(ft_div_fix(PIXEL_SIZE, 2048), 4_444_448);
        assert_eq!(ft_mul_fix(1484, 4_444_160), 100_634);
        assert_eq!(ft_mul_fix(-1484, 4_444_160), -100_634);
        assert_eq!(ft_mul_div(455, 4_444_448, 64), 31_597_248);
        assert_eq!(ft_div_fix(-1, 2), -32_768);
    }
}
