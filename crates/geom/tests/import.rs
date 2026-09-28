//! `import()` and `surface()` through the evaluator and renderer, reading
//! from an in-memory file system.

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use geom::{Geometry, RenderOptions, Renderer};
use lang::loader::FileSystem;

#[derive(Default)]
struct MemFs {
    files: HashMap<PathBuf, Vec<u8>>,
    reads: AtomicUsize,
}

impl FileSystem for MemFs {
    fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        self.files
            .get(path)
            .cloned()
            .ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))
    }
    fn exists(&self, path: &Path) -> bool {
        self.files.contains_key(path)
    }
    fn is_dir(&self, _: &Path) -> bool {
        false
    }
    fn canonicalize(&self, path: &Path) -> Option<PathBuf> {
        self.exists(path).then(|| path.to_path_buf())
    }
}

fn fs(files: &[(&str, &[u8])]) -> Arc<MemFs> {
    Arc::new(MemFs {
        files: files
            .iter()
            .map(|(p, b)| (PathBuf::from(p), b.to_vec()))
            .collect(),
        reads: AtomicUsize::new(0),
    })
}

fn render(r: &Renderer, fs: Arc<MemFs>, src: &str) -> (Option<Geometry>, Vec<String>) {
    let path = PathBuf::from("/mem/test.scad");
    let mut text = src.as_bytes().to_vec();
    text.extend_from_slice(b"\n\x03\n");
    let program = lang::parse_file(path, text);
    let mut out = eval::Collect::default();
    let ev = eval::with_stack(eval::DEFAULT_THREAD_STACK, || {
        eval::evaluate(
            &program,
            &[],
            &[],
            PathBuf::from("/mem"),
            &eval::Options::default(),
            &mut out,
        )
    });
    let keys = eval::dump::Keys::new(&ev.root, &*fs);
    let opts = RenderOptions {
        fs,
        work_dir: PathBuf::from("/mem"),
        ..Default::default()
    };
    let out = r.render(&ev.root, &keys, opts).expect("supported");
    let msgs = out
        .messages
        .iter()
        .map(|m| {
            format!(
                "{:?}: {} @{}",
                m.severity,
                m.text,
                m.loc.as_ref().map_or(0, |l| l.line)
            )
        })
        .collect();
    (out.geometry, msgs)
}

const TRI_STL: &[u8] = b"solid t\n facet normal 0 0 1\n  outer loop\n   vertex 0 0 0\n   vertex 4 0 0\n   vertex 0 2 0\n  endloop\n endfacet\nendsolid t\n";

#[test]
fn stl_import_centres_and_is_cached() {
    let files = fs(&[("/mem/t.stl", TRI_STL)]);
    let r = Renderer::new();
    let (g, msgs) = render(&r, files.clone(), "import(\"t.stl\", center=true);");
    assert!(msgs.is_empty(), "{msgs:?}");
    let Some(Geometry::PolySet(ps)) = g else {
        panic!("expected a mesh")
    };
    assert_eq!(
        ps.vertices,
        vec![[-2.0, -1.0, 0.0], [2.0, -1.0, 0.0], [-2.0, 1.0, 0.0]]
    );
    assert_eq!(files.reads.load(Ordering::SeqCst), 1);
    // The same file and parameters: the cached mesh, no second read.
    render(&r, files.clone(), "import(\"t.stl\", center=true);");
    assert_eq!(files.reads.load(Ordering::SeqCst), 1);
    // Different parameters are a different key.
    render(&r, files.clone(), "import(\"t.stl\");");
    assert_eq!(files.reads.load(Ordering::SeqCst), 2);
}

#[test]
fn missing_and_unknown_files_report_like_openscad() {
    let (g, msgs) = render(&Renderer::new(), fs(&[]), "import(\"gone.stl\");");
    assert!(g.is_some_and(|g| g.dimension() == 3 && g.is_empty()));
    assert_eq!(
        msgs,
        ["Some(Warning): Can't open import file '/mem/gone.stl', import() at line 1 @0"]
    );
    let (_, msgs) = render(&Renderer::new(), fs(&[]), "\nimport(\"x.abc\");");
    assert_eq!(
        msgs,
        [
            "Some(Error): Unsupported file format while trying to import file '\"x.abc\"', import() at line 2 @0"
        ]
    );
    let (_, msgs) = render(&Renderer::new(), fs(&[]), "surface(\"gone.dat\");");
    assert_eq!(
        msgs,
        ["Some(Warning): The file '/mem/gone.dat' couldn't be opened. @0"]
    );
}

#[test]
fn svg_and_dat_imports() {
    let svg: &[u8] = br#"<svg width="10mm" height="10mm" viewBox="0 0 10 10"><rect x="0" y="0" width="10" height="5"/></svg>"#;
    let files = fs(&[("/mem/r.svg", svg), ("/mem/h.dat", b"1 2\n3 4\n")]);
    let (g, msgs) = render(&Renderer::new(), files.clone(), "import(\"r.svg\");");
    assert!(msgs.is_empty(), "{msgs:?}");
    let Some(Geometry::Polygon2d(p)) = g else {
        panic!("expected 2D")
    };
    assert_eq!(p.bounds(), Some(([0.0, 5.0], [10.0, 10.0])));
    let (g, _) = render(&Renderer::new(), files, "surface(\"h.dat\");");
    let Some(Geometry::PolySet(ps)) = g else {
        panic!("expected a mesh")
    };
    assert_eq!(ps.faces.len(), 9);
}

/// A closed bipyramid with `n` sides and a different colour on each of its
/// `2n` faces, as OFF.
fn coloured_bipyramid(n: usize, shade: usize) -> Vec<u8> {
    use std::fmt::Write;
    let mut t = format!("OFF\n{} {} 0\n", n + 2, 2 * n);
    for i in 0..n {
        let a = std::f64::consts::TAU * i as f64 / n as f64;
        writeln!(t, "{} {} 0", 5.0 * a.cos(), 5.0 * a.sin()).unwrap();
    }
    t += "0 0 4\n0 0 -4\n";
    for i in 0..n {
        let j = (i + 1) % n;
        for (k, face) in [[n, i, j], [n + 1, j, i]].iter().enumerate() {
            let c = 2 * i + k + shade;
            writeln!(
                t,
                "3 {} {} {} {} {} {} 255",
                face[0],
                face[1],
                face[2],
                (c * 37) % 256,
                (c * 91) % 256,
                (c * 13) % 256
            )
            .unwrap();
        }
    }
    t.into_bytes()
}

/// A mesh with more colours than an ID block holds, converted inside
/// sibling subtrees that run on different threads: the extra IDs used to
/// come from Manifold's global counter in whatever order the threads drew
/// them, so the parent union ordered the siblings' faces differently from
/// run to run. Now the render starts again with blocks big enough.
#[test]
fn many_coloured_meshes_export_identically_every_time() {
    let a = coloured_bipyramid(40, 0);
    let b = coloured_bipyramid(40, 1);
    let files = fs(&[("/mem/a.off", &a), ("/mem/b.off", &b)]);
    let src = "union() { cube(1); import(\"a.off\"); }\n\
               translate([20,0,0]) union() { cube(2); import(\"b.off\"); }\n\
               translate([40,0,0]) union() { cube(3); import(\"a.off\"); }\n\
               translate([60,0,0]) union() { cube(4); import(\"b.off\"); }";
    let export = || {
        let (g, msgs) = render(&Renderer::new(), files.clone(), src);
        assert!(msgs.is_empty(), "{msgs:?}");
        let ps =
            geom::export::as_polyset(&g.expect("geometry"), &geom::color::CORNFIELD).expect("3D");
        assert!(ps.colors.len() > 64, "{} colours", ps.colors.len());
        geom::export::off(&ps, &mut Vec::new())
    };
    let first = export();
    for run in 0..12 {
        assert!(export() == first, "run {run} exported different bytes");
    }
}

/// `.nef3`: facets with holes become outward triangles in the scheme's
/// front colour, `center` is ignored as in OpenSCAD, and a file CGAL would
/// reject is OpenSCAD's warning plus CGAL's text and an empty mesh.
#[test]
fn nef3_imports_tessellate_holes_and_report_cgal_failures() {
    let hole: &[u8] = include_bytes!("../../io/tests/data/square-hole.nef3");
    let files = fs(&[("/mem/h.nef3", hole), ("/mem/bad.nef3", b"Selective Nef\n")]);
    let (g, msgs) = render(
        &Renderer::new(),
        files.clone(),
        "import(\"h.nef3\", center=true);",
    );
    assert!(msgs.is_empty(), "{msgs:?}");
    let Some(Geometry::PolySet(ps)) = g else {
        panic!("expected a mesh")
    };
    // A 4 x 4 x 4 cube with a 2 x 2 hole through it, as the nightly
    // meshes it: 16 vertices, 32 triangles.
    assert_eq!((ps.vertices.len(), ps.faces.len()), (16, 32));
    assert!(ps.triangular);
    let volume: f64 = ps
        .faces
        .iter()
        .map(|f| {
            let [a, b, c] = [0, 1, 2].map(|k| ps.vertices[f[k] as usize]);
            (a[0] * (b[1] * c[2] - b[2] * c[1]) - a[1] * (b[0] * c[2] - b[2] * c[0])
                + a[2] * (b[0] * c[1] - b[1] * c[0]))
                / 6.0
        })
        .sum();
    assert!((volume - 48.0).abs() < 1e-9, "{volume}");
    assert_eq!(ps.bounds(), Some(([-2.0; 3], [2.0; 3])));
    let (g, msgs) = render(&Renderer::new(), files, "import(\"bad.nef3\");");
    assert!(g.is_some_and(|g| g.dimension() == 3 && g.is_empty()));
    assert_eq!(msgs.len(), 2);
    assert_eq!(
        msgs[0],
        "Some(Warning): Failure trying to import '/mem/bad.nef3', import() at line 1 @0"
    );
    assert!(msgs[1].starts_with("None: CGAL ERROR: warning condition failed!"));
    assert!(msgs[1].contains("Explanation: SNC_io_parser::read: no SNC header."));
}
