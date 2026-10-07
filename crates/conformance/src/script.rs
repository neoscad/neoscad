//! Tests OpenSCAD drives with a Python script or a raw `add_test` command,
//! ported step for step ([`crate::manifest::Runner::Script`]). None of them
//! needs the renderer that tier 3 geometry cases use: the binary under test
//! does every OpenSCAD step itself.
//!
//! - **SVG re-export** (`export-svg*`, `tests/export_import_pngtest.py` with
//!   an `.svg` result): export the input as SVG, import that file in a
//!   one-line wrapper and export it again as SVG; the second file must
//!   match the expected SVG as text (`compare_default`). Both steps get the
//!   test's arguments, `-O export-svg/...` included.
//! - **PDF** (`export-pdf*`, `tests/export_pngtest.py`): export a PDF, turn
//!   it into a PNG at 300 dpi and compare the image with OpenSCAD's
//!   comparator. Upstream converts with Ghostscript (`gs -sDEVICE=png16m
//!   -r300` with 4-bit text and graphics antialiasing); that is used when
//!   `gs` is on `PATH`. Otherwise poppler's `pdftoppm -r 300`, whose image
//!   is cropped to Ghostscript's page size (poppler rounds the A4 page up
//!   to 2480x3509 pixels, Ghostscript to 2479x3508); every PDF case passes
//!   the comparison either way when the OpenSCAD nightly writes the PDF.
//!   With neither tool the case fails and says so.
//! - **Exit codes** (`tests/shouldfail.py`): run the binary with the test's
//!   arguments plus `--export-format=<suffix> -o -`; the exit code must be
//!   the `--retval` the test names.
//! - **Relative output** (`add_output_file_test`): `_run` runs the binary
//!   with `-o relative-output.<format>` in the working directory and must
//!   succeed; `_check` (`cmake -E cat FILE`) passes when the file is there.
//!   The checks run after every other case (see `run.rs`), as ctest's
//!   `DEPENDS` orders them.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;
use std::time::Instant;

use crate::geometry::{FONT_DIR_VAR, exec, exec_status};
use crate::image_compare;
use crate::manifest::Case;
use crate::run::{Env, Outcome, Status, outcome};

/// Whether `c` reads what another case wrote (a relative-output check),
/// so it must run after the others.
pub fn is_dependent(c: &Case) -> bool {
    c.command.first().is_some_and(|p| p == "cmake")
}

/// Run one script case.
pub fn run(env: &Env, c: &Case) -> Outcome {
    let started = Instant::now();
    let result = match (c.script.as_deref(), c.command.is_empty()) {
        (Some(s), _) if s.ends_with("shouldfail.py") => should_fail(env, c),
        (Some(s), _) if s.ends_with("export_import_pngtest.py") => {
            return timed(started, svg_reexport(env, c));
        }
        (Some(s), _) if s.ends_with("export_pngtest.py") => pdf(env, c),
        (None, false) => raw(env, c),
        _ => Err("no script runner for this case".into()),
    };
    let o = match result {
        Ok(()) => outcome(c, Status::Pass, None),
        Err(reason) => outcome(c, Status::Fail, Some(reason)),
    };
    timed(started, o)
}

fn timed(started: Instant, mut o: Outcome) -> Outcome {
    let ms = started.elapsed().as_secs_f64() * 1000.0;
    o.ms = Some((ms * 10.0).round() / 10.0);
    o
}

/// A command for the binary under test with the test environment
/// (`test_cmdline_tool.py`'s font path and library path, and neoscad's
/// bundled-font directory), run in the ctest working directory.
fn binary(env: &Env) -> Command {
    let mut cmd = Command::new(&env.binary);
    cmd.current_dir(&env.work_dir)
        .env("OPENSCAD_FONT_PATH", &env.font_path)
        .env_remove(FONT_DIR_VAR)
        .env(crate::geometry::NO_SERVER_VAR, "1")
        .env("OPENSCADPATH", &env.library_path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .args(&env.extra_args);
    cmd
}

/// The case's arguments with `{REF}` and `{OPENSCAD}` filled in.
fn args(env: &Env, a: &[String]) -> Vec<String> {
    a.iter()
        .map(|a| {
            a.replace("{REF}", &env.ref_str)
                .replace("{OPENSCAD}", &env.binary.to_string_lossy())
        })
        .collect()
}

/// Input path, output directory (created) and the ctest basename.
fn paths(env: &Env, c: &Case) -> Result<(PathBuf, PathBuf, String), String> {
    let input = c
        .input
        .as_deref()
        .map(|i| env.ref_root.join(i))
        .ok_or("manifest case lacks an input")?;
    if !input.is_file() {
        return Err(format!("missing input {}", input.display()));
    }
    let out_dir = env.actual_dir.join(&c.group);
    fs::create_dir_all(&out_dir).map_err(|e| e.to_string())?;
    let basename =
        c.id.strip_prefix(&format!("{}_", c.group))
            .unwrap_or(&c.id)
            .to_string();
    Ok((input, out_dir, basename))
}

/// `tests/shouldfail.py`.
fn should_fail(env: &Env, c: &Case) -> Result<(), String> {
    let (input, out_dir, basename) = paths(env, c)?;
    let mut retval = None;
    let mut rest = Vec::new();
    for a in args(env, &c.args) {
        match a.strip_prefix("--retval=") {
            Some(v) => retval = Some(v.to_string()),
            None => rest.push(a),
        }
    }
    let retval = retval.ok_or("shouldfail.py case without --retval")?;
    let stderr = out_dir.join(format!("{basename}.stderr"));
    let mut cmd = binary(env);
    cmd.arg(&input)
        .args(&rest)
        .arg(format!("--export-format={}", c.suffix))
        .args(["-o", "-"]);
    let code = exec_status(&mut cmd, env.timeout, &stderr)?;
    // `str(result) != str(args.retval)`: a signal is never the answer.
    match code {
        Some(n) if n.to_string() == retval => Ok(()),
        Some(n) => Err(format!("exit {n}, expected {retval}")),
        None => Err(format!("killed by a signal, expected exit {retval}")),
    }
}

/// `tests/export_import_pngtest.py` whose result is an SVG: both OpenSCAD
/// steps with the binary under test, then the text comparison.
fn svg_reexport(env: &Env, c: &Case) -> Outcome {
    let fail = |reason: String| outcome(c, Status::Fail, Some(reason));
    let (input, out_dir, basename) = match paths(env, c) {
        Ok(p) => p,
        Err(e) => return fail(e),
    };
    let Some(expected) = c.expected.as_deref().map(|e| env.ref_root.join(e)) else {
        return fail("manifest case lacks an expected path".into());
    };
    let mut format = None;
    let mut remaining = Vec::new();
    let all = args(env, &c.args);
    let mut it = all.iter();
    while let Some(a) = it.next() {
        if a.starts_with("--openscad=") {
            continue;
        } else if let Some(f) = a.strip_prefix("--format=") {
            format = Some(f.to_lowercase());
        } else if a == "--format" {
            format = it.next().map(|f| f.to_lowercase());
        } else {
            remaining.push(a.clone());
        }
    }
    let Some(format) = format else {
        return fail("export_import_pngtest.py case without --format".into());
    };
    // The "PNG" is the test's actual output file, opened (and so created)
    // by test_cmdline_tool.py before the script runs.
    let actual = out_dir.join(format!("{basename}-actual.{}", c.suffix));
    if let Err(e) = fs::write(&actual, b"") {
        return fail(format!("{}: {e}", actual.display()));
    }
    let file_name = input
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    // `exportfile = join(outputdir, inputfilename) + "." + format`.
    let export = out_dir.join(format!("{file_name}.{format}"));
    let _ = fs::remove_file(&export);
    let tmpargs: Vec<String> = remaining
        .iter()
        .map(|a| {
            if a.starts_with("--render") {
                "--render=force".into()
            } else {
                a.clone()
            }
        })
        .collect();
    let mut cmd = binary(env);
    cmd.arg(&input).arg("-o").arg(&export).args(&tmpargs);
    let stderr = out_dir.join(format!("{basename}-export.stderr"));
    if let Err(f) = exec(&mut cmd, env.timeout, &stderr) {
        return fail(format!("export: {}", f.reason));
    }
    if !export.is_file() {
        return fail("export: no file written".into());
    }
    // `createImport`: the wrapper names the export relative to itself.
    let wrapper = PathBuf::from(format!("{}.scad", export.display()));
    let export_name = export
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    if let Err(e) = fs::write(&wrapper, format!("import(\"{export_name}\");\n")) {
        return fail(format!("{}: {e}", wrapper.display()));
    }
    let mut cmd = binary(env);
    cmd.arg(&wrapper).arg("-o").arg(&actual).args(&remaining);
    let stderr = out_dir.join(format!("{basename}-reexport.stderr"));
    if let Err(f) = exec(&mut cmd, env.timeout, &stderr) {
        return fail(format!("re-export: {}", f.reason));
    }
    let _ = fs::remove_file(&export);
    let _ = fs::remove_file(&wrapper);
    env.compare(c, &expected, &actual)
}

/// How PDFs become PNGs on this machine.
#[derive(Debug, Clone)]
enum Rasteriser {
    Ghostscript(PathBuf),
    Poppler { pdftoppm: PathBuf, pdfinfo: PathBuf },
    Missing,
}

fn find_on_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|d| d.join(name))
        .find(|p| p.is_file())
}

fn rasteriser() -> &'static Rasteriser {
    static R: OnceLock<Rasteriser> = OnceLock::new();
    R.get_or_init(|| {
        if let Some(gs) = find_on_path("gs") {
            return Rasteriser::Ghostscript(gs);
        }
        match (find_on_path("pdftoppm"), find_on_path("pdfinfo")) {
            (Some(pdftoppm), Some(pdfinfo)) => Rasteriser::Poppler { pdftoppm, pdfinfo },
            _ => Rasteriser::Missing,
        }
    })
}

/// `tests/export_pngtest.py`, then `compare_png`.
fn pdf(env: &Env, c: &Case) -> Result<(), String> {
    let (input, out_dir, basename) = paths(env, c)?;
    let expected = c
        .expected
        .as_deref()
        .map(|e| env.ref_root.join(e))
        .ok_or("manifest case lacks an expected path")?;
    let remaining: Vec<String> = args(env, &c.args)
        .into_iter()
        .filter(|a| !a.starts_with("--openscad=") && !a.starts_with("--format="))
        .collect();
    let png = out_dir.join(format!("{basename}-actual.png"));
    let _ = fs::remove_file(&png);
    let stem = input
        .file_stem()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let export = out_dir.join(format!("{stem}.pdf"));
    let _ = fs::remove_file(&export);
    let mut cmd = binary(env);
    cmd.arg(&input).arg("-o").arg(&export).args(&remaining);
    let stderr = out_dir.join(format!("{basename}-export.stderr"));
    exec(&mut cmd, env.timeout, &stderr).map_err(|f| format!("export: {}", f.reason))?;
    let stderr = out_dir.join(format!("{basename}-convert.stderr"));
    rasterise(env, &export, &png, &stderr)?;
    let cmp =
        image_compare::compare_files(&expected, &png).map_err(|e| format!("image compare: {e}"))?;
    if cmp.passed() {
        Ok(())
    } else {
        Err(format!("image differs: {}", cmp.describe()))
    }
}

/// The PDF's first page as an 8-bit RGB PNG at 300 dpi.
fn rasterise(env: &Env, pdf: &Path, png: &Path, stderr: &Path) -> Result<(), String> {
    let mut cmd = match rasteriser() {
        Rasteriser::Ghostscript(gs) => {
            // `gs_cmd` in export_pngtest.py.
            let mut cmd = Command::new(gs);
            cmd.args([
                "-dSAFER",
                "-dNOPAUSE",
                "-dBATCH",
                "-sDEVICE=png16m",
                "-dTextAlphaBits=4",
                "-dGraphicsAlphaBits=4",
                "-r300",
            ])
            .arg(format!("-sOutputFile={}", png.display()))
            .arg(pdf);
            cmd
        }
        Rasteriser::Poppler { pdftoppm, pdfinfo } => {
            let (w, h) = page_size(pdfinfo, pdf)?;
            // Ghostscript's device is the page at 300 dpi rounded to whole
            // pixels; poppler draws the same page from the same top-left
            // corner, so cropping gives the same raster size.
            let px = |pts: f64| (pts * 300.0 / 72.0 + 0.5).floor() as i64;
            let prefix = png.with_extension("");
            let mut cmd = Command::new(pdftoppm);
            cmd.args(["-r", "300", "-png", "-singlefile", "-x", "0", "-y", "0"])
                .arg("-W")
                .arg(px(w).to_string())
                .arg("-H")
                .arg(px(h).to_string())
                .arg(pdf)
                .arg(&prefix);
            cmd
        }
        Rasteriser::Missing => {
            return Err(
                "no PDF rasteriser: install Ghostscript (gs) or poppler (pdftoppm, pdfinfo)".into(),
            );
        }
    };
    cmd.stdin(Stdio::null()).stdout(Stdio::null());
    exec(&mut cmd, env.timeout, stderr).map_err(|f| format!("convert: {}", f.reason))?;
    if !png.is_file() {
        return Err("convert: no image written".into());
    }
    Ok(())
}

/// The first page's size in points, from `pdfinfo` ("Page size: W x H pts").
fn page_size(pdfinfo: &Path, pdf: &Path) -> Result<(f64, f64), String> {
    let out = Command::new(pdfinfo)
        .arg(pdf)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("cannot run {}: {e}", pdfinfo.display()))?;
    let text = String::from_utf8_lossy(&out.stdout);
    let line = text
        .lines()
        .find_map(|l| l.strip_prefix("Page size:"))
        .ok_or_else(|| format!("pdfinfo: no page size for {}", pdf.display()))?;
    let mut nums = line
        .split_whitespace()
        .filter_map(|w| w.parse::<f64>().ok());
    match (nums.next(), nums.next()) {
        (Some(w), Some(h)) => Ok((w, h)),
        _ => Err(format!("pdfinfo: cannot read page size {line:?}")),
    }
}

/// A raw `add_test` command: the binary under test writing a relative
/// output, or `cmake -E cat` reading it back.
fn raw(env: &Env, c: &Case) -> Result<(), String> {
    let command = args(env, &c.command);
    let out_dir = env.actual_dir.join(&c.group);
    fs::create_dir_all(&out_dir).map_err(|e| e.to_string())?;
    let stderr = out_dir.join(format!("{}.stderr", c.id));
    match command.first().map(String::as_str) {
        Some("cmake") => {
            // `cmake -E cat FILE...` fails when a file cannot be read.
            if command.get(1).map(String::as_str) != Some("-E")
                || command.get(2).map(String::as_str) != Some("cat")
            {
                return Err(format!("unsupported cmake command {:?}", &command[1..]));
            }
            for f in &command[3..] {
                let p = env.work_dir.join(f);
                fs::read(&p).map_err(|e| format!("{}: {e}", p.display()))?;
            }
            Ok(())
        }
        Some(program) if Path::new(program) == env.binary => {
            // A leftover file from an earlier run must not pass the check.
            if let Some(i) = command.iter().position(|a| a == "-o")
                && let Some(target) = command.get(i + 1)
            {
                let _ = fs::remove_file(env.work_dir.join(target));
            }
            let mut cmd = binary(env);
            cmd.args(&command[1..]);
            exec(&mut cmd, env.timeout, &stderr).map_err(|f| f.reason)
        }
        _ => Err(format!("unsupported command {command:?}")),
    }
}
