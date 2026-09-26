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
    let out = neoscad(&d, &["-d", "a.d", "-o", "a.off", "a.scad"]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    let root = d.canonicalize().unwrap();
    let r = root.to_string_lossy();
    // The nightly writes the same five entries, in hash order.
    assert_eq!(
        std::fs::read_to_string(d.join("a.d")).unwrap(),
        format!(
            "a.off: \\\n\ta.scad \\\n\t{r}/sub/b.scad \\\n\t{r}/sub/u\\ 1.scad \\\n\t{r}/missing.dat\n"
        )
    );

    // `-m` runs for the missing imported file, with it quoted.
    let out = neoscad(&d, &["-m", "echo MAKE", "-o", "a.off", "a.scad"]);
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
