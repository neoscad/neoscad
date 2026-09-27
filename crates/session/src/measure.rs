//! `measure`: the numbers of a model and its parts (`neoscad measure`, the
//! server's `measure` method; JSON in `docs/cli-json.md`).
//!
//! - volume, surface area, bounding box and centre of mass (the centroid
//!   of the enclosed volume, assuming uniform density) of the model and of
//!   each part's own solid;
//! - the smallest distance between two parts, and whether they touch or
//!   overlap (overlap by a boolean intersection, distance by an exact
//!   triangle-to-triangle search over bounding volume hierarchies);
//! - a cross-section at an axis plane: area, perimeter, contours and
//!   bounding box, optionally as an SVG outline.

use geom::Geometry;
use geom::manifold_geom::{ManifoldGeometry, OpType};
use geom::polygon2d::Polygon2d;
use serde_json::{Value, json};

use crate::mesh::{Bvh, Mesh};
use crate::parts::{Part, is_within};
use crate::{Cancelled, Log, Run, Session, stats};

/// An axis-aligned cutting plane.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Plane {
    X(f64),
    Y(f64),
    Z(f64),
}

impl Plane {
    /// `z=10`, `x=-2.5`, `y=0`.
    pub fn parse(s: &str) -> Option<Plane> {
        let (axis, v) = s.split_once('=')?;
        let v: f64 = v.trim().parse().ok()?;
        if !v.is_finite() {
            return None;
        }
        match axis.trim() {
            "x" | "X" => Some(Plane::X(v)),
            "y" | "Y" => Some(Plane::Y(v)),
            "z" | "Z" => Some(Plane::Z(v)),
            _ => None,
        }
    }

    pub fn name(&self) -> String {
        let (a, v) = match self {
            Plane::X(v) => ("x", v),
            Plane::Y(v) => ("y", v),
            Plane::Z(v) => ("z", v),
        };
        format!("{a}={}", render::snapshot::number(*v))
    }

    /// A rotation (and shift) taking the plane to z = 0 with the section's
    /// 2D axes as x and y: (x, y) for z, (y, z) for x, (x, z) for y. Each
    /// is a proper rotation, so the solid stays inside out as it was.
    fn to_z0(self) -> eval::node::Matrix {
        match self {
            Plane::Z(h) => [
                [1.0, 0.0, 0.0, 0.0],
                [0.0, 1.0, 0.0, 0.0],
                [0.0, 0.0, 1.0, -h],
                [0.0, 0.0, 0.0, 1.0],
            ],
            Plane::X(h) => [
                [0.0, 1.0, 0.0, 0.0],
                [0.0, 0.0, 1.0, 0.0],
                [1.0, 0.0, 0.0, -h],
                [0.0, 0.0, 0.0, 1.0],
            ],
            Plane::Y(h) => [
                [1.0, 0.0, 0.0, 0.0],
                [0.0, 0.0, 1.0, 0.0],
                [0.0, -1.0, 0.0, h],
                [0.0, 0.0, 0.0, 1.0],
            ],
        }
    }

    /// A section point back in model coordinates.
    pub fn to_model(self, p: [f64; 2]) -> [f64; 3] {
        match self {
            Plane::Z(h) => [p[0], p[1], h],
            Plane::X(h) => [h, p[0], p[1]],
            Plane::Y(h) => [p[0], h, p[1]],
        }
    }

    /// The names of the section's 2D axes.
    pub fn axes(self) -> (&'static str, &'static str) {
        match self {
            Plane::Z(_) => ("x", "y"),
            Plane::X(_) => ("y", "z"),
            Plane::Y(_) => ("x", "z"),
        }
    }
}

/// A `measure` request.
#[derive(Debug, Clone)]
pub struct MeasureRequest {
    pub run: Run,
    /// Report only this part (and the parts nested in it); a section is
    /// taken through its solid rather than the model's.
    pub part: Option<String>,
    /// Two parts to measure the distance between.
    pub between: Option<(String, String)>,
    pub section: Option<Plane>,
    /// Also draw the section as SVG ([`Measured::svg`]).
    pub svg: bool,
}

impl MeasureRequest {
    pub fn new(run: Run) -> MeasureRequest {
        MeasureRequest {
            run,
            part: None,
            between: None,
            section: None,
            svg: false,
        }
    }
}

/// A measurement.
#[derive(Debug)]
pub struct Measured {
    /// 0; 1 when the model failed or a named part does not exist.
    pub exit_code: u8,
    pub summary: Value,
    /// The section's outline as an SVG document, when asked for.
    pub svg: Option<String>,
    pub log: Log,
}

fn r6(x: f64) -> f64 {
    let y = (x * 1e6).round() / 1e6;
    if y == 0.0 { 0.0 } else { y }
}

fn p6(p: [f64; 3]) -> [f64; 3] {
    p.map(r6)
}

fn bbox(lo: [f64; 3], hi: [f64; 3]) -> Value {
    stats::bbox_json(&p6(lo), &p6(hi))
        .as_object()
        .map_or(Value::Null, |o| {
            let mut o = o.clone();
            if let Some(Value::Array(s)) = o.get_mut("size") {
                for x in s.iter_mut() {
                    *x = json!(r6(x.as_f64().unwrap_or(0.0)));
                }
            }
            Value::Object(o)
        })
}

/// Volume, area, box and centre of mass of a solid.
pub fn solid_json(m: &ManifoldGeometry) -> Value {
    let mesh = Mesh::of_solid(m);
    let (vol, area, c) = mesh.mass();
    let b = mesh.bbox();
    json!({
        "volume": r6(vol),
        "area": r6(area),
        "bbox": if b.is_empty() { Value::Null } else { bbox(b.lo, b.hi) },
        "centroid": p6(c),
        "triangles": mesh.tris.len(),
    })
}

/// A cross-section's numbers, and its outlines in the section's 2D axes.
pub fn section(m: &ManifoldGeometry, plane: Plane) -> (Value, Polygon2d) {
    let mut moved = m.clone();
    moved.transform(&plane.to_z0());
    let poly = moved.slice();
    let mut area = 0.0;
    let mut perimeter = 0.0;
    let mut lo = [f64::INFINITY; 3];
    let mut hi = [f64::NEG_INFINITY; 3];
    for o in &poly.outlines {
        let v = &o.vertices;
        for i in 0..v.len() {
            let (a, b) = (v[i], v[(i + 1) % v.len()]);
            area += (a[0] * b[1] - b[0] * a[1]) / 2.0;
            perimeter += ((b[0] - a[0]).powi(2) + (b[1] - a[1]).powi(2)).sqrt();
            let p = plane.to_model(a);
            for k in 0..3 {
                lo[k] = lo[k].min(p[k]);
                hi[k] = hi[k].max(p[k]);
            }
        }
    }
    let (u, v) = plane.axes();
    let empty = poly.outlines.is_empty();
    (
        json!({
            "plane": plane.name(),
            "axes": [u, v],
            "area": r6(area.abs()),
            "perimeter": r6(perimeter),
            "contours": poly.outlines.len(),
            "bbox": if empty { Value::Null } else { bbox(lo, hi) },
        }),
        poly,
    )
}

/// The section as an SVG document in mm, the second axis pointing up.
pub fn section_svg(poly: &Polygon2d, plane: Plane) -> String {
    let (mut lo, mut hi) = ([f64::INFINITY; 2], [f64::NEG_INFINITY; 2]);
    for o in &poly.outlines {
        for p in &o.vertices {
            for k in 0..2 {
                lo[k] = lo[k].min(p[k]);
                hi[k] = hi[k].max(p[k]);
            }
        }
    }
    if poly.outlines.is_empty() {
        lo = [0.0; 2];
        hi = [1.0; 2];
    }
    let pad = ((hi[0] - lo[0]).max(hi[1] - lo[1]) * 0.05).max(0.5);
    let (w, h) = (hi[0] - lo[0] + 2.0 * pad, hi[1] - lo[1] + 2.0 * pad);
    let n = |x: f64| {
        let s = format!("{:.4}", x);
        let s = s.trim_end_matches('0').trim_end_matches('.').to_string();
        if s == "-0" { "0".into() } else { s }
    };
    let mut d = String::new();
    for o in &poly.outlines {
        for (i, p) in o.vertices.iter().enumerate() {
            d.push_str(if i == 0 { "M" } else { "L" });
            d.push_str(&format!(
                "{},{} ",
                n(p[0] - lo[0] + pad),
                n(hi[1] - p[1] + pad)
            ));
        }
        d.push_str("Z ");
    }
    let (u, v) = plane.axes();
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{w}mm\" height=\"{h}mm\" viewBox=\"0 0 {w} {h}\">\n\
         <title>section {plane} ({u} right, {v} up; origin at {u}={x0}, {v}={y0})</title>\n\
         <path d=\"{d}\" fill=\"#d8d4c8\" fill-rule=\"evenodd\" stroke=\"#202020\" stroke-width=\"{sw}\"/>\n\
         </svg>\n",
        w = n(w),
        h = n(h),
        plane = plane.name(),
        x0 = n(lo[0] - pad),
        y0 = n(hi[1] + pad),
        d = d.trim_end(),
        sw = n((w.max(h) / 400.0).max(0.01)),
    )
}

/// Distance, touch and overlap of two solids. Touching is within a
/// micrometre (below that, the kernels' own rounding decides).
pub fn between(a: &ManifoldGeometry, b: &ManifoldGeometry) -> Value {
    const TOUCH: f64 = 1e-3;
    let both = a.boolean(b, OpType::Intersect);
    let overlap = both.manifold.volume();
    let floor = 1e-9_f64.max(1e-9 * a.manifold.volume().min(b.manifold.volume()));
    let (ma, mb) = (Mesh::of_solid(a), Mesh::of_solid(b));
    let closest = Bvh::new(&ma).closest(&ma, &Bvh::new(&mb), &mb);
    if overlap > floor {
        let bx = both.bounds();
        return json!({
            "distance": 0.0,
            "touching": true,
            "overlapping": true,
            "overlap_volume": r6(overlap),
            "overlap_bbox": bx.map(|(lo, hi)| bbox(lo, hi)),
            "points": Value::Null,
        });
    }
    match closest {
        Some((d, p, q)) => json!({
            "distance": r6(d),
            "touching": d <= TOUCH,
            "overlapping": false,
            "overlap_volume": 0.0,
            "overlap_bbox": Value::Null,
            "points": [p6(p), p6(q)],
        }),
        None => Value::Null,
    }
}

impl Session {
    /// Measure a model (see [`MeasureRequest`]).
    pub fn measure(&self, req: &MeasureRequest) -> Result<Measured, Cancelled> {
        let started = self.now();
        let scheme = render::ColorScheme::cornfield();
        let (model, parts) = self.render_parts(&req.run, &scheme)?;
        let diagnostics = crate::diag::summary_json(&model.log.lines, &model.log.names);
        let fail = |log: Log, code: u8, error: Option<String>| Measured {
            exit_code: code,
            summary: json!({
                "schema": 1,
                "input": req.run.input,
                "failed": true,
                "exit_code": code,
                "error": error,
                "diagnostics": diagnostics,
            }),
            svg: None,
            log,
        };
        if model.exit_code != 0 {
            return Ok(fail(model.log, model.exit_code, None));
        }
        let names: Vec<&str> = parts.iter().map(|p| p.name.as_str()).collect();
        let find = |name: &str| -> Result<&Part, String> {
            parts.iter().find(|p| p.name == name).ok_or_else(|| {
                if names.is_empty() {
                    format!("no part '{name}': the model has no parts (they need `--enable part`)")
                } else {
                    format!("no part '{name}' (parts: {})", names.join(", "))
                }
            })
        };
        let t = self.now();
        let mut out = serde_json::Map::new();
        out.insert("schema".into(), json!(1));
        out.insert("input".into(), json!(req.run.input));
        out.insert("exit_code".into(), json!(0));
        let model_json = match &model.geometry {
            None => Value::Null,
            Some(g @ Geometry::Polygon2d(_)) => stats::geometry(g, &scheme.geometry_scheme()),
            Some(g) => {
                let solid = stats::solid(g);
                let mut v = solid_json(&solid);
                let mesh = Mesh::of_solid(&solid);
                v["dimensions"] = json!(3);
                v["components"] = json!(mesh.components().1);
                v["manifold"] = json!(solid.is_valid());
                v
            }
        };
        out.insert("model".into(), model_json);
        // Parts: all of them, or the one asked for and its nested ones.
        let chosen = match &req.part {
            Some(p) => match find(p) {
                Ok(_) => Some(p.as_str()),
                Err(e) => return Ok(fail(model.log, 1, Some(e))),
            },
            None => None,
        };
        let part_json: Vec<Value> = parts
            .iter()
            .filter(|p| chosen.is_none_or(|c| is_within(&p.name, c)))
            .map(|p| {
                let mut v = p
                    .solid
                    .as_ref()
                    .map_or(json!({"dimensions": 2}), solid_json);
                v["name"] = json!(p.name);
                v["instances"] = json!(p.instances);
                v["context"] = json!(p.context);
                v
            })
            .collect();
        out.insert("parts".into(), json!(part_json));
        if let Some((a, b)) = &req.between {
            let (pa, pb) = match (find(a), find(b)) {
                (Ok(x), Ok(y)) => (x, y),
                (Err(e), _) | (_, Err(e)) => return Ok(fail(model.log, 1, Some(e))),
            };
            let v = match (&pa.solid, &pb.solid) {
                (Some(sa), Some(sb)) => {
                    let mut v = between(sa, sb);
                    v["a"] = json!(a);
                    v["b"] = json!(b);
                    v
                }
                _ => json!({"a": a, "b": b, "distance": Value::Null}),
            };
            out.insert("between".into(), v);
        }
        let mut svg = None;
        if let Some(plane) = req.section {
            let solid = match chosen {
                Some(c) => find(c).ok().and_then(|p| p.solid.clone()),
                None => match &model.geometry {
                    Some(Geometry::Polygon2d(_)) | None => None,
                    Some(g) => Some(stats::solid(g)),
                },
            };
            match solid {
                Some(s) => {
                    let (mut v, poly) = section(&s, plane);
                    if let Some(c) = chosen {
                        v["part"] = json!(c);
                    }
                    if req.svg {
                        svg = Some(section_svg(&poly, plane));
                    }
                    out.insert("section".into(), v);
                }
                None => {
                    out.insert("section".into(), Value::Null);
                }
            }
        }
        let round = |ms: f64| (ms * 10.0).round() / 10.0;
        out.insert(
            "timings_ms".into(),
            json!({
                "evaluate": round(model.timings.parse + model.timings.evaluate),
                "geometry": round(model.timings.geometry),
                "measure": round(self.now() - t),
                "total": round(self.now() - started),
            }),
        );
        out.insert("diagnostics".into(), diagnostics);
        Ok(Measured {
            exit_code: 0,
            summary: Value::Object(out),
            svg,
            log: model.log,
        })
    }
}

/// The human-readable report of a measurement.
pub fn text(summary: &Value) -> String {
    let n = |v: &Value| render::snapshot::number(v.as_f64().unwrap_or(0.0));
    let vec = |v: &Value| -> String {
        v.as_array()
            .into_iter()
            .flatten()
            .map(n)
            .collect::<Vec<_>>()
            .join(", ")
    };
    let mut out = String::new();
    let input = summary["input"].as_str().unwrap_or("");
    if summary["failed"] == json!(true) {
        match summary["error"].as_str() {
            Some(e) => out.push_str(&format!("measure {input}: {e}\n")),
            None => out.push_str(&format!("measure {input}: the model did not render\n")),
        }
        return out;
    }
    let solid = |out: &mut String, label: &str, v: &Value| {
        if v["dimensions"] == json!(2) && v.get("volume").is_none() {
            out.push_str(&format!("{label}: 2D"));
            if let Some(a) = v.get("area") {
                out.push_str(&format!(", area {} mm²", n(a)));
            }
            out.push('\n');
            return;
        }
        out.push_str(&format!(
            "{label}: volume {} mm³, area {} mm², size [{}] mm, centre of mass [{}]\n",
            n(&v["volume"]),
            n(&v["area"]),
            vec(&v["bbox"]["size"]),
            vec(&v["centroid"]),
        ));
    };
    if summary["model"].is_null() {
        out.push_str(&format!("{input}: empty\n"));
    } else {
        solid(&mut out, input, &summary["model"]);
    }
    for p in summary["parts"].as_array().into_iter().flatten() {
        let label = format!("  part '{}'", p["name"].as_str().unwrap_or(""));
        solid(&mut out, &label, p);
    }
    if let Some(b) = summary.get("between").filter(|b| !b.is_null()) {
        let (a, c) = (b["a"].as_str().unwrap_or(""), b["b"].as_str().unwrap_or(""));
        if b["overlapping"] == json!(true) {
            out.push_str(&format!(
                "'{a}' and '{c}' overlap by {} mm³\n",
                n(&b["overlap_volume"])
            ));
        } else {
            out.push_str(&format!(
                "'{a}' to '{c}': {} mm{}\n",
                n(&b["distance"]),
                if b["touching"] == json!(true) {
                    " (touching)"
                } else {
                    ""
                }
            ));
        }
    }
    if let Some(s) = summary.get("section") {
        if s.is_null() {
            out.push_str("section: nothing to cut\n");
        } else {
            out.push_str(&format!(
                "section {}: area {} mm², perimeter {} mm, {} contour{}",
                s["plane"].as_str().unwrap_or(""),
                n(&s["area"]),
                n(&s["perimeter"]),
                s["contours"],
                if s["contours"] == json!(1) { "" } else { "s" }
            ));
            if !s["bbox"].is_null() {
                out.push_str(&format!(", size [{}] mm", vec(&s["bbox"]["size"])));
            }
            out.push('\n');
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn planes_parse() {
        assert_eq!(Plane::parse("z=10"), Some(Plane::Z(10.0)));
        assert_eq!(Plane::parse("x = -2.5"), Some(Plane::X(-2.5)));
        assert_eq!(Plane::parse("w=1"), None);
        assert_eq!(Plane::parse("z=nan"), None);
    }
}
