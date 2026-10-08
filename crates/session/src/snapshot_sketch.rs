//! `snapshot --sketch NAME` (`docs/language-extensions.md`, section 4.8):
//! a constrained sketch drawn flat in its own plane, from the run's sketch
//! facts (`crate::sketches`): the solved profile filled, every entity as a
//! line (construction ones dashed), its points, the names it has in the
//! source, and a glyph per constraint (`H`, `V`, `||`, dimensions...) in
//! the colour of its state. Entities the constraints leave free to move
//! are orange and those in a conflict red, so the sheet shows where a
//! sketch needs another constraint, as the `sketch-underconstrained` and
//! `sketch-conflict` messages say in words.
//!
//! The overlay is drawn on the CPU over the panels
//! (`render::snapshot::SketchOverlay`), so the same sketch gives the same
//! pixels.

use std::sync::Arc;

use geom::color::Color;
use geom::polygon2d::{Outline, Polygon2d};
use render::scene::{DrawState, Surface};
use render::snapshot::{Label, SketchOverlay, Stroke, number};
use render::{ColorScheme, Scene};
use serde_json::{Value, json};

const PROFILE: [u8; 3] = [30, 70, 160];
const CONSTRUCTION: [u8; 3] = [125, 125, 120];
const FREE: [u8; 3] = [225, 115, 0];
const CONFLICT: [u8; 3] = [200, 30, 30];
const REDUNDANT: [u8; 3] = [190, 140, 0];
const SATISFIED: [u8; 3] = [25, 115, 60];
const NAME: [u8; 3] = [40, 40, 40];
/// The solved profile's fill: pale, so every line over it reads.
const FILL: Color = Color([0.78, 0.85, 0.95, 1.0]);

/// The key to the glyphs, a header line.
pub const KEY: &str = "H/V horiz./vert., || parallel, _|_ perp., T tangent, = equal, F fix, \
                       o coincident; L length, R/D radius/diam., d distance, < angle, r/c fillet/chamfer";

/// What the sheet shows for a sketch.
pub struct SketchSheet {
    pub scene: Scene,
    pub overlay: SketchOverlay,
    pub header: Vec<String>,
    pub legend: Vec<([f32; 4], String)>,
    /// The summary's `sketch` object.
    pub summary: Value,
}

impl std::fmt::Debug for SketchSheet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SketchSheet")
            .field("overlay", &self.overlay)
            .field("header", &self.header)
            .field("summary", &self.summary)
            .finish_non_exhaustive()
    }
}

fn rgba(c: [u8; 3]) -> [f32; 4] {
    [
        f32::from(c[0]) / 255.0,
        f32::from(c[1]) / 255.0,
        f32::from(c[2]) / 255.0,
        1.0,
    ]
}

fn pt(v: &Value) -> Option<[f64; 2]> {
    Some([v.get(0)?.as_f64()?, v.get(1)?.as_f64()?])
}

fn at3(p: [f64; 2]) -> [f64; 3] {
    [p[0], p[1], 0.0]
}

/// The points along an arc around `c` from `s`, `sweep` degrees in its
/// direction; a whole circle for a sweep of 360. For the picture only.
fn arc_points(c: [f64; 2], s: [f64; 2], sweep: f64, cw: bool) -> Vec<[f64; 3]> {
    let (dx, dy) = (s[0] - c[0], s[1] - c[1]);
    let r = (dx * dx + dy * dy).sqrt();
    let a0 = eval::trig::atan2_degrees(dy, dx);
    let n = ((sweep.abs() / 360.0 * 72.0).ceil() as usize).max(8);
    (0..=n)
        .map(|i| {
            let t = sweep * i as f64 / n as f64;
            let a = if cw { a0 - t } else { a0 + t };
            at3([
                c[0] + r * eval::trig::cos_degrees(a),
                c[1] + r * eval::trig::sin_degrees(a),
            ])
        })
        .collect()
}

/// Where an entity's glyphs and name go: a point itself, a line's
/// middle, the middle of an arc's sweep, the top of a circle.
fn anchor(e: &Value) -> Option<[f64; 2]> {
    match e["kind"].as_str()? {
        "point" => pt(&e["at"]),
        "line" => {
            let (a, b) = (pt(&e["start"])?, pt(&e["end"])?);
            Some([(a[0] + b[0]) / 2.0, (a[1] + b[1]) / 2.0])
        }
        "arc" => {
            let c = pt(&e["center"])?;
            let s = pt(&e["start"])?;
            let sweep = e["sweep"].as_f64()?;
            let cw = e["cw"] == json!(true);
            let p = arc_points(c, s, sweep / 2.0, cw);
            p.last().map(|q| [q[0], q[1]])
        }
        "circle" => {
            let c = pt(&e["center"])?;
            Some([c[0], c[1] + e["radius"].as_f64()?])
        }
        _ => None,
    }
}

/// A constraint's glyph: a letter for a geometric one, the value for a
/// dimension.
fn glyph(c: &Value) -> String {
    let v = c["value"].as_f64().map(number).unwrap_or_default();
    match c["kind"].as_str().unwrap_or("") {
        "horizontal" => "H".into(),
        "vertical" => "V".into(),
        "parallel" => "||".into(),
        "perpendicular" => "_|_".into(),
        "tangent" => "T".into(),
        "equal" => "=".into(),
        "fix" => "F".into(),
        "coincident" => "o".into(),
        "on" => "on".into(),
        "midpoint" => "M".into(),
        "symmetric" => "S".into(),
        "length" => format!("L {v}"),
        "radius" => format!("R {v}"),
        "diameter" => format!("D {v}"),
        "distance" => format!("d {v}"),
        "angle" => format!("< {v}"),
        "fillet" => format!("r {v}"),
        "chamfer" => format!("c {v}"),
        k => k.to_string(),
    }
}

/// The sheet for sketch `s` (one of [`crate::sketches::collect`]'s).
pub fn sheet(s: &Value, scheme: &ColorScheme) -> Result<SketchSheet, String> {
    let ents: Vec<&Value> = s["entities"].as_array().into_iter().flatten().collect();
    let cons: Vec<&Value> = s["constraints"].as_array().into_iter().flatten().collect();
    let by_id = |id: u64| ents.iter().copied().find(|e| e["id"].as_u64() == Some(id));
    if !ents.iter().any(|e| anchor(e).is_some()) {
        return Err(format!(
            "{} has no solved geometry to draw",
            crate::sketches::line_text(s)
        ));
    }
    // Entities of a conflicting (or unmet) constraint are red; free ones
    // orange.
    let mut conflicted: Vec<u64> = Vec::new();
    for c in &cons {
        if matches!(c["status"].as_str(), Some("conflicting" | "unmet")) {
            conflicted.extend(
                c["entities"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_u64),
            );
        }
    }
    let color_of = |e: &Value| -> [u8; 3] {
        let id = e["id"].as_u64().unwrap_or(0);
        if conflicted.contains(&id) {
            CONFLICT
        } else if e["free"] == json!(true) {
            FREE
        } else if e["construction"] == json!(true) {
            CONSTRUCTION
        } else {
            PROFILE
        }
    };
    let mut o = SketchOverlay::default();
    let mut lo = [f64::INFINITY; 2];
    let mut hi = [f64::NEG_INFINITY; 2];
    let mut grow = |p: [f64; 2]| {
        for k in 0..2 {
            lo[k] = lo[k].min(p[k]);
            hi[k] = hi[k].max(p[k]);
        }
    };
    for e in &ents {
        let color = color_of(e);
        let construction = e["construction"] == json!(true);
        let points: Vec<[f64; 3]> = match e["kind"].as_str() {
            Some("line") => match (pt(&e["start"]), pt(&e["end"])) {
                (Some(a), Some(b)) => vec![at3(a), at3(b)],
                _ => continue,
            },
            Some("arc") => match (pt(&e["center"]), pt(&e["start"]), e["sweep"].as_f64()) {
                (Some(c), Some(st), Some(sw)) => arc_points(c, st, sw, e["cw"] == json!(true)),
                _ => continue,
            },
            Some("circle") => match (pt(&e["center"]), e["radius"].as_f64()) {
                (Some(c), Some(r)) => arc_points(c, [c[0] + r, c[1]], 360.0, false),
                _ => continue,
            },
            Some("point") => {
                if let Some(p) = pt(&e["at"]) {
                    grow(p);
                    o.dots.push((at3(p), color));
                }
                continue;
            }
            _ => continue,
        };
        for p in &points {
            grow([p[0], p[1]]);
        }
        o.strokes.push(Stroke {
            points,
            color,
            dashed: construction,
            bold: !construction,
        });
    }
    // Names: the variables the source gives, not a member's (`top.start`
    // would sit on top of `top`'s end).
    for e in &ents {
        let Some(name) = e["name"].as_str().filter(|n| !n.contains('.')) else {
            continue;
        };
        if let Some(p) = anchor(e) {
            o.labels.push(Label {
                point: at3(p),
                text: name.to_string(),
                color: NAME,
                boxed: false,
            });
        }
    }
    let mut counts = serde_json::Map::new();
    for c in &cons {
        let status = c["status"].as_str().unwrap_or("unknown");
        let n = counts.get(status).and_then(Value::as_u64).unwrap_or(0);
        counts.insert(status.to_string(), json!(n + 1));
        let color = match status {
            "conflicting" | "unmet" => CONFLICT,
            "redundant" => REDUNDANT,
            _ => SATISFIED,
        };
        let ids: Vec<u64> = c["entities"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_u64)
            .collect();
        let text = glyph(c);
        let anchors: Vec<[f64; 2]> = ids
            .iter()
            .filter_map(|&i| by_id(i).and_then(anchor))
            .collect();
        let places: Vec<[f64; 2]> = match c["kind"].as_str().unwrap_or("") {
            // One glyph on each entity it relates.
            "horizontal" | "vertical" | "parallel" | "perpendicular" | "equal" | "tangent"
            | "fix" | "symmetric" => anchors,
            // Between the two things measured.
            "distance" if anchors.len() == 2 => vec![[
                (anchors[0][0] + anchors[1][0]) / 2.0,
                (anchors[0][1] + anchors[1][1]) / 2.0,
            ]],
            _ => anchors.into_iter().take(1).collect(),
        };
        for p in places {
            o.labels.push(Label {
                point: at3(p),
                text: text.clone(),
                color,
                boxed: true,
            });
        }
    }
    // The profile, filled, as the sketch's node holds it.
    let outlines: Vec<Outline> = s["profile"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|l| Outline::new(l.as_array().into_iter().flatten().filter_map(pt).collect()))
        .filter(|o: &Outline| o.vertices.len() >= 3)
        .collect();
    for ol in &outlines {
        for p in &ol.vertices {
            grow(*p);
        }
    }
    // The profile filled in a pale colour without the 2D outline a render
    // draws: the entities' own lines are the edges, in the colours that
    // say their state.
    let mut scene = Scene::empty(scheme, Some(([lo[0], lo[1], 0.0], [hi[0], hi[1], 0.0])));
    if !outlines.is_empty() {
        let poly = geom::clipper::sanitize(&Polygon2d {
            outlines,
            sanitized: false,
        });
        scene.push(Surface {
            mesh: Arc::new(poly.tessellate()),
            matrix: None,
            color: FILL,
            force_color: true,
            lit: false,
            state: DrawState::DEFAULT,
        });
    }
    let free: Vec<&str> = ents
        .iter()
        .filter(|e| e["free"] == json!(true))
        .filter_map(|e| e["name"].as_str())
        .collect();
    let mut legend = vec![
        (rgba(PROFILE), "profile".to_string()),
        (rgba(CONSTRUCTION), "construction".to_string()),
    ];
    if !free.is_empty() {
        legend.push((rgba(FREE), "free to move".to_string()));
    }
    if counts.contains_key("conflicting") || counts.contains_key("unmet") {
        legend.push((rgba(CONFLICT), "conflict".to_string()));
    }
    if counts.contains_key("redundant") {
        legend.push((rgba(REDUNDANT), "redundant".to_string()));
    }
    let header = vec![
        format!("{}, drawn in its own plane", crate::sketches::line_text(s)),
        KEY.to_string(),
    ];
    let summary = json!({
        "name": s["name"],
        "status": s["status"],
        "dof": s["dof"],
        "entities": ents.len(),
        "constraints": Value::Object(counts),
        "free": free,
    });
    Ok(SketchSheet {
        scene,
        overlay: o,
        header,
        legend,
        summary,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glyphs_name_constraints_and_dimensions() {
        assert_eq!(glyph(&json!({"kind": "horizontal"})), "H");
        assert_eq!(glyph(&json!({"kind": "length", "value": 30.0})), "L 30");
        assert_eq!(glyph(&json!({"kind": "angle", "value": 12.5})), "< 12.5");
    }

    #[test]
    fn an_arc_anchor_is_the_middle_of_its_sweep() {
        let e = json!({"kind": "arc", "center": [0.0, 0.0], "start": [1.0, 0.0], "end": [-1.0, 0.0], "sweep": 180.0});
        let a = anchor(&e).unwrap();
        assert!(a[0].abs() < 1e-12 && (a[1] - 1.0).abs() < 1e-12, "{a:?}");
    }
}
