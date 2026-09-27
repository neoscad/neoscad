//! What the app supplies to its session from the machine: the disk with
//! the bundled MCAD mounted, the library path, fonts, a clock, the seed of
//! unseeded `rands()` and the GPU. This crate is a host, like
//! `crates/cli`: the library crates never read the disk, the environment
//! or the clock, so a host has to (`CLAUDE.md`, "Rules").
//!
//! It follows `crates/cli/src/host.rs` (the command line's host) so a
//! model renders the same in the app as with `neoscad`: the same library
//! path order and the same fonts.

use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use lang::loader::{FileSystem, LibraryPath, StdFs};
use render::offscreen::{Backends, Offscreen};

/// Where the bundled libraries are mounted when the app names no resource
/// directory. It exists only in memory, so it cannot shadow a real file.
const DEFAULT_RESOURCES: &str = "/NeoSCAD.resources";

/// The session configuration for the app.
///
/// - Files: the disk, with MCAD mounted in memory at
///   `<resource_dir>/libraries`, appended to the library path after
///   `OPENSCADPATH` and the user library folder, where OpenSCAD puts
///   `<resources>/libraries` (`parser_init()`, `parsersettings.cc`).
/// - Fonts: the bundled Liberation fonts, `~/.fonts`, then any
///   `use <font.ttf>` of the program.
/// - Limits: [`session::Limits::AGENT`]; see [`crate::types::ResourceLimits`].
pub fn config(resource_dir: Option<&str>) -> session::Config {
    let root = PathBuf::from(resource_dir.unwrap_or(DEFAULT_RESOURCES)).join("libraries");
    let mut libs = LibraryPath::from_env();
    libs.0.push(root.clone());
    let fs: Arc<dyn FileSystem + Send + Sync> = Arc::new(assets::libraries(Arc::new(StdFs), root));
    let mut cfg = session::Config::new(fs.clone(), libs);
    let home_fonts = std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".fonts"));
    cfg.fonts = Arc::new(move |used: &[String]| {
        let mut db = text::FontDb::with_fs(fs.clone());
        assets::add_fonts(&mut db);
        if let Some(d) = &home_fonts {
            db.add_dir(d.clone());
        }
        for u in used {
            let p = Path::new(u);
            if session::is_font(u) && fs.exists(p) && !fs.is_dir(p) {
                db.add_file(p);
            }
        }
        db
    });
    cfg.work_dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
    let t0 = std::time::Instant::now();
    cfg.clock = Some(Arc::new(move || t0.elapsed().as_secs_f64() * 1000.0));
    cfg.rng_seed = entropy_seed();
    cfg.gpu = Some(Arc::new(offscreen));
    cfg.limits = session::Limits::AGENT;
    cfg
}

/// The seed of unseeded `rands()`, once per core: OpenSCAD seeds from the
/// time and process ID at startup (`builtin_functions.cc:69`), so results
/// differ between launches but stay stable while the app runs, and a
/// re-render after an edit does not reshuffle every random value.
fn entropy_seed() -> u32 {
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    (t as u32).wrapping_add(std::process::id())
}

/// The GPU snapshots draw on: an offscreen renderer on the viewports'
/// device ([`viewport_gpu`]), made on first use and shared by every core
/// in the process. Until phase 8f it opened a Metal device of its own,
/// because `Offscreen` dropped the instance a window surface needs; one
/// device serves both now, so the app pays for one set of queues,
/// pipeline caches and driver allocations. Metal only: this crate is
/// built for macOS.
fn offscreen() -> Result<&'static Offscreen, String> {
    static DEVICE: OnceLock<Result<Offscreen, String>> = OnceLock::new();
    DEVICE
        .get_or_init(|| viewport_gpu().map(|gpu| Offscreen::on_gpu(&gpu)))
        .as_ref()
        .map_err(Clone::clone)
}

/// The GPU the viewports (and snapshots) draw on, opened on first use and
/// shared by every window and by the background uploads for them. It keeps
/// the instance it was opened on, which a window surface must come from.
pub fn viewport_gpu() -> Result<Arc<render::viewport::Gpu>, String> {
    static GPU: OnceLock<Result<Arc<render::viewport::Gpu>, String>> = OnceLock::new();
    GPU.get_or_init(|| {
        render::viewport::Gpu::new_blocking(Backends::METAL)
            .map(Arc::new)
            .map_err(|e| e.to_string())
    })
    .clone()
}
