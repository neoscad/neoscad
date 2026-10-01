//! `neoscad IN -o OUT` through a running server: which command lines the
//! server can run, what the client sends, and how the server runs it
//! (`cli.export` in `docs/serve-protocol.md`).
//!
//! The server runs the export on its session (`session::Session::export`),
//! which follows the command line's own export step for step and encodes
//! with the same function, so the files and messages are the ones a local
//! run makes. Two things necessarily differ: the render summary's
//! cache figures and times are the server's (its cache is
//! warm; that is the point), and unseeded `rands()` gets the client's
//! seed, as a local run would.
//!
//! PNG exports go too: the server renders (or previews) on its session
//! and draws with the command line's own `png` module and flags.
//!
//! Only plain geometry and image exports go to the server; one run's
//! outputs must be all images or all geometry. Anything with state outside
//! the run (dependency files, `-m`, parameter sets, animation, summary
//! files), `--hardwarnings` (whose early stops the session does not
//! model), the evaluation flags and the tree, echo, AST and param formats
//! run locally, unchanged.

use std::path::Path;

use serde_json::{Value, json};

use crate::export_options::ExportOptions;
use crate::outcome::Outcome;

/// What the command line decided before exporting, for [`params`].
#[derive(Debug)]
pub struct Plan<'a> {
    pub input: &'a str,
    pub outputs: &'a [String],
    /// Each output's format identifier.
    pub formats: Vec<&'a str>,
    pub defines: &'a [String],
    pub export_options: &'a [String],
    pub summary: &'a [String],
    pub scheme: &'a str,
    pub force: bool,
    pub camera: eval::Camera,
    pub quiet: bool,
    pub json: bool,
    pub rich: bool,
    pub seed: u32,
    /// `--enable part`.
    pub parts: bool,
    /// `--enable`'s names, for OpenSCAD's experimental features.
    pub enable: &'a [String],
    /// The image flags, when the outputs are PNGs.
    pub png: Option<PngArgs<'a>>,
}

/// The command line's image flags, as given (`crate::png` parses them).
#[derive(Debug)]
pub struct PngArgs<'a> {
    pub camera: Option<&'a str>,
    pub viewall: bool,
    pub autocenter: bool,
    pub projection: Option<&'a str>,
    pub imgsize: Option<&'a str>,
    pub render: Option<&'a str>,
    pub preview: Option<&'a str>,
    pub view: &'a [String],
    pub csglimit: Option<u32>,
}

fn camera_json(c: &eval::Camera) -> Value {
    json!({"vpt": c.vpt, "vpr": c.vpr, "vpd": c.vpd, "vpf": c.vpf, "auto": c.auto, "locked": c.locked})
}

fn camera_of(v: &Value) -> eval::Camera {
    let d = eval::Camera::default();
    let v3 = |k: &str, d: [f64; 3]| {
        v.get(k)
            .and_then(Value::as_array)
            .filter(|a| a.len() == 3)
            .map_or(d, |a| {
                std::array::from_fn(|i| a[i].as_f64().unwrap_or(d[i]))
            })
    };
    eval::Camera {
        vpt: v3("vpt", d.vpt),
        vpr: v3("vpr", d.vpr),
        vpd: v.get("vpd").and_then(Value::as_f64).unwrap_or(d.vpd),
        vpf: v.get("vpf").and_then(Value::as_f64).unwrap_or(d.vpf),
        auto: v.get("auto").and_then(Value::as_bool).unwrap_or(d.auto),
        locked: v.get("locked").and_then(Value::as_bool).unwrap_or(d.locked),
    }
}

/// The `cli.export` parameters.
pub fn params(p: &Plan<'_>) -> Value {
    json!({
        "input": p.input,
        "outputs": p.outputs,
        "formats": p.formats,
        "defines": p.defines,
        "export_options": p.export_options,
        "summary": p.summary,
        "colorscheme": p.scheme,
        "force": p.force,
        "camera": camera_json(&p.camera),
        "quiet": p.quiet,
        "json": p.json,
        "rich": p.rich,
        "seed": p.seed,
        "parts": p.parts,
        "enable": p.enable,
        "png": p.png.as_ref().map(|g| json!({
            "camera": g.camera, "viewall": g.viewall, "autocenter": g.autocenter,
            "projection": g.projection, "imgsize": g.imgsize, "render": g.render,
            "preview": g.preview, "view": g.view, "csglimit": g.csglimit,
        })),
    })
}

/// Files go where the client's run would write them.
struct Sink<'a> {
    cwd: &'a Path,
    stdout: Vec<u8>,
    summary: crate::summary::Request,
}

impl session::ExportSink for Sink<'_> {
    fn write(&mut self, target: &str, data: &[u8]) -> Result<(), String> {
        if target == "-" {
            self.stdout.extend_from_slice(data);
            return Ok(());
        }
        std::fs::write(self.cwd.join(target), data)
            .map_err(|e| format!("ERROR: Can't write to '{target}': {e}"))
    }

    fn summary(
        &mut self,
        facts: &session::SummaryFacts<'_>,
        con: &mut eval::Console<Vec<u8>>,
    ) -> bool {
        let f = crate::summary::Facts {
            cache_entries: facts.cache_entries,
            cache_bytes: facts.cache_bytes,
            cache_budget: facts.cache_budget,
            elapsed_ms: facts.elapsed_ms.max(0.0) as u128,
            geometry: facts.geometry,
            camera: facts.camera,
        };
        crate::summary::emit(&self.summary, &f, con)
    }
}

fn strings(params: &Value, k: &str) -> Vec<String> {
    params
        .get(k)
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|v| v.as_str().map(str::to_string))
        .collect()
}

/// The session's run for a `cli.export` request: one-shot (it cancels
/// nothing), with the client's seed and camera.
fn run_of(params: &Value, input: &str, cwd: &Path) -> session::Run {
    let b = |k: &str| params.get(k).and_then(Value::as_bool).unwrap_or(false);
    let mut run = session::Run::new(input);
    run.cwd = Some(cwd.to_path_buf());
    run.defines = strings(params, "defines");
    run.quiet = b("quiet");
    run.rich = b("rich") && !b("json");
    run.camera = camera_of(params.get("camera").unwrap_or(&Value::Null));
    run.rng_seed = params.get("seed").and_then(Value::as_u64).map(|n| n as u32);
    run.supersede = false;
    run.parts = b("parts");
    run.features = crate::features(&strings(params, "enable"));
    // Unlimited, as the command line is: the client sends no `limits`,
    // and the server fills in an explicit unlimited object on every
    // `cli.*` request (`serve::cli`; `crate::limits`). An absent object
    // would leave `run.limits` at `None`, which means the server's
    // `Limits::AGENT`.
    run.limits = crate::limits::of_params(params, session::Limits::NONE)
        .ok()
        .flatten();
    run
}

/// The `--format json` report of a served run, where it goes.
fn report(
    out: &mut Outcome,
    run: &crate::report::Run<'_>,
    log: &session::Log,
    extra: &[eval::Logged],
    geometry: Option<Value>,
    to_stderr: bool,
) {
    let mut lines = log.lines.clone();
    lines.extend(extra.iter().cloned());
    let text = format!(
        "{}\n",
        crate::report::json(run, &lines, &log.names, geometry)
    );
    out.stderr.clear();
    if to_stderr {
        out.stderr = text.into_bytes();
    } else {
        out.stdout = text.into_bytes();
    }
}

/// `cli.export` of PNGs: the command line's image export
/// (`run::render_frame`'s image branch) on the session's warm caches.
fn execute_png(
    session: &session::Session,
    params: &Value,
    input: &str,
    outputs: &[String],
    cwd: &Path,
) -> Outcome {
    let g = &params["png"];
    let s = |k: &str| g.get(k).and_then(Value::as_str);
    let b = |k: &str| g.get(k).and_then(Value::as_bool).unwrap_or(false);
    let json_out = params.get("json").and_then(Value::as_bool).unwrap_or(false);
    let quiet = params
        .get("quiet")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let scheme = render::scheme::find(params["colorscheme"].as_str().unwrap_or(""))
        .unwrap_or_else(render::ColorScheme::cornfield);
    // The client parsed these already and printed any complaint.
    let Ok(camera) = crate::png::camera(
        s("camera"),
        b("viewall"),
        b("autocenter"),
        s("projection"),
        s("imgsize"),
    ) else {
        return Outcome::fail(1, "neoscad: bad camera flags");
    };
    let settings = crate::png::Settings {
        camera,
        scheme: scheme.clone(),
        previewer: crate::png::previewer(s("render"), s("preview")),
        view: crate::png::view_options(&strings(g, "view"), true),
        csg_limit: g
            .get("csglimit")
            .and_then(Value::as_u64)
            .map_or(geom::csg::DEFAULT_TERM_LIMIT, |n| n as usize),
    };
    let force = params
        .get("force")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let mode = match (settings.previewer, force) {
        (Some(_), _) => session::Mode::Preview,
        (None, true) => session::Mode::Force,
        (None, false) => session::Mode::Render,
    };
    let run = run_of(params, input, cwd);
    let main_dir = cwd
        .join(input)
        .parent()
        .map_or_else(|| cwd.to_path_buf(), Path::to_path_buf);
    let started = std::time::Instant::now();
    let r = match session.render_with_limit(&run, mode, &scheme, settings.csg_limit) {
        Ok(r) => r,
        Err(c) => return Outcome::fail(1, format!("neoscad: {c}")),
    };
    let mut out = Outcome {
        exit_code: r.exit_code,
        stderr: r.log.stderr.clone(),
        stdout: Vec::new(),
    };
    // What the command line prints after the geometry: the same console
    // rules, continuing the log.
    let mut con = eval::Console::new(Vec::new(), main_dir, quiet).record(true);
    if r.exit_code == 0 {
        let dim = r.geometry.as_ref().map_or(3, geom::Geometry::dimension);
        if mode == session::Mode::Force && dim == 3 {
            con.print(None, b"Converted to backend-specific geometry");
        }
        let mut summary_camera = r.camera;
        for target in outputs {
            let drawn = match &r.tree {
                Some(t) => crate::png::preview_png(&settings, t, &r.camera),
                None => crate::png::render_png(&settings, r.geometry.as_ref(), &r.camera),
            };
            let data = match drawn {
                Ok((d, cam)) => {
                    summary_camera = crate::png::summary_camera(&cam, &r.camera);
                    d
                }
                Err(e) => {
                    con.print_unfiltered(format!("neoscad: cannot export PNG: {e}").as_bytes());
                    out.exit_code = 1;
                    break;
                }
            };
            let written = if target == "-" {
                out.stdout.extend_from_slice(&data);
                Ok(())
            } else {
                std::fs::write(cwd.join(target), &data)
                    .map_err(|e| format!("ERROR: Can't write to '{target}': {e}"))
            };
            if let Err(line) = written {
                con.print_unfiltered(line.as_bytes());
                out.exit_code = 1;
                break;
            }
        }
        if out.exit_code == 0 {
            let facts = crate::summary::Facts {
                // A preview reports none, as the command line's does.
                cache_entries: if r.tree.is_some() { 0 } else { r.cache_entries },
                cache_bytes: if r.tree.is_some() { 0 } else { r.cache_bytes },
                cache_budget: r.cache_budget,
                elapsed_ms: started.elapsed().as_millis(),
                geometry: r.geometry.as_ref(),
                camera: &summary_camera,
            };
            let request = crate::summary::Request {
                options: strings(params, "summary"),
                file: None,
            };
            if !crate::summary::emit(&request, &facts, &mut con) {
                out.exit_code = 1;
            }
        }
    }
    let extra = con.take_records();
    out.stderr.extend(con.into_inner());
    if json_out {
        let run = crate::report::Run {
            command: "export",
            input,
            outputs: outputs
                .iter()
                .map(|o| (o.clone(), "png".to_string()))
                .collect(),
            exit_code: out.exit_code,
            timings: r.timings.json(),
            served: true,
        };
        let geometry = r
            .geometry
            .as_ref()
            .map(|g| session::stats::geometry(g, &scheme.geometry_scheme()));
        report(
            &mut out,
            &run,
            &r.log,
            &extra,
            geometry,
            outputs.iter().any(|o| o == "-"),
        );
    }
    out
}

/// Run a `cli.export` request on `session`, with names relative to `cwd`.
pub fn execute(session: &session::Session, params: &Value, cwd: &Path) -> Outcome {
    let s = |k: &str| params.get(k).and_then(Value::as_str);
    let b = |k: &str| params.get(k).and_then(Value::as_bool).unwrap_or(false);
    let strings = |k: &str| -> Vec<String> {
        params
            .get(k)
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect()
    };
    let (Some(input), outputs, formats) = (s("input"), strings("outputs"), strings("formats"))
    else {
        return Outcome::fail(1, "neoscad: cli.export needs an input");
    };
    if !formats.is_empty() && formats.iter().all(|f| f == "png") {
        return execute_png(session, params, input, &outputs, cwd);
    }
    let mut targets = Vec::new();
    for (o, f) in outputs.iter().zip(&formats) {
        match session::export::Format::from_id(f) {
            Some(fmt) => targets.push((o.clone(), fmt)),
            None => return Outcome::fail(1, format!("neoscad: cli.export cannot write '{f}'")),
        }
    }
    let scheme = match render::scheme::find(s("colorscheme").unwrap_or("")) {
        Some(sc) => sc,
        None => render::ColorScheme::cornfield(),
    };
    let export_options = ExportOptions::parse(&strings("export_options"));
    let run = run_of(params, input, cwd);
    let camera = run.camera;
    let req = session::ExportRequest {
        settings: crate::run::encode_settings(
            &export_options,
            scheme.geometry_scheme(),
            input,
            &camera,
            run.features,
        ),
        run,
        outputs: targets.clone(),
        force: b("force"),
        scheme,
    };
    let mut sink = Sink {
        cwd,
        stdout: Vec::new(),
        summary: crate::summary::Request {
            options: strings("summary"),
            file: None,
        },
    };
    let done = match session.export(&req, &mut sink) {
        Ok(d) => d,
        Err(c) => return Outcome::fail(1, format!("neoscad: {c}")),
    };
    let mut out = Outcome {
        exit_code: done.exit_code,
        stderr: done.log.stderr.clone(),
        stdout: sink.stdout,
    };
    if b("json") {
        let run = crate::report::Run {
            command: "export",
            input,
            outputs: targets
                .iter()
                .map(|(o, f)| (o.clone(), f.id().to_string()))
                .collect(),
            exit_code: done.exit_code,
            timings: done.timings.json(),
            served: true,
        };
        let geometry = done
            .geometry
            .as_ref()
            .map(|g| session::stats::geometry(g, &req.scheme.geometry_scheme()));
        // With `-o -` the data owns stdout; the report goes to stderr.
        let to_stderr = outputs.iter().any(|o| o == "-");
        report(&mut out, &run, &done.log, &[], geometry, to_stderr);
    }
    out
}
