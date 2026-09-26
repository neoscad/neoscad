//! The progress grid: one coloured cell per manifest test, grouped by tier,
//! on a fixed 1920x1080 canvas so snapshots can be stitched into a video.
//!
//! Cell positions depend only on the manifest (its order and per-tier
//! counts), so a test stays in the same place from frame to frame and only
//! its colour changes. Text uses a built-in 5x7 bitmap font to avoid
//! shipping or locating a font file.

use std::fs::File;
use std::io::BufWriter;
use std::path::Path;

use crate::manifest::TIER_NAMES;
use crate::run::Status;

pub const WIDTH: usize = 1920;
pub const HEIGHT: usize = 1080;
const MARGIN: usize = 24;

type Rgb = [u8; 3];
const BG: Rgb = [22, 22, 26];
const TEXT: Rgb = [235, 235, 235];
const DIM: Rgb = [150, 150, 160];

pub fn colour(s: Status) -> Rgb {
    match s {
        Status::Pass => [46, 204, 113],
        Status::Fail => [231, 76, 60],
        Status::Skip => [62, 62, 70],
        Status::Pending => [72, 96, 150],
    }
}

struct Canvas {
    px: Vec<u8>,
}

impl Canvas {
    fn new() -> Self {
        let mut px = vec![0; WIDTH * HEIGHT * 3];
        for p in px.as_chunks_mut::<3>().0 {
            *p = BG;
        }
        Self { px }
    }

    fn rect(&mut self, x: usize, y: usize, w: usize, h: usize, c: Rgb) {
        for yy in y..(y + h).min(HEIGHT) {
            for xx in x..(x + w).min(WIDTH) {
                let i = (yy * WIDTH + xx) * 3;
                self.px[i..i + 3].copy_from_slice(&c);
            }
        }
    }

    /// Draw text at `scale` (glyphs are 5x7, advance 6). Returns the x after
    /// the last glyph. Text past the right margin is cut off.
    fn text(&mut self, x: usize, y: usize, s: &str, scale: usize, c: Rgb) -> usize {
        let mut cx = x;
        for ch in s.chars() {
            if cx + 5 * scale > WIDTH - MARGIN {
                break;
            }
            let g = glyph(ch);
            for (row, bits) in g.iter().enumerate() {
                for col in 0..5 {
                    if bits & (0x10 >> col) != 0 {
                        self.rect(cx + col * scale, y + row * scale, scale, scale, c);
                    }
                }
            }
            cx += 6 * scale;
        }
        cx
    }
}

/// Render `cells` (tier, status) in manifest order to a PNG.
pub fn write_png(path: &Path, title: &str, subject: &str, cells: &[(u8, Status)]) -> Result<(), String> {
    let img = render(title, subject, cells);
    let file = File::create(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut enc = png::Encoder::new(BufWriter::new(file), WIDTH as u32, HEIGHT as u32);
    enc.set_color(png::ColorType::Rgb);
    enc.set_depth(png::BitDepth::Eight);
    let mut w = enc.write_header().map_err(|e| e.to_string())?;
    w.write_image_data(&img).map_err(|e| e.to_string())?;
    w.finish().map_err(|e| e.to_string())
}

fn render(title: &str, subject: &str, cells: &[(u8, Status)]) -> Vec<u8> {
    let mut cv = Canvas::new();
    let mut totals = [0usize; 4];
    let mut tiers: Vec<(u8, Vec<Status>)> = Vec::new();
    for &(t, s) in cells {
        totals[s as usize] += 1;
        match tiers.last_mut() {
            Some((lt, v)) if *lt == t => v.push(s),
            _ => match tiers.iter_mut().find(|(lt, _)| *lt == t) {
                Some((_, v)) => v.push(s),
                None => tiers.push((t, vec![s])),
            },
        }
    }
    tiers.sort_by_key(|(t, _)| *t);

    // Header: title, commit subject, legend with totals.
    cv.text(MARGIN, 18, title, 3, TEXT);
    cv.text(MARGIN, 52, subject, 2, DIM);
    let mut x = MARGIN;
    let legend_y = 78;
    for (label, s) in [("PASS", Status::Pass), ("FAIL", Status::Fail), ("SKIP", Status::Skip), ("PENDING", Status::Pending)] {
        cv.rect(x, legend_y, 14, 14, colour(s));
        x = cv.text(x + 22, legend_y, &format!("{label} {}", totals[s as usize]), 2, TEXT) + 30;
    }
    cv.text(x + 10, legend_y, &format!("TOTAL {}", cells.len()), 2, DIM);
    let header = 110;

    // Largest cell pitch that fits every tier on the canvas.
    const LABEL: usize = 22;
    const SECTION_GAP: usize = 10;
    let avail_w = WIDTH - 2 * MARGIN;
    let pitch = (3..=48)
        .rev()
        .find(|&p| {
            let cols = avail_w / p;
            let h: usize = tiers
                .iter()
                .map(|(_, v)| LABEL + v.len().div_ceil(cols) * p + SECTION_GAP)
                .sum();
            header + h <= HEIGHT - MARGIN / 2
        })
        .unwrap_or(3);
    let cols = avail_w / pitch;
    let gap = (pitch / 6).max(1);

    let mut y = header;
    for (t, statuses) in &tiers {
        let mut c = [0usize; 4];
        for s in statuses {
            c[*s as usize] += 1;
        }
        let name = TIER_NAMES.get(usize::from(*t)).copied().unwrap_or("?").to_uppercase();
        let label = format!(
            "TIER {t} {name}   PASS {}  FAIL {}  SKIP {}  PENDING {}   ({})",
            c[0], c[1], c[2], c[3], statuses.len()
        );
        cv.text(MARGIN, y, &label, 2, TEXT);
        y += LABEL;
        for (i, s) in statuses.iter().enumerate() {
            let (row, col) = (i / cols, i % cols);
            cv.rect(MARGIN + col * pitch, y + row * pitch, pitch - gap, pitch - gap, colour(*s));
        }
        y += statuses.len().div_ceil(cols) * pitch + SECTION_GAP;
    }
    cv.px
}

/// 5x7 glyphs, one byte per row, bit 4 = leftmost column. Lower case is
/// drawn as upper case; unknown characters as '?'.
fn glyph(c: char) -> [u8; 7] {
    match c.to_ascii_uppercase() {
        'A' => [0x0E, 0x11, 0x11, 0x1F, 0x11, 0x11, 0x11],
        'B' => [0x1E, 0x11, 0x11, 0x1E, 0x11, 0x11, 0x1E],
        'C' => [0x0E, 0x11, 0x10, 0x10, 0x10, 0x11, 0x0E],
        'D' => [0x1E, 0x11, 0x11, 0x11, 0x11, 0x11, 0x1E],
        'E' => [0x1F, 0x10, 0x10, 0x1E, 0x10, 0x10, 0x1F],
        'F' => [0x1F, 0x10, 0x10, 0x1E, 0x10, 0x10, 0x10],
        'G' => [0x0E, 0x11, 0x10, 0x17, 0x11, 0x11, 0x0F],
        'H' => [0x11, 0x11, 0x11, 0x1F, 0x11, 0x11, 0x11],
        'I' => [0x0E, 0x04, 0x04, 0x04, 0x04, 0x04, 0x0E],
        'J' => [0x07, 0x02, 0x02, 0x02, 0x02, 0x12, 0x0C],
        'K' => [0x11, 0x12, 0x14, 0x18, 0x14, 0x12, 0x11],
        'L' => [0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0x1F],
        'M' => [0x11, 0x1B, 0x15, 0x15, 0x11, 0x11, 0x11],
        'N' => [0x11, 0x11, 0x19, 0x15, 0x13, 0x11, 0x11],
        'O' => [0x0E, 0x11, 0x11, 0x11, 0x11, 0x11, 0x0E],
        'P' => [0x1E, 0x11, 0x11, 0x1E, 0x10, 0x10, 0x10],
        'Q' => [0x0E, 0x11, 0x11, 0x11, 0x15, 0x12, 0x0D],
        'R' => [0x1E, 0x11, 0x11, 0x1E, 0x14, 0x12, 0x11],
        'S' => [0x0F, 0x10, 0x10, 0x0E, 0x01, 0x01, 0x1E],
        'T' => [0x1F, 0x04, 0x04, 0x04, 0x04, 0x04, 0x04],
        'U' => [0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x0E],
        'V' => [0x11, 0x11, 0x11, 0x11, 0x0A, 0x0A, 0x04],
        'W' => [0x11, 0x11, 0x11, 0x15, 0x15, 0x15, 0x0A],
        'X' => [0x11, 0x11, 0x0A, 0x04, 0x0A, 0x11, 0x11],
        'Y' => [0x11, 0x11, 0x11, 0x0A, 0x04, 0x04, 0x04],
        'Z' => [0x1F, 0x01, 0x02, 0x04, 0x08, 0x10, 0x1F],
        '0' => [0x0E, 0x11, 0x13, 0x15, 0x19, 0x11, 0x0E],
        '1' => [0x04, 0x0C, 0x04, 0x04, 0x04, 0x04, 0x0E],
        '2' => [0x0E, 0x11, 0x01, 0x02, 0x04, 0x08, 0x1F],
        '3' => [0x1F, 0x02, 0x04, 0x02, 0x01, 0x11, 0x0E],
        '4' => [0x02, 0x06, 0x0A, 0x12, 0x1F, 0x02, 0x02],
        '5' => [0x1F, 0x10, 0x1E, 0x01, 0x01, 0x11, 0x0E],
        '6' => [0x06, 0x08, 0x10, 0x1E, 0x11, 0x11, 0x0E],
        '7' => [0x1F, 0x01, 0x02, 0x04, 0x08, 0x08, 0x08],
        '8' => [0x0E, 0x11, 0x11, 0x0E, 0x11, 0x11, 0x0E],
        '9' => [0x0E, 0x11, 0x11, 0x0F, 0x01, 0x02, 0x0C],
        ' ' => [0; 7],
        '.' => [0, 0, 0, 0, 0, 0x0C, 0x0C],
        ',' => [0, 0, 0, 0, 0x0C, 0x04, 0x08],
        ':' => [0, 0x0C, 0x0C, 0, 0x0C, 0x0C, 0],
        ';' => [0, 0x0C, 0x0C, 0, 0x0C, 0x04, 0x08],
        '-' => [0, 0, 0, 0x1F, 0, 0, 0],
        '_' => [0, 0, 0, 0, 0, 0, 0x1F],
        '/' => [0, 0x01, 0x02, 0x04, 0x08, 0x10, 0],
        '\\' => [0, 0x10, 0x08, 0x04, 0x02, 0x01, 0],
        '(' => [0x02, 0x04, 0x08, 0x08, 0x08, 0x04, 0x02],
        ')' => [0x08, 0x04, 0x02, 0x02, 0x02, 0x04, 0x08],
        '[' => [0x0E, 0x08, 0x08, 0x08, 0x08, 0x08, 0x0E],
        ']' => [0x0E, 0x02, 0x02, 0x02, 0x02, 0x02, 0x0E],
        '<' => [0x02, 0x04, 0x08, 0x10, 0x08, 0x04, 0x02],
        '>' => [0x08, 0x04, 0x02, 0x01, 0x02, 0x04, 0x08],
        '%' => [0x18, 0x19, 0x02, 0x04, 0x08, 0x13, 0x03],
        '+' => [0, 0x04, 0x04, 0x1F, 0x04, 0x04, 0],
        '=' => [0, 0, 0x1F, 0, 0x1F, 0, 0],
        '#' => [0x0A, 0x0A, 0x1F, 0x0A, 0x1F, 0x0A, 0x0A],
        '*' => [0, 0x04, 0x15, 0x0E, 0x15, 0x04, 0],
        '&' => [0x0C, 0x12, 0x14, 0x08, 0x15, 0x12, 0x0D],
        '@' => [0x0E, 0x11, 0x17, 0x15, 0x17, 0x10, 0x0F],
        '!' => [0x04, 0x04, 0x04, 0x04, 0x04, 0, 0x04],
        '|' => [0x04; 7],
        '\'' | '`' => [0x0C, 0x04, 0x08, 0, 0, 0, 0],
        '"' => [0x0A, 0x0A, 0, 0, 0, 0, 0],
        _ => [0x0E, 0x11, 0x01, 0x02, 0x04, 0, 0x04],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_large_manifest_fits_the_canvas() {
        // Roughly the real manifest's shape: ~3,300 cells over six tiers.
        let mut cells = Vec::new();
        for (t, n) in [(0u8, 36), (1, 140), (2, 190), (3, 900), (4, 1900), (5, 60)] {
            cells.extend(std::iter::repeat_n((t, Status::Pending), n));
        }
        let px = render("T", "S", &cells);
        assert_eq!(px.len(), WIDTH * HEIGHT * 3);
        // The last pending cell must have been drawn somewhere.
        assert!(px.as_chunks::<3>().0.contains(&colour(Status::Pending)));
    }
}
