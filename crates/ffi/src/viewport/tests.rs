//! The viewport through the bridge, drawing into a real `CAMetalLayer`
//! that is in no window: the surface is made from a raw layer pointer
//! exactly as the app makes it, and frames are read back from the layer's
//! drawables. Skipped with a note when there is no GPU.

use super::*;
use crate::{CoreConfig, CoreError};

const DOC: &str = "/NeoSCAD-ffi-viewport-test/model.scad";

fn core(text: &str) -> Arc<Core> {
    let c = Core::new(CoreConfig {
        resource_dir: None,
        test_hooks: true,
    })
    .unwrap();
    c.open(DOC.into(), Some(text.into())).unwrap();
    c
}

fn viewport() -> Option<Arc<Viewport>> {
    match Viewport::new("Cornfield".into()) {
        Ok(v) => Some(v),
        Err(e) => {
            eprintln!("skipped: {e}");
            None
        }
    }
}

/// Settings with nothing but the model, so "not background" means model.
fn bare(v: &Viewport) {
    v.set_settings(ViewportSettings {
        axes: false,
        scales: false,
        grid: false,
        edges: false,
        crosshairs: false,
        lighting: LightingStyle::OpenScad,
        orthographic: false,
    })
    .unwrap();
}

/// Pixels that are not Cornfield's background (#ffffe5).
fn foreground(image: &ViewportImage) -> usize {
    image
        .rgba
        .chunks(4)
        .filter(|p| {
            let d = |a: u8, b: u8| (i32::from(a) - i32::from(b)).abs() > 2;
            d(p[0], 0xff) || d(p[1], 0xff) || d(p[2], 0xe5)
        })
        .count()
}

#[cfg(target_vendor = "apple")]
mod metal {
    use objc2_quartz_core::CAMetalLayer;

    /// A layer in no window, and its address as the app passes it.
    pub fn layer() -> (impl std::ops::Deref<Target = CAMetalLayer>, u64) {
        let layer = CAMetalLayer::new();
        let addr = (&*layer as *const CAMetalLayer).expose_provenance() as u64;
        (layer, addr)
    }
}

#[test]
fn a_null_layer_is_refused() {
    let Some(v) = viewport() else { return };
    let e = v.attach_layer(0, 10.0, 10.0, 1.0, false).unwrap_err();
    assert!(matches!(e, CoreError::InvalidArgument { .. }), "{e:?}");
    assert!(!v.detach().unwrap());
}

#[test]
fn an_unknown_scheme_is_refused() {
    assert!(matches!(
        Viewport::new("No Such Scheme".into()),
        Err(CoreError::InvalidArgument { .. })
    ));
    let Some(v) = viewport() else { return };
    assert!(v.set_color_scheme("Nope".into()).is_err());
    v.set_color_scheme("Tomorrow Night".into()).unwrap();
    assert_eq!(v.color_scheme().unwrap(), "Tomorrow Night");
}

#[cfg(target_vendor = "apple")]
#[test]
fn a_layer_shows_the_rendered_model() {
    let Some(v) = viewport() else { return };
    bare(&v);
    let (layer, addr) = metal::layer();
    // 64 x 48 points at 2x: a 128 x 96 drawable, multisampled.
    v.attach_layer(addr, 64.0, 48.0, 2.0, true).unwrap();
    assert_eq!(v.samples().unwrap(), 4);
    assert!(v.needs_draw().unwrap());
    let empty = v.read_pixels().unwrap();
    assert_eq!((empty.width, empty.height), (128, 96));
    assert_eq!(foreground(&empty), 0);
    assert!(!v.needs_draw().unwrap());
    assert!(!v.draw().unwrap().drawn, "nothing changed: no frame");

    // A preview (the app's default) fits the first model and fills the view.
    let c = core("cube(10);");
    let r = c
        .render_into(DOC.into(), RenderMode::Preview, v.clone())
        .unwrap();
    assert_eq!(r.exit_code, 0, "{}", r.console);
    assert_eq!(v.camera().unwrap().vpt, vec![5.0, 5.0, 5.0]);
    assert!(v.needs_draw().unwrap());
    let cube = v.read_pixels().unwrap();
    assert!(foreground(&cube) > 128 * 96 / 10, "{}", foreground(&cube));

    // A camera move draws exactly one frame into the layer.
    v.orbit(20.0, 5.0).unwrap();
    let f = v.draw().unwrap();
    assert!(f.drawn && !f.deferred, "{f:?}");
    assert!(!v.draw().unwrap().drawn);

    // A syntax error leaves the last model on screen.
    c.update(DOC.into(), "cube(;".into()).unwrap();
    let r = c
        .render_into(DOC.into(), RenderMode::Render, v.clone())
        .unwrap();
    assert_ne!(r.exit_code, 0);
    assert!(foreground(&v.read_pixels().unwrap()) > 0);

    // An empty model clears it.
    c.update(DOC.into(), "".into()).unwrap();
    c.render_into(DOC.into(), RenderMode::Render, v.clone())
        .unwrap();
    assert_eq!(foreground(&v.read_pixels().unwrap()), 0);

    // Resize, then detach: the surface (and its retain on the layer) goes.
    v.resize(32.0, 32.0, 1.0).unwrap();
    assert_eq!(v.read_pixels().unwrap().width, 32);
    assert!(v.detach().unwrap());
    v.orbit(1.0, 0.0).unwrap();
    assert!(!v.needs_draw().unwrap());
    assert!(v.read_pixels().is_err());
    drop(layer);
}

#[cfg(target_vendor = "apple")]
#[test]
fn a_layer_attached_without_readable_cannot_be_read() {
    let Some(v) = viewport() else { return };
    let (_layer, addr) = metal::layer();
    v.attach_layer(addr, 16.0, 16.0, 1.0, false).unwrap();
    assert!(v.draw().unwrap().drawn);
    assert!(v.read_pixels().is_err());
}

/// Frame times on a moderate model (`csg_spheres` from
/// `conformance/bench.json`: a cube minus 125 spheres at `$fn = 48`), in
/// the app's window size. Ignored by default; run with
/// `cargo test --release -p neoscad-ffi frame_time -- --ignored --nocapture`.
#[cfg(target_vendor = "apple")]
#[test]
#[ignore]
fn frame_time() {
    let Some(v) = viewport() else { return };
    let (_layer, addr) = metal::layer();
    // A 900 x 600 point view on a 2x display.
    v.attach_layer(addr, 900.0, 600.0, 2.0, false).unwrap();
    let c = core(
        "difference() {\n  cube(60, center=true);\n  for (x=[-2:2], y=[-2:2], z=[-2:2]) translate([x,y,z]*12) sphere(r=6.5, $fn=48);\n}\n",
    );
    for (mode, target) in [
        (RenderMode::Preview, "layer"),
        (RenderMode::Render, "layer"),
        (RenderMode::Preview, "texture"),
        (RenderMode::Render, "texture"),
    ] {
        if target == "texture" {
            // The same frames into a texture: no drawable to wait for, so
            // the difference is the time spent acquiring one.
            v.lock().attach_texture(1800, 1200, 2.0);
        }
        let t0 = Instant::now();
        let r = c.render_into(DOC.into(), mode, v.clone()).unwrap();
        let build = t0.elapsed().as_secs_f64() * 1000.0;
        assert_eq!(r.exit_code, 0);
        let n = 240;
        let (mut cpu, mut total) = (Vec::with_capacity(n), Vec::with_capacity(n));
        for _ in 0..n {
            v.orbit(1.0, 0.0).unwrap();
            let t = Instant::now();
            let f = v.draw().unwrap();
            // Wait for the GPU, so `total` is the whole frame's cost.
            v.gpu
                .device()
                .poll(wgpu::PollType::wait_indefinitely())
                .unwrap();
            total.push(t.elapsed().as_secs_f64() * 1000.0);
            assert!(f.drawn);
            cpu.push(f.cpu_ms);
        }
        let stats = |x: &mut Vec<f64>| {
            x.sort_by(f64::total_cmp);
            (x[x.len() / 2], x[x.len() * 95 / 100], x[x.len() - 1])
        };
        let (c50, c95, cmax) = stats(&mut cpu);
        let (t50, t95, tmax) = stats(&mut total);
        println!(
            "{mode:?} into a {target}: render+upload {build:.1} ms; per frame at 1800x1200 4x MSAA: \
             cpu p50 {c50:.3} p95 {c95:.3} max {cmax:.3} ms; \
             cpu+gpu p50 {t50:.3} p95 {t95:.3} max {tmax:.3} ms"
        );
    }
}

/// Every kind of GPU work the app does on its one shared device, at once
/// from [`STRESS_THREADS`] threads: `Core::picture` (Quick Look), a
/// snapshot sheet, a viewport texture readback, File > Export's `image`,
/// a model upload (`render_into`), and a `CAMetalLayer` surface being
/// resized (a surface configure) and read. Before the submit gate and the
/// self-driven readback, a configure racing a submission failed and
/// dropped the map callbacks it had collected, and `picture` waited for
/// ever (`cargo test` hung for hours). A watchdog turns any hang into a
/// failure instead of a stuck test run.
#[cfg(target_vendor = "apple")]
#[test]
fn the_shared_device_survives_concurrent_use() {
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    const STRESS_THREADS: usize = 8;
    const ITERATIONS: usize = 200;
    // Generous for 1600 small GPU jobs in a debug build; a hang, not a
    // slow machine, is what trips it.
    const WATCHDOG: Duration = Duration::from_secs(240);

    if viewport().is_none() {
        return;
    }
    let (done, finished) = mpsc::channel::<Result<usize, String>>();
    let t0 = Instant::now();
    for t in 0..STRESS_THREADS {
        let done = done.clone();
        std::thread::spawn(move || {
            let run = || -> Result<usize, String> {
                let err = |e: CoreError| e.to_string();
                let c = core("cube(4); translate([6, 0, 0]) sphere(2, $fn = 12);");
                let texture = viewport().ok_or("no GPU")?;
                texture.lock().attach_texture(48, 32, 1.0);
                let surface = viewport().ok_or("no GPU")?;
                let (layer, addr) = metal::layer();
                surface
                    .attach_layer(addr, 24.0, 16.0, 2.0, true)
                    .map_err(err)?;
                let mut ops = 0;
                for i in 0..ITERATIONS {
                    match (t + i) % 6 {
                        0 => {
                            let p = c
                                .picture(
                                    DOC.into(),
                                    crate::PictureOptions {
                                        width: 32,
                                        height: 32,
                                        mode: RenderMode::Preview,
                                    },
                                )
                                .map_err(err)?;
                            if p.png.is_none() {
                                return Err(format!("picture drew nothing: {}", p.console));
                            }
                        }
                        1 => {
                            let s = c
                                .snapshot(
                                    DOC.into(),
                                    crate::SnapshotOptions {
                                        width: 64,
                                        height: 64,
                                        views: vec![],
                                        dims: false,
                                        preview: true,
                                    },
                                )
                                .map_err(err)?;
                            if s.png.is_none() {
                                return Err(format!("snapshot drew nothing: {}", s.console));
                            }
                        }
                        2 => {
                            texture.orbit(3.0, 0.0).map_err(err)?;
                            let image = texture.read_pixels().map_err(err)?;
                            assert_eq!((image.width, image.height), (48, 32));
                        }
                        3 => {
                            texture.image(16, 16).map_err(err)?;
                        }
                        4 => {
                            c.render_into(DOC.into(), RenderMode::Preview, texture.clone())
                                .map_err(err)?;
                        }
                        _ => {
                            // A new size configures the surface: the call
                            // that used to fail while others submitted.
                            let w = 16.0 + (i % 5) as f64;
                            surface.resize(w, 16.0, 2.0).map_err(err)?;
                            surface.draw().map_err(err)?;
                            surface.read_pixels().map_err(err)?;
                        }
                    }
                    ops += 1;
                }
                drop(layer);
                Ok(ops)
            };
            let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(run))
                .unwrap_or_else(|_| Err("the worker panicked".into()));
            let _ = done.send(r);
        });
    }
    drop(done);
    let mut total = 0;
    for _ in 0..STRESS_THREADS {
        let left = WATCHDOG.saturating_sub(t0.elapsed());
        match finished.recv_timeout(left) {
            Ok(Ok(ops)) => total += ops,
            Ok(Err(e)) => panic!("a worker failed: {e}"),
            Err(_) => panic!(
                "GPU work hung: not every worker finished within {} s",
                WATCHDOG.as_secs()
            ),
        }
    }
    assert_eq!(total, STRESS_THREADS * ITERATIONS);
    eprintln!(
        "{total} concurrent GPU jobs on {STRESS_THREADS} threads in {:.1} s",
        t0.elapsed().as_secs_f64()
    );
}
