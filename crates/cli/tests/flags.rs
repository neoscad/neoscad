//! The command-line flags end to end, against outputs of the 2026.09.23
//! nightly (`/Applications/OpenSCAD.app`) for the same arguments. Unit
//! tests cover the formats; these check that the flags reach them.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// A fresh directory for one test.
fn scratch(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("neoscad-flags-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn neoscad(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_neoscad"))
        .args(args)
        .current_dir(dir)
        .env_remove("OPENSCADPATH")
        .output()
        .unwrap()
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

#[test]
fn dependency_file_lists_inputs_includes_uses_and_imports() {
    let d = scratch("deps");
    std::fs::create_dir_all(d.join("sub")).unwrap();
    std::fs::write(d.join("sub/b.scad"), "module bm() cube(2);\n").unwrap();
    std::fs::write(d.join("sub/u 1.scad"), "module um() sphere(1);\n").unwrap();
    std::fs::write(
        d.join("a.scad"),
        "include <sub/b.scad>\nuse <sub/u 1.scad>\nbm(); um();\nsurface(\"missing.dat\");\n",
    )
    .unwrap();
    // Run in the canonical directory, so that files found by include (which
    // are canonicalised) and files named relative to the input (which are
    // not) share one prefix: on macOS the temporary directory is behind the
    // `/var` link, on Windows under an 8.3 short name (`RUNNER~1`).
    let root = lang::paths::plain(d.canonicalize().unwrap());
    let out = neoscad(&root, &["-d", "a.d", "-o", "a.off", "a.scad"]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    // Every entry `/`-separated, as OpenSCAD's `generic_string()`, on
    // Windows too.
    let r = root.to_string_lossy().replace('\\', "/");
    // The nightly writes the same five entries, in hash order.
    assert_eq!(
        std::fs::read_to_string(d.join("a.d")).unwrap(),
        format!(
            "a.off: \\\n\ta.scad \\\n\t{r}/sub/b.scad \\\n\t{r}/sub/u\\ 1.scad \\\n\t{r}/missing.dat\n"
        )
    );

    // `-m` runs for the missing imported file, with it quoted. The command
    // goes through `sh`, which a Windows machine may not have.
    if Command::new("sh").args(["-c", "true"]).status().is_err() {
        eprintln!("skipped -m: no sh");
        return;
    }
    let out = neoscad(&root, &["-m", "echo MAKE", "-o", "a.off", "a.scad"]);
    assert_eq!(text(&out.stdout), format!("MAKE {r}/missing.dat\n"));
}

#[test]
fn summary_file_is_the_nightlys_json() {
    let d = scratch("summary");
    std::fs::write(d.join("c.scad"), "cube(1);\n").unwrap();
    let out = neoscad(
        &d,
        &[
            "--summary",
            "geometry",
            "--summary",
            "bounding-box",
            "--summary-file",
            "-",
            "-o",
            "x.stl",
            "c.scad",
        ],
    );
    assert!(out.status.success());
    assert_eq!(
        text(&out.stdout),
        r#"{"geometry":{"bounding_box":{"max":[1.0,1.0,1.0],"min":[0.0,0.0,0.0],"size":[1.0,1.0,1.0]},"convex":true,"dimensions":3,"facets":6,"triangular":false}}"#
    );
    // With a summary file, nothing of the summary goes to stderr.
    assert_eq!(text(&out.stderr), "");

    let out = neoscad(&d, &["--summary-file", "s.json", "-o", "x.stl", "c.scad"]);
    assert!(out.status.success());
    assert_eq!(std::fs::read_to_string(d.join("s.json")).unwrap(), "null");
}

#[test]
fn animation_writes_one_file_per_frame_and_one_echo_file() {
    let d = scratch("animate");
    std::fs::write(d.join("t.scad"), "echo($t); cube($t + 1);\n").unwrap();
    let out = neoscad(
        &d,
        &[
            "--animate",
            "4",
            "--animate_sharding",
            "2/2",
            "-o",
            "fr.off",
            "t.scad",
        ],
    );
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert!(d.join("fr00002.off").is_file() && d.join("fr00003.off").is_file());
    assert!(!d.join("fr00001.off").exists() && !d.join("fr.off").exists());
    let err = text(&out.stderr);
    assert!(err.starts_with("Exporting t.scad...\nECHO: 0.5\n"), "{err}");

    // The nightly's `--animate 2 -o fr.echo t.scad`, byte for byte.
    let out = neoscad(&d, &["--animate", "2", "-o", "fr.echo", "t.scad"]);
    assert!(out.status.success());
    assert_eq!(
        std::fs::read_to_string(d.join("fr.echo")).unwrap(),
        "Exporting t.scad...\nECHO: 0\nExporting t.scad...\nECHO: 0.5\n"
    );

    let out = neoscad(
        &d,
        &[
            "--animate",
            "2",
            "--export-format",
            "stl",
            "-o",
            "-",
            "t.scad",
        ],
    );
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(
        text(&out.stderr),
        "Option --animate is not supported when exporting to stdout.\n"
    );
}

#[test]
fn pov_and_3mf_options_are_exported() {
    let d = scratch("formats");
    std::fs::write(d.join("c.scad"), "cube(1);\n").unwrap();
    let out = neoscad(
        &d,
        &["--camera=1,2,3,10,20,30,300", "-o", "c.pov", "c.scad"],
    );
    assert!(out.status.success(), "{}", text(&out.stderr));
    let pov = std::fs::read_to_string(d.join("c.pov")).unwrap();
    // The camera block of the nightly's file for the same arguments.
    assert!(pov.contains("camera { look_at <0, 0, 0>\n location <0, 0, 300>\n angle 22.5 up <0, 1, 0> right <1, 0, 0> sky <0, 1, 0> right -x*image_width/image_height\ntranslate <1, 2, 3>\nrotate <10, 20 + clock * 3, 30 + clock>\n}\n"));

    let out = neoscad(
        &d,
        &[
            "-O",
            "export-3mf/color-mode=selected-only",
            "-O",
            "export-3mf/color=notacolor",
            "-o",
            "c.3mf",
            "c.scad",
        ],
    );
    assert!(out.status.success());
    assert!(
        text(&out.stderr).contains(
            "WARNING: Unable to parse color \"notacolor\", reverting to default color.\n"
        )
    );
}

#[test]
fn help_export_and_debug() {
    let d = scratch("help");
    let out = neoscad(&d, &["--help-export"]);
    assert!(out.status.success());
    assert!(
        text(&out.stderr).contains(
            "Section 'export-3mf':\n  - color-mode (enum): [<model>,none,selected-only]\n"
        )
    );
    std::fs::write(d.join("c.scad"), "cube(1);\n").unwrap();
    let out = neoscad(&d, &["--debug=all", "-q", "-o", "x.stl", "c.scad"]);
    assert_eq!(text(&out.stderr), "Debug on. --debug=all\n");
}

/// `--colorscheme` colours exported Manifold meshes, as in the nightly
/// (Metallic's #ddddff front and #dd22dd cut faces), and an unknown name
/// lists the schemes and exits 1.
#[test]
fn colour_scheme_reaches_mesh_export() {
    let d = scratch("scheme");
    std::fs::write(d.join("d.scad"), "difference() { cube(10); cube(5); }\n").unwrap();
    let out = neoscad(&d, &["--colorscheme=Metallic", "-o", "d.off", "d.scad"]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    let off = std::fs::read_to_string(d.join("d.off")).unwrap();
    let faces = |c: &str| off.lines().filter(|l| l.ends_with(c)).count();
    assert_eq!((faces(" 221 221 255"), faces(" 221 34 221")), (18, 6));
    let out = neoscad(&d, &["--colorscheme=Nope", "-o", "d.off", "d.scad"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(text(&out.stderr).starts_with("Cornfield\nMetallic\n"));
}

/// A PNG takes its size from `--imgsize` and its view from the file's
/// `$vp*` unless `--camera` locks it. Skipped without a GPU.
#[test]
fn png_export_uses_the_camera_and_the_files_view() {
    let d = scratch("png");
    // Far away, the cube covers few pixels; `--camera` overrides that.
    std::fs::write(d.join("c.scad"), "$vpd = 2000;\ncube(10, center=true);\n").unwrap();
    let png = |args: &[&str]| -> Option<(u32, u32, usize)> {
        let mut all = vec!["c.scad", "--render", "--imgsize=200,100", "-o", "c.png"];
        all.extend_from_slice(args);
        let out = neoscad(&d, &all);
        if !out.status.success() {
            let err = text(&out.stderr);
            assert!(err.contains("GPU"), "{err}");
            eprintln!("skipped: {err}");
            return None;
        }
        // The view variables disable the default --viewall, with a warning.
        if args.is_empty() {
            assert!(text(&out.stderr).contains("Viewall and autocenter disabled in favor of $vp*"));
        }
        let file = std::fs::File::open(d.join("c.png")).unwrap();
        let mut reader = png::Decoder::new(std::io::BufReader::new(file))
            .read_info()
            .unwrap();
        let mut buf = vec![0; reader.output_buffer_size().unwrap()];
        let info = reader.next_frame(&mut buf).unwrap();
        assert_eq!(info.color_type, png::ColorType::Rgb);
        // Pixels that are not Cornfield's background.
        let model = buf[..info.buffer_size()]
            .chunks(3)
            .filter(|p| *p != [0xff, 0xff, 0xe5])
            .count();
        Some((info.width, info.height, model))
    };
    let Some((w, h, far)) = png(&[]) else { return };
    assert_eq!((w, h), (200, 100));
    let (_, _, near) = png(&["--camera=0,0,0,55,0,25,40"]).unwrap();
    assert!(far > 0 && near > 20 * far, "{far} vs {near} model pixels");
}

/// Without `--render` a PNG is OpenSCAD's preview: a `#` object is drawn
/// again in translucent red, and the same input gives the same bytes.
/// Skipped without a GPU.
#[test]
fn png_preview_draws_highlights_and_is_deterministic() {
    let d = scratch("preview");
    std::fs::write(
        d.join("h.scad"),
        "difference() { cube(10, center=true); #cylinder(h=20, r=3, center=true); }\n",
    )
    .unwrap();
    let run = |args: &[&str]| -> Option<Vec<u8>> {
        let mut all = vec!["h.scad", "--imgsize=200,200", "-o", "h.png"];
        all.extend_from_slice(args);
        let out = neoscad(&d, &all);
        if !out.status.success() {
            let err = text(&out.stderr);
            assert!(err.contains("GPU"), "{err}");
            eprintln!("skipped: {err}");
            return None;
        }
        Some(std::fs::read(d.join("h.png")).unwrap())
    };
    let Some(first) = run(&[]) else { return };
    assert_eq!(run(&[]).unwrap(), first, "same input, same bytes");
    let reddish = |bytes: &[u8]| {
        let mut reader = png::Decoder::new(std::io::Cursor::new(bytes))
            .read_info()
            .unwrap();
        let mut buf = vec![0; reader.output_buffer_size().unwrap()];
        let info = reader.next_frame(&mut buf).unwrap();
        buf[..info.buffer_size()]
            .chunks(3)
            .filter(|p| p[0] > 150 && p[1] < 120 && p[2] < 120)
            .count()
    };
    assert!(
        reddish(&first) > 200,
        "{} highlight pixels",
        reddish(&first)
    );
    // Render mode draws the geometry only: no highlight.
    assert_eq!(reddish(&run(&["--render"]).unwrap()), 0);
    // The throwntogether view also draws the cylinder, in the highlight
    // colour because it is marked.
    assert!(reddish(&run(&["--preview=throwntogether"]).unwrap()) > 200);
}

/// `neoscad snapshot`: a sheet of the requested size, and a JSON summary
/// with the solid's numbers and, with `--diff`, the changed volumes.
/// Skipped without a GPU.
#[test]
fn snapshot_writes_a_sheet_and_a_summary() {
    let d = scratch("snapshot");
    std::fs::write(d.join("a.scad"), "cube(10);\n").unwrap();
    std::fs::write(d.join("b.scad"), "cube([10, 10, 5]);\n").unwrap();
    let out = neoscad(
        &d,
        &[
            "snapshot", "a.scad", "--size", "400x300", "--dims", "--diff", "b.scad", "--format",
            "json",
        ],
    );
    if !out.status.success() {
        let err = text(&out.stderr);
        assert!(err.contains("GPU"), "{err}");
        eprintln!("skipped: {err}");
        return;
    }
    let summary = text(&out.stdout);
    assert!(summary.contains(r#""volume":1000.0"#), "{summary}");
    assert!(summary.contains(r#""added_volume":500.0"#), "{summary}");
    assert!(summary.contains(r#""removed_volume":0.0"#), "{summary}");
    assert!(
        summary.contains(r#""output":"a-snapshot.png""#),
        "{summary}"
    );
    let file = std::fs::File::open(d.join("a-snapshot.png")).unwrap();
    let reader = png::Decoder::new(std::io::BufReader::new(file))
        .read_info()
        .unwrap();
    let info = reader.info();
    assert_eq!((info.width, info.height), (400, 300));
    // Unknown views are an error naming the valid ones.
    let out = neoscad(&d, &["snapshot", "a.scad", "--views", "side"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(text(&out.stderr).contains("iso, front, back"));
}

#[test]
fn json_reports_say_why_a_run_failed() {
    let d = scratch("whyfail");
    // A missing input is a diagnostic with a stable code, not only a line.
    let o = neoscad(&d, &["nope.scad", "-o", "x.stl", "--format", "json"]);
    assert_eq!(o.status.code(), Some(1));
    let v: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(v["diagnostics"][0]["code"], "input-not-found", "{v}");
    assert_eq!(v["counts"]["errors"], 1, "{v}");
    // A syntax error's hint names the token and its column.
    std::fs::write(d.join("se.scad"), "rotate(45 cube(3);\n").unwrap();
    let o = neoscad(&d, &["se.scad", "-o", "x.stl", "--format", "json"]);
    let v: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    let diag = &v["diagnostics"][0];
    assert_eq!(diag["span"]["start"]["column"], 11, "{v}");
    assert!(
        diag["hints"][0]["message"]
            .as_str()
            .unwrap()
            .starts_with("unexpected `cube` at line 1, column 11"),
        "{v}"
    );
    // Without --format json the bytes are OpenSCAD's.
    let o = neoscad(&d, &["nope.scad", "-o", "x.stl"]);
    assert_eq!(text(&o.stderr), "Can't open input file 'nope.scad'!\n\n");
}

/// A run that draws nothing must not initialize the GPU frameworks: they
/// are linked delay-init (`build.rs`, `delay_gpu_frameworks`), which saves
/// about 400 initializers and most of a millisecond per process. The linker
/// warns that it ignores the flag for CoreGraphics, so a linker that one
/// day means it would bring the cost back silently; dyld's own trace of the
/// initializers it runs is what shows it.
#[cfg(target_os = "macos")]
#[test]
fn gpu_frameworks_are_not_initialized_at_launch() {
    let out = Command::new(env!("CARGO_BIN_EXE_neoscad"))
        .arg("--version")
        .env("DYLD_PRINT_INITIALIZERS", "1")
        .output()
        .unwrap();
    assert!(out.status.success());
    let trace = text(&out.stderr);
    assert!(
        trace.contains("running initializer"),
        "no dyld trace:\n{trace}"
    );
    let frameworks: Vec<&str> = trace
        .lines()
        .filter(|l| l.contains("running initializer") && l.contains("/System/Library/"))
        .collect();
    assert!(
        frameworks.is_empty(),
        "framework initializers ran at launch:\n{}",
        frameworks.join("\n")
    );
}

/// `-o x.step` is NeoSCAD's `--enable exact` extension. Without the flag
/// the suffix is unknown, exactly as in OpenSCAD (its message, exit 1, no
/// file); with it the file is STEP with exact surfaces, the substitutions
/// are reported at their source lines, and `--format json` carries the
/// export's numbers. A failed export writes no file.
#[test]
fn step_export_needs_the_exact_extension() {
    let d = scratch("step");
    std::fs::write(
        d.join("a.scad"),
        "difference() {\n  cube(20);\n  translate([10, 10, -1]) cylinder(r=4, h=22);\n}\n",
    )
    .unwrap();
    let out = neoscad(&d, &["-o", "a.step", "a.scad"]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(
        text(&out.stderr),
        "Invalid suffix step. Either add a valid suffix or specify one using the --export-format option.\n"
    );
    assert!(!d.join("a.step").exists());
    let out = neoscad(&d, &["--enable", "all", "-o", "a.stp", "a.scad"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(text(&out.stderr).contains("Invalid suffix stp."));

    let out = neoscad(&d, &["--enable", "exact", "-o", "a.step", "a.scad"]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    let step = std::fs::read_to_string(d.join("a.step")).unwrap();
    assert!(step.starts_with("ISO-10303-21;"), "{step}");
    assert!(step.contains("CYLINDRICAL_SURFACE"));
    assert!(step.contains("FILE_NAME('a.step','1970-01-01T00:00:00'"));
    assert!(
        text(&out.stderr).contains(
            "INFO: STEP export: cylinder() is exported as an exact cylinder, not the 13-sided polygon of the mesh ($fn is not set) in file a.scad, line 3"
        ),
        "{}",
        text(&out.stderr)
    );
    // The same bytes again.
    let again = neoscad(&d, &["--enable", "exact", "-o", "b.step", "a.scad"]);
    assert!(again.status.success());
    let b = std::fs::read_to_string(d.join("b.step")).unwrap();
    assert_eq!(b.replace("'b.step'", "'a.step'"), step);

    let out = neoscad(
        &d,
        &[
            "--enable", "exact", "--format", "json", "-o", "c.step", "a.scad",
        ],
    );
    assert!(out.status.success());
    let j: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(j["exact"]["ok"], true);
    assert_eq!(j["exact"]["faces"], 7);
    assert_eq!(j["exact"]["substitutions"]["exact"], 1);

    // An inside-out polyhedron has no STEP solid: an error, and no file.
    std::fs::write(
        d.join("bad.scad"),
        "polyhedron(points = [[1,0,0],[-1,0,0],[0,1,0],[0,-1,0],[0,0,1],[0,0,-1]], faces = [[0,2,4],[0,5,2],[0,4,3],[0,3,5],[1,4,2],[1,2,5],[1,3,4],[1,5,3]]);\n",
    )
    .unwrap();
    let out = neoscad(&d, &["--enable", "exact", "-o", "bad.step", "bad.scad"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(
        text(&out.stderr).contains("ERROR: STEP export failed:"),
        "{}",
        text(&out.stderr)
    );
    assert!(!d.join("bad.step").exists());
    let _ = std::fs::remove_dir_all(&d);
}
