//! OpenSCAD's PNG comparison, ported from `tests/image_compare.py`.
//!
//! `test_cmdline_tool.py` defaults to this comparator
//! (`USE_IMAGE_COMPARE_PY`, `tests/CMakeLists.txt:12`), so a tier 3 image
//! passes exactly when OpenSCAD's own suite would accept it:
//!
//! - per-sample differences (expected − actual) below `PIXEL_TOLERANCE`
//!   (8) are zeroed (`image_compare.py:8,30`);
//! - every overlapping 3×3 block is checked per channel, and it counts as
//!   different only if all nine samples differ with the same sign
//!   (`:35-38`);
//! - the images match only if no such block exists (`perc_diff == 0`,
//!   `:74-76`).
//!
//! The Python version also computes each block's geometric-mean difference
//! and writes a red mask image; only the pass/fail decision matters to the
//! runner, but the block count and median difference are kept because the
//! script prints them and they make failures easy to triage.

use std::fs::File;
use std::path::Path;

/// `PIXEL_TOLERANCE` (`image_compare.py:8`).
const PIXEL_TOLERANCE: f64 = 8.0;

/// A decoded image as Pillow's `np.array(Image.open(p))` sees it: raw
/// samples, `channels` per pixel.
#[derive(Debug)]
struct Samples {
    width: usize,
    height: usize,
    channels: usize,
    data: Vec<u8>,
}

/// The result of comparing two images.
#[derive(Debug, Clone, PartialEq)]
pub struct Comparison {
    /// Blocks (per channel) whose nine samples all differ the same way.
    pub differing: usize,
    /// All blocks (per channel).
    pub total: usize,
    /// Median geometric-mean difference of the differing blocks.
    pub median: f64,
}

impl Comparison {
    pub fn passed(&self) -> bool {
        self.differing == 0
    }

    /// The line `image_compare.py` prints for a failure (`:81`).
    pub fn describe(&self) -> String {
        let perc = 100.0 * self.differing as f64 / self.total.max(1) as f64;
        format!("{perc:0.8}% of 3x3 blocks differ with median block diff: {:0.2}", self.median)
    }
}

fn decode(path: &Path) -> Result<Samples, String> {
    let file = File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    // No transformations: Pillow hands numpy the stored samples, so a
    // palette image would compare indices and a 16-bit image would fail.
    // OpenSCAD writes and commits 8-bit RGB; anything else is reported
    // rather than silently converted.
    let decoder = png::Decoder::new(std::io::BufReader::new(file));
    let mut reader = decoder.read_info().map_err(|e| format!("{}: {e}", path.display()))?;
    let (color, depth) = reader.output_color_type();
    if depth != png::BitDepth::Eight {
        return Err(format!("{}: unsupported bit depth {depth:?}", path.display()));
    }
    let channels = match color {
        png::ColorType::Grayscale | png::ColorType::Indexed => 1,
        png::ColorType::GrayscaleAlpha => 2,
        png::ColorType::Rgb => 3,
        png::ColorType::Rgba => 4,
    };
    let size = reader.output_buffer_size().ok_or_else(|| format!("{}: image too large", path.display()))?;
    let mut data = vec![0; size];
    let info = reader.next_frame(&mut data).map_err(|e| format!("{}: {e}", path.display()))?;
    data.truncate(info.buffer_size());
    Ok(Samples { width: info.width as usize, height: info.height as usize, channels, data })
}

/// Compare `expected` with `actual` as `CompareImageFiles(expected, actual)`
/// does. Images of different shapes are an error, as numpy's broadcasting
/// failure is in the Python script.
pub fn compare_files(expected: &Path, actual: &Path) -> Result<Comparison, String> {
    let a = decode(expected)?;
    let b = decode(actual)?;
    compare(&a, &b)
}

fn compare(a: &Samples, b: &Samples) -> Result<Comparison, String> {
    if (a.width, a.height, a.channels) != (b.width, b.height, b.channels) {
        return Err(format!(
            "image shapes differ: expected {}x{}x{}, got {}x{}x{}",
            a.width, a.height, a.channels, b.width, b.height, b.channels
        ));
    }
    let (w, h, c) = (a.width, a.height, a.channels);
    if w < 3 || h < 3 {
        // numpy's slices are empty, so there are no blocks and the script
        // divides 0 by 0: perc_diff is NaN, which is not 0, so it fails.
        return Ok(Comparison { differing: 1, total: 0, median: f64::NAN });
    }
    // d = a1 - a2 with small differences zeroed (`:28-30`).
    let d: Vec<f64> = a
        .data
        .iter()
        .zip(&b.data)
        .map(|(&x, &y)| {
            let v = f64::from(x) - f64::from(y);
            if v.abs() < PIXEL_TOLERANCE { 0.0 } else { v }
        })
        .collect();
    let at = |y: usize, x: usize, ch: usize| d[(y * w + x) * c + ch];
    let mut diffs = Vec::new();
    for y in 0..h - 2 {
        for x in 0..w - 2 {
            for ch in 0..c {
                let mut sign = 0i32;
                let mut prod = 1.0f64;
                for dy in 0..3 {
                    for dx in 0..3 {
                        let v = at(y + dy, x + dx, ch);
                        sign += v.signum() as i32 * i32::from(v != 0.0);
                        prod *= v;
                    }
                }
                if sign.abs() == 9 {
                    diffs.push(prod.abs().powf(1.0 / 9.0));
                }
            }
        }
    }
    let total = (h - 2) * (w - 2) * c;
    let median = median(&mut diffs);
    Ok(Comparison { differing: diffs.len(), total, median })
}

/// numpy's median: the mean of the two middle values for an even count.
fn median(v: &mut [f64]) -> f64 {
    if v.is_empty() {
        return f64::NAN;
    }
    v.sort_by(f64::total_cmp);
    let n = v.len();
    if n % 2 == 1 { v[n / 2] } else { (v[n / 2 - 1] + v[n / 2]) / 2.0 }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn img(w: usize, h: usize, f: impl Fn(usize, usize) -> u8) -> Samples {
        let mut data = Vec::new();
        for y in 0..h {
            for x in 0..w {
                data.push(f(x, y));
            }
        }
        Samples { width: w, height: h, channels: 1, data }
    }

    #[test]
    fn identical_and_small_differences_pass() {
        let a = img(8, 8, |x, y| (x * 10 + y) as u8);
        assert!(compare(&a, &a).unwrap().passed());
        // Differences of 7 are under the tolerance.
        let b = img(8, 8, |x, y| (x * 10 + y) as u8 + 7);
        assert!(compare(&a, &b).unwrap().passed());
    }

    #[test]
    fn one_consistent_block_fails() {
        let a = img(8, 8, |_, _| 100);
        let b = img(8, 8, |x, y| if (2..5).contains(&x) && (2..5).contains(&y) { 120 } else { 100 });
        let c = compare(&a, &b).unwrap();
        assert_eq!(c.differing, 1);
        assert!((c.median - 20.0).abs() < 1e-9, "{}", c.median);
        assert!(!c.passed());
    }

    #[test]
    fn mixed_signs_or_thin_lines_pass() {
        let a = img(8, 8, |_, _| 100);
        // A 2-pixel-wide stripe never fills a 3x3 block.
        let stripe = img(8, 8, |x, _| if x == 3 || x == 4 { 150 } else { 100 });
        assert!(compare(&a, &stripe).unwrap().passed());
        // A block with one sample differing the other way is not counted.
        let mixed = img(8, 8, |x, y| match (x, y) {
            (3, 3) => 50,
            (2..=4, 2..=4) => 150,
            _ => 100,
        });
        assert!(compare(&a, &mixed).unwrap().passed());
    }

    #[test]
    fn shape_mismatch_is_an_error() {
        assert!(compare(&img(8, 8, |_, _| 0), &img(8, 9, |_, _| 0)).is_err());
    }
}
