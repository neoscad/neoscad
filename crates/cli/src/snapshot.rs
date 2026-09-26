//! `neoscad snapshot MODEL.scad`: one contact-sheet PNG of a model for
//! agents (docs/architecture.md, "Agent surface"), and with
//! `--format json` a small summary of it on stdout
//! (`docs/cli-json.md`, "snapshot").
//!
//! The sheet shows the standard views (iso, front, top and right by
//! default; only top for a 2D model) at one shared scale, each with a
//! grid, an axis cross and a caption; `--dims` adds the bounding box's
//! size. The model is drawn from its rendered geometry by default, so the
//! picture and the numbers describe the same solid; `--preview` draws
//! OpenSCAD's preview instead (fast, and it shows `%` and `#` objects).
//!
//! `--diff OTHER.scad` compares against another version with real
//! booleans on both rendered solids: MODEL - OTHER is what was added
//! (green), OTHER - MODEL what was removed (red, translucent), and their
//! intersection what stayed (grey). Both versions render through one
//! geometry cache, so the subtrees they share are computed once.

use std::ffi::OsString;
use std::io::Write as _;
use std::sync::Arc;
use std::time::Instant;

use clap::Parser;
use geom::Geometry;
use geom::color::Color;
use geom::manifold_geom::{GlobalIds, ManifoldGeometry, OpType};
use geom::polyset::PolySet;
use render::scene::{Cull, Depth, DrawState, Surface};
use render::snapshot::{Sheet, View, number};
use render::{ColorScheme, Scene};
use serde_json::{Value, json};

use crate::run;

/// OpenSCAD's general failure exit code.
const EXIT_ERROR: u8 = 1;

/// `neoscad snapshot`: a contact sheet of a model's standard views.
#[derive(Parser, Debug)]
#[command(
    name = "neoscad snapshot",
    about = "Draw a model's standard views as one PNG contact sheet",
    version
)]
struct Args {
    /// The model.
    model: String,

    /// The PNG to write (default: MODEL's name with `-snapshot.png`, in the
    /// working directory).
    #[arg(short = 'o', long = "output", value_name = "FILE")]
    output: Option<String>,

    /// Views, comma separated: iso, front, back, left, right, top, bottom.
    #[arg(long, value_delimiter = ',', value_name = "LIST")]
    views: Vec<String>,

    /// Size of the whole sheet in pixels.
    #[arg(long, value_name = "WxH", default_value = "1024x1024")]
    size: String,

    /// Annotate the bounding box's size (mm) in every view.
    #[arg(long)]
    dims: bool,

    /// Draw the rendered geometry (the default).
    #[arg(long, conflicts_with = "preview")]
    render: bool,

    /// Draw OpenSCAD's preview (the CSG products; shows `%` and `#`).
    #[arg(long)]
    preview: bool,

    /// Compare with another version of the model: added (green),
    /// removed (red), unchanged (grey).
    #[arg(long, value_name = "OTHER.scad", conflicts_with = "preview")]
    diff: Option<String>,

    /// `json` also writes a summary to stdout.
    #[arg(long, value_name = "FORMAT")]
    format: Option<String>,

    /// Set a top-level variable (`-D var=value`), as for export.
    #[arg(short = 'D', value_name = "var=val", action = clap::ArgAction::Append)]
    define: Vec<String>,

    /// Only errors on stderr.
    #[arg(short = 'q', long)]
    quiet: bool,
}

/// Run `neoscad snapshot` with the arguments after `snapshot`.
pub fn main(args: Vec<OsString>) -> u8 {
    let argv = std::iter::once(OsString::from("neoscad snapshot")).chain(args);
    let a = match Args::try_parse_from(argv) {
        Ok(a) => a,
        Err(e) => {
            let _ = e.print();
            return if e.use_stderr() { EXIT_ERROR } else { 0 };
        }
    };
    match snapshot(&a) {
        Ok(()) => 0,
        Err(code) => code,
    }
}

fn fail(msg: impl std::fmt::Display) -> u8 {
    eprintln!("neoscad snapshot: {msg}");
    EXIT_ERROR
}

/// Messages a build printed, sorted by kind for the summary.
#[derive(Debug, Default)]
struct Diagnostics {
    errors: Vec<String>,
    warnings: Vec<String>,
    echoes: Vec<String>,
}

impl Diagnostics {
    /// Pass a build's log on to stderr and keep its lines.
    fn take(&mut self, log: &[u8], quiet: bool) {
        if !quiet || log.windows(6).any(|w| w == b"ERROR:") {
            let _ = std::io::stderr().write_all(log);
        }
        for line in String::from_utf8_lossy(log).lines() {
            if line.starts_with("ERROR:") {
                self.errors.push(line.to_string());
            } else if line.starts_with("WARNING:") {
                self.warnings.push(line.to_string());
            } else if line.starts_with("ECHO:") {
                self.echoes.push(line.to_string());
            }
        }
    }

    fn json(&self) -> Value {
        // At most this many lines of each kind: the summary stays small.
        const KEEP: usize = 20;
        let first = |v: &[String]| v.iter().take(KEEP).cloned().collect::<Vec<_>>();
        json!({
            "errors": self.errors.len(),
            "warnings": self.warnings.len(),
            "echoes": self.echoes.len(),
            "messages": first(&self.errors).into_iter().chain(first(&self.warnings)).collect::<Vec<_>>(),
            "echo": first(&self.echoes),
        })
    }
}

/// Load and build one model; its log goes to stderr and `diags`.
fn build(
    input: &str,
    a: &Args,
    renderer: &geom::Renderer,
    scheme: &ColorScheme,
    preview: bool,
    diags: &mut Diagnostics,
) -> Result<run::Built, u8> {
    let export_options = crate::export_options::ExportOptions::parse(&[]);
    let summary = crate::summary::Request {
        options: Vec::new(),
        file: None,
    };
    let job = run::Job {
        input,
        outputs: &[],
        defines: &a.define,
        parameter_file: None,
        parameter_set: None,
        quiet: a.quiet,
        hardwarnings: false,
        export_options: &export_options,
        summary: &summary,
        animate: None,
        scheme,
        png: None,
    };
    let options = eval::Options {
        preview,
        ..eval::Options::default()
    };
    let mut log = Vec::new();
    let built = run::build(&job, &options, renderer, preview, &mut log);
    diags.take(&log, a.quiet);
    built
}

/// A 3D solid for statistics and booleans: a Manifold result as it is, a
/// mesh converted as `--render=force` would, a 2D shape as the preview's
/// one-unit slab (so a 2D diff still has volumes to compare).
fn solid(g: &Geometry) -> ManifoldGeometry {
    match g {
        Geometry::Manifold(m) => (**m).clone(),
        Geometry::PolySet(ps) => {
            ManifoldGeometry::from_polyset(ps, &GlobalIds, &mut Vec::new(), &mut Vec::new())
        }
        Geometry::Polygon2d(p) => ManifoldGeometry::from_polyset(
            &geom::csg::slab(p),
            &GlobalIds,
            &mut Vec::new(),
            &mut Vec::new(),
        ),
    }
}

/// Connected pieces of a mesh: faces sharing a vertex are one piece.
fn components(ps: &PolySet) -> usize {
    let mut parent: Vec<usize> = (0..ps.vertices.len()).collect();
    fn find(p: &mut [usize], mut x: usize) -> usize {
        while p[x] != x {
            p[x] = p[p[x]];
            x = p[x];
        }
        x
    }
    let mut used = vec![false; ps.vertices.len()];
    for f in &ps.faces {
        for w in f.windows(2) {
            let (a, b) = (
                find(&mut parent, w[0] as usize),
                find(&mut parent, w[1] as usize),
            );
            parent[a] = b;
        }
        for &v in f {
            used[v as usize] = true;
        }
    }
    (0..ps.vertices.len())
        .filter(|&v| used[v] && find(&mut parent, v) == v)
        .count()
}

/// The `geometry` object of the summary.
fn stats(g: &Geometry, scheme: &geom::color::Scheme) -> Value {
    let bbox = |lo: &[f64], hi: &[f64]| {
        let size: Vec<f64> = lo.iter().zip(hi).map(|(l, h)| h - l).collect();
        json!({"min": lo, "max": hi, "size": size})
    };
    if let Geometry::Polygon2d(p) = g {
        // Outlines are sanitised: outer ones counter-clockwise, holes
        // clockwise, so the signed areas add up to the shape's.
        let area: f64 = p
            .outlines
            .iter()
            .map(|o| {
                let v = &o.vertices;
                (0..v.len())
                    .map(|i| {
                        let (a, b) = (v[i], v[(i + 1) % v.len()]);
                        a[0] * b[1] - b[0] * a[1]
                    })
                    .sum::<f64>()
                    / 2.0
            })
            .sum();
        let (lo, hi) = p.bounds().unwrap_or(([0.0; 2], [0.0; 2]));
        return json!({
            "dimensions": 2,
            "bbox": bbox(&lo, &hi),
            "area": area.abs(),
            "contours": p.outlines.len(),
        });
    }
    let m = solid(g);
    let ps = m.to_polyset(scheme);
    let (lo, hi) = m.bounds().unwrap_or(([0.0; 3], [0.0; 3]));
    json!({
        "dimensions": 3,
        "bbox": bbox(&lo, &hi),
        "volume": m.manifold.volume(),
        "area": m.manifold.surface_area(),
        "triangles": m.manifold.num_tri(),
        "vertices": m.manifold.num_vert(),
        "manifold": m.is_valid(),
        "components": components(&ps),
    })
}

/// `WxH`.
fn parse_size(s: &str) -> Option<(u32, u32)> {
    let (w, h) = s.split_once(['x', 'X', ','])?;
    let (w, h) = (w.trim().parse::<u32>().ok()?, h.trim().parse::<u32>().ok()?);
    (w >= 64 && h >= 64 && w <= 8192 && h <= 8192).then_some((w, h))
}

/// Colours of a diff.
const ADDED: Color = Color([0.25, 0.72, 0.30, 1.0]);
const REMOVED: Color = Color([0.90, 0.22, 0.20, 0.55]);
const UNCHANGED: Color = Color([0.78, 0.78, 0.74, 1.0]);

fn snapshot(a: &Args) -> Result<(), u8> {
    let started = Instant::now();
    let json_out = match a.format.as_deref() {
        None => false,
        Some("json") => true,
        Some(f) => return Err(fail(format!("unknown --format '{f}' (only json)"))),
    };
    let (width, height) = parse_size(&a.size).ok_or_else(|| {
        fail(format!(
            "--size must be WxH, 64 to 8192 each (got '{}')",
            a.size
        ))
    })?;
    let preview = a.preview;
    let scheme = ColorScheme::cornfield();
    let renderer = geom::Renderer::new();
    let mut diags = Diagnostics::default();
    // A model that fails to load or evaluate still gets its summary, with
    // the diagnostics that say why: that is what an agent needs next.
    let failed = |diags: &Diagnostics, code: u8| {
        if json_out {
            let out = json!({
                "schema": 1,
                "input": a.model,
                "failed": true,
                "exit_code": code,
                "diagnostics": diags.json(),
            });
            println!("{out}");
        }
        code
    };
    let model = match build(&a.model, a, &renderer, &scheme, preview, &mut diags) {
        Ok(m) => m,
        Err(code) => return Err(failed(&diags, code)),
    };
    let mut geometry_ms = model.geometry_ms;
    let evaluate_ms = model.evaluate_ms;

    // The scene, and what the header and summary say about it.
    let geom_scheme = scheme.geometry_scheme();
    let mut header = vec![format!(
        "{} - {}",
        std::path::Path::new(&a.model)
            .file_name()
            .map_or(a.model.clone(), |f| f.to_string_lossy().into_owned()),
        if a.diff.is_some() {
            "diff"
        } else if preview {
            "preview"
        } else {
            "render"
        }
    )];
    let mut legend = Vec::new();
    let mut summary = serde_json::Map::new();
    let (scene, is_2d) = if let Some(other) = &a.diff {
        let other_built = match build(other, a, &renderer, &scheme, false, &mut diags) {
            Ok(m) => m,
            Err(code) => return Err(failed(&diags, code)),
        };
        geometry_ms += other_built.geometry_ms;
        let t = Instant::now();
        let solid_of = |b: &run::Built| b.geometry.as_ref().map(solid).unwrap_or_default();
        let (new, old) = (solid_of(&model), solid_of(&other_built));
        let added = new.boolean(&old, OpType::Subtract);
        let removed = old.boolean(&new, OpType::Subtract);
        let unchanged = new.boolean(&old, OpType::Intersect);
        geometry_ms += t.elapsed().as_secs_f64() * 1000.0;
        let bbox = [&new, &old]
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
                "other_geometry": other_built.geometry.as_ref().map(|g| stats(g, &geom_scheme)),
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
    let geometry = model.geometry.as_ref().map(|g| stats(g, &geom_scheme));
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
        if a.diff.is_none() {
            header.push(line);
        }
    } else if model.tree.is_some() {
        if let Some((lo, hi)) = scene.bounding_box() {
            let d: Vec<String> = (0..3).map(|k| number(hi[k] - lo[k])).collect();
            header.push(format!("preview size {} mm", d.join(" x ")));
        }
    } else if a.diff.is_none() {
        header.push("empty: nothing to draw".into());
    }

    let views: Vec<View> = if a.views.is_empty() {
        if is_2d {
            vec![View::Top]
        } else {
            vec![View::Iso, View::Front, View::Top, View::Right]
        }
    } else {
        a.views
            .iter()
            .map(|v| {
                View::parse(v.trim()).ok_or_else(|| {
                    let names: Vec<&str> = View::ALL.iter().map(|v| v.name()).collect();
                    fail(format!("unknown view '{v}' (one of {})", names.join(", ")))
                })
            })
            .collect::<Result<_, _>>()?
    };
    let sheet = Sheet {
        views: views.clone(),
        width,
        height,
        dims: a.dims,
        header,
        legend,
    };

    let t = Instant::now();
    let gpu = crate::png::offscreen().map_err(|e| fail(format!("cannot draw: {e}")))?;
    let gpu_ms = t.elapsed().as_secs_f64() * 1000.0;
    let t = Instant::now();
    let image = render::snapshot::draw_blocking(gpu, &scene, &scheme, &sheet)
        .map_err(|e| fail(format!("cannot draw: {e}")))?;
    let draw_ms = t.elapsed().as_secs_f64() * 1000.0;
    let t = Instant::now();
    let png = render::encode_png(image.width, image.height, &image.rgba);
    let encode_ms = t.elapsed().as_secs_f64() * 1000.0;
    let output = a.output.clone().unwrap_or_else(|| {
        let stem = std::path::Path::new(&a.model)
            .file_stem()
            .map_or("model".into(), |s| s.to_string_lossy().into_owned());
        format!("{stem}-snapshot.png")
    });
    std::fs::write(&output, &png).map_err(|e| fail(format!("cannot write '{output}': {e}")))?;

    if json_out {
        let mode = if a.diff.is_some() {
            "diff"
        } else if preview {
            "preview"
        } else {
            "render"
        };
        let round = |ms: f64| (ms * 10.0).round() / 10.0;
        let mut out = serde_json::Map::new();
        out.insert("schema".into(), json!(1));
        out.insert("input".into(), json!(a.model));
        out.insert("output".into(), json!(output));
        out.insert("mode".into(), json!(mode));
        out.insert(
            "views".into(),
            json!(views.iter().map(|v| v.name()).collect::<Vec<_>>()),
        );
        out.insert("size".into(), json!([width, height]));
        out.insert("geometry".into(), geometry.unwrap_or(Value::Null));
        if let (Some(tree), None) = (&model.tree, &model.geometry) {
            let bbox = tree.bounding_box(false).map(|(lo, hi)| {
                let size: Vec<f64> = (0..3).map(|k| hi[k] - lo[k]).collect();
                json!({"min": lo, "max": hi, "size": size})
            });
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
                "total": round(started.elapsed().as_secs_f64() * 1000.0),
            }),
        );
        out.insert("diagnostics".into(), diags.json());
        println!("{}", Value::Object(out));
    }
    Ok(())
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

    #[test]
    fn two_cubes_are_two_components() {
        let mut a = geom::primitives::cube([1.0; 3], false);
        let b = geom::primitives::cube([1.0; 3], false);
        let n = a.vertices.len() as u32;
        a.vertices
            .extend(b.vertices.iter().map(|v| [v[0] + 3.0, v[1], v[2]]));
        a.faces.extend(
            b.faces
                .iter()
                .map(|f| f.iter().map(|&i| i + n).collect::<Vec<_>>()),
        );
        assert_eq!(components(&a), 2);
    }

    #[test]
    fn diagnostics_are_sorted_by_kind() {
        let mut d = Diagnostics::default();
        d.take(b"ECHO: 1\nWARNING: w\nERROR: e\nplain\n", true);
        assert_eq!(
            (d.errors.len(), d.warnings.len(), d.echoes.len()),
            (1, 1, 1)
        );
    }
}
