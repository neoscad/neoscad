//! One plain picture of a model, for Quick Look's thumbnails and previews
//! (`docs/audits/macos-prep.md`, step 8h).
//!
//! `snapshot` already draws offscreen, but it draws a contact sheet: a
//! caption, a grid and an axis cross in every panel, laid out for an agent
//! to read. Scaled down to a Finder icon those become noise, and the
//! caption's text is unreadable at 64 pixels. A picture is what
//! `openscad -o model.png` draws instead: the whole model from OpenSCAD's
//! default camera (the diagonal view, fitted and centred), with no
//! overlay, in the Cornfield scheme.
//!
//! The file's own `$vpr`/`$vpt`/`$vpd` are not applied: a thumbnail should
//! show the whole model the same way for every file, and a view set up
//! for an animation or a close-up would crop it.

use render::camera::Camera;

use crate::{Core, CoreError, GeometryStats, RenderMode, guarded, host, types};

/// What `picture` draws.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Record)]
pub struct PictureOptions {
    /// The image in pixels, 16 to 8192 each way. Quick Look asks for
    /// thumbnails as small as 16 points, so the floor is lower than a
    /// snapshot's.
    #[uniffi(default = 512)]
    pub width: u32,
    #[uniffi(default = 512)]
    pub height: u32,
    /// `Preview` (OpenSCAD's F5: fast, coloured, shows `%` and `#`) or a
    /// full render.
    pub mode: RenderMode,
}

/// The result of `picture`.
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct PictureResult {
    /// 0, or the exit code of a model that failed (then no PNG).
    pub exit_code: u8,
    /// An opaque RGB PNG, `width` by `height`.
    pub png: Option<Vec<u8>>,
    /// Whether the model produced nothing to draw (the PNG is then the
    /// empty background, as OpenSCAD draws an empty model).
    pub empty: bool,
    /// `None` for an empty result or a preview.
    pub geometry: Option<GeometryStats>,
    pub diagnostics: Vec<types::Diagnostic>,
    pub console: String,
}

/// The side lengths `picture` accepts.
const SIZES: std::ops::RangeInclusive<u32> = 16..=8192;

#[uniffi::export]
impl Core {
    /// Render (or preview) a document and draw it from the default camera
    /// as a PNG (see the module documentation). Runs under the core's
    /// limits like every request, and `cancel` stops it.
    pub fn picture(
        &self,
        path: String,
        options: PictureOptions,
    ) -> Result<PictureResult, CoreError> {
        guarded(|| {
            let PictureOptions {
                width,
                height,
                mode,
            } = options;
            if !SIZES.contains(&width) || !SIZES.contains(&height) {
                return Err(CoreError::InvalidArgument {
                    message: format!(
                        "the size must be 16 to 8192 pixels each way (got {width}x{height})"
                    ),
                });
            }
            let run = self.run(&path)?;
            let scheme = render::ColorScheme::cornfield();
            let r = self.session().render(&run, mode.into(), &scheme)?;
            let geometry = r
                .geometry
                .as_ref()
                .map(|g| types::geometry_stats(g, &scheme.geometry_scheme()));
            let diagnostics = types::diagnostics(&r.log);
            let console = types::console(&r.log);
            if r.exit_code != 0 {
                return Ok(PictureResult {
                    exit_code: r.exit_code,
                    png: None,
                    empty: true,
                    geometry,
                    diagnostics,
                    console,
                });
            }
            let preview = r.tree.is_some();
            let scene = match &r.tree {
                Some(tree) => render::preview::scene(tree, &scheme, render::Previewer::OpenCsg),
                None => render::Scene::new(r.geometry.as_ref(), &scheme),
            };
            let empty = scene.bounding_box().is_none();
            let mut camera = Camera {
                viewall: true,
                autocenter: true,
                pixel_width: width,
                pixel_height: height,
                ..Camera::default()
            };
            render::fit_camera(&mut camera, &scene);
            // No axes, scale markers or crosshairs: the command line's
            // defaults, and what reads at icon size.
            let overlay = render::overlay::overlay(
                &camera,
                &scheme,
                &render::overlay::ViewOptions::default(),
                preview,
            );
            let gpu = host::offscreen().map_err(|message| CoreError::Failed { message })?;
            let image = gpu
                .render_view_blocking(&scene, &camera, &scheme, &overlay, false)
                .map_err(|e| CoreError::Failed {
                    message: format!("cannot draw the model: {e}"),
                })?;
            Ok(PictureResult {
                exit_code: 0,
                png: Some(render::encode_png(image.width, image.height, &image.rgba)),
                empty,
                geometry,
                diagnostics,
                console,
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use crate::{Core, CoreConfig, CoreError, PictureOptions, PictureResult, RenderMode};

    const DOC: &str = "/NeoSCAD-ffi-test/picture.scad";

    fn options(width: u32, height: u32, mode: RenderMode) -> PictureOptions {
        PictureOptions {
            width,
            height,
            mode,
        }
    }

    fn picture(text: &str, options: PictureOptions) -> Option<PictureResult> {
        let c = Core::new(CoreConfig::default()).unwrap();
        c.open(DOC.into(), Some(text.into())).unwrap();
        match c.picture(DOC.into(), options) {
            Ok(p) => Some(p),
            // A machine without a GPU (CI) cannot draw.
            Err(CoreError::Failed { message }) => {
                eprintln!("skipped: {message}");
                None
            }
            Err(e) => panic!("{e}"),
        }
    }

    /// Width and height from a PNG's IHDR chunk.
    fn png_size(png: &[u8]) -> (u32, u32) {
        assert!(png.starts_with(b"\x89PNG\r\n\x1a\n"));
        let be = |i: usize| u32::from_be_bytes(png[i..i + 4].try_into().unwrap());
        (be(16), be(20))
    }

    #[test]
    fn draws_a_png_of_the_requested_size() {
        for mode in [RenderMode::Preview, RenderMode::Render] {
            let Some(p) = picture("color(\"red\") cube(10);", options(96, 64, mode)) else {
                return;
            };
            assert_eq!(p.exit_code, 0, "{}", p.console);
            assert!(!p.empty);
            assert_eq!(png_size(&p.png.unwrap()), (96, 64));
            assert_eq!(p.geometry.is_some(), mode == RenderMode::Render);
        }
    }

    #[test]
    fn an_empty_model_draws_the_background() {
        let Some(p) = picture("echo(1);", options(32, 32, RenderMode::Preview)) else {
            return;
        };
        assert_eq!(p.exit_code, 0);
        assert!(p.empty);
        assert!(p.png.is_some());
    }

    #[test]
    fn a_failed_model_has_diagnostics_and_no_png() {
        let Some(p) = picture("cube(;", options(32, 32, RenderMode::Preview)) else {
            return;
        };
        assert_ne!(p.exit_code, 0);
        assert!(p.png.is_none());
        assert_eq!(p.diagnostics[0].code, "syntax-error");
    }

    #[test]
    fn sizes_out_of_range_are_invalid() {
        let c = Core::new(CoreConfig::default()).unwrap();
        c.open(DOC.into(), Some("cube(1);".into())).unwrap();
        for (width, height) in [(8, 64), (64, 9000)] {
            assert!(matches!(
                c.picture(DOC.into(), options(width, height, RenderMode::Preview)),
                Err(CoreError::InvalidArgument { .. })
            ));
        }
    }
}
