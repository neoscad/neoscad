//! SVG and DXF exports that keep the model's parts apart (`--enable part`,
//! `session::layers`): one group or layer per part with its colour, the
//! geometry outside every part on its own, subtracted parts drawn by the
//! cut they make only, parts drawn through `projection()`, and every file
//! without parts (or without the extension) byte-identical to the plain
//! export.

use std::path::PathBuf;
use std::sync::Arc;

use lang::loader::LibraryPath;
use lang::vfs::MemFs;
use session::export::{Format, Settings};
use session::{Config, ExportRequest, ExportSink, Run, Session};

fn session(files: &[(&str, &str)]) -> Session {
    let fs = Arc::new(MemFs::new());
    for (p, t) in files {
        fs.insert(format!("/doc/{p}"), t.as_bytes().to_vec());
    }
    let mut cfg = Config::new(fs, LibraryPath(Vec::new()));
    cfg.work_dir = PathBuf::from("/doc");
    Session::new(cfg)
}

struct Sink(Vec<u8>);

impl ExportSink for Sink {
    fn write(&mut self, _: &str, data: &[u8]) -> Result<(), String> {
        self.0 = data.to_vec();
        Ok(())
    }
    fn summary(&mut self, _: &session::SummaryFacts<'_>, _: &mut eval::Console<Vec<u8>>) -> bool {
        true
    }
}

fn export(s: &Session, path: &str, format: &str, parts: bool) -> String {
    let scheme = render::ColorScheme::cornfield();
    let mut run = Run::new(path);
    if parts {
        run.extensions = eval::Extensions::NONE.with(eval::Extension::Part);
    }
    let req = ExportRequest {
        run,
        outputs: vec![(format!("out.{format}"), Format::from_id(format).unwrap())],
        force: false,
        scheme: scheme.clone(),
        settings: Settings {
            scheme: scheme.geometry_scheme(),
            svg: io::svg::SvgStyle::default(),
            pdf: io::pdf::PdfOptions::default(),
            pdf_warnings: Vec::new(),
            threemf: io::threemf::Options::default(),
            threemf_warning: None,
            title: path.to_string(),
            source_path: path.to_string(),
            creation_date: "2026-01-01T00:00:00Z".to_string(),
            pov_camera: None,
            predictible_output: false,
        },
    };
    let mut sink = Sink(Vec::new());
    let r = s.export(&req, &mut sink).expect("not cancelled");
    assert_eq!(r.exit_code, 0, "{}", String::from_utf8_lossy(&r.log.stderr));
    String::from_utf8(sink.0).unwrap()
}

const WALL: &str = r#"
part("plate") color("peru") square([48, 2]);
for (i = [0:2]) part(str("stud", i)) color([0.2, 0.4, 0.8, 0.5]) translate([i * 16, 2]) square([2, 90]);
difference() { translate([60, 0]) square(10); part("hole") translate([65, 5]) circle(2, $fn = 4); }
part("frame") { square([1, 1]); part("pin") translate([0, 95]) square(1); }
"#;

#[test]
fn each_part_is_its_own_group() {
    let union = WALL.replace("part(", "group(");
    let s = session(&[("wall.scad", WALL), ("union.scad", &union)]);
    let svg = export(&s, "wall.scad", "svg", true);
    let ids: Vec<&str> = svg
        .lines()
        .filter_map(|l| l.strip_prefix("<g id=\"")?.strip_suffix("\">"))
        .collect();
    // In order of first appearance; the subtracted `hole` has no outline
    // of its own; `frame` has what is in it outside `frame.pin`.
    assert_eq!(
        ids,
        ["plate", "stud0", "stud1", "stud2", "frame", "frame.pin"]
    );
    // The geometry outside every part comes first, ungrouped, and keeps
    // the hole the part cut into it.
    let first = svg.split("<g id=").next().unwrap();
    assert_eq!(first.matches("<path").count(), 1);
    assert!(
        first.contains("M 63,-5 L 65,-7 L 67,-5 L 65,-3 z"),
        "{first}"
    );
    assert!(svg.contains(
        "<g id=\"plate\">\n<path d=\"\nM 48,-2 L 0,-2 L 0,-0 L 48,-0 z\n\" stroke=\"#cd853f\""
    ));
    assert!(svg.contains(
        "stroke=\"#3366cc\" fill=\"none\" stroke-width=\"0.35\" stroke-opacity=\"0.5\"/>"
    ));
    // The same view box as the union.
    let plain = export(&s, "union.scad", "svg", false);
    assert_eq!(svg.lines().nth(2), plain.lines().nth(2));

    let dxf = export(&s, "wall.scad", "dxf", true);
    for layer in ["plate", "stud1", "frame.pin"] {
        assert!(
            dxf.contains(&format!("  0\nLAYER\n  2\n{layer}\n")),
            "{layer}"
        );
        assert!(
            dxf.contains(&format!("AcDbEntity\n  8\n{layer}\n")),
            "{layer}"
        );
    }
    assert!(!dxf.contains("\n  2\nhole\n"));
}

#[test]
fn parts_drawn_through_a_projection_stay_apart() {
    let src = r#"projection() {
  part("joist_a") cube([2, 50, 6]);
  part("joist_b") translate([16, 0, 0]) cube([2, 50, 6]);
  part("rim") translate([0, -2, 0]) cube([18, 2, 6]);
}"#;
    let union = src.replace("part(", "group(");
    let s = session(&[("floor.scad", src), ("union.scad", &union)]);
    let svg = export(&s, "floor.scad", "svg", true);
    assert_eq!(svg.matches("<g id=").count(), 3);
    // The union is one outline; the parts are three.
    assert_eq!(svg.matches(" z\n").count(), 3);
    assert_eq!(
        export(&s, "union.scad", "svg", false)
            .matches(" z\n")
            .count(),
        1
    );
}

#[test]
fn without_parts_or_the_extension_the_file_is_unchanged() {
    let none = "square(10); translate([20, 0]) circle(3);";
    let s = session(&[("a.scad", none), ("wall.scad", WALL)]);
    for format in ["svg", "dxf"] {
        assert_eq!(
            export(&s, "a.scad", format, true),
            export(&s, "a.scad", format, false)
        );
    }
    // Without the extension `part` is an unknown module, as in OpenSCAD.
    let plain = export(&s, "wall.scad", "svg", false);
    assert!(!plain.contains("<g "));
    assert_eq!(plain.matches("<path").count(), 1);
}
