//! A warm session exports the same bytes as a cold one, whatever it
//! rendered before.
//!
//! The geometry cache keeps subtrees across renders, with the Manifold
//! original IDs they were built with, and those IDs order a solid's
//! triangle runs in every exported mesh. In the CAD pilot the MCP server
//! rendered an earlier variant of a model, then exported the final one:
//! same triangles as the command line's export, in a different order.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use lang::loader::LibraryPath;
use lang::vfs::MemFs;
use session::export::{Format, Settings};
use session::{Config, ExportRequest, ExportSink, Run, Session};

fn session(fs: &Arc<MemFs>) -> Session {
    let mut cfg = Config::new(fs.clone(), LibraryPath(vec![PathBuf::from("/lib")]));
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

/// `path` exported as `format` (`stl`, `off`, `svg`), under the agent
/// limits so that a generated model cannot run away.
fn export(s: &Session, path: &str, format: &str) -> Vec<u8> {
    let scheme = render::ColorScheme::cornfield();
    let mut run = Run::new(path);
    run.limits = Some(session::Limits::AGENT);
    let req = ExportRequest {
        run,
        outputs: vec![(
            format!("out.{format}"),
            Format::from_id(format).expect("a format"),
        )],
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
    assert!(!sink.0.is_empty());
    sink.0
}

fn cold(fs: &Arc<MemFs>, path: &str, format: &str) -> Vec<u8> {
    export(&session(fs), path, format)
}

/// The pilot's history in miniature: a render of an earlier variant
/// caches the second body with ID blocks drawn before the edited first
/// body's, so reusing it as it was put its triangles first.
#[test]
fn an_export_after_an_earlier_variant_matches_a_cold_one() {
    let fs = Arc::new(MemFs::new());
    let body = |a: u32| {
        format!(
            "difference() {{ cube({a}); translate([1, 1, 1]) sphere(3, $fn = 12); }}\n\
             translate([20, 0, 0]) difference() {{ cube(8); sphere(4, $fn = 12); }}\n"
        )
    };
    fs.insert("/doc/m.scad", body(10).into_bytes());
    let s = session(&fs);
    export(&s, "m.scad", "stl");
    fs.insert("/doc/m.scad", body(11).into_bytes());
    let warm = export(&s, "m.scad", "stl");
    assert!(warm == cold(&fs, "m.scad", "stl"), "warm export differs");
    // Rendering it again (now with every block in order) changes nothing.
    assert!(export(&s, "m.scad", "stl") == warm);
}

/// A small xorshift generator: the property test must be reproducible.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

/// 3D bodies, each with a size parameter, so that variants share some
/// subtrees and not others: overlapping booleans, colours (whose IDs
/// reach the OFF export's colours), parts of an extrusion, a hull and a
/// minkowski (whose IDs come from the global counter).
const BODIES: &[&str] = &[
    "union() { cube(@); translate([2, 2, 2]) sphere(@ * 0.6, $fn = 10); }",
    "difference() { cube(@, center = true); cylinder(r = @ / 4, h = 3 * @, center = true, $fn = 8); }",
    "intersection() { cube(@, center = true); sphere(@ * 0.65, $fn = 10); }",
    "color(\"red\") cube(@); color(\"blue\") translate([1, 1, 1]) cube(@);",
    "linear_extrude(3) difference() { square(@); translate([@ / 2, @ / 2]) circle(@ / 4, $fn = 10); }",
    "hull() { cube(@ / 2); translate([@, 0, 0]) sphere(1, $fn = 8); }",
    "minkowski() { difference() { cube(@ / 2); translate([1, 1, 1]) cube(@); } sphere(0.5, $fn = 6); }",
];

/// 2D shapes for SVG exports.
const SHAPES: &[&str] = &[
    "difference() { square(@); translate([@ / 2, @ / 2]) circle(@ / 4, $fn = 10); }",
    "union() { circle(@ / 2, $fn = 12); translate([@ / 2, 0]) square(@ / 2); }",
    "offset(r = 0.5, $fn = 8) square(@);",
];

fn model(rng: &mut Rng) -> (String, &'static str) {
    let two_d = rng.below(5) == 0;
    let pool = if two_d { SHAPES } else { BODIES };
    let n = 1 + rng.below(3);
    let mut src = String::new();
    for i in 0..n {
        let body = pool[rng.below(pool.len())].replace('@', &(4 + rng.below(2) * 2).to_string());
        let place = if two_d {
            format!("translate([{}, 0])", i * 30)
        } else {
            format!("translate([{}, 0, 0])", i * 30)
        };
        // Overlapping neighbours now and then, so that the top-level
        // union has work to do across bodies.
        let place = if rng.below(3) == 0 {
            String::new()
        } else {
            place
        };
        src.push_str(&format!("{place} {{ {body} }}\n"));
    }
    let format = if two_d {
        "svg"
    } else if rng.below(2) == 0 {
        "off"
    } else {
        "stl"
    };
    (src, format)
}

/// Random histories of renders in one session: every export equals a cold
/// session's. Bounded: four histories of 40 small models, each exported
/// under the agent limits.
#[test]
fn random_histories_export_what_a_cold_session_does() {
    for seed in [
        0x9e37_79b9_7f4a_7c15_u64,
        0x2545_f491_4f6c_dd1d,
        0xd1b5_4a32_d192_ed03,
        7,
    ] {
        let fs = Arc::new(MemFs::new());
        let s = session(&fs);
        let mut rng = Rng(seed);
        let mut colds: HashMap<(String, &str), Vec<u8>> = HashMap::new();
        for step in 0..40 {
            let (src, format) = model(&mut rng);
            fs.insert("/doc/m.scad", src.clone().into_bytes());
            let warm = export(&s, "m.scad", format);
            let cold = colds
                .entry((src.clone(), format))
                .or_insert_with(|| cold(&fs, "m.scad", format));
            assert!(
                warm == *cold,
                "seed {seed:#x} step {step}: the warm {format} export of\n{src}differs from a cold one"
            );
        }
    }
}
