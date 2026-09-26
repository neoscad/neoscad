//! `text()`: font lookup, shaping and glyph outlines, reproducing what
//! OpenSCAD's `FreetypeRenderer`, `FontCache` and `DrawingCallback`
//! (`src/core/FreetypeRenderer.cc`, `src/FontCache.cc`,
//! `src/core/DrawingCallback.cc`) produce with FreeType, HarfBuzz and
//! fontconfig, without any of the three:
//!
//! - [`fontdb`]: the fonts the host makes available, and the lookup of a
//!   fontconfig-style name among them ([`pattern`]);
//! - [`shape`]: harfrust with FreeType's metrics, as `hb_ft` supplies them;
//! - [`outline`]: skrifa's hinted outlines, flattened as `DrawingCallback`
//!   flattens FreeType's.
//!
//! This is its own crate rather than a module of `geom` because the
//! evaluator needs it too (the experimental `textmetrics()` and
//! `fontmetrics()` functions shape text, and `eval` sits below `geom`), and
//! because it is the only user of the font parsers, which keeps them out of
//! the geometry crate's build.
//!
//! [`render`] returns each glyph's contours; the caller unions them with the
//! non-zero rule (`ClipperUtils::apply(..., Union)` in
//! `GeometryEvaluator::visit(TextNode)`).

pub mod fontdb;
pub mod outline;
pub mod pattern;
pub mod shape;

use std::sync::Arc;

pub use fontdb::{FontData, FontDb, LookupError};

use shape::{FaceState, SCALE};

/// `text()`'s parameters after `detect_properties`.
#[derive(Debug, Clone, PartialEq)]
pub struct Params<'a> {
    pub text: &'a str,
    pub size: f64,
    pub spacing: f64,
    pub font: &'a str,
    /// The resolved direction (`ltr`, `rtl`, `ttb`, `btt`).
    pub direction: &'a str,
    pub language: &'a str,
    /// The resolved script: an ISO 15924 tag, or the string as given.
    pub script: &'a str,
    pub halign: &'a str,
    pub valign: &'a str,
    /// Curve steps: `max(circle segments for radius size / 8 + 1, 2)`.
    pub segments: u32,
}

/// How a message is printed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    /// `FONT-WARNING:` with no location (`message_group::Font_Warning`).
    FontWarning,
    /// `WARNING:` at the `text()` call.
    Warning,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Message {
    pub level: Level,
    pub text: String,
}

/// The shapes of one `text()`: a list of contours per glyph with ink.
#[derive(Debug, Default)]
pub struct Rendered {
    pub glyphs: Vec<Vec<Vec<[f64; 2]>>>,
    pub messages: Vec<Message>,
}

/// Text segments for a circle segment count, as `detect_properties`
/// computes them.
pub fn segments_for(circle_segments: Option<i32>) -> u32 {
    let s = circle_segments.unwrap_or(3);
    (s / 8 + 1).max(2) as u32
}

/// A shaped string: `FreetypeRenderer::ShapeResults`.
struct ShapeResults {
    face: Arc<fontdb::Face>,
    glyphs: Vec<(shape::Shaped, Arc<outline::Glyph>)>,
    x_offset: f64,
    y_offset: f64,
}

fn warn(out: &mut Vec<Message>, text: String) {
    out.push(Message {
        level: Level::Warning,
        text,
    });
}

fn shape_results(db: &FontDb, p: &Params<'_>, msgs: &mut Vec<Message>) -> Option<ShapeResults> {
    let face = match db.lookup(p.font) {
        Ok(f) => f,
        Err(e) => {
            if e == LookupError::Parse {
                let t = p.font.trim();
                let lookup = if t.is_empty() {
                    "Liberation Sans:style=Regular"
                } else {
                    t
                };
                msgs.push(Message {
                    level: Level::FontWarning,
                    text: format!("Could not parse font '{lookup}'"),
                });
            }
            warn(msgs, format!("Can't get font {}", p.font));
            return None;
        }
    };
    let font = face.font()?;
    let state = face.state.get_or_init(|| FaceState::new(&font));
    let features = pattern::parse(if p.font.trim().is_empty() {
        "Liberation Sans:style=Regular"
    } else {
        p.font.trim()
    })
    .map(|pt| {
        pt.features
            .iter()
            .flat_map(|f| f.split(';').map(str::to_string).collect::<Vec<_>>())
            .collect::<Vec<_>>()
    })
    .unwrap_or_default();
    let (shaped, horizontal) = shape::shape(
        &face,
        state,
        p.text,
        p.direction,
        p.script,
        p.language,
        &features,
    );
    let mut glyphs = Vec::with_capacity(shaped.len());
    for (idx, s) in shaped.into_iter().enumerate() {
        match state.cache.glyph(&font, state, s.glyph) {
            Some(g) => glyphs.push((s, g)),
            None => warn(
                msgs,
                format!(
                    "Could not load glyph {} for char at index {idx} in text '{}'",
                    s.glyph, p.text
                ),
            ),
        }
    }

    let mut ascent = f64::MIN;
    let mut descent = f64::MAX;
    let (mut advance_x, mut advance_y) = (0.0, 0.0);
    let (mut left, mut right) = (f64::MAX, f64::MIN);
    for (s, g) in &glyphs {
        let bbox = g.cbox.unwrap_or_default().grid_fit();
        // Glyphs without ink leave the extents alone.
        if bbox.x_max > bbox.x_min && bbox.y_max > bbox.y_min {
            ascent = ascent.max(bbox.y_max as f64 / SCALE);
            descent = descent.min(bbox.y_min as f64 / SCALE);
            let gxoff = f64::from(s.x_offset) / SCALE;
            left = left.min(advance_x + gxoff + bbox.x_min as f64 / SCALE);
            right = right.max(advance_x + gxoff + bbox.x_max as f64 / SCALE);
        }
        advance_x += f64::from(s.x_advance) / SCALE * p.spacing;
        advance_y += f64::from(s.y_advance) / SCALE * p.spacing;
    }
    let (mut x_offset, mut y_offset) = (0.0, 0.0);
    // Right and left start out reversed; if they still are, there was no
    // ink and every offset stays zero.
    if right >= left {
        if horizontal {
            x_offset = match p.halign {
                "right" => -advance_x,
                "center" => -advance_x / 2.0,
                "left" | "default" => 0.0,
                other => {
                    warn(msgs, halign_warning(other));
                    0.0
                }
            };
            y_offset = match p.valign {
                "top" => -ascent,
                "center" => {
                    let height = ascent - descent;
                    -height / 2.0 - descent
                }
                "bottom" => -descent,
                "baseline" | "default" => 0.0,
                other => {
                    warn(msgs, valign_warning(other));
                    0.0
                }
            };
        } else {
            x_offset = match p.halign {
                "right" => -right,
                "left" => -left,
                "center" | "default" => 0.0,
                other => {
                    warn(msgs, halign_warning(other));
                    0.0
                }
            };
            y_offset = match p.valign {
                "baseline" => {
                    warn(
                        msgs,
                        "Don't use valign=\"baseline\" with vertical layouts".into(),
                    );
                    0.0
                }
                "center" => -advance_y / 2.0,
                "bottom" => -advance_y,
                "top" | "default" => 0.0,
                other => {
                    warn(msgs, valign_warning(other));
                    0.0
                }
            };
        }
    }
    Some(ShapeResults {
        face: face.clone(),
        glyphs,
        x_offset,
        y_offset,
    })
}

fn halign_warning(v: &str) -> String {
    format!("Unknown value for the halign parameter (use \"left\", \"right\" or \"center\"): '{v}'")
}

fn valign_warning(v: &str) -> String {
    format!(
        "Unknown value for the valign parameter (use \"baseline\", \"bottom\", \"top\" or \"center\"): '{v}'"
    )
}

/// `FreetypeRenderer::render`: each glyph's contours, placed and scaled.
pub fn render(db: &FontDb, p: &Params<'_>) -> Rendered {
    let mut out = Rendered::default();
    let Some(sr) = shape_results(db, p, &mut out.messages) else {
        return out;
    };
    let state = sr.face.state.get().expect("shaped face has state");
    let (mut adv_x, mut adv_y) = (0.0, 0.0);
    for (s, g) in &sr.glyphs {
        let off_x = sr.x_offset + f64::from(s.x_offset) / SCALE;
        let off_y = sr.y_offset + f64::from(s.y_offset) / SCALE;
        let flat = state.cache.flat(g, s.glyph, p.segments);
        // `add_vertex`: `size * (v + offset + advance)`.
        let contours: Vec<Vec<[f64; 2]>> = flat
            .iter()
            .map(|c| {
                c.iter()
                    .map(|v| {
                        [
                            p.size * (v[0] + off_x + adv_x),
                            p.size * (v[1] + off_y + adv_y),
                        ]
                    })
                    .collect()
            })
            .collect();
        adv_x += f64::from(s.x_advance) / SCALE * p.spacing;
        adv_y += f64::from(s.y_advance) / SCALE * p.spacing;
        if !contours.is_empty() {
            out.glyphs.push(contours);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// The reference checkout's bundled fonts and test fonts, or `None`
    /// (the test is skipped) when the checkout is missing.
    fn db() -> Option<FontDb> {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.reference/openscad");
        if !root.join("fonts").is_dir() {
            eprintln!("skipped: no reference checkout");
            return None;
        }
        let mut db = FontDb::new();
        db.add_dir(root.join("fonts"));
        db.add_dir(root.join("tests/data/ttf"));
        Some(db)
    }

    fn style_of(db: &FontDb, name: &str) -> String {
        let f = db.lookup(name).expect("a face");
        let font = f.font().expect("font");
        use skrifa::MetadataProvider;
        let fam: String = font
            .localized_strings(skrifa::string::StringId::FAMILY_NAME)
            .english_or_first()
            .map(|s| s.chars().collect())
            .unwrap_or_default();
        let sty: String = font
            .localized_strings(skrifa::string::StringId::SUBFAMILY_NAME)
            .english_or_first()
            .map(|s| s.chars().collect())
            .unwrap_or_default();
        format!("{fam}/{sty}")
    }

    /// Choices checked against the nightly (same SVG output).
    #[test]
    fn lookup_matches_fontconfig() {
        let Some(db) = db() else { return };
        for (name, want) in [
            ("", "Liberation Sans/Regular"),
            ("Liberation Sans", "Liberation Sans/Regular"),
            ("liberationsans", "Liberation Sans/Regular"),
            ("Liberation Sans:style=Bold", "Liberation Sans/Bold"),
            ("Liberation Sans:bold", "Liberation Sans/Bold"),
            ("Liberation Sans:weight=200", "Liberation Sans/Bold"),
            (
                "Liberation Sans:style=Bold Italic",
                "Liberation Sans/Bold Italic",
            ),
            ("Liberation Serif:style=Oblique", "Liberation Serif/Regular"),
            (":style=Bold", "Liberation Sans/Bold"),
            ("Bogus Family", "Liberation Sans/Regular"),
            ("serif", "Liberation Serif/Regular"),
            ("monospace", "Liberation Mono/Regular"),
            ("Amiri:style=Regular", "Amiri/Regular"),
            ("EvenOddTTa", "EvenOddTTa/Regular"),
            ("Liberation Sans:charset=76,78", "Liberation Sans/Regular"),
        ] {
            assert_eq!(style_of(&db, name), want, "{name:?}");
        }
        assert_eq!(
            db.lookup(":charset=xxx").map(|_| ()),
            Err(LookupError::Parse)
        );
    }

    fn params<'a>(text: &'a str, size: f64) -> Params<'a> {
        Params {
            text,
            size,
            spacing: 1.0,
            font: "",
            direction: "ltr",
            language: "en",
            script: "Latn",
            halign: "default",
            valign: "default",
            segments: 2,
        }
    }

    /// At `size = 1e5` the vertices are FreeType's 26.6 coordinates. The
    /// nightly draws Liberation Sans `l` from x 9358 to 21564 and up to
    /// 100634: the hinted size's integer ppem (2170), not the requested
    /// 2170.14, which would give 9359, 21566 and 100640.
    #[test]
    fn outline_at_freetype_scale() {
        let Some(db) = db() else { return };
        let r = render(&db, &params("l", 1e5));
        assert!(r.messages.is_empty());
        assert_eq!(
            r.glyphs,
            vec![vec![vec![
                [9358.0, 0.0],
                [9358.0, 100634.0],
                [21564.0, 100634.0],
                [21564.0, 0.0]
            ]]]
        );
        // The second glyph starts one advance on: hb_ft's 30857, not the
        // hinted 30854.
        let r = render(&db, &params("ll", 1e5));
        assert_eq!(r.glyphs[1][0][0], [9358.0 + 30857.0, 0.0]);
    }

    #[test]
    fn alignment_and_warnings() {
        let Some(db) = db() else { return };
        let mut p = params("l", 1e5);
        p.halign = "middle";
        let r = render(&db, &p);
        assert_eq!(r.messages.len(), 1);
        assert!(
            r.messages[0]
                .text
                .starts_with("Unknown value for the halign")
        );
        // Whitespace has no ink, so no alignment and no warning.
        let mut p = params("  ", 10.0);
        p.valign = "bogus";
        let r = render(&db, &p);
        assert!(r.glyphs.is_empty() && r.messages.is_empty());
        let mut p = params("x", 10.0);
        p.font = ":charset=xxx";
        let r = render(&db, &p);
        let texts: Vec<_> = r
            .messages
            .iter()
            .map(|m| (m.level, m.text.as_str()))
            .collect();
        assert_eq!(
            texts,
            [
                (Level::FontWarning, "Could not parse font ':charset=xxx'"),
                (Level::Warning, "Can't get font :charset=xxx"),
            ]
        );
    }

    #[test]
    fn segments() {
        assert_eq!(segments_for(None), 2);
        assert_eq!(segments_for(Some(30)), 4);
        assert_eq!(segments_for(Some(64)), 9);
    }

    #[test]
    fn no_fonts() {
        let db = FontDb::new();
        let r = render(&db, &params("x", 10.0));
        assert!(r.glyphs.is_empty());
        assert_eq!(r.messages[0].text, "Can't get font ");
    }
}
