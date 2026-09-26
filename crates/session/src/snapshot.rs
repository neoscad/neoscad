//! Snapshots: one contact-sheet PNG of a model for agents
//! (docs/architecture.md, "Agent surface"), with a JSON summary of it
//! (`docs/cli-json.md`, "snapshot"). `neoscad snapshot` and the server's
//! `snapshot` method both come here.
//!
//! The sheet shows the standard views (iso, front, top and right by
//! default; only top for a 2D model) at one shared scale, each with a
//! grid, an axis cross and a caption; `dims` adds the bounding box's size.
//! The model is drawn from its rendered geometry by default, so the
//! picture and the numbers describe the same solid; `preview` draws
//! OpenSCAD's preview instead (fast, and it shows `%` and `#` objects).
//! Panels are lit by a headlight by default ([`render::Lighting`]), so a
//! face that slopes away from OpenSCAD's fixed light is still legible.
//!
//! `diff` compares against another version with real booleans on both
//! rendered solids: MODEL - OTHER is what was added (green), OTHER - MODEL
//! what was removed (red, translucent), and their intersection what stayed
//! (grey). Both versions render through one geometry cache, so the
//! subtrees they share are computed once.

use std::sync::Arc;

use geom::Geometry;
use geom::color::Color;
use geom::manifold_geom::{ManifoldGeometry, OpType};
use render::scene::{Cull, Depth, DrawState, Surface};
use render::snapshot::{Sheet, View, number};
use render::{ColorScheme, Scene};
use serde_json::{Value, json};

use crate::{Cancelled, Log, Mode, Rendered, Run, Session, stats};

/// What to draw.
#[derive(Debug, Clone)]
pub struct SnapshotRequest {
    pub run: Run,
    /// The PNG's name, as the summary reports it (the host writes it).
    pub output: String,
    /// View names (iso, front, back, left, right, top, bottom); empty for
    /// the default set.
    pub views: Vec<String>,
    /// The whole sheet in pixels.
    pub size: (u32, u32),
    pub dims: bool,
    pub preview: bool,
    /// Another version of the model (as named, like [`Run::input`]).
    pub diff: Option<String>,
    pub lighting: render::Lighting,
}

impl SnapshotRequest {
    pub fn new(run: Run, output: impl Into<String>) -> SnapshotRequest {
        SnapshotRequest {
            run,
            output: output.into(),
            views: Vec::new(),
            size: (1024, 1024),
            dims: false,
            preview: false,
            diff: None,
            lighting: render::Lighting::Headlight,
        }
    }
}

/// A snapshot.
#[derive(Debug)]
pub struct Snapshot {
    /// 0, or the exit code of a model that failed to load or evaluate (the
    /// summary then says `failed`, and there is no sheet).
    pub exit_code: u8,
    pub png: Option<Vec<u8>>,
    /// The JSON summary (`docs/cli-json.md`).
    pub summary: Value,
    pub log: Log,
}

/// Why no snapshot was made.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SnapshotError {
    Cancelled,
    /// A bad request or no GPU: the message says which.
    Failed(String),
}

impl From<Cancelled> for SnapshotError {
    fn from(_: Cancelled) -> Self {
        SnapshotError::Cancelled
    }
}

impl std::fmt::Display for SnapshotError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SnapshotError::Cancelled => Cancelled.fmt(f),
            SnapshotError::Failed(m) => f.write_str(m),
        }
    }
}

/// Colours of a diff.
const ADDED: Color = Color([0.25, 0.72, 0.30, 1.0]);
const REMOVED: Color = Color([0.90, 0.22, 0.20, 0.55]);
const UNCHANGED: Color = Color([0.78, 0.78, 0.74, 1.0]);

/// `WxH`, 64 to 8192 each.
pub fn parse_size(s: &str) -> Option<(u32, u32)> {
    let (w, h) = s.split_once(['x', 'X', ','])?;
    let (w, h) = (w.trim().parse::<u32>().ok()?, h.trim().parse::<u32>().ok()?);
    (w >= 64 && h >= 64 && w <= 8192 && h <= 8192).then_some((w, h))
}

fn round(ms: f64) -> f64 {
    (ms * 10.0).round() / 10.0
}

/// Two logs as one, in order.
fn join(mut a: Log, b: Log) -> Log {
    a.stderr.extend(b.stderr);
    a.lines.extend(b.lines);
    a
}

impl Session {
    /// Draw a contact sheet of `req.run`'s model.
    pub fn snapshot(&self, req: &SnapshotRequest) -> Result<Snapshot, SnapshotError> {
        let started = self.now();
        let (width, height) = req.size;
        if !(64..=8192).contains(&width) || !(64..=8192).contains(&height) {
            return Err(SnapshotError::Failed(format!(
                "the size must be 64 to 8192 pixels each way (got {width}x{height})"
            )));
        }
        let requested: Option<Vec<View>> = if req.views.is_empty() {
            None
        } else {
            Some(
                req.views
                    .iter()
                    .map(|v| {
                        View::parse(v.trim()).ok_or_else(|| {
                            let names: Vec<&str> = View::ALL.iter().map(|v| v.name()).collect();
                            SnapshotError::Failed(format!(
                                "unknown view '{v}' (one of {})",
                                names.join(", ")
                            ))
                        })
                    })
                    .collect::<Result<_, _>>()?,
            )
        };
        let scheme = ColorScheme::cornfield();
        let geom_scheme = scheme.geometry_scheme();
        let mode = if req.preview {
            Mode::Preview
        } else {
            Mode::Render
        };
        let model = self.render(&req.run, mode, &scheme)?;
        // A model that fails to load or evaluate still gets its summary,
        // with the diagnostics that say why: that is what an agent needs.
        let failed = |log: Log, code: u8| {
            let summary = json!({
                "schema": 1,
                "input": req.run.input,
                "failed": true,
                "exit_code": code,
                "diagnostics": crate::diag::summary_json(&log.lines, &log.names),
            });
            Ok(Snapshot {
                exit_code: code,
                png: None,
                summary,
                log,
            })
        };
        if model.exit_code != 0 {
            return failed(model.log, model.exit_code);
        }
        let evaluate_ms = model.timings.parse + model.timings.evaluate;
        let mut geometry_ms = model.timings.geometry;

        let mut header = vec![format!(
            "{} - {}",
            std::path::Path::new(&req.run.input)
                .file_name()
                .map_or(req.run.input.clone(), |f| f.to_string_lossy().into_owned()),
            if req.diff.is_some() {
                "diff"
            } else if req.preview {
                "preview"
            } else {
                "render"
            }
        )];
        let mut legend = Vec::new();
        let mut summary = serde_json::Map::new();
        let mut log = model.log.clone();
        let (scene, is_2d) = if let Some(other) = &req.diff {
            let run = Run {
                input: other.clone(),
                supersede: false,
                ..req.run.clone()
            };
            let other_built = self.render(&run, Mode::Render, &scheme)?;
            log = join(log, other_built.log.clone());
            if other_built.exit_code != 0 {
                return failed(log, other_built.exit_code);
            }
            geometry_ms += other_built.timings.geometry;
            let t = self.now();
            let solid_of = |b: &Rendered| b.geometry.as_ref().map(stats::solid).unwrap_or_default();
            let (new, old) = (solid_of(&model), solid_of(&other_built));
            let added = new.boolean(&old, OpType::Subtract);
            let removed = old.boolean(&new, OpType::Subtract);
            let unchanged = new.boolean(&old, OpType::Intersect);
            geometry_ms += self.now() - t;
            let bbox =
                [&new, &old]
                    .iter()
                    .filter_map(|m| m.bounds())
                    .reduce(|(al, ah), (bl, bh)| {
                        (
                            std::array::from_fn(|k| al[k].min(bl[k])),
                            std::array::from_fn(|k| ah[k].max(bh[k])),
                        )
                    });
            let mut scene = Scene::empty(&scheme, bbox);
            let state = DrawState {
                cull: Cull::None,
                depth: Depth::LessEqual,
                color_write: true,
                bias: false,
            };
            let mut add = |m: &ManifoldGeometry, color: Color, transparent: bool| {
                if m.is_empty() {
                    return;
                }
                let mesh = Arc::new(m.to_polyset(&geom_scheme));
                let surface = |cull| Surface {
                    mesh: mesh.clone(),
                    matrix: None,
                    color,
                    force_color: true,
                    lit: true,
                    state: DrawState { cull, ..state },
                };
                if transparent {
                    // Back faces first, as OpenSCAD draws transparent objects.
                    scene.push(surface(Cull::Front));
                    scene.push(surface(Cull::Back));
                } else {
                    scene.push(surface(Cull::None));
                }
            };
            add(&unchanged, UNCHANGED, false);
            add(&added, ADDED, false);
            add(&removed, REMOVED, true);
            let (av, rv, uv) = (
                added.manifold.volume(),
                removed.manifold.volume(),
                unchanged.manifold.volume(),
            );
            header.push(format!(
                "vs {}: added {} mm3, removed {} mm3, unchanged {} mm3",
                other,
                number(av),
                number(rv),
                number(uv)
            ));
            legend = vec![
                (ADDED.0, "added".to_string()),
                (REMOVED.0, "removed".to_string()),
                (UNCHANGED.0, "unchanged".to_string()),
            ];
            summary.insert(
                "diff".into(),
                json!({
                    "other": other,
                    "added_volume": av,
                    "removed_volume": rv,
                    "unchanged_volume": uv,
                    "other_geometry": other_built.geometry.as_ref().map(|g| stats::geometry(g, &geom_scheme)),
                }),
            );
            let is_2d = matches!(model.geometry, Some(Geometry::Polygon2d(_)))
                && matches!(other_built.geometry, Some(Geometry::Polygon2d(_)));
            (scene, is_2d)
        } else if let Some(tree) = &model.tree {
            let scene = render::preview::scene(tree, &scheme, render::Previewer::OpenCsg);
            (scene, false)
        } else {
            (
                Scene::new(model.geometry.as_ref(), &scheme),
                matches!(model.geometry, Some(Geometry::Polygon2d(_))),
            )
        };
        let geometry = model
            .geometry
            .as_ref()
            .map(|g| stats::geometry(g, &geom_scheme));
        if let Some(Value::Object(g)) = &geometry {
            let size = &g["bbox"]["size"];
            let dims: Vec<String> = size
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_f64)
                .map(number)
                .collect();
            let mut line = format!("size {} mm", dims.join(" x "));
            if let Some(v) = g.get("volume").and_then(Value::as_f64) {
                line.push_str(&format!(", volume {} mm3", number(v)));
            }
            if let Some(t) = g.get("triangles").and_then(Value::as_u64) {
                line.push_str(&format!(", {t} triangles"));
            }
            if req.diff.is_none() {
                header.push(line);
            }
        } else if model.tree.is_some() {
            if let Some((lo, hi)) = scene.bounding_box() {
                let d: Vec<String> = (0..3).map(|k| number(hi[k] - lo[k])).collect();
                header.push(format!("preview size {} mm", d.join(" x ")));
            }
        } else if req.diff.is_none() {
            header.push("empty: nothing to draw".into());
        }

        let views = requested.unwrap_or_else(|| {
            if is_2d {
                vec![View::Top]
            } else {
                vec![View::Iso, View::Front, View::Top, View::Right]
            }
        });
        let sheet = Sheet {
            views: views.clone(),
            width,
            height,
            dims: req.dims,
            header,
            legend,
            lighting: req.lighting,
        };
        req.run.stage(crate::Stage::Draw);
        let (png, gpu_ms, draw_ms, encode_ms) = self.draw_sheet(&scene, &scheme, &sheet)?;

        let mode_name = if req.diff.is_some() {
            "diff"
        } else if req.preview {
            "preview"
        } else {
            "render"
        };
        let mut out = serde_json::Map::new();
        out.insert("schema".into(), json!(1));
        out.insert("input".into(), json!(req.run.input));
        out.insert("output".into(), json!(req.output));
        out.insert("mode".into(), json!(mode_name));
        out.insert(
            "views".into(),
            json!(views.iter().map(|v| v.name()).collect::<Vec<_>>()),
        );
        out.insert("size".into(), json!([width, height]));
        out.insert(
            "lighting".into(),
            json!(match req.lighting {
                render::Lighting::Headlight => "headlight",
                render::Lighting::OpenScad => "openscad",
            }),
        );
        out.insert("geometry".into(), geometry.unwrap_or(Value::Null));
        if let (Some(tree), None) = (&model.tree, &model.geometry) {
            let bbox = tree
                .bounding_box(false)
                .map(|(lo, hi)| stats::bbox_json(&lo, &hi));
            out.insert("preview_bbox".into(), bbox.unwrap_or(Value::Null));
        }
        out.extend(summary);
        out.insert(
            "timings_ms".into(),
            json!({
                "evaluate": round(evaluate_ms),
                "geometry": round(geometry_ms),
                "gpu_init": round(gpu_ms),
                "draw": round(draw_ms),
                "encode": round(encode_ms),
                "total": round(self.now() - started),
            }),
        );
        out.insert(
            "diagnostics".into(),
            crate::diag::summary_json(&log.lines, &log.names),
        );
        Ok(Snapshot {
            exit_code: 0,
            png: Some(png),
            summary: Value::Object(out),
            log,
        })
    }

    /// The sheet as PNG bytes, and the milliseconds spent opening the GPU,
    /// drawing and encoding.
    #[cfg(feature = "gpu")]
    fn draw_sheet(
        &self,
        scene: &Scene,
        scheme: &ColorScheme,
        sheet: &Sheet,
    ) -> Result<(Vec<u8>, f64, f64, f64), SnapshotError> {
        let t = self.now();
        let provider = self
            .cfg
            .gpu
            .as_ref()
            .ok_or_else(|| SnapshotError::Failed("cannot draw: no GPU".into()))?;
        let gpu = provider().map_err(|e| SnapshotError::Failed(format!("cannot draw: {e}")))?;
        let gpu_ms = self.now() - t;
        let t = self.now();
        let image = render::snapshot::draw_blocking(gpu, scene, scheme, sheet)
            .map_err(|e| SnapshotError::Failed(format!("cannot draw: {e}")))?;
        let draw_ms = self.now() - t;
        let t = self.now();
        let png = render::encode_png(image.width, image.height, &image.rgba);
        Ok((png, gpu_ms, draw_ms, self.now() - t))
    }

    #[cfg(not(feature = "gpu"))]
    fn draw_sheet(
        &self,
        _scene: &Scene,
        _scheme: &ColorScheme,
        _sheet: &Sheet,
    ) -> Result<(Vec<u8>, f64, f64, f64), SnapshotError> {
        Err(SnapshotError::Failed(
            "cannot draw: this build has no GPU support".into(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_parse() {
        assert_eq!(parse_size("1024x768"), Some((1024, 768)));
        assert_eq!(parse_size("800,600"), Some((800, 600)));
        assert_eq!(parse_size("10x10"), None);
        assert_eq!(parse_size("big"), None);
    }
}
