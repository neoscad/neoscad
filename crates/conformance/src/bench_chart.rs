//! `conformance bench-chart`: one benchmark result drawn as a 1920x1080 PNG,
//! legible as a video frame.
//!
//! One row group per model (and the cold-start metric): a horizontal bar
//! per reference on a shared logarithmic time axis. A run that timed out
//! is drawn hatched to the axis end, labelled `> TIMEOUT`; a failed run
//! gets `FAILED` and no bar. The right column shows neoscad's time and its
//! speedup over the nightly (Manifold backend). The footer gives the geometric-mean speedup of
//! neoscad against each reference, which leaves timeouts out, as the chart
//! says. It draws on the progress grid's canvas and bitmap font
//! (`grid.rs`), so it needs no plotting library.

use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::ctx::Ctx;
use crate::grid::{self, Canvas, DIM, HEIGHT, MARGIN, Rgb, TEXT, WIDTH};

/// Reference order and colours.
const REFS: &[(&str, &str, Rgb)] = &[
    ("neoscad", "NEOSCAD", [46, 204, 113]),
    ("nightly-manifold", "NIGHTLY MANIFOLD", [52, 152, 219]),
    ("nightly-cgal", "NIGHTLY CGAL", [230, 126, 34]),
    ("openscad-2021.01", "OPENSCAD 2021.01", [155, 89, 182]),
];
const RED: Rgb = [231, 76, 60];
const GRIDLINE: Rgb = [48, 48, 56];

/// Horizontal layout: model labels, bars, timeout labels, right column.
const LABEL_X: usize = MARGIN;
const BAR_X0: usize = 330;
const BAR_X1: usize = 1500;
const RIGHT_X: usize = 1650;

/// A bar's value: seconds, a timeout, a failure, or no run.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Cell {
    Time(f64),
    Timeout,
    Failed,
    Missing,
}

fn cell(v: &Value) -> Cell {
    if v.is_null() {
        return Cell::Missing;
    }
    if v["rc"] == "timeout" {
        return Cell::Timeout;
    }
    match v["best_s"].as_f64() {
        Some(t) => Cell::Time(t),
        None => Cell::Failed,
    }
}

/// A short label for a model id.
fn label(id: &str) -> String {
    id.replace("__", " ").replace('_', " ").to_uppercase()
}

fn fmt_time(t: f64) -> String {
    if t >= 10.0 {
        format!("{t:.1}S")
    } else if t >= 1.0 {
        format!("{t:.2}S")
    } else {
        format!("{t:.3}S")
    }
}

/// Render `doc` (a `conformance bench` result) to PNG pixels.
pub fn render(doc: &Value) -> Vec<u8> {
    let mut cv = Canvas::new();
    let refs: Vec<(&str, &str, Rgb)> = REFS
        .iter()
        .copied()
        .filter(|(id, _, _)| doc["binaries"].get(*id).is_some())
        .collect();

    // Header: title, subject, legend.
    let title = format!(
        "NEOSCAD BENCHMARK  {}  {}{}{}",
        doc["timestamp"].as_str().unwrap_or(""),
        doc["short_sha"].as_str().unwrap_or(""),
        if doc["dirty"].as_bool() == Some(true) {
            "-DIRTY"
        } else {
            ""
        },
        if doc["quick"].as_bool() == Some(true) {
            "  QUICK"
        } else {
            ""
        }
    );
    cv.text(MARGIN, 18, &title, 3, TEXT);
    cv.text(MARGIN, 52, doc["subject"].as_str().unwrap_or(""), 2, DIM);
    let mut x = MARGIN;
    for (_, name, c) in &refs {
        cv.rect(x, 80, 14, 14, *c);
        x = cv.text(x + 22, 80, name, 2, TEXT) + 30;
    }
    hatch(&mut cv, x, 80, 40, 14, DIM);
    x = cv.text(x + 48, 80, "TIMEOUT", 2, TEXT) + 30;
    cv.text(x, 80, "! MESH DIFFERS FROM NEOSCAD", 2, RED);

    // Rows: the models, then cold start.
    let mut rows: Vec<(String, &Value, bool)> = Vec::new();
    if let Some(models) = doc["models"].as_object() {
        for (id, m) in models {
            rows.push((label(id), &m["results"], m.get("mesh_flags").is_some()));
        }
    }
    if let Some(c) = doc["extra"].get("cold_start") {
        rows.push(("COLD START".into(), &c["results"], false));
    }

    // Time axis: whole decades from below the fastest run to the timeout.
    let timeout = timeout_s(doc);
    let fastest = rows
        .iter()
        .flat_map(|(_, r, _)| refs.iter().map(move |(id, _, _)| cell(&r[*id])))
        .filter_map(|c| match c {
            Cell::Time(t) if t > 0.0 => Some(t),
            _ => None,
        })
        .fold(timeout, f64::min);
    let lo = 10f64.powf(fastest.log10().floor());
    let hi = timeout.max(lo * 10.0);
    let xpos = |t: f64| {
        let f = (t.max(lo).ln() - lo.ln()) / (hi.ln() - lo.ln());
        BAR_X0 + ((BAR_X1 - BAR_X0) as f64 * f.clamp(0.0, 1.0)).round() as usize
    };

    let top = 124;
    let footer_h = 30 + 26 * refs.len().saturating_sub(1).max(1);
    let bottom = HEIGHT - MARGIN - footer_h;
    let n_bars = refs.len().max(1);
    let group = ((bottom - top) / rows.len().max(1)).max(n_bars * 4 + 4);
    // Thick enough to read, never so thick that a short run looks heavy.
    let bar_pitch = ((group - 6) / n_bars).clamp(4, 26);
    let bar_h = bar_pitch.saturating_sub(2).max(3);

    // Gridlines and their labels, at every decade and at the timeout.
    let mut marks: Vec<(f64, String)> = Vec::new();
    let mut d = lo;
    while d <= hi * 1.0001 {
        let l = if d < 1.0 {
            format!("{d}S")
        } else {
            format!("{}S", d as u64)
        };
        marks.push((d, l));
        d *= 10.0;
    }
    let timeout_label = format!("{}S TIMEOUT", hi as u64);
    let timeout_w = timeout_label.len() * 12;
    marks.push((hi, timeout_label));
    for (t, l) in &marks {
        let gx = xpos(*t);
        cv.rect(gx, top, 1, bottom - top, GRIDLINE);
        let w = l.len() * 12;
        let lx = gx
            .saturating_sub(w / 2)
            .clamp(BAR_X0 - 20, WIDTH - MARGIN - w);
        // A decade label that would run into the timeout label is left out.
        if *t == hi || xpos(hi).saturating_sub(gx) > (w + timeout_w) / 2 + 12 {
            cv.text(lx, top - 20, l, 2, DIM);
        }
    }
    cv.text(RIGHT_X, top - 20, "NEOSCAD", 2, DIM);
    cv.text(RIGHT_X + 120, top - 20, "VS NIGHTLY", 2, DIM);

    for (i, (name, res, flagged)) in rows.iter().enumerate() {
        let y0 = top + i * group + 3;
        let mid = y0 + (n_bars * bar_pitch) / 2;
        if i % 2 == 1 {
            // Faint banding keeps a row's bars together.
            cv.rect(MARGIN, y0 - 3, WIDTH - 2 * MARGIN, group, [28, 28, 33]);
            for (t, _) in &marks {
                cv.rect(xpos(*t), y0 - 3, 1, group, GRIDLINE);
            }
        }
        let name_end = cv.text(LABEL_X, mid - 7, name, 2, TEXT);
        if *flagged {
            cv.text(name_end + 6, mid - 7, "!", 2, RED);
        }
        let mut clipped: Vec<usize> = Vec::new();
        for (k, (id, _, colour)) in refs.iter().enumerate() {
            let y = y0 + k * bar_pitch;
            match cell(&res[*id]) {
                Cell::Time(t) => {
                    let w = xpos(t).saturating_sub(BAR_X0).max(2);
                    cv.rect(BAR_X0, y, w, bar_h, *colour);
                }
                Cell::Timeout => {
                    hatch(&mut cv, BAR_X0, y, BAR_X1 - BAR_X0, bar_h, *colour);
                    // The clip: a gap and a stub past the axis end.
                    cv.rect(BAR_X1 + 4, y, 10, bar_h, *colour);
                    clipped.push(y);
                }
                Cell::Failed => {
                    cv.text(BAR_X0 + 4, y + bar_h / 2 - 3, "FAILED", 1, RED);
                }
                Cell::Missing => {}
            }
        }
        // One label for the group's clipped bars, centred on them, so
        // stacked timeouts do not print over each other.
        if let (Some(first), Some(last)) = (clipped.first(), clipped.last()) {
            let cy = (first + last + bar_h) / 2;
            cv.text(BAR_X1 + 20, cy.saturating_sub(7), "> TIMEOUT", 2, TEXT);
        }
        let neo = cell(&res["neoscad"]);
        let right = match neo {
            Cell::Time(t) => fmt_time(t),
            Cell::Timeout => "> TIMEOUT".into(),
            Cell::Failed => "FAILED".into(),
            Cell::Missing => "-".into(),
        };
        cv.text(RIGHT_X, mid - 7, &right, 2, TEXT);
        let ratio = match (neo, cell(&res["nightly-manifold"])) {
            (Cell::Time(a), Cell::Time(b)) if a > 0.0 => format!("{:.2}X", b / a),
            (Cell::Time(_), Cell::Timeout) => "> LIMIT".into(),
            _ => "-".into(),
        };
        cv.text(RIGHT_X + 120, mid - 7, &ratio, 2, TEXT);
    }

    // Footer: geometric means (computed here for a file that predates them).
    let geomeans = match doc.get("geomean_speedup") {
        Some(g) if g.is_object() => g.clone(),
        _ => crate::bench::geomeans(doc),
    };
    let mut y = bottom + 10;
    cv.text(
        MARGIN,
        y,
        "GEOMETRIC MEAN SPEEDUP OF NEOSCAD (REFERENCE TIME / NEOSCAD TIME), TIMEOUTS EXCLUDED:",
        2,
        TEXT,
    );
    y += 26;
    let mut x = MARGIN;
    for (id, name, colour) in refs.iter().filter(|(id, _, _)| *id != "neoscad") {
        let g = &geomeans[*id];
        let excluded = g["excluded_timeouts_or_failures"]
            .as_array()
            .map_or(0, Vec::len);
        let value = g["value"]
            .as_f64()
            .map_or("-".to_string(), |v| format!("{v:.2}X"));
        let text = if excluded > 0 {
            format!(
                "{name} {value} ({} MODELS, {excluded} EXCLUDED)",
                g["models"]
            )
        } else {
            format!("{name} {value} ({} MODELS)", g["models"])
        };
        cv.rect(x, y, 14, 14, *colour);
        x = cv.text(x + 22, y, &text, 2, TEXT) + 40;
        if x > WIDTH - 600 {
            x = MARGIN;
            y += 26;
        }
    }
    cv.px
}

/// Diagonal stripes of `c` over a darker `c`: a timed-out bar.
fn hatch(cv: &mut Canvas, x: usize, y: usize, w: usize, h: usize, c: Rgb) {
    let dark = c.map(|v| v / 3);
    cv.rect(x, y, w, h, dark);
    for dx in 0..w {
        for dy in 0..h {
            if (dx + dy) % 8 < 3 {
                cv.rect(x + dx, y + dy, 1, 1, c);
            }
        }
    }
}

/// The run's timeout in seconds: recorded, or from the method text of a
/// file written before it was (the audit's), else 300.
fn timeout_s(doc: &Value) -> f64 {
    if let Some(t) = doc["timeout_s"].as_f64() {
        return t;
    }
    let m = doc["method"].as_str().unwrap_or("");
    m.split(" s timeout")
        .next()
        .and_then(|s| s.rsplit(' ').next())
        .and_then(|s| s.parse().ok())
        .unwrap_or(300.0)
}

pub fn command(
    ctx: &Ctx,
    file: Option<&Path>,
    latest: bool,
    out: Option<&Path>,
) -> Result<u8, String> {
    let path: PathBuf = match (file, latest) {
        (Some(f), false) => {
            if f.is_file() {
                f.to_path_buf()
            } else {
                ctx.progress_dir().join("bench").join(f)
            }
        }
        (None, _) | (Some(_), true) => crate::bench::latest(ctx)?,
    };
    let doc: Value = serde_json::from_str(
        &std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?,
    )
    .map_err(|e| format!("{}: {e}", path.display()))?;
    let out = out
        .map(Path::to_path_buf)
        .unwrap_or_else(|| path.with_extension("png"));
    grid::save_png(&out, &render(&doc))?;
    println!("wrote {} ({WIDTH}x{HEIGHT})", out.display());
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn renders_timeouts_failures_and_missing_runs() {
        let doc = json!({
            "timestamp": "2026-09-26T00:00:00Z", "short_sha": "abc1234", "dirty": true,
            "subject": "test", "method": "best of 3; 300 s timeout per run.",
            "binaries": {"neoscad": {}, "nightly-manifold": {}, "nightly-cgal": {}},
            "models": {
                "a": {"results": {
                    "neoscad": {"rc": 0, "best_s": 0.01},
                    "nightly-manifold": {"rc": 0, "best_s": 0.1},
                    "nightly-cgal": {"rc": "timeout", "best_s": null}}},
                "b": {"results": {
                    "neoscad": {"rc": 0, "best_s": 12.0},
                    "nightly-manifold": {"rc": 1, "best_s": null}},
                    "mesh_flags": ["nightly-cgal: volume"]},
            },
            "extra": {"cold_start": {"results": {"neoscad": {"rc": 0, "best_s": 0.003}}}},
            "geomean_speedup": {"nightly-manifold": {"value": 10.0, "models": 1, "excluded_timeouts_or_failures": ["b"]}},
        });
        let px = render(&doc);
        assert_eq!(px.len(), WIDTH * HEIGHT * 3);
        assert_eq!(timeout_s(&doc), 300.0);
    }
}
