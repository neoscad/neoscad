//! `-o x.png`: the camera from the command line (`get_camera` in
//! `openscad.cc`), the file's `$vp*` view, and the offscreen render
//! (`export_png` and `prepare_preview` in `src/io/export_png.cc`): the
//! rendered geometry with `--render`, otherwise a preview of the CSG
//! products (the OpenCSG preview, or `--preview=throwntogether`), with the
//! `--view` options drawn around it.

use std::sync::OnceLock;

use render::offscreen::Offscreen;
use render::{Camera, ColorScheme, Previewer, Projection, Scene, ViewOptions};

/// OpenSCAD's general failure exit code.
const EXIT_ERROR: u8 = 1;

/// Everything a PNG export needs besides the geometry.
#[derive(Debug, Clone)]
pub struct Settings {
    /// The camera from `--camera`, `--viewall`, `--autocenter`,
    /// `--projection` and `--imgsize`, before the file's `$vp*`.
    pub camera: Camera,
    pub scheme: ColorScheme,
    /// `None` draws the rendered geometry (`--render`), otherwise the
    /// preview.
    pub previewer: Option<Previewer>,
    pub view: ViewOptions,
    /// `--csglimit`: the most operations a normalised CSG term may have.
    pub csg_limit: usize,
}

/// `viewOptions.renderer` (`openscad.cc:1035-1049`): `--preview` wins over
/// `--render`, and its value only matters when it is `throwntogether`;
/// with neither, a PNG is an OpenCSG preview.
pub fn previewer(render: Option<&str>, preview: Option<&str>) -> Option<Previewer> {
    match (preview, render) {
        (Some("throwntogether"), _) => Some(Previewer::ThrownTogether),
        (Some(_), _) | (None, None) => Some(Previewer::OpenCsg),
        (None, Some(_)) => None,
    }
}

/// `--view` names as OpenSCAD reads them: an unknown name gets its
/// message and is ignored.
pub fn view_options(names: &[String], quiet: bool) -> ViewOptions {
    let mut v = ViewOptions::default();
    for n in names {
        if !v.set(n) && !quiet {
            eprintln!("Unknown --view option '{n}' ignored. Use -h to list available options.");
        }
    }
    v
}

/// `get_camera`: the camera the command line describes. Errors that
/// OpenSCAD answers with `exit(1)` print its message and return the code;
/// numbers it cannot parse print its message and leave the default camera
/// (without `--viewall`, since the parse failed after that choice).
pub fn camera(
    camera: Option<&str>,
    viewall: bool,
    autocenter: bool,
    projection: Option<&str>,
    imgsize: Option<&str>,
) -> Result<Camera, u8> {
    let mut cam = Camera::default();
    if let Some(spec) = camera {
        let parts: Vec<&str> = spec.split(',').collect();
        if parts.len() != 6 && parts.len() != 7 {
            eprintln!(
                "Camera setup requires either 7 numbers for Gimbal Camera or 6 numbers for Vector Camera"
            );
            return Err(EXIT_ERROR);
        }
        // A parse failure is reported by the evaluation options, which
        // read the same flag.
        if let Ok(nums) = parts
            .iter()
            .map(|s| s.parse::<f64>())
            .collect::<Result<Vec<_>, _>>()
        {
            cam.setup(&nums);
        }
    } else {
        cam.viewall = true;
        cam.autocenter = true;
    }
    cam.viewall |= viewall;
    cam.autocenter |= autocenter;
    if let Some(p) = projection {
        cam.projection = match p {
            "o" | "ortho" | "orthogonal" => Projection::Orthogonal,
            "p" | "perspective" => Projection::Perspective,
            _ => {
                eprintln!("projection needs to be 'o' or 'p' for ortho or perspective\n");
                return Err(EXIT_ERROR);
            }
        };
    }
    if let Some(size) = imgsize {
        let parts: Vec<&str> = size.split(',').collect();
        if parts.len() != 2 {
            eprintln!("Need 2 numbers for imgsize");
            return Err(EXIT_ERROR);
        }
        // `boost::lexical_cast<int>`: the whole string must be an integer.
        match (parts[0].parse::<i32>(), parts[1].parse::<i32>()) {
            (Ok(w), Ok(h)) => {
                // OpenSCAD stores them in unsigned ints; a negative size
                // wraps and then fails to allocate. Reject it here instead.
                if w <= 0 || h <= 0 {
                    eprintln!("Need 2 numbers for imgsize");
                    return Err(EXIT_ERROR);
                }
                cam.pixel_width = w as u32;
                cam.pixel_height = h as u32;
            }
            _ => eprintln!("Need 2 numbers for imgsize"),
        }
    }
    Ok(cam)
}

/// `Camera::updateView`'s effect on the image: unless `--camera` locked
/// it, the camera takes the view the evaluation ended with (the defaults
/// or the file's `$vp*`), and `$vp*` assignments that cleared
/// [`eval::Camera::auto`] turn off `--viewall` and `--autocenter`.
pub fn with_view(mut cam: Camera, view: &eval::Camera) -> Camera {
    if cam.locked {
        return cam;
    }
    cam.set_vpr(view.vpr[0], view.vpr[1], view.vpr[2]);
    cam.set_vpt(view.vpt[0], view.vpt[1], view.vpt[2]);
    cam.set_vpd(view.vpd);
    cam.set_vpf(view.vpf);
    if !view.auto {
        cam.viewall = false;
        cam.autocenter = false;
    }
    cam
}

/// The GPU device, opened on first use and kept for later frames of an
/// animation.
pub fn offscreen() -> Result<&'static Offscreen, String> {
    static DEVICE: OnceLock<Result<Offscreen, String>> = OnceLock::new();
    DEVICE
        .get_or_init(|| Offscreen::new_blocking(wgpu_backends()).map_err(|e| e.to_string()))
        .as_ref()
        .map_err(Clone::clone)
}

/// Metal alone on Apple platforms, which saves probing for a Vulkan
/// loader on every cold start; the primary backends elsewhere.
fn wgpu_backends() -> render::offscreen::Backends {
    if cfg!(target_vendor = "apple") {
        render::offscreen::Backends::METAL
    } else {
        render::offscreen::Backends::PRIMARY
    }
}

/// Draw `geometry` (the render result; `None` when empty) as a PNG, and
/// the camera it was drawn with.
pub fn render_png(
    settings: &Settings,
    geometry: Option<&geom::Geometry>,
    view: &eval::Camera,
) -> Result<(Vec<u8>, Camera), String> {
    draw(
        settings,
        &Scene::new(geometry, &settings.scheme),
        view,
        false,
    )
}

/// Draw the preview of `tree` as a PNG, and the camera it was drawn with.
pub fn preview_png(
    settings: &Settings,
    tree: &geom::csg::CsgTree,
    view: &eval::Camera,
) -> Result<(Vec<u8>, Camera), String> {
    let previewer = settings.previewer.unwrap_or(Previewer::OpenCsg);
    let scene = render::preview::scene(tree, &settings.scheme, previewer);
    draw(settings, &scene, view, true)
}

fn draw(
    settings: &Settings,
    scene: &Scene,
    view: &eval::Camera,
    preview: bool,
) -> Result<(Vec<u8>, Camera), String> {
    let mut cam = with_view(settings.camera, view);
    render::fit_camera(&mut cam, scene);
    let overlay = render::overlay::overlay(&cam, &settings.scheme, &settings.view, preview);
    let image = offscreen()?
        .render_view_blocking(scene, &cam, &settings.scheme, &overlay, settings.view.edges)
        .map_err(|e| e.to_string())?;
    Ok((
        render::encode_png(image.width, image.height, &image.rgba),
        cam,
    ))
}

/// The summary's view of the camera an image was drawn with.
pub fn summary_camera(cam: &Camera, view: &eval::Camera) -> eval::Camera {
    eval::Camera {
        vpt: cam.vpt(),
        vpr: cam.vpr(),
        vpd: cam.viewer_distance,
        vpf: cam.fov,
        ..*view
    }
}

/// `set_render_color_scheme(name, true)`: an unknown name prints every
/// scheme's name, one per line, and exits 1.
pub fn scheme(name: Option<&str>) -> Result<ColorScheme, u8> {
    let Some(name) = name.filter(|n| !n.is_empty()) else {
        return Ok(ColorScheme::cornfield());
    };
    let all = render::scheme::all();
    match all.iter().find(|s| s.name == name) {
        Some(s) => Ok(s.clone()),
        None => {
            let names: Vec<&str> = all.iter().map(|s| s.name.as_str()).collect();
            eprintln!("{}", names.join("\n"));
            Err(EXIT_ERROR)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_camera_fits_and_centres() {
        let c = camera(None, false, false, None, None).unwrap();
        assert!(c.viewall && c.autocenter && !c.locked);
        let c = camera(
            Some("0,0,0,90,0,90,200"),
            false,
            false,
            Some("o"),
            Some("500,100"),
        )
        .unwrap();
        assert!(!c.viewall && c.locked);
        assert_eq!(c.projection, Projection::Orthogonal);
        assert_eq!((c.pixel_width, c.pixel_height), (500, 100));
        assert_eq!(camera(Some("1,2,3"), false, false, None, None), Err(1));
        assert_eq!(camera(None, false, false, Some("x"), None), Err(1));
        assert_eq!(camera(None, false, false, None, Some("1,2,3")), Err(1));
    }

    #[test]
    fn file_view_applies_unless_locked() {
        let view = eval::Camera {
            vpt: [5.0, 5.0, 5.0],
            vpd: 200.0,
            auto: false,
            ..eval::Camera::default()
        };
        let c = with_view(camera(None, false, false, None, None).unwrap(), &view);
        assert_eq!((c.vpt(), c.viewer_distance), ([5.0; 3], 200.0));
        assert!(!c.viewall && !c.autocenter);
        let locked = camera(Some("1,2,3,0,0,0"), false, false, None, None).unwrap();
        assert_eq!(with_view(locked, &view), locked);
    }

    #[test]
    fn preview_wins_over_render() {
        assert_eq!(previewer(None, None), Some(Previewer::OpenCsg));
        assert_eq!(previewer(Some(""), None), None);
        assert_eq!(previewer(Some("force"), Some("")), Some(Previewer::OpenCsg));
        assert_eq!(
            previewer(Some(""), Some("throwntogether")),
            Some(Previewer::ThrownTogether)
        );
        let v = view_options(&["axes".into(), "nope".into(), "edges".into()], true);
        assert!(v.axes && v.edges && !v.scales && !v.crosshairs);
    }

    #[test]
    fn unknown_scheme_is_an_error() {
        assert_eq!(scheme(Some("Metallic")).unwrap().name, "Metallic");
        assert_eq!(scheme(None).unwrap().name, "Cornfield");
        assert_eq!(scheme(Some("nope")), Err(1));
    }
}
