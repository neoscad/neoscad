//! Offscreen rendering on the machine's GPU. Each test is skipped (with a
//! note) when no adapter is available, as on a headless CI runner without
//! a software Vulkan driver.

#![cfg(feature = "gpu")]

use std::path::PathBuf;
use std::sync::Arc;

use geom::Geometry;
use geom::polygon2d::Polygon2d;
use geom::polyset::PolySet;
use render::offscreen::{Backends, Offscreen};
use render::{Camera, ColorScheme, Scene};

fn gpu() -> Option<Offscreen> {
    match Offscreen::new_blocking(Backends::PRIMARY) {
        Ok(o) => Some(o),
        Err(e) => {
            eprintln!("skipped: {e}");
            None
        }
    }
}

/// `cube(10)` as OpenSCAD's primitive builds it: eight corners, six quads.
fn cube10() -> Geometry {
    let v = |x: f64, y: f64, z: f64| [x * 10.0, y * 10.0, z * 10.0];
    let ps = PolySet {
        vertices: vec![
            v(0.0, 0.0, 0.0),
            v(1.0, 0.0, 0.0),
            v(1.0, 1.0, 0.0),
            v(0.0, 1.0, 0.0),
            v(0.0, 0.0, 1.0),
            v(1.0, 0.0, 1.0),
            v(1.0, 1.0, 1.0),
            v(0.0, 1.0, 1.0),
        ],
        faces: vec![
            vec![4, 5, 6, 7],
            vec![3, 2, 1, 0],
            vec![0, 1, 5, 4],
            vec![1, 2, 6, 5],
            vec![2, 3, 7, 6],
            vec![3, 0, 4, 7],
        ],
        convex: Some(true),
        ..Default::default()
    };
    Geometry::PolySet(Arc::new(ps))
}

fn default_view(scene: &Scene) -> Camera {
    let mut camera = Camera {
        viewall: true,
        autocenter: true,
        ..Camera::default()
    };
    render::fit_camera(&mut camera, scene);
    camera
}

/// The same scene twice, and again from a second device, gives the same
/// PNG bytes: rasterisation on one GPU is deterministic and the encoder
/// adds nothing variable.
#[test]
fn same_input_same_png_bytes() {
    let Some(gpu) = gpu() else { return };
    let scheme = ColorScheme::cornfield();
    let scene = Scene::new(Some(&cube10()), &scheme);
    let camera = default_view(&scene);
    let png = |o: &Offscreen| {
        let img = o.render_blocking(&scene, &camera, &scheme).unwrap();
        render::encode_png(img.width, img.height, &img.rgba)
    };
    let first = png(&gpu);
    assert_eq!(png(&gpu), first);
    let other = Offscreen::new_blocking(Backends::PRIMARY).unwrap();
    assert_eq!(png(&other), first);
}

/// `render-monotone_cube10`: the Monotone scheme's cube with the default
/// camera is OpenSCAD's four colours (background and three lit faces),
/// and matches the expected image but for a handful of edge pixels.
#[test]
fn monotone_cube_matches_openscads_image() {
    let Some(gpu) = gpu() else { return };
    let scheme = render::scheme::find("Monotone").unwrap();
    let scene = Scene::new(Some(&cube10()), &scheme);
    let camera = default_view(&scene);
    let img = gpu.render_blocking(&scene, &camera, &scheme).unwrap();
    assert_eq!((img.width, img.height), (512, 512));
    let mut colors: Vec<[u8; 3]> = img.rgba.chunks(4).map(|p| [p[0], p[1], p[2]]).collect();
    colors.sort();
    colors.dedup();
    assert_eq!(
        colors,
        [
            [143, 123, 25],
            [165, 143, 29],
            [250, 216, 44],
            [255, 255, 229]
        ]
    );

    let expected = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../.reference/openscad/tests/regression/render-monotone/cube10-expected.png");
    let Ok(file) = std::fs::File::open(&expected) else {
        eprintln!("skipped comparison: no reference checkout");
        return;
    };
    let mut reader = png::Decoder::new(std::io::BufReader::new(file))
        .read_info()
        .unwrap();
    let mut buf = vec![0; reader.output_buffer_size().unwrap()];
    reader.next_frame(&mut buf).unwrap();
    let differing = buf
        .chunks(3)
        .zip(img.rgba.chunks(4))
        .filter(|(e, a)| e[..3] != a[..3])
        .count();
    assert!(differing <= 10, "{differing} pixels differ");
}

/// A 2D square: the fill in the scheme's 2D colour, unlit, and red
/// outlines two pixels wide.
#[test]
fn square_is_flat_with_red_outline() {
    let Some(gpu) = gpu() else { return };
    let scheme = ColorScheme::cornfield();
    let square = Polygon2d::from_outline(vec![[0.0, 0.0], [10.0, 0.0], [10.0, 10.0], [0.0, 10.0]]);
    let scene = Scene::new(Some(&Geometry::Polygon2d(Arc::new(square))), &scheme);
    let camera = default_view(&scene);
    let img = gpu.render_blocking(&scene, &camera, &scheme).unwrap();
    let count = |c: [u8; 3]| img.rgba.chunks(4).filter(|p| p[..3] == c).count();
    let (fill, edge) = (count([0, 191, 153]), count([255, 0, 0]));
    assert!(fill > 10_000, "{fill} fill pixels");
    assert!(edge > 1_000, "{edge} outline pixels");
    assert_eq!(count([255, 255, 229]) + fill + edge, 512 * 512);
}
