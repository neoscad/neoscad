//! A document run and the exports, off the GTK main thread: what
//! `Core::run_document` and `Core::export_file` do for the macOS app
//! (`crates/ffi/src/document.rs`, `inspect.rs`), over the same `client`
//! calls, minus UniFFI and the language server (not in this milestone).
//!
//! The window's loop (`client::DocumentLoop`) decides when to run and
//! whether a result is still current; this module only runs.

use std::path::Path;
use std::sync::Arc;

use client::{Client, ConsoleLine, CoreError, RenderMode, RenderResult, RunPlan};

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
}

/// Run `plan` on `client`: evaluate and build the document as its request
/// asks, seen from `camera` (the program reads the view it is shown in,
/// as in OpenSCAD's GUI), and upload what it built to `gpu`.
pub fn run_document(
    client: &Client,
    plan: &RunPlan,
    camera: eval::Camera,
    scheme: &render::ColorScheme,
    gpu: Option<&render::viewport::Gpu>,
) -> Result<RunOutput, CoreError> {
    let (mut run, doc, text) = client.document_run(&plan.path, &plan.request)?;
    run.camera = camera;
    let mode = plan.request.mode;
    let r = client.session.render(&run, mode.into(), scheme)?;
    let scene = match (&r.tree, &r.geometry) {
        (Some(tree), _) => Some(render::preview::scene(
            tree,
            scheme,
            render::Previewer::OpenCsg,
        )),
        (None, Some(g)) => Some(render::Scene::new(Some(g), scheme)),
        (None, None) if r.exit_code == 0 => Some(render::Scene::new(None, scheme)),
        (None, None) => None,
    };
    let model = match (scene, gpu) {
        (Some(scene), Some(gpu)) => Some(Arc::new(gpu.upload(&scene).map_err(|e| {
            CoreError::Failed {
                message: e.to_string(),
            }
        })?)),
        _ => None,
    };
    let a = r.camera_assigned;
    let c = &r.camera;
    let file_view = a.any().then_some((
        a.vpt.then_some(c.vpt),
        a.vpr.then_some(c.vpr),
        a.vpd.then_some(c.vpd),
        a.vpf.then_some(c.vpf),
    ));
    let render = client::render_result(&r, scheme);
    Ok(RunOutput {
        generation: plan.generation,
        mode,
        summary: client::describe_render(&render, mode),
        console: client.console_lines(&r.log, &doc, text),
        render,
        model,
        file_view,
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
/// not cancel it) and with the customizer's values.
pub fn export_file(
    client: &Client,
    path: &str,
    output: &str,
    id: &str,
    options: &client::RunOptions,
) -> Result<client::ExportResult, CoreError> {
    let format = client::export_format(Some(id), output)?;
    let run = client.detached(path, options, None, None)?;
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
        let out =
            run_document(&c, &plan, camera(), &render::ColorScheme::cornfield(), None).unwrap();
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
        let out =
            run_document(&c, &plan, camera(), &render::ColorScheme::cornfield(), None).unwrap();
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
        )
        .unwrap();
        assert_eq!(r.exit_code, 0, "{}", r.console);
        // A binary STL of a cube: 84 header bytes and 12 triangles of 50.
        assert_eq!(r.bytes, 84 + 12 * 50);
        assert_eq!(std::fs::metadata(&stl).unwrap().len(), r.bytes);
        assert!(!dir.join(".m.stl.neoscad-export").exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
