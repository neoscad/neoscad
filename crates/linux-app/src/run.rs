//! A document run and the exports, off the GTK main thread: what
//! `Core::run_document` and `Core::export_file` do for the macOS app
//! (`crates/ffi/src/document.rs`, `inspect.rs`), over the same `client`
//! calls, minus UniFFI.
//!
//! The window's loop (`client::DocumentLoop`) decides when to run and
//! whether a result is still current; this module only runs.

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex, PoisonError};

use client::{Client, ConsoleLine, CoreError, RenderMode, RenderResult, RunPlan};

use crate::language::Language;

/// The `$vpt`, `$vpr`, `$vpd` and `$vpf` a file assigned (`None` for those
/// it did not): OpenSCAD's GUI moves its camera to them after a run.
pub type FileView = (Option<[f64; 3]>, Option<[f64; 3]>, Option<f64>, Option<f64>);

/// What one run hands back to the window.
#[derive(Debug)]
pub struct RunOutput {
    pub generation: u64,
    pub mode: RenderMode,
    pub render: RenderResult,
    /// The console's lines, in editor positions.
    pub console: Vec<ConsoleLine>,
    /// The one-line summary (`client::describe_render`).
    pub summary: String,
    /// The scene on the GPU, for `Viewport::set_model`; `None` without a
    /// GPU or when the run built nothing to show (a failed evaluation).
    pub model: Option<Arc<render::viewport::Model>>,
    pub file_view: Option<FileView>,
    /// The files the run read that another program could change, on this
    /// disk: what the window watches (`crate::watch`).
    pub files: Vec<PathBuf>,
}

/// Run `plan` on `client`: evaluate and build the document as its request
/// asks, seen from `camera` (the program reads the view it is shown in,
/// as in OpenSCAD's GUI), and upload what it built to `gpu`.
///
/// With `language`, the run's diagnostics become the editor's markers
/// (`crates/ffi/src/document.rs` does the same for the macOS app): the
/// evaluation's go to the server as soon as it ends, before the geometry
/// stage (which a large preview can spend seconds in), and the finished
/// run's again only if the geometry stage added to them. Both are
/// delivered through the server's sink from this thread, in that order,
/// so a later publication never arrives before an earlier one.
pub fn run_document(
    client: &Client,
    plan: &RunPlan,
    camera: eval::Camera,
    scheme: &render::ColorScheme,
    gpu: Option<&render::viewport::Gpu>,
    language: Option<&Language>,
) -> Result<RunOutput, CoreError> {
    let (mut run, doc, text) = client.document_run(&plan.path, &plan.request)?;
    run.camera = camera;
    let mode = plan.request.mode;
    let published: Arc<Mutex<Option<Vec<serde_json::Value>>>> = Arc::default();
    if let Some(ls) = language {
        let (ls, published) = (ls.clone(), published.clone());
        let (doc, text) = (doc.clone(), text.clone());
        run.on_evaluated = Some(Arc::new(move |log: &session::Log| {
            let diags = log.diagnostics_json();
            ls.supply(&doc, text.clone(), log);
            *published.lock().unwrap_or_else(PoisonError::into_inner) = Some(diags);
        }));
    }
    let r = client.session.render(&run, mode.into(), scheme)?;
    if let Some(ls) = language {
        let diags = r.log.diagnostics_json();
        let early = published
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if early.as_ref() != Some(&diags) {
            ls.supply(&doc, text.clone(), &r.log);
        }
    }
    let scene = client::run_scene(&r, scheme, render::Previewer::OpenCsg)?;
    let t0 = std::time::Instant::now();
    let model = match (scene, gpu) {
        (Some(scene), Some(gpu)) => Some(Arc::new(gpu.upload(&scene).map_err(|e| {
            CoreError::Failed {
                message: e.to_string(),
            }
        })?)),
        _ => None,
    };
    let upload_ms = t0.elapsed().as_secs_f64() * 1000.0;
    let a = r.camera_assigned;
    let c = &r.camera;
    let file_view = a.any().then_some((
        a.vpt.then_some(c.vpt),
        a.vpr.then_some(c.vpr),
        a.vpd.then_some(c.vpd),
        a.vpf.then_some(c.vpf),
    ));
    // The summary's time is how long the user waited for the model to
    // appear, and copying the scene into GPU buffers is part of that wait,
    // so the total counts it (as the macOS and Windows apps' core does,
    // `crates/ffi/src/document.rs`). Not the geometry time: the tooltip's
    // stages stay what the core computed.
    let mut render = client::render_result(&r, scheme);
    render.timings.total_ms += upload_ms;
    let files = crate::watch::on_disk(client.run_files(&r, &doc));
    Ok(RunOutput {
        generation: plan.generation,
        mode,
        summary: client::describe_render(&render, mode),
        console: client.console_lines(&r.log, &doc, text),
        render,
        model,
        file_view,
        files,
    })
}

/// Writes an export's one output through a temporary file beside it,
/// renamed over the target once complete: a failed or cancelled export
/// never leaves a truncated file where the user's old one was (as the
/// macOS app's `AtomicFile`, `crates/ffi/src/inspect.rs`).
#[derive(Debug, Default)]
pub struct AtomicFile {
    pub bytes: u64,
}

impl session::ExportSink for AtomicFile {
    fn write(&mut self, target: &str, data: &[u8]) -> Result<(), String> {
        let bytes = write_atomic(Path::new(target), data)
            .map_err(|e| format!("ERROR: Can't write to '{target}': {e}"))?;
        self.bytes = bytes;
        Ok(())
    }

    fn summary(&mut self, _: &session::SummaryFacts<'_>, _: &mut eval::Console<Vec<u8>>) -> bool {
        true
    }
}

/// Write `data` to `target` through `.<name>.neoscad-export` beside it,
/// renamed over it when complete. How many bytes.
pub fn write_atomic(target: &Path, data: &[u8]) -> std::io::Result<u64> {
    let name = target
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let temp = target.with_file_name(format!(".{name}.neoscad-export"));
    let result = std::fs::write(&temp, data).and_then(|()| std::fs::rename(&temp, target));
    if result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    result.map(|()| data.len() as u64)
}

/// Export the document at `path` to `output` in the core's format `id`
/// (`binstl`, `3mf`, ...), detached from the document's runs (typing does
/// not cancel it) and with the customizer's values. `interrupt` stops it
/// (the result is then `CoreError::Cancelled`, and a file already at
/// `output` is left as it was); `progress` hears each stage as it starts.
pub fn export_file(
    client: &Client,
    path: &str,
    output: &str,
    id: &str,
    options: &client::RunOptions,
    interrupt: Option<Arc<AtomicBool>>,
    progress: Option<session::Progress>,
) -> Result<client::ExportResult, CoreError> {
    let format = client::export_format(Some(id), output)?;
    let run = client.detached(path, options, interrupt, progress)?;
    let mut sink = AtomicFile::default();
    let export_options = client::ExportOptions {
        format: Some(id.to_string()),
        ..Default::default()
    };
    let mut r = client.export(
        run,
        output,
        format,
        &export_options,
        crate::host::iso8601_now(),
        &mut sink,
    )?;
    r.bytes = sink.bytes;
    Ok(r)
}

/// A contact sheet of standard views (`neoscad snapshot`) of the document,
/// `width` by `height` pixels, written to `output`, detached like
/// [`export_file`]. The bytes written, or why nothing was: the model's
/// first error, or the core's reason (no GPU, a cancel).
pub fn snapshot_file(
    client: &Client,
    path: &str,
    output: &Path,
    (width, height): (u32, u32),
    options: &client::RunOptions,
    interrupt: Option<Arc<AtomicBool>>,
) -> Result<u64, CoreError> {
    use session::snapshot::{SnapshotError, SnapshotRequest};
    let run = client.detached(path, options, interrupt, None)?;
    let mut req = SnapshotRequest::new(run, output.to_string_lossy());
    req.size = (width, height);
    let s = client.session.snapshot(&req).map_err(|e| match e {
        SnapshotError::Cancelled => CoreError::Cancelled,
        SnapshotError::Failed(message) => CoreError::Failed { message },
    })?;
    let png = match (s.exit_code, s.png) {
        (0, Some(png)) => png,
        _ => {
            return Err(CoreError::Failed {
                message: client::first_error(&client::console(&s.log))
                    .unwrap_or_else(|| "The model did not render.".into()),
            });
        }
    };
    write_atomic(output, &png).map_err(|e| CoreError::Failed {
        message: format!("Can't write to '{}': {e}", output.display()),
    })
}

/// An export's stage as its progress toast says it (the macOS app's
/// export sheet says the same).
pub fn stage_label(stage: session::Stage) -> &'static str {
    match stage {
        session::Stage::Parse => "Reading the files",
        session::Stage::Evaluate => "Evaluating",
        session::Stage::Geometry => "Building the geometry and writing",
        session::Stage::Draw => "Drawing",
    }
}

/// An export that failed, as its alert says it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportFailure {
    /// The file was written all the same: a model whose fillet or chamfer
    /// calls failed is written with those edges sharp, and the export
    /// still fails (`docs/fillets.md`, section 18, decision 2). The alert
    /// then says it was exported, not that it was not.
    pub written: bool,
    pub message: String,
}

impl ExportFailure {
    pub fn not_written(message: String) -> Self {
        Self {
            written: false,
            message,
        }
    }
}

/// The message for a finished export of `output`: what was written, or
/// why nothing was (the core's reason, never silence). `Ok` for a toast,
/// `Err` for an alert. A STEP export adds its report: the share of exact
/// faces, and on further lines (an alert rather than a toast) the regions
/// written as facets; refused, the report is the reason. A file written
/// with failed fillets is an `Err` that is `written`: "Wrote part.step
/// (N bytes), but 1 fillet_edges() call failed and its edges are sharp:
/// ...", so the user never takes the sharp part for the rounded one.
pub fn export_message(
    output: &Path,
    r: &Result<client::ExportResult, CoreError>,
) -> Option<Result<String, ExportFailure>> {
    let name = output.file_name().map_or_else(
        || output.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    );
    Some(match r {
        Err(CoreError::Cancelled) => return None,
        Err(e) => Err(ExportFailure::not_written(e.to_string())),
        Ok(r) if r.written && !r.fillet_errors.is_empty() => {
            let mut message = format!(
                "Wrote {name} ({} bytes), but {}",
                r.bytes,
                session::fillets::failure_text(&r.fillet_errors)
            );
            if let Some(step) = &r.step {
                message.push_str("\n\n");
                message.push_str(&step.summary);
            }
            Err(ExportFailure {
                written: true,
                message,
            })
        }
        Ok(r) => match (client::export_failure_reason(r), &r.step) {
            (None, Some(step)) => Ok(format!(
                "Exported {name} ({} bytes). {}",
                r.bytes, step.summary
            )),
            (None, None) => Ok(format!("Exported {name} ({} bytes)", r.bytes)),
            (Some(_), Some(step)) => Err(ExportFailure::not_written(step.summary.clone())),
            (Some(why), None) => Err(ExportFailure::not_written(why)),
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use client::DocumentLoop;

    fn client() -> Client {
        // The disk plus MCAD, no GPU: runs evaluate and build, nothing is
        // uploaded.
        let mut cfg = crate::host::config();
        cfg.gpu = None;
        cfg.limits = session::Limits::AGENT;
        Client::new(cfg)
    }

    fn camera() -> eval::Camera {
        eval::Camera {
            vpt: [0.0; 3],
            vpr: [55.0, 0.0, 25.0],
            vpd: 140.0,
            vpf: 22.5,
            auto: false,
            locked: false,
        }
    }

    #[test]
    fn a_run_reports_the_console_and_the_files_view() {
        let c = client();
        let path = "/nonexistent-neoscad-linux-app/a.scad";
        c.open(path, Some("$vpd = 300; echo(\"hi\"); cube(2);".into()))
            .unwrap();
        let mut l = DocumentLoop::new(client::DEFAULT_PREVIEW_DELAY_MS);
        l.schedule(0);
        let mode = l.due(1_000).unwrap();
        let plan = l.begin_run(mode, path).unwrap();
        let out = run_document(
            &c,
            &plan,
            camera(),
            &render::ColorScheme::cornfield(),
            None,
            None,
        )
        .unwrap();
        assert_eq!(out.render.exit_code, 0);
        assert!(l.is_current(out.generation));
        assert!(
            out.console.iter().any(|l| l.text.contains("ECHO: \"hi\"")),
            "{:?}",
            out.console
        );
        assert_eq!(out.file_view, Some((None, None, Some(300.0), None)));
        assert!(out.summary.starts_with("Previewed in "), "{}", out.summary);
        assert!(out.model.is_none(), "no GPU given");
    }

    #[test]
    fn a_render_counts_the_geometry_and_an_export_writes_atomically() {
        let c = client();
        let dir =
            std::env::temp_dir().join(format!("neoscad-linux-app-run-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("m.scad");
        let path = path.to_str().unwrap();
        c.open(path, Some("cube(3);".into())).unwrap();
        let mut l = DocumentLoop::new(0);
        let plan = l.begin_run(RenderMode::Render, path).unwrap();
        let out = run_document(
            &c,
            &plan,
            camera(),
            &render::ColorScheme::cornfield(),
            None,
            None,
        )
        .unwrap();
        assert!(
            out.summary.contains("3D, bbox 3 × 3 × 3"),
            "{}",
            out.summary
        );

        let stl = dir.join("m.stl");
        let r = export_file(
            &c,
            path,
            stl.to_str().unwrap(),
            "binstl",
            &client::RunOptions::default(),
            None,
            None,
        )
        .unwrap();
        assert_eq!(r.exit_code, 0, "{}", r.console);
        // A binary STL of a cube: 84 header bytes and 12 triangles of 50.
        assert_eq!(r.bytes, 84 + 12 * 50);
        assert_eq!(std::fs::metadata(&stl).unwrap().len(), r.bytes);
        assert!(!dir.join(".m.stl.neoscad-export").exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Every geometry format File > Export offers writes a file of a model
    /// of its dimension, reporting its stages; the wrong dimension fails
    /// with the core's reason; a cancelled export leaves the old file.
    #[test]
    fn every_export_format_writes_or_says_why_not() {
        let c = client();
        let dir =
            std::env::temp_dir().join(format!("neoscad-linux-app-export-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let solid = dir.join("solid.scad");
        let flat = dir.join("flat.scad");
        let (solid, flat) = (solid.to_str().unwrap(), flat.to_str().unwrap());
        c.open(solid, Some("cube(3);".into())).unwrap();
        c.open(flat, Some("square(3);".into())).unwrap();
        let geometry: Vec<_> = client::export_formats()
            .into_iter()
            .filter(|f| f.kind == client::ExportKind::Geometry)
            .collect();
        assert!(geometry.len() >= 8, "{geometry:?}");
        for f in &geometry {
            let doc = if f.dimension == Some(2) { flat } else { solid };
            let out = dir.join(format!("out-{}.{}", f.id, f.extension));
            let stages: Arc<Mutex<Vec<&'static str>>> = Arc::default();
            let s = stages.clone();
            let progress: session::Progress = Arc::new(move |st| {
                s.lock().unwrap().push(stage_label(st));
            });
            let r = export_file(
                &c,
                doc,
                out.to_str().unwrap(),
                &f.id,
                &client::RunOptions::default(),
                None,
                Some(progress),
            );
            let message = export_message(&out, &r).unwrap();
            assert!(message.is_ok(), "{}: {message:?}", f.id);
            assert!(std::fs::metadata(&out).unwrap().len() > 0, "{}", f.id);
            assert!(
                stages.lock().unwrap().contains(&"Evaluating"),
                "{}: {:?}",
                f.id,
                stages.lock().unwrap()
            );
        }
        // A square to a 3D format: the reason, not silence.
        let out = dir.join("square.3mf");
        let r = export_file(
            &c,
            flat,
            out.to_str().unwrap(),
            "3mf",
            &client::RunOptions::default(),
            None,
            None,
        );
        let why = export_message(&out, &r).unwrap().unwrap_err().message;
        assert!(why.contains("3D"), "{why}");
        // STEP: refused with the reason without Preferences > Language's
        // `exact`, and with it written with the report.
        let out = dir.join("solid.step");
        let r = export_file(
            &c,
            solid,
            out.to_str().unwrap(),
            "step",
            &client::RunOptions::default(),
            None,
            None,
        );
        let why = export_message(&out, &r).unwrap().unwrap_err().message;
        assert!(why.contains("--enable exact"), "{why}");
        assert!(!out.exists());
        let exact = client::RunOptions {
            enable: vec!["exact".into()],
            ..Default::default()
        };
        let r = export_file(&c, solid, out.to_str().unwrap(), "step", &exact, None, None);
        let message = export_message(&out, &r).unwrap().unwrap();
        assert!(
            message.contains("STEP: 6 of 6 faces exact (100%)."),
            "{message}"
        );
        assert!(
            std::fs::read_to_string(&out)
                .unwrap()
                .starts_with("ISO-10303-21;")
        );
        assert!(
            client::export_formats_with(&exact.enable)
                .iter()
                .any(|f| f.id == "step")
        );
        // Cancelled before it starts: nothing is reported and the file
        // from before is still there.
        let out = dir.join("out-binstl.stl");
        let before = std::fs::read(&out).unwrap();
        let r = export_file(
            &c,
            solid,
            out.to_str().unwrap(),
            "binstl",
            &client::RunOptions::default(),
            Some(Arc::new(AtomicBool::new(true))),
            None,
        );
        assert!(export_message(&out, &r).is_none(), "{r:?}");
        assert_eq!(std::fs::read(&out).unwrap(), before);
        // Without a GPU the snapshot says why it made nothing.
        let png = dir.join("sheet.png");
        let r = snapshot_file(
            &c,
            solid,
            &png,
            (256, 256),
            &client::RunOptions::default(),
            None,
        );
        assert!(r.is_err() && !png.exists(), "{r:?}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// File > Export > STEP as a window runs it: with the `--enable`
    /// names Preferences > Language keeps (`extensions::Settings::names`,
    /// which `run_options` hands every export), the message the window
    /// shows (`export_message`; one with further lines is an alert, not a
    /// toast) and the file. The macOS app's `ExportTests` check the same
    /// mixed model's report.
    #[test]
    fn a_step_export_reports_its_substitutions_fillets_and_refusals() {
        use crate::extensions::Settings;
        let c = client();
        let dir =
            std::env::temp_dir().join(format!("neoscad-linux-app-step-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let options = |s: Settings| client::RunOptions {
            enable: s.names(),
            ..Default::default()
        };
        let export = |name: &str, text: &str, s: Settings| {
            let doc = dir.join(format!("{name}.scad"));
            let doc = doc.to_str().unwrap();
            c.open(doc, Some(text.into())).unwrap();
            let out = dir.join(format!("{name}.step"));
            let r = export_file(
                &c,
                doc,
                out.to_str().unwrap(),
                "step",
                &options(s),
                None,
                None,
            );
            (export_message(&out, &r).unwrap(), r, out)
        };
        let exact = Settings {
            exact: true,
            ..Default::default()
        };

        // A hole made exact and a hull written as facets: the share of
        // exact faces, the curve counted and the faceted region at its
        // line, in an alert since it has more than one line.
        let (message, r, out) = export(
            "part",
            "difference() {\n  cube(20);\n  translate([10, 10, -1]) cylinder(r = 4, h = 22);\n\
             \x20 hull() { cube(1); translate([2, 2, 2]) cube(1); }\n}\n",
            exact,
        );
        let message = message.unwrap();
        assert!(message.starts_with("Exported part.step ("), "{message}");
        assert!(
            message.contains(
                "STEP: 7 of 16 faces exact (43.8%).\n1 curve made exact.\n\
                 Faceted: hull() at part.scad, line 4 is exported as planar facets"
            ),
            "{message}"
        );
        let step = r.unwrap().step.unwrap();
        assert!(step.ok && step.faceted_regions.len() == 1, "{step:?}");
        let text = std::fs::read_to_string(&out).unwrap();
        assert!(text.starts_with("ISO-10303-21;"));
        assert_eq!(text.matches("CYLINDRICAL_SURFACE").count(), 1);

        // Fillets on as well: the blends are exact cylinders, one per
        // edge, and the whole part is exact.
        let both = Settings {
            exact: true,
            fillet: true,
            ..Default::default()
        };
        let filleted = "fillet_edges(r = 1, edges = \"|y\") cube([20, 10, 4]);\n";
        let (message, _, out) = export("filleted", filleted, both);
        let message = message.unwrap();
        assert!(
            message.contains("STEP: 10 of 10 faces exact (100%)."),
            "{message}"
        );
        let text = std::fs::read_to_string(&out).unwrap();
        assert_eq!(text.matches("CYLINDRICAL_SURFACE").count(), 4);
        // A radius that does not fit (decision 2): the part is written
        // with those edges sharp, and the alert says so rather than
        // "Exported" (or "Was Not Exported").
        let sharp = filleted.replacen("r = 1", "r = 3", 1);
        let (message, r, sharp_out) = export("sharp", &sharp, both);
        let failure = message.unwrap_err();
        assert!(failure.written);
        assert!(
            failure.message.starts_with(
                "Wrote sharp.step (6419 bytes), but 1 fillet_edges() call failed and its edges \
                 are sharp: fillet_edges(): the blends of edges 1 and 2 overlap"
            ),
            "{}",
            failure.message
        );
        assert!(
            failure
                .message
                .ends_with("\n\nSTEP: 6 of 6 faces exact (100%)."),
            "{}",
            failure.message
        );
        let r = r.unwrap();
        assert_eq!((r.exit_code, r.fillet_errors.len()), (1, 1));
        assert!(
            std::fs::read_to_string(&sharp_out)
                .unwrap()
                .starts_with("ISO-10303-21;")
        );
        // With `fillet` off, `fillet_edges()` is OpenSCAD's unknown
        // module: nothing is left to export, and the alert says so.
        std::fs::remove_file(&out).unwrap();
        let (message, _, out) = export("filleted", filleted, exact);
        let why = message.unwrap_err().message;
        assert!(why.contains("empty"), "{why}");
        assert!(!out.exists());

        // Refused with `exact` on: a fin of no thickness cannot be a
        // valid B-rep. The report is the alert, and no file is written.
        let (message, r, out) = export(
            "fin",
            "cube(10); translate([10, 0, 0]) cube([10, 10, 0.000001]);\n",
            exact,
        );
        let why = message.unwrap_err().message;
        assert!(why.starts_with("STEP export refused: "), "{why}");
        assert!(why.ends_with("No file was written."), "{why}");
        assert!(!r.unwrap().step.unwrap().ok);
        assert!(!out.exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A run reports the included files on disk, which the window
    /// watches, and not the document itself.
    #[test]
    fn a_run_names_its_included_files() {
        let c = client();
        let dir =
            std::env::temp_dir().join(format!("neoscad-linux-app-files-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let inc = dir.join("inc.scad");
        std::fs::write(&inc, "module part() cube(1);").unwrap();
        let doc = dir.join("main.scad");
        let path = doc.to_str().unwrap();
        c.open(path, Some("include <inc.scad>\npart();".into()))
            .unwrap();
        let mut l = DocumentLoop::new(0);
        let plan = l.begin_run(RenderMode::Preview, path).unwrap();
        let out = run_document(
            &c,
            &plan,
            camera(),
            &render::ColorScheme::cornfield(),
            None,
            None,
        )
        .unwrap();
        // The core names files by their real path (macOS's temporary
        // folder is a symlink into /private).
        assert_eq!(out.files, [inc.canonicalize().unwrap()]);
        // The include changes on disk: the loop schedules a preview.
        assert_eq!(l.files_changed(10), None);
        assert!(l.next_due_ms().is_some());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A run with the editor's server: the diagnostics become markers for
    /// the client's version with the text the run read.
    #[test]
    fn a_runs_diagnostics_become_the_editors_markers() {
        use crate::language::{self, tests as lt};
        use serde_json::json;
        let c = lt::client();
        let (ls, rx) = lt::server_over(c.clone());
        ls.send(lt::msg(
            json!({"jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": {"capabilities": {}}}),
        ));
        lt::next(&rx);
        let path = "/nonexistent-neoscad-linux-app/lsp.scad";
        let uri = language::document_uri(path);
        let text = "echo(nope); cube(1);";
        ls.send(lt::msg(
            json!({"jsonrpc": "2.0", "method": "textDocument/didOpen", "params": {
            "textDocument": {"uri": uri, "languageId": "openscad", "version": 4, "text": text}}}),
        ));
        // The worker answers in order: once this is answered, didOpen
        // has been handled.
        ls.send(lt::msg(
            json!({"jsonrpc": "2.0", "id": 2, "method": "textDocument/hover",
            "params": {"textDocument": {"uri": uri}, "position": {"line": 0, "character": 13}}}),
        ));
        assert_eq!(lt::next(&rx)[0]["id"], 2);
        c.open(path, Some(text.into())).unwrap();
        let mut l = DocumentLoop::new(0);
        let plan = l.begin_run(RenderMode::Preview, path).unwrap();
        run_document(
            &c,
            &plan,
            camera(),
            &render::ColorScheme::cornfield(),
            None,
            Some(&ls),
        )
        .unwrap();
        let out = lt::next(&rx);
        assert_eq!(out[0]["method"], "textDocument/publishDiagnostics");
        assert_eq!(out[0]["params"]["uri"], uri);
        assert_eq!(out[0]["params"]["version"], 4);
        let diags = out[0]["params"]["diagnostics"].as_array().unwrap();
        assert!(
            diags
                .iter()
                .any(|d| d["message"].as_str().unwrap_or("").contains("nope")),
            "{diags:?}"
        );
        // A preview adds no geometry warnings: one publication, not two.
        assert!(
            rx.recv_timeout(std::time::Duration::from_millis(300))
                .is_err()
        );
        ls.stop();
    }
}
