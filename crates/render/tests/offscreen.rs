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

/// Pixels that are not the scheme's background.
fn foreground(image: &render::Image, scheme: &ColorScheme) -> usize {
    let bg = scheme.background.0.map(|c| (c * 255.0).round() as i32);
    image
        .rgba
        .chunks(4)
        .filter(|p| (0..3).any(|i| (i32::from(p[i]) - bg[i]).abs() > 2))
        .count()
}

/// `polyhedron-tests.scad`'s octahedron scaled by 10, its faces as
/// `polyhedron()` stores them (reversed), optionally all flipped.
fn octahedron(inside_out: bool) -> Arc<PolySet> {
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
}

/// A preview product drawn in image space as `preview.rs` lays it out:
/// the octahedron minus `cubes` (10 mm cubes at the given corners), then
/// the colour pass.
fn image_csg_scene(positive: Arc<PolySet>, cubes: &[[f64; 3]], scheme: &ColorScheme) -> Scene {
    use render::scene::{CsgOp, CsgPrimitive, Cull, Depth, DrawState, Surface};
    let Geometry::PolySet(cube) = cube10() else {
        unreachable!()
    };
    let at = |t: [f64; 3]| {
        let mut m = geom::IDENTITY;
        for (r, x) in t.into_iter().enumerate() {
            m[r][3] = x;
        }
        m
    };
    let mut scene = Scene::empty(scheme, Some(([-10.0; 3], [10.0; 3])));
    let mut primitives = vec![CsgPrimitive {
        mesh: positive.clone(),
        matrix: None,
        op: CsgOp::Intersection,
    }];
    primitives.extend(cubes.iter().map(|&t| CsgPrimitive {
        mesh: cube.clone(),
        matrix: Some(at(t)),
        op: CsgOp::Subtraction,
    }));
    scene.push_image_csg(primitives);
    let state = |cull| DrawState {
        cull,
        depth: Depth::Equal,
        color_write: true,
        bias: false,
    };
    scene.push(Surface {
        mesh: positive,
        matrix: None,
        color: scheme.opencsg_face_front,
        force_color: true,
        lit: true,
        state: state(Cull::None),
    });
    for &t in cubes {
        scene.push(Surface {
            mesh: cube.clone(),
            matrix: Some(at(t)),
            color: scheme.opencsg_face_back,
            force_color: true,
            lit: true,
            state: state(Cull::Front),
        });
    }
    scene
}

/// OpenCSG's SCS reads a face by its winding on screen: an octahedron cut
/// by cubes shows its cut faces in the cut-out colour, and the same
/// octahedron inside out, cut the same way, shows nothing at all (the
/// second row of `polyhedron-tests.scad`'s preview). Three cubes take the
/// Schoenfield sequence past its short cases.
#[test]
fn image_space_csg_cuts_a_solid_and_drops_an_inside_out_one() {
    let Some(gpu) = gpu() else { return };
    let scheme = ColorScheme::cornfield();
    let cubes = [[0.0, 0.0, 0.0], [-10.0, -10.0, -10.0], [-10.0, 0.0, 0.0]];
    let draw = |inside_out| {
        let scene = image_csg_scene(octahedron(inside_out), &cubes, &scheme);
        let camera = default_view(&scene);
        gpu.render_blocking(&scene, &camera, &scheme).unwrap()
    };
    let solid = draw(false);
    assert!(foreground(&solid, &scheme) > 1000);
    let cut = scheme
        .opencsg_face_back
        .0
        .map(|c| (c * 255.0).round() as i32);
    let cut_pixels = solid
        .rgba
        .chunks(4)
        .filter(|p| (0..3).all(|i| i32::from(p[i]) <= cut[i] + 2))
        .filter(|p| i32::from(p[1]) > i32::from(p[0]))
        .count();
    assert!(cut_pixels > 100, "the cuts show in the cut-out colour");
    assert_eq!(foreground(&draw(true), &scheme), 0);
}
