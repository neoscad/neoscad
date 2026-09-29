//! Packed scenes for the viewer's demo page (`crates/web-view/demo/`),
//! built natively through the same session the web worker runs:
//!
//!     cargo run -p neoscad-web-view --example pack_fixtures -- OUT_DIR
//!
//! For each model it writes `NAME.faces.bin`, `NAME.edges.bin` and
//! `NAME.meta.json` (the three parts of a `render::PackedScene`, as the
//! worker sends them), and `index.json` lists the names with their sizes.
//! The models are small on purpose: a render, a preview with a highlighted
//! (`#`) object, a preview whose inside-out polyhedron is drawn with
//! image-space CSG, and 2D text with its outlines.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use lang::loader::{FileSystem, LibraryPath};
use lang::vfs::MemFs;

const LIBRARY_DIR: &str = "/neoscad/libraries";

const MODELS: &[(&str, session::Mode, &str)] = &[
    (
        "render",
        session::Mode::Render,
        "difference() { color(\"gold\") cube(20, center = true); sphere(13, $fn = 48); }\n\
         translate([25, 0, 0]) cylinder(h = 20, r1 = 8, r2 = 3, center = true, $fn = 32);\n",
    ),
    (
        "preview",
        session::Mode::Preview,
        "difference() { cube(20, center = true); #cylinder(h = 30, r = 6, center = true, $fn = 32); }\n\
         %translate([0, 0, 18]) sphere(5, $fn = 24);\n",
    ),
    (
        // A polyhedron with every face reversed, subtracted: not a solid,
        // so its whole product is drawn with OpenCSG's image-space
        // algorithm (the primitive ID buffer), and the cylinder's hole
        // through the cube shows that algorithm cutting.
        "image-csg",
        session::Mode::Preview,
        "difference() {\n\
           cube(10, center = true);\n\
           cylinder(h = 20, r = 3, center = true, $fn = 24);\n\
           translate([6, 0, 0]) polyhedron(points = [[-3,-3,-8],[3,-3,-8],[3,3,-8],[-3,3,-8],\n\
                                [-3,-3,8],[3,-3,8],[3,3,8],[-3,3,8]],\n\
                      faces = [[3,2,1,0],[4,5,6,7],[0,1,5,4],[1,2,6,5],[2,3,7,6],[3,0,4,7]]);\n\
         }\n",
    ),
    (
        "2d",
        session::Mode::Render,
        "text(\"NeoSCAD\", size = 10);\n",
    ),
];

fn main() {
    // No default: the fixtures are build output and belong under the
    // target directory (`scripts/web/build-view.sh --demo` passes one),
    // not in the source tree.
    let Some(out) = std::env::args().nth(1).map(PathBuf::from) else {
        eprintln!("usage: pack_fixtures OUT_DIR");
        std::process::exit(2);
    };
    std::fs::create_dir_all(&out).expect("create the output directory");
    let files = Arc::new(MemFs::new());
    let base: Arc<dyn FileSystem + Send + Sync> = files.clone();
    let fs: Arc<dyn FileSystem + Send + Sync> = Arc::new(assets::libraries(base, LIBRARY_DIR));
    let mut cfg = session::Config::new(fs, LibraryPath(vec![PathBuf::from(LIBRARY_DIR)]));
    cfg.work_dir = PathBuf::from("/doc");
    cfg.fonts = Arc::new(|_used: &[String]| {
        let mut db = text::FontDb::new();
        assets::add_fonts(&mut db);
        db
    });
    cfg.limits = session::Limits::AGENT;
    let s = session::Session::new(cfg);
    let scheme = render::ColorScheme::cornfield();
    let mut index = Vec::new();
    for (name, mode, src) in MODELS {
        s.update(Path::new("main.scad"), src.as_bytes().to_vec());
        let r = s
            .render(&session::Run::new("main.scad"), *mode, &scheme)
            .expect("not cancelled");
        assert_eq!(
            r.exit_code,
            0,
            "{name}: {}",
            String::from_utf8_lossy(&r.log.stderr)
        );
        let scene = match (&r.tree, &r.geometry) {
            (Some(tree), _) => render::preview::scene(tree, &scheme, render::Previewer::OpenCsg),
            (None, g) => render::Scene::new(g.as_ref(), &scheme),
        };
        let packed = scene.pack();
        let meta = packed.meta.to_json();
        std::fs::write(out.join(format!("{name}.faces.bin")), &packed.faces).expect("write");
        std::fs::write(out.join(format!("{name}.edges.bin")), &packed.edges).expect("write");
        std::fs::write(out.join(format!("{name}.meta.json")), &meta).expect("write");
        println!(
            "{name}: {} face vertices, {} edge segments, {} draws, {} image-space products",
            packed.face_vertex_count(),
            packed.edge_segment_count(),
            packed.meta.draws.len(),
            packed.meta.image_csg.len()
        );
        index.push(serde_json::json!({
            "name": name,
            "faceBytes": packed.faces.len(),
            "edgeBytes": packed.edges.len(),
            "imageCsg": packed.meta.image_csg.len(),
        }));
    }
    std::fs::write(
        out.join("index.json"),
        serde_json::to_string_pretty(&index).expect("json"),
    )
    .expect("write");
}
