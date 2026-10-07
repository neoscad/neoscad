//! Tier 3: checking neoscad's geometry against OpenSCAD's expected images.
//!
//! OpenSCAD checks geometry by rendering it to PNG. Matching its renderer
//! pixel for pixel is not a goal of neoscad (docs/architecture.md, tier 3),
//! so a tier 3 case splits the work: neoscad produces the geometry as a
//! mesh file, the pinned OpenSCAD nightly (`--renderer`) renders that mesh
//! with the test's own arguments, and the PNG is compared with the expected
//! image by OpenSCAD's own comparator ([`crate::image_compare`]).
//!
//! Two kinds of case take this path:
//!
//! - **Direct renders** (`render-manifold`, `render-force-manifold`, ...):
//!   upstream runs `openscad in.scad ARGS -o out.png`. Here the binary under
//!   test runs `in.scad ARGS -o in.off`, and the renderer runs a one-line
//!   wrapper, `import(file);`, with `-D file="…/in.off"` and the same ARGS.
//!   OFF is the export that keeps what the render shows: OpenSCAD's Manifold
//!   export writes one colour per face, including the colour scheme's
//!   front colour and the "back" colour it gives faces cut by a
//!   `difference()` (`ManifoldGeometry::toPolySet`,
//!   `src/geometry/manifold/ManifoldGeometry.cc:147-208`; written by
//!   `src/io/export_off.cc:61-78`), and the OFF importer reads those colours
//!   back (`src/io/import_off.cc:268-282`). STL and OBJ would lose the green
//!   cut faces and every `color()`. A 2D result (OFF export refuses it with
//!   "not a 3D object") is exported as SVG instead, and an empty one
//!   ("Current top level object is empty.") renders an empty wrapper, which
//!   draws the same empty scene a direct render of nothing does.
//! - **Export/import tests** (`export_import_pngtest.py`): the upstream test
//!   already exports with the binary under test and re-imports into a
//!   wrapper, so it is ported step for step (see [`GeometryEnv::export_import`])
//!   with the renderer doing the second step.
//!
//! Running the whole suite with `--binary` pointing at the nightly measures
//! the pipeline's ceiling: whatever fails then is a harness artefact, not a
//! geometry bug. Known artefacts are listed with a reason in
//! `conformance/tier3-limits.json`; a listed case that fails is reported as
//! skipped with that reason, and one that passes counts as a pass.
//!
//! The renderer's PNGs are cached under `target/conformance/image-cache`,
//! keyed by the renderer, its arguments and the mesh bytes, so a rerun only
//! renders meshes that changed.

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::ctx::Ctx;
use crate::image_compare;
use crate::manifest::Case;

/// The wrapper the renderer runs; `file` comes from `-D`, so one wrapper
/// serves every case and nothing is written per case but the mesh.
const WRAPPER: &str = "import(file);\n";

/// A process failure: the reason line for the report and the full stderr,
/// which callers inspect for OpenSCAD's export messages.
#[derive(Debug)]
pub struct Failure {
    pub reason: String,
    pub stderr: String,
}

/// Run `cmd` with its stderr going to `stderr_path`, killing it after
/// `timeout`. Returns the exit code (`None` for a signal); an error is a
/// failure to run or wait, or the timeout.
pub fn exec_status(
    cmd: &mut Command,
    timeout: Duration,
    stderr_path: &Path,
) -> Result<Option<i32>, String> {
    let err_file =
        File::create(stderr_path).map_err(|e| format!("{}: {e}", stderr_path.display()))?;
    cmd.stderr(err_file);
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("cannot run {:?}: {e}", cmd.get_program()))?;
    let start = Instant::now();
    // Start polling finely so the recorded time of a fast case is not
    // dominated by the poll interval, then back off for slow ones.
    let mut poll = Duration::from_micros(200);
    loop {
        match child.try_wait() {
            Ok(Some(s)) => return Ok(s.code()),
            Ok(None) if start.elapsed() > timeout => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("timeout after {:.0}s", timeout.as_secs_f64()));
            }
            Ok(None) => {
                std::thread::sleep(poll);
                poll = (poll * 2).min(Duration::from_millis(10));
            }
            Err(e) => return Err(format!("wait failed: {e}")),
        }
    }
}

/// Run `cmd` with its stderr going to `stderr_path`, killing it after
/// `timeout`. A non-zero exit is a failure whose reason carries the first
/// explanatory stderr line.
pub fn exec(cmd: &mut Command, timeout: Duration, stderr_path: &Path) -> Result<(), Failure> {
    let code = exec_status(cmd, timeout, stderr_path).map_err(|reason| Failure {
        reason,
        stderr: String::new(),
    })?;
    if code == Some(0) {
        return Ok(());
    }
    let mut stderr = String::new();
    if let Ok(mut f) = File::open(stderr_path) {
        let mut bytes = Vec::new();
        let _ = f.read_to_end(&mut bytes);
        stderr = String::from_utf8_lossy(&bytes).into_owned();
    }
    // The line that explains the exit: neoscad's own refusal or an error,
    // else the first line (echo and warnings usually come first).
    let first = stderr
        .lines()
        .find(|l| l.starts_with("neoscad:") || l.starts_with("ERROR:"))
        .or_else(|| stderr.lines().find(|l| !l.trim().is_empty()))
        .unwrap_or_default();
    let code = code.map_or("signal".to_string(), |c| format!("exit {c}"));
    let reason = if first.is_empty() {
        code
    } else {
        format!("{code}: {first}")
    };
    Err(Failure { reason, stderr })
}

#[derive(Debug, Default, Deserialize)]
struct Limits {
    #[serde(default)]
    limits: BTreeMap<String, String>,
}

/// Per-run state for image cases.
#[derive(Debug)]
pub struct GeometryEnv {
    pub renderer: PathBuf,
    /// Identifies the renderer build in cache keys.
    renderer_id: String,
    cache_dir: PathBuf,
    wrapper: PathBuf,
    empty_wrapper: PathBuf,
    /// Known harness artefacts: test id → reason.
    pub limits: BTreeMap<String, String>,
}

/// How one image case ended, before it becomes an `Outcome`.
#[derive(Debug)]
pub enum Result3 {
    Pass,
    Fail(String),
}

/// The variable that replaces neoscad's bundled fonts (OpenSCAD's
/// `<resources>/fonts`) with a directory. Runs remove it from the
/// environment: neoscad's bundled Liberation 2.00.1 is byte-identical to
/// the reference checkout's `fonts/`, which made the expected outputs
/// (`crates/assets` tests this), so the suite runs with the fonts neoscad
/// ships, and a developer's own setting can't change the results.
/// OpenSCAD ignores the variable.
/// Set for every neoscad the harness runs: a `neoscad serve` the user has
/// running must not answer the harness's exports. Its output should be the
/// same, but the suite measures the binary under test, in its own process.
pub const NO_SERVER_VAR: &str = "NEOSCAD_NO_SERVER";

pub const FONT_DIR_VAR: &str = "NEOSCAD_FONT_DIR";

/// The parts of the process environment a case needs from the runner.

#[derive(Debug)]
pub struct CaseEnv<'a> {
    pub ref_root: &'a Path,
    pub ref_str: &'a str,
    pub work_dir: &'a Path,
    pub actual_dir: &'a Path,
    pub binary: &'a Path,
    pub timeout: Duration,
    pub font_path: &'a Path,
    pub library_path: &'a Path,
    /// Added to every run of the binary under test (`--extra-enable`).
    pub extra_args: &'a [String],
}

impl GeometryEnv {
    pub fn new(ctx: &Ctx, renderer: &Path) -> Result<GeometryEnv, String> {
        if !renderer.is_file() {
            return Err(format!(
                "renderer {} not found (tier 3 needs the pinned OpenSCAD nightly)",
                renderer.display()
            ));
        }
        let out = Command::new(renderer)
            .arg("--version")
            .output()
            .map_err(|e| format!("cannot run {}: {e}", renderer.display()))?;
        // OpenSCAD prints its version on stderr.
        let version = String::from_utf8_lossy(&out.stderr).trim().to_string()
            + String::from_utf8_lossy(&out.stdout).trim();
        let renderer_id = format!("{}\n{version}", renderer.display());
        let base = ctx.repo.join("target/conformance");
        let cache_dir = base.join("image-cache");
        fs::create_dir_all(&cache_dir).map_err(|e| e.to_string())?;
        let wrapper = base.join("image-wrapper.scad");
        let empty_wrapper = base.join("image-wrapper-empty.scad");
        fs::write(&wrapper, WRAPPER).map_err(|e| e.to_string())?;
        fs::write(&empty_wrapper, "").map_err(|e| e.to_string())?;
        let limits_path = ctx.repo.join("conformance/tier3-limits.json");
        let limits: Limits = match fs::read_to_string(&limits_path) {
            Ok(t) => {
                serde_json::from_str(&t).map_err(|e| format!("{}: {e}", limits_path.display()))?
            }
            Err(_) => Limits::default(),
        };
        Ok(GeometryEnv {
            renderer: renderer.to_path_buf(),
            renderer_id,
            cache_dir,
            wrapper,
            empty_wrapper,
            limits: limits.limits,
        })
    }

    /// Run one geometry case: compare its PNG with the expected image, or
    /// validate its STL.
    pub fn run(&self, env: &CaseEnv<'_>, c: &Case) -> Result3 {
        let prepared = match self.prepare(env, c) {
            Ok(p) => p,
            Err(reason) => return Result3::Fail(reason),
        };
        let (input, expected, out_dir, basename, args) = prepared;
        let result = match c.script.as_deref() {
            None => self.direct(env, &input, &out_dir, &basename, &args),
            Some(s) if s.ends_with("export_import_pngtest.py") => {
                self.export_import(env, &input, &out_dir, &basename, &args)
            }
            Some(s) if s.ends_with("stlexportsanitytest.py") => {
                return match self.stl_sanity(env, &input, &out_dir, &basename, &args) {
                    Ok(()) => Result3::Pass,
                    Err(e) => Result3::Fail(e),
                };
            }
            Some(s) => Err(format!("no geometry runner for script {s}")),
        };
        match result.and_then(|png| {
            image_compare::compare_files(&expected, &png).map_err(|e| format!("image compare: {e}"))
        }) {
            Err(reason) => Result3::Fail(reason),
            Ok(cmp) if cmp.passed() => Result3::Pass,
            Ok(cmp) => Result3::Fail(format!("image differs: {}", cmp.describe())),
        }
    }

    /// Resolve a case's paths and arguments.
    fn prepare(
        &self,
        env: &CaseEnv<'_>,
        c: &Case,
    ) -> Result<(PathBuf, PathBuf, PathBuf, String, Vec<String>), String> {
        let (Some(input), Some(expected)) = (&c.input, &c.expected) else {
            return Err("manifest case lacks input or expected path".into());
        };
        let input = env.ref_root.join(input);
        let expected = env.ref_root.join(expected);
        if !input.is_file() {
            return Err(format!("missing input {}", input.display()));
        }
        if !expected.is_file() {
            return Err(format!("missing expected output {}", expected.display()));
        }
        let basename =
            c.id.strip_prefix(&format!("{}_", c.group))
                .unwrap_or(&c.id)
                .to_string();
        let out_dir = env.actual_dir.join(&c.group);
        fs::create_dir_all(&out_dir).map_err(|e| e.to_string())?;
        let _ = fs::remove_file(out_dir.join(format!("{basename}-actual.png")));
        let args: Vec<String> = c
            .args
            .iter()
            .map(|a| {
                a.replace("{REF}", env.ref_str)
                    .replace("{OPENSCAD}", &env.binary.to_string_lossy())
            })
            .collect();
        Ok((input, expected, out_dir, basename, args))
    }

    /// `tests/stlexportsanitytest.py`: export an STL with the binary under
    /// test (which must succeed, `check_call`) and validate it as
    /// `validatestl.py` does. The registered expected file is empty and so
    /// is the output file the harness creates, so validation decides.
    fn stl_sanity(
        &self,
        env: &CaseEnv<'_>,
        input: &Path,
        out_dir: &Path,
        basename: &str,
        args: &[String],
    ) -> Result<(), String> {
        let remaining: Vec<&String> = args
            .iter()
            .filter(|a| !a.starts_with("--openscad="))
            .collect();
        let stl = out_dir.join(format!("{basename}-actual.txt.stl"));
        let _ = fs::remove_file(&stl);
        let stderr = out_dir.join(format!("{basename}-export.stderr"));
        let mut cmd = self.command(env, env.binary);
        cmd.arg(input).arg("-o").arg(&stl).args(remaining);
        exec(&mut cmd, env.timeout, &stderr).map_err(|f| format!("export: {}", f.reason))?;
        let data = fs::read(&stl).map_err(|e| format!("export: {}: {e}", stl.display()))?;
        crate::validatestl::validate(&data)
    }

    fn command(&self, env: &CaseEnv<'_>, program: &Path) -> Command {
        let mut cmd = Command::new(program);
        cmd.current_dir(env.work_dir)
            .env("OPENSCAD_FONT_PATH", env.font_path)
            .env_remove(FONT_DIR_VAR)
            .env(NO_SERVER_VAR, "1")
            .env("OPENSCADPATH", env.library_path)
            .stdin(Stdio::null())
            .stdout(Stdio::null());
        // Only the binary under test: the renderer is the reference
        // OpenSCAD, which knows none of NeoSCAD's names.
        if program == env.binary {
            cmd.args(env.extra_args);
        }
        cmd
    }

    /// A direct `--render` test: export a mesh with the binary under test,
    /// then render it.
    fn direct(
        &self,
        env: &CaseEnv<'_>,
        input: &Path,
        out_dir: &Path,
        basename: &str,
        args: &[String],
    ) -> Result<PathBuf, String> {
        let png = out_dir.join(format!("{basename}-actual.png"));
        let stderr = out_dir.join(format!("{basename}-export.stderr"));
        let mut mesh = None;
        let mut empty = false;
        for ext in ["off", "svg"] {
            let target = out_dir.join(format!("{basename}-actual.{ext}"));
            let _ = fs::remove_file(&target);
            let mut cmd = self.command(env, env.binary);
            cmd.arg(input).args(args).arg("-o").arg(&target);
            match exec(&mut cmd, env.timeout, &stderr) {
                Ok(()) => {
                    mesh = Some(target);
                    break;
                }
                // OpenSCAD's export refuses these with exit 1 (`openscad.cc`,
                // the checks before `exportFileByName`), where a direct
                // render would still draw them: a 2D result is tried again
                // as SVG, and an empty one draws the empty scene.
                Err(f)
                    if f.stderr
                        .contains("Current top level object is not a 3D object.") =>
                {
                    continue;
                }
                Err(f) if f.stderr.contains("Current top level object is empty.") => {
                    empty = true;
                    break;
                }
                Err(f) => return Err(format!("export: {}", f.reason)),
            }
        }
        if mesh.is_none() && !empty {
            return Err("export: result is neither 3D, 2D nor empty".into());
        }
        let mut render_args = viewport_defines(input);
        render_args.extend(args.iter().cloned());
        self.render(env, mesh.as_deref(), &render_args, &png, out_dir, basename)?;
        Ok(png)
    }

    /// `tests/export_import_pngtest.py`, step by step: export with the
    /// binary under test (every `--render…` argument becomes
    /// `--render=force`, `:120`, and `asciistl`/`binstl` add
    /// `--export-format`, `:122-123`), then import the export in a wrapper
    /// and render it with the original arguments (`:143-149`).
    fn export_import(
        &self,
        env: &CaseEnv<'_>,
        input: &Path,
        out_dir: &Path,
        basename: &str,
        args: &[String],
    ) -> Result<PathBuf, String> {
        let png = out_dir.join(format!("{basename}-actual.png"));
        let mut format = None;
        let mut remaining = Vec::new();
        let mut it = args.iter();
        while let Some(a) = it.next() {
            if a.starts_with("--openscad=") {
                continue;
            } else if let Some(f) = a.strip_prefix("--format=") {
                format = Some(f.to_lowercase());
            } else if a == "--format" {
                format = it.next().map(|f| f.to_lowercase());
            } else if a == "--require-manifold" {
                return Err("--require-manifold (validatestl.py) is not ported".into());
            } else {
                remaining.push(a.clone());
            }
        }
        let format = format.ok_or("export_import_pngtest.py case without --format")?;
        let (format, export_format) = match format.as_str() {
            "asciistl" => ("stl".to_string(), Some("asciistl")),
            "binstl" => ("stl".to_string(), Some("binstl")),
            _ => (format, None),
        };
        if format == "csg" {
            return Err("the csg export/import form is not ported".into());
        }
        // `exportfile = join(outputdir, inputfilename) + "." + format`
        // (`:104-107`); the input is always a .scad here.
        let file_name = input
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let target = out_dir.join(format!("{file_name}.{format}"));
        let _ = fs::remove_file(&target);
        let mut tmpargs: Vec<String> = remaining
            .iter()
            .map(|a| {
                if a.starts_with("--render") {
                    "--render=force".into()
                } else {
                    a.clone()
                }
            })
            .collect();
        if let Some(f) = export_format {
            tmpargs.extend(["--export-format".into(), f.into()]);
        }
        let stderr = out_dir.join(format!("{basename}-export.stderr"));
        let mut cmd = self.command(env, env.binary);
        cmd.arg(input).arg("-o").arg(&target).args(&tmpargs);
        exec(&mut cmd, env.timeout, &stderr).map_err(|f| format!("export: {}", f.reason))?;
        if !target.is_file() {
            // The script's `os.stat(exportfile)` raises here (`:130`).
            return Err("export: no file written".into());
        }
        self.render(env, Some(&target), &remaining, &png, out_dir, basename)?;
        Ok(png)
    }

    /// Render `mesh` (or nothing) through the wrapper, from the cache when
    /// this renderer has already drawn these bytes with these arguments.
    fn render(
        &self,
        env: &CaseEnv<'_>,
        mesh: Option<&Path>,
        args: &[String],
        png: &Path,
        out_dir: &Path,
        basename: &str,
    ) -> Result<(), String> {
        let mut h = Sha256::new();
        h.update(self.renderer_id.as_bytes());
        for a in args {
            h.update([0]);
            h.update(a.as_bytes());
        }
        h.update([1]);
        if let Some(m) = mesh {
            let bytes = fs::read(m).map_err(|e| format!("{}: {e}", m.display()))?;
            // The importer picks the parser by extension.
            h.update(
                m.extension()
                    .map(|e| e.to_string_lossy().into_owned())
                    .unwrap_or_default()
                    .as_bytes(),
            );
            h.update([0]);
            h.update(&bytes);
        }
        let key: String = h.finalize().iter().map(|b| format!("{b:02x}")).collect();
        let cached = self.cache_dir.join(format!("{key}.png"));
        if cached.is_file() {
            fs::copy(&cached, png).map_err(|e| e.to_string())?;
            return Ok(());
        }
        let stderr = out_dir.join(format!("{basename}-render.stderr"));
        let mut cmd = self.command(env, &self.renderer);
        match mesh {
            Some(m) => {
                let path = m
                    .to_string_lossy()
                    .replace('\\', "\\\\")
                    .replace('"', "\\\"");
                cmd.arg(&self.wrapper)
                    .arg("-D")
                    .arg(format!("file=\"{path}\";"));
            }
            None => {
                cmd.arg(&self.empty_wrapper);
            }
        }
        cmd.args(args).arg("-o").arg(png);
        exec(&mut cmd, env.timeout, &stderr).map_err(|f| format!("render: {}", f.reason))?;
        if !png.is_file() {
            return Err("render: no image written".into());
        }
        // Write through a temporary name so a concurrent reader never sees
        // a partial file.
        let tmp = self
            .cache_dir
            .join(format!("{key}.{}.tmp", std::process::id()));
        if fs::copy(png, &tmp).is_ok() {
            let _ = fs::rename(&tmp, &cached);
        }
        Ok(())
    }
}

/// `-D` definitions that give the wrapper the camera the input sets.
///
/// A direct render takes its camera from `$vpr`, `$vpt`, `$vpd` and `$vpf`
/// when the main file assigns them (`Camera::updateView`,
/// `src/glview/Camera.cc:103-152`, called from `openscad.cc:426`); the
/// wrapper assigns nothing, so it would fall back to the default view.
/// Copying the assignments over restores the camera. Only literal values
/// are copied: the one tier 3 input that sets them
/// (`examples/Basics/logo_and_text.scad`) uses literals, and evaluating
/// arbitrary expressions here would mean running the model twice.
fn viewport_defines(input: &Path) -> Vec<String> {
    let Ok(text) = fs::read_to_string(input) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        let Some(rest) = line.strip_prefix("$vp") else {
            continue;
        };
        let Some((name, value)) = rest.split_once('=') else {
            continue;
        };
        let name = name.trim();
        let Some(value) = value.trim().strip_suffix(';') else {
            continue;
        };
        let literal = value
            .chars()
            .all(|ch| ch.is_ascii_digit() || " \t[],.-+eE".contains(ch));
        if ["r", "t", "d", "f"].contains(&name) && literal {
            out.push("-D".into());
            out.push(format!("$vp{name}={value};"));
        }
    }
    out
}

/// Coarse category of an image case for the report: its registration group
/// and whether it draws 2D or 3D geometry. OpenSCAD registers 2D inputs
/// with the top-down orthographic camera (`TestFunctions.cmake`, `is_2d`),
/// and DXF/SVG export/import tests are 2D by construction.
pub fn category(c: &Case) -> (String, &'static str) {
    let two_d = c.args.iter().any(|a| a == "--projection=ortho")
        || c.args.iter().any(|a| {
            let a = a.to_lowercase();
            a == "--format=dxf" || a == "--format=svg"
        });
    (c.group.clone(), if two_d { "2D" } else { "3D" })
}
