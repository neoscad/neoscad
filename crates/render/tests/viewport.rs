//! The interactive viewport's lifecycle, drawn into a texture on the
//! machine's GPU (the window-surface path is exercised through a real
//! `CAMetalLayer` in `crates/ffi`). Skipped with a note when no adapter is
//! available.

#![cfg(feature = "gpu")]

use std::sync::Arc;

use geom::Geometry;
use geom::polyset::PolySet;
use render::offscreen::Backends;
use render::viewport::{Drawn, Gpu, MSAA_SAMPLES, ViewSettings, Viewport};
use render::{ColorScheme, Image, Scene};

fn gpu() -> Option<Arc<Gpu>> {
    match Gpu::new_blocking(Backends::PRIMARY) {
        Ok(g) => Some(Arc::new(g)),
        Err(e) => {
            eprintln!("skipped: {e}");
            None
        }
    }
}

fn cube10() -> Geometry {
    let v = |x: f64, y: f64, z: f64| [x * 10.0, y * 10.0, z * 10.0];
    Geometry::PolySet(Arc::new(PolySet {
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
    }))
}

/// Pixels that differ from the scheme's background.
fn foreground(image: &Image, scheme: &ColorScheme) -> usize {
    let bg = scheme.background.0.map(|c| (c * 255.0).round() as i32);
    image
        .rgba
        .chunks(4)
        .filter(|p| (0..3).any(|i| (i32::from(p[i]) - bg[i]).abs() > 2))
        .count()
}

const BARE: ViewSettings = ViewSettings {
    axes: false,
    scales: false,
    grid: false,
    edges: false,
    crosshairs: false,
    lighting: render::Lighting::OpenScad,
};

#[test]
fn lifecycle_attach_draw_update_detach() {
    let Some(gpu) = gpu() else { return };
    let scheme = ColorScheme::cornfield();
    let mut vp = Viewport::new(gpu.clone(), scheme.clone()).unwrap();
    vp.set_settings(BARE);

    // Detached: nothing to draw into.
    assert!(!vp.needs_draw());
    assert_eq!(vp.draw().unwrap(), Drawn::Idle);
    assert!(vp.read_pixels_blocking().is_err());

    // Attached at 80x60 pixels (40x30 points at 2x): multisampled, and a
    // first frame is due.
    vp.attach_texture(80, 60, 2.0);
    assert_eq!(vp.samples(), MSAA_SAMPLES);
    assert!(vp.needs_draw());
    let empty = vp.read_pixels_blocking().unwrap();
    assert_eq!((empty.width, empty.height), (80, 60));
    assert_eq!(
        foreground(&empty, &scheme),
        0,
        "an empty view is background"
    );
    // Reading drew the frame: nothing is left to do.
    assert!(!vp.needs_draw());
    assert_eq!(vp.draw().unwrap(), Drawn::Idle);

    // A model, uploaded as a background thread would, fitted on arrival.
    let model = Arc::new(gpu.upload(&Scene::new(Some(&cube10()), &scheme)).unwrap());
    assert!(vp.set_model(model.clone(), 1));
    assert_eq!(vp.camera().vpt(), [5.0, 5.0, 5.0]);
    assert!(vp.needs_draw());
    let cube = vp.read_pixels_blocking().unwrap();
    let covered = foreground(&cube, &scheme);
    assert!(
        covered > 80 * 60 / 20,
        "the cube covers the view: {covered}"
    );

    // An older request's model arriving late is ignored.
    let stale = Arc::new(gpu.upload(&Scene::empty(&scheme, None)).unwrap());
    assert!(!vp.set_model(stale, 0));
    assert!(vp.model().is_some_and(|m| Arc::ptr_eq(m, &model)));

    // A camera move needs one frame, then nothing.
    vp.orbit(10.0, 0.0);
    assert!(vp.needs_draw());
    assert_eq!(vp.draw().unwrap(), Drawn::Frame);
    assert_eq!(vp.draw().unwrap(), Drawn::Idle);
    // A second model keeps the camera the user chose.
    let before = *vp.camera();
    assert!(vp.set_model(model, 2));
    assert_eq!(*vp.camera(), before);

    // Collapsed to nothing, then back.
    vp.resize(0, 0, 2.0);
    assert!(!vp.needs_draw());
    vp.resize(40, 40, 1.0);
    assert!(vp.needs_draw());
    assert_eq!(vp.read_pixels_blocking().unwrap().width, 40);

    // Detached: no frames, and the viewport keeps its state for the next
    // target.
    assert!(vp.detach());
    assert!(!vp.detach());
    vp.orbit(1.0, 1.0);
    assert!(!vp.needs_draw());
    assert_eq!(vp.draw().unwrap(), Drawn::Idle);
    vp.attach_texture(32, 32, 1.0);
    assert!(vp.needs_draw());
    assert!(foreground(&vp.read_pixels_blocking().unwrap(), &scheme) > 0);
}

#[test]
fn view_options_and_scheme_change_the_frame() {
    let Some(gpu) = gpu() else { return };
    let scheme = ColorScheme::cornfield();
    let mut vp = Viewport::new(gpu, scheme.clone()).unwrap();
    vp.attach_texture(96, 96, 1.0);
    vp.set_settings(BARE);
    assert_eq!(foreground(&vp.read_pixels_blocking().unwrap(), &scheme), 0);

    vp.set_settings(ViewSettings { grid: true, ..BARE });
    assert!(vp.needs_draw(), "a settings change asks for a frame");
    let grid = foreground(&vp.read_pixels_blocking().unwrap(), &scheme);
    assert!(grid > 0, "the grid draws lines");

    vp.set_settings(ViewSettings { axes: true, ..BARE });
    assert!(foreground(&vp.read_pixels_blocking().unwrap(), &scheme) > 0);

    let dark = render::scheme::find("Tomorrow Night").unwrap();
    vp.set_settings(BARE);
    vp.set_scheme(dark.clone());
    assert!(vp.needs_draw());
    let image = vp.read_pixels_blocking().unwrap();
    assert_eq!(foreground(&image, &dark), 0, "drawn in the new background");
    assert!(foreground(&image, &scheme) > 0);
}

#[test]
fn msaa_softens_edges_that_single_sample_leaves_hard() {
    let Some(gpu) = gpu() else { return };
    let scheme = ColorScheme::cornfield();
    let mut vp = Viewport::new(gpu.clone(), scheme.clone()).unwrap();
    vp.set_settings(BARE);
    vp.attach_texture(128, 128, 1.0);
    let model = Arc::new(gpu.upload(&Scene::new(Some(&cube10()), &scheme)).unwrap());
    vp.set_model(model, 1);
    let image = vp.read_pixels_blocking().unwrap();
    // A single-sample cube is its background plus three lit face colours
    // (the offscreen tests check exactly that); resolved samples add the
    // blends along every silhouette and crease.
    let mut colours: Vec<[u8; 4]> = image
        .rgba
        .chunks(4)
        .map(|p| [p[0], p[1], p[2], p[3]])
        .collect();
    colours.sort_unstable();
    colours.dedup();
    assert!(colours.len() > 8, "{} colours", colours.len());
}
