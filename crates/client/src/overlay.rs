//! What the check and measure panels draw over the model in the 3D view:
//! every located finding's numbered marker (the selected one with its
//! box), the section's outline, the closest points between two parts and
//! the picked points (`docs/audits/shared-core.md`, step 4).
//!
//! The macOS app built these lines in Swift and the web demo again in
//! JavaScript, with the finding colours copied from the snapshot's; here
//! they are built once from the panels' state, in the snapshot's own
//! colours ([`session::snapshot::marker_color`]).

use serde::{Deserialize, Serialize};
use session::check::Level;

use crate::{
    BetweenResult, CheckFinding, FindingSeverity, PartStats, SectionAxis, SectionResult, SolidStats,
};

/// A polyline: points flattened (`x0, y0, z0, x1, ...`) in model
/// coordinates, and an RGBA colour (0 to 1 each).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ViewLine {
    pub points: Vec<f64>,
    pub closed: bool,
    pub color: Vec<f32>,
}

/// A marked, labelled point.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ViewMarker {
    pub point: Vec<f64>,
    pub label: String,
    pub color: Vec<f32>,
}

/// The lines and markers to draw.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ViewOverlay {
    pub lines: Vec<ViewLine>,
    pub markers: Vec<ViewMarker>,
}

/// What the panels show, which the overlay draws.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct OverlayState {
    /// The last check's findings, and the one selected.
    pub findings: Vec<CheckFinding>,
    pub selected: Option<u32>,
    /// The section shown, if any.
    pub section: Option<SectionResult>,
    /// The distance between two parts, if measured.
    pub between: Option<BetweenResult>,
    /// Up to two picked surface points.
    pub picks: Vec<Vec<f64>>,
}

/// The section outline's colour.
pub const SECTION_COLOR: [f32; 4] = [0.0, 0.62, 0.85, 1.0];
/// The closest points between two parts.
pub const DISTANCE_COLOR: [f32; 4] = [0.58, 0.40, 0.74, 1.0];
/// Picked points.
pub const PICK_COLOR: [f32; 4] = [0.10, 0.60, 0.20, 1.0];

/// A finding's colour: the snapshot's marker colour, faded when it is not
/// the selected one so the chosen one stands out.
pub fn finding_color(severity: FindingSeverity, selected: bool) -> Vec<f32> {
    let level = match severity {
        FindingSeverity::Error => Level::Error,
        FindingSeverity::Warning => Level::Warning,
        FindingSeverity::Info => Level::Info,
    };
    let [r, g, b] = session::snapshot::marker_color(level);
    vec![
        f32::from(r) / 255.0,
        f32::from(g) / 255.0,
        f32::from(b) / 255.0,
        if selected { 1.0 } else { 0.55 },
    ]
}

/// The twelve edges of a box, as four lines around the bottom and top and
/// four uprights.
pub fn box_lines(lo: &[f64], hi: &[f64], color: &[f32]) -> Vec<ViewLine> {
    if lo.len() != 3 || hi.len() != 3 {
        return Vec::new();
    }
    let p = |i: usize| -> [f64; 3] {
        [
            if i & 1 == 0 { lo[0] } else { hi[0] },
            if i & 2 == 0 { lo[1] } else { hi[1] },
            if i & 4 == 0 { lo[2] } else { hi[2] },
        ]
    };
    let ring = |ids: [usize; 4]| -> Vec<f64> { ids.iter().flat_map(|&i| p(i)).collect() };
    let mut out = vec![
        ViewLine {
            points: ring([0, 1, 3, 2]),
            closed: true,
            color: color.to_vec(),
        },
        ViewLine {
            points: ring([4, 5, 7, 6]),
            closed: true,
            color: color.to_vec(),
        },
    ];
    for i in 0..4 {
        out.push(ViewLine {
            points: p(i).into_iter().chain(p(i + 4)).collect(),
            closed: false,
            color: color.to_vec(),
        });
    }
    out
}

/// The overlay for the panels' state.
pub fn view_overlay(state: &OverlayState) -> ViewOverlay {
    let mut o = ViewOverlay::default();
    for f in &state.findings {
        if f.point.len() != 3 || f.severity == FindingSeverity::Info {
            continue;
        }
        let selected = state.selected == Some(f.id);
        let color = finding_color(f.severity, selected);
        o.markers.push(ViewMarker {
            point: f.point.clone(),
            label: f.id.to_string(),
            color: color.clone(),
        });
        if selected && let (Some(lo), Some(hi)) = (&f.bbox_min, &f.bbox_max) {
            o.lines.extend(box_lines(lo, hi, &color));
        }
    }
    if let Some(s) = &state.section {
        for c in &s.outline {
            o.lines.push(ViewLine {
                points: c.clone(),
                closed: true,
                color: SECTION_COLOR.to_vec(),
            });
        }
    }
    if let Some(b) = &state.between
        && let (Some(pa), Some(pb)) = (&b.point_a, &b.point_b)
    {
        o.lines.push(ViewLine {
            points: pa.iter().chain(pb).copied().collect(),
            closed: false,
            color: DISTANCE_COLOR.to_vec(),
        });
        for (p, label) in [(pa, &b.a), (pb, &b.b)] {
            o.markers.push(ViewMarker {
                point: p.clone(),
                label: label.clone(),
                color: DISTANCE_COLOR.to_vec(),
            });
        }
    }
    for (i, p) in state.picks.iter().take(2).enumerate() {
        o.markers.push(ViewMarker {
            point: p.clone(),
            label: if i == 0 { "A" } else { "B" }.into(),
            color: PICK_COLOR.to_vec(),
        });
    }
    if let [a, b, ..] = state.picks.as_slice() {
        o.lines.push(ViewLine {
            points: a.iter().chain(b).copied().collect(),
            closed: false,
            color: PICK_COLOR.to_vec(),
        });
    }
    o
}

/// The distance between two picked points; `None` without two 3D points.
pub fn pick_distance(picks: &[Vec<f64>]) -> Option<f64> {
    match picks {
        [a, b] if a.len() == 3 && b.len() == 3 => {
            Some((0..3).map(|i| (a[i] - b[i]).powi(2)).sum::<f64>().sqrt())
        }
        _ => None,
    }
}

/// The section slider's range along `axis`: the box of `part` (the whole
/// model without one, or when it is not a solid), `[lo, hi]`; `[0, 1]`
/// without a box, and a millimetre wide when the box is flat, so the
/// slider always has room to move.
pub fn section_range(
    model: Option<&SolidStats>,
    parts: &[PartStats],
    axis: SectionAxis,
    part: Option<&str>,
) -> [f64; 2] {
    let solid = part
        .and_then(|name| parts.iter().find(|p| p.name == name))
        .and_then(|p| p.solid.as_ref())
        .or(model);
    let Some(s) = solid else { return [0.0, 1.0] };
    let i = match axis {
        SectionAxis::X => 0,
        SectionAxis::Y => 1,
        SectionAxis::Z => 2,
    };
    let (Some(lo), Some(hi)) = (s.bbox_min.get(i), s.bbox_max.get(i)) else {
        return [0.0, 1.0];
    };
    if hi > lo { [*lo, *hi] } else { [*lo, lo + 1.0] }
}
