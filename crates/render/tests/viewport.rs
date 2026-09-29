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

#[test]
fn annotations_draw_over_the_model_and_stay_out_of_the_image() {
    use render::viewport::{AnnotationLine, AnnotationMarker, Annotations};
    let Some(gpu) = gpu() else { return };
    let scheme = ColorScheme::cornfield();
    let mut vp = Viewport::new(gpu.clone(), scheme.clone()).unwrap();
    vp.set_settings(BARE);
    vp.attach_texture(160, 120, 1.0);
    let model = gpu.upload(&Scene::new(None, &scheme)).unwrap();
    vp.set_model(Arc::new(model), 1);
    let empty = vp.read_pixels_blocking().unwrap();
    assert_eq!(foreground(&empty, &scheme), 0);
    // A square outline around the view centre and a labelled marker at
    // the origin: both draw on an empty scene.
    let s = 20.0;
    vp.set_annotations(Annotations {
        lines: vec![AnnotationLine {
            points: vec![[-s, -s, 0.0], [s, -s, 0.0], [s, s, 0.0], [-s, s, 0.0]],
            closed: true,
            color: [1.0, 0.0, 0.0, 1.0],
        }],
        markers: vec![AnnotationMarker {
            point: [0.0, 0.0, 0.0],
            label: "1".into(),
            color: [0.0, 0.0, 1.0, 1.0],
        }],
    });
    assert!(vp.needs_draw());
    let marked = vp.read_pixels_blocking().unwrap();
    let red = marked
        .rgba
        .chunks(4)
        .filter(|p| p[0] > 200 && p[1] < 80 && p[2] < 80)
        .count();
    let blue = marked
        .rgba
        .chunks(4)
        .filter(|p| p[2] > 200 && p[0] < 80 && p[1] < 80)
        .count();
    assert!(red > 50 && blue > 20, "red {red}, blue {blue}");
    // The image of the view leaves them out.
    let mut copy = vp.copy_for_image(160, 120).unwrap();
    let image = copy.read_pixels_blocking().unwrap();
    assert_eq!((image.width, image.height), (160, 120));
    assert_eq!(foreground(&image, &scheme), 0);
}

/// View All fits both directions of the view at its current size, and an
/// untouched fit follows resizes until the camera is moved.
#[test]
fn view_all_fits_the_view_shape_and_follows_resizes() {
    let Some(gpu) = gpu() else { return };
    let scheme = ColorScheme::cornfield();
    let model = Arc::new(gpu.upload(&Scene::new(Some(&cube10()), &scheme)).unwrap());
    let fitted = |w: u32, h: u32| {
        let mut c = render::Camera {
            pixel_width: w,
            pixel_height: h,
            ..render::Camera::default()
        };
        c.view_all_to_fit(Some(([0.0; 3], [10.0; 3])));
        c.viewer_distance
    };
    let square = fitted(100, 100);
    let tall = fitted(100, 300);
    assert!(tall > square);

    // The model arrives before the view has a size: it fits as a square,
    // then again at the size the view is given.
    let mut vp = Viewport::new(gpu.clone(), scheme.clone()).unwrap();
    assert!(vp.set_model(model.clone(), 1));
    assert_eq!(vp.camera().viewer_distance, square);
    vp.attach_texture(100, 300, 1.0);
    assert_eq!(vp.camera().viewer_distance, tall);
    // A wider view (the fit is OpenSCAD's there) and back.
    vp.resize(300, 100, 1.0);
    assert_eq!(vp.camera().viewer_distance, square);
    vp.resize(100, 300, 1.0);
    assert_eq!(vp.camera().viewer_distance, tall);
    // A redraw is not a camera move.
    vp.redraw();
    vp.resize(100, 100, 1.0);
    assert_eq!(vp.camera().viewer_distance, square);

    // Once the camera moves, a resize leaves it where the user put it;
    // View All fits (and follows) again.
    vp.orbit(10.0, 0.0);
    vp.resize(100, 300, 1.0);
    assert_eq!(vp.camera().viewer_distance, square);
    vp.view_all();
    assert_eq!(vp.camera().viewer_distance, tall);
    vp.resize(100, 100, 1.0);
    assert_eq!(vp.camera().viewer_distance, square);
    vp.with_camera(|c| c.zoom_by(2.0));
    vp.resize(100, 300, 1.0);
    assert_eq!(vp.camera().viewer_distance, square / 2.0);
}

#[test]
fn an_image_of_the_view_shows_the_model_from_the_same_camera() {
    let Some(gpu) = gpu() else { return };
    let scheme = ColorScheme::cornfield();
    let mut vp = Viewport::new(gpu.clone(), scheme.clone()).unwrap();
    vp.set_settings(BARE);
    vp.attach_texture(200, 200, 1.0);
    let model = gpu.upload(&Scene::new(Some(&cube10()), &scheme)).unwrap();
    vp.set_model(Arc::new(model), 1);
    let shown = vp.read_pixels_blocking().unwrap();
    let mut copy = vp.copy_for_image(200, 200).unwrap();
    let image = copy.read_pixels_blocking().unwrap();
    assert!(foreground(&image, &scheme) > 1000);
    assert_eq!(image.rgba, shown.rgba);
    // Looking at a point moves the view's centre there.
    vp.look_at([5.0, 5.0, 5.0]);
    assert_eq!(vp.camera().vpt(), [5.0, 5.0, 5.0]);
    // The ray through the middle of the view passes through that centre.
    let (o, d) = vp.ray_at(100.0, 100.0).unwrap();
    let v = [5.0 - o[0], 5.0 - o[1], 5.0 - o[2]];
    let t = v[0] * d[0] + v[1] * d[1] + v[2] * d[2];
    let miss = (0..3)
        .map(|i| (v[i] - t * d[i]).powi(2))
        .sum::<f64>()
        .sqrt();
    assert!(miss < 1e-6, "{miss}");
}

/// An image-space CSG product in a multisampled view: the ID buffer has
/// one sample a pixel and the frame four, and the merge reads one from the
/// other. An inside-out octahedron minus a cube draws nothing; the same
/// scene with an ordinary octahedron draws it.
#[test]
fn image_space_csg_draws_in_a_multisampled_view() {
    use render::scene::{CsgOp, CsgPrimitive, Cull, Depth, DrawState, Surface};
    let Some(gpu) = gpu() else { return };
    let scheme = ColorScheme::cornfield();
    let octahedron = |inside_out: bool| {
        let faces = [
            [0, 4, 2],
            [0, 2, 5],
            [0, 3, 4],
            [0, 5, 3],
            [1, 2, 4],
            [1, 5, 2],
            [1, 4, 3],
            [1, 3, 5],
        ];
        Arc::new(PolySet {
            vertices: vec![
                [10.0, 0.0, 0.0],
                [-10.0, 0.0, 0.0],
                [0.0, 10.0, 0.0],
                [0.0, -10.0, 0.0],
                [0.0, 0.0, 10.0],
                [0.0, 0.0, -10.0],
            ],
            faces: faces
                .iter()
                .map(|&[a, b, c]| {
                    if inside_out {
                        vec![a, b, c]
                    } else {
                        vec![c, b, a]
                    }
                })
                .collect(),
            triangular: true,
            ..Default::default()
        })
    };
    let Geometry::PolySet(cube) = cube10() else {
        unreachable!()
    };
    let foreground_of = |inside_out| {
        let oct = octahedron(inside_out);
        let mut scene = Scene::empty(&scheme, Some(([-10.0; 3], [10.0; 3])));
        scene.push_image_csg(vec![
            CsgPrimitive {
                mesh: oct.clone(),
                matrix: None,
                op: CsgOp::Intersection,
            },
            CsgPrimitive {
                mesh: cube.clone(),
                matrix: None,
                op: CsgOp::Subtraction,
            },
        ]);
        scene.push(Surface {
            mesh: oct,
            matrix: None,
            color: scheme.opencsg_face_front,
            force_color: true,
            lit: true,
            state: DrawState {
                cull: Cull::None,
                depth: Depth::Equal,
                color_write: true,
                bias: false,
            },
        });
        let mut vp = Viewport::new(gpu.clone(), scheme.clone()).unwrap();
        vp.set_settings(BARE);
        vp.attach_texture(128, 128, 1.0);
        vp.set_model(Arc::new(gpu.upload(&scene).unwrap()), 1);
        foreground(&vp.read_pixels_blocking().unwrap(), &scheme)
    };
    assert!(foreground_of(false) > 100);
    assert_eq!(foreground_of(true), 0);
}
