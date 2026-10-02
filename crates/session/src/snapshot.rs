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
use render::snapshot::{Marker, Sheet, View, number};
use render::{ColorScheme, Scene};
use serde_json::{Value, json};

use crate::check::{Analysis, CheckSettings, Level};
use crate::mesh::Mesh;
use crate::parts::is_within;
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
    /// Parts to show in colour, the others ghosted (with `--enable part`;
    /// a name also selects the parts nested in it).
    pub highlight: Vec<String>,
    /// Run `check` with these settings and mark its findings on the sheet.
    pub issues: Option<CheckSettings>,
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
            highlight: Vec::new(),
            issues: None,
        }
    }
}

/// Part colours, in order of first appearance (Tableau 10 without its red
/// and orange, which mark issues).
const PALETTE: [[f32; 3]; 8] = [
    [0.12, 0.47, 0.71],
    [0.17, 0.63, 0.17],
    [0.58, 0.40, 0.74],
    [0.09, 0.75, 0.81],
    [0.55, 0.34, 0.29],
    [0.89, 0.47, 0.76],
    [0.74, 0.74, 0.13],
    [0.50, 0.50, 0.50],
];
/// Faces of no part, and a whole model under `--issues`.
const NEUTRAL: Color = Color([0.80, 0.80, 0.77, 1.0]);
/// Parts not highlighted.
const GHOST: Color = Color([0.62, 0.62, 0.60, 0.22]);
const THIN: Color = Color([0.86, 0.12, 0.10, 1.0]);
const OVERHANG: Color = Color([1.00, 0.62, 0.05, 1.0]);
const FLOATING: Color = Color([0.62, 0.36, 0.85, 1.0]);
/// The most legend entries the header has room for.
const LEGEND_MAX: usize = 8;

/// A finding's marker colour, shared with the apps' 3D view (`client::overlay`).
pub fn marker_color(l: Level) -> [u8; 3] {
    match l {
        Level::Error => [190, 20, 20],
        Level::Warning => [200, 110, 0],
        Level::Info => [40, 90, 190],
    }
}

/// The faces `tris` of `mesh` as a mesh to draw.
fn subset(mesh: &Mesh, tris: impl Iterator<Item = usize>) -> Arc<geom::polyset::PolySet> {
    Arc::new(geom::polyset::PolySet {
        vertices: mesh.verts.clone(),
        faces: tris.map(|t| mesh.tris[t].to_vec()).collect(),
        triangular: true,
        ..Default::default()
    })
}

/// What a part or issue scene adds to the sheet.
struct Marked {
    scene: Scene,
    legend: Vec<([f32; 4], String)>,
    markers: Vec<Marker>,
    parts: Vec<String>,
}

/// The rendered model drawn by part (each in its colour, or the
/// highlighted ones in colour and the rest ghosted), with the issues of
/// `analysis` painted on (thin walls red, overhangs amber) and its
/// findings marked by number.
fn marked_scene(
    mesh: &Mesh,
    scheme: &ColorScheme,
    highlight: &[String],
    analysis: Option<&Analysis>,
) -> Result<Marked, SnapshotError> {
    let names: Vec<String> = mesh.part_names.iter().map(|n| n.to_string()).collect();
    for h in highlight {
        if !names.iter().any(|n| is_within(n, h)) {
            return Err(SnapshotError::Failed(if names.is_empty() {
                format!("no part '{h}': the model has no parts (they need `--enable part`)")
            } else {
                format!("no part '{h}' (parts: {})", names.join(", "))
            }));
        }
    }
    let b = mesh.bbox();
    let bbox = (!b.is_empty()).then_some((b.lo, b.hi));
    let mut scene = Scene::empty(scheme, bbox);
    let opaque = DrawState {
        cull: Cull::None,
        depth: Depth::LessEqual,
        color_write: true,
        bias: false,
    };
    let surface = |mesh: Arc<geom::polyset::PolySet>, color: Color, state: DrawState| Surface {
        mesh,
        matrix: None,
        color,
        force_color: true,
        lit: true,
        state,
    };
    // Groups of faces by colour: `None` for faces of no part.
    let lit = |p: Option<u32>| -> bool {
        highlight.is_empty()
            || p.is_some_and(|p| highlight.iter().any(|h| is_within(&names[p as usize], h)))
    };
    let color_of = |p: Option<u32>| -> Color {
        match p {
            _ if analysis.is_some() && highlight.is_empty() => NEUTRAL,
            None => NEUTRAL,
            Some(p) => {
                let c = PALETTE[p as usize % PALETTE.len()];
                Color([c[0], c[1], c[2], 1.0])
            }
        }
    };
    let mut groups: Vec<(Option<u32>, Vec<usize>)> = Vec::new();
    for t in 0..mesh.tris.len() {
        let p = mesh.part[t];
        match groups.iter_mut().find(|(q, _)| *q == p) {
            Some(g) => g.1.push(t),
            None => groups.push((p, vec![t])),
        }
    }
    let mut ghosts = Vec::new();
    for (p, tris) in &groups {
        if lit(*p) {
            scene.push(surface(
                subset(mesh, tris.iter().copied()),
                color_of(*p),
                opaque,
            ));
        } else {
            ghosts.extend(tris.iter().copied());
        }
    }
    let mut legend = Vec::new();
    let mut markers = Vec::new();
    if let Some(a) = analysis {
        // Painted over the model, pulled towards the camera so they win the
        // depth test against the faces they cover.
        let over = DrawState {
            bias: true,
            ..opaque
        };
        let keep = |t: &&u32| lit(mesh.part[**t as usize]);
        let thin: Vec<usize> = a.thin.iter().filter(keep).map(|&t| t as usize).collect();
        let hang: Vec<usize> = a
            .overhang
            .iter()
            .filter(keep)
            .map(|&t| t as usize)
            .collect();
        let float: Vec<usize> = a
            .floating
            .iter()
            .filter(keep)
            .map(|&t| t as usize)
            .collect();
        if !float.is_empty() {
            scene.push(surface(subset(mesh, float.into_iter()), FLOATING, over));
        }
        if !hang.is_empty() {
            scene.push(surface(subset(mesh, hang.into_iter()), OVERHANG, over));
        }
        if !thin.is_empty() {
            scene.push(surface(subset(mesh, thin.into_iter()), THIN, over));
        }
        legend.push((THIN.0, "thin wall".to_string()));
        legend.push((OVERHANG.0, "overhang".to_string()));
        legend.push((FLOATING.0, "floating".to_string()));
        for (i, f) in a.findings.iter().enumerate() {
            // Info findings (a model off the bed) point at nothing to fix.
            if f.bbox.is_empty() || f.level == Level::Info {
                continue;
            }
            let c = marker_color(f.level);
            markers.push(Marker {
                point: f.point,
                label: (i + 1).to_string(),
                color: c,
            });
        }
    }
    if !ghosts.is_empty() {
        // Translucent: back faces first, as OpenSCAD draws transparent
        // objects, after everything opaque.
        let mesh = subset(mesh, ghosts.into_iter());
        for cull in [Cull::Front, Cull::Back] {
            scene.push(surface(mesh.clone(), GHOST, DrawState { cull, ..opaque }));
        }
    }
    if analysis.is_none() || !highlight.is_empty() {
        let shown: Vec<(usize, &String)> = names
            .iter()
            .enumerate()
            .filter(|(i, _)| lit(Some(*i as u32)))
            .collect();
        let room = LEGEND_MAX.saturating_sub(legend.len());
        for (i, n) in shown.iter().take(room) {
            legend.push((color_of(Some(*i as u32)).0, n.to_string()));
        }
        if shown.len() > room {
            legend.push((GHOST.0, format!("+{} more", shown.len() - room)));
        }
    }
    Ok(Marked {
        scene,
        legend,
        markers,
        parts: names,
    })
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
        let marking = !req.highlight.is_empty() || req.issues.is_some();
        if marking && (req.preview || req.diff.is_some()) {
            return Err(SnapshotError::Failed(
                "--highlight and --issues draw the rendered model: they cannot be combined \
                 with --preview or --diff"
                    .into(),
            ));
        }
        let (model, parts) = if req.issues.is_some() {
            self.render_parts(&req.run, &scheme)?
        } else {
            (self.render(&req.run, mode, &scheme)?, Vec::new())
        };
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
        // Parts and issues: the rendered solid by part, with the findings.
        let solid_mesh = match &model.geometry {
            Some(g) if !req.preview && req.diff.is_none() && g.dimension() == 3 => {
                Some(Mesh::of_solid(&stats::solid(g)))
            }
            _ => None,
        };
        let mut check_line: Option<String> = None;
        let has_parts = solid_mesh
            .as_ref()
            .is_some_and(|m| !m.part_names.is_empty());
        let mut marked = None;
        if marking || has_parts {
            let t = self.now();
            let analysis = req.issues.as_ref().map(|settings| {
                let clock = || self.now();
                crate::check::analyze_with(
                    model.geometry.as_ref(),
                    &parts,
                    &model.inputs,
                    settings,
                    &clock,
                )
            });
            let mesh = analysis
                .as_ref()
                .map(|a| &a.mesh)
                .or(solid_mesh.as_ref())
                .filter(|m| !m.tris.is_empty());
            if let Some(mesh) = mesh {
                let m = marked_scene(mesh, &scheme, &req.highlight, analysis.as_ref())?;
                legend = m.legend.clone();
                if !m.parts.is_empty() {
                    summary.insert("parts".into(), json!(m.parts));
                }
                if !req.highlight.is_empty() {
                    summary.insert("highlight".into(), json!(req.highlight));
                }
                marked = Some(m);
            } else if !req.highlight.is_empty() {
                return Err(SnapshotError::Failed(
                    "--highlight needs a 3D model with parts (`--enable part`)".into(),
                ));
            }
            if let Some(a) = &analysis {
                let [e, w, i] = a.counts;
                // Markers are numbered by `issues.findings[].id`.
                check_line = Some(format!("; check: {e} errors, {w} warnings"));
                summary.insert(
                    "issues".into(),
                    json!({
                        "counts": {"errors": e, "warnings": w, "info": i},
                        "findings": a.findings.iter().enumerate().map(|(k, f)| f.json(k + 1)).collect::<Vec<_>>(),
                    }),
                );
            }
            geometry_ms += self.now() - t;
        }
        let mut markers = Vec::new();
        let (scene, is_2d) = if let Some(m) = marked {
            markers = m.markers;
            (m.scene, false)
        } else if let Some(other) = &req.diff {
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
            // With the session's product cache, so a snapshot after an edit
            // recomputes only the products the edit changed.
            let scene = render::preview::scene_cached(
                tree,
                &scheme,
                render::Previewer::OpenCsg,
                &geom::csg::Stop::default(),
                model.scene.cache(),
            )
            .unwrap_or_else(|_| Scene::empty(&scheme, None));
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
            if let Some(c) = &check_line {
                line.push_str(c);
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
            markers,
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
