//! What the app supplies to its session from the machine: the disk with
//! the bundled MCAD mounted, the library path, fonts, a clock, the seed of
//! unseeded `rands()` and the GPU. It follows the macOS app's host
//! (`crates/ffi/src/host.rs`) and the command line's, so a model renders
//! the same in every NeoSCAD: the same library path order, the same fonts,
//! the same limits. That host is not reused directly because it opens
//! Metal only and lives behind UniFFI; this one differs in the GPU it asks
//! for and in taking `$XDG_DATA_HOME`'s fonts too.

use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use lang::loader::{FileSystem, LibraryPath, StdFs};
use render::offscreen::{Backends, Offscreen};

/// Where the bundled libraries are mounted. It exists only in memory, so
/// it cannot shadow a real file.
const RESOURCES: &str = "/NeoSCAD.resources";

/// The session configuration for the app: the disk with MCAD in memory
/// after `OPENSCADPATH` and the user library folder, the Liberation fonts
/// then the user's, the agent limits (`session::Limits::AGENT`, as the
/// macOS app), and snapshots on the viewports' GPU.
pub fn config() -> session::Config {
    let root = PathBuf::from(RESOURCES).join("libraries");
    let mut libs = LibraryPath::from_env();
    libs.0.push(root.clone());
    let fs: Arc<dyn FileSystem + Send + Sync> = Arc::new(assets::libraries(Arc::new(StdFs), root));
    let mut cfg = session::Config::new(fs.clone(), libs);
    let font_dirs = user_font_dirs(
        std::env::var_os("HOME").map(PathBuf::from),
        std::env::var_os("XDG_DATA_HOME").map(PathBuf::from),
    );
    cfg.fonts = Arc::new(move |used: &[String]| {
        let mut db = text::FontDb::with_fs(fs.clone());
        assets::add_fonts(&mut db);
        for d in &font_dirs {
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

/// The user's font folders: `~/.fonts` (what the macOS app and the command
/// line read) and the XDG one, `$XDG_DATA_HOME/fonts` or
/// `~/.local/share/fonts`, where GNOME's font installer puts fonts.
pub fn user_font_dirs(home: Option<PathBuf>, xdg_data_home: Option<PathBuf>) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(h) = &home {
        out.push(h.join(".fonts"));
    }
    match (xdg_data_home.filter(|p| p.is_absolute()), &home) {
        (Some(x), _) => out.push(x.join("fonts")),
        (None, Some(h)) => out.push(h.join(".local/share/fonts")),
        (None, None) => {}
    }
    out
}

/// The seed of unseeded `rands()`, once per process: OpenSCAD seeds from
/// the time and process id at startup, so results differ between launches
/// but a re-render after an edit does not reshuffle every random value.
fn entropy_seed() -> u32 {
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    (t as u32).wrapping_add(std::process::id())
}

/// The GPU the viewports and snapshots draw on, opened on first use and
/// shared by every window. Vulkan first (Mesa's drivers, NVIDIA's, and
/// lavapipe where there is no GPU), then GL through EGL: the command
/// line's order (`crates/cli/src/png.rs`), so a machine that exports PNGs
/// also shows the view. The error names both failures.
pub fn gpu() -> Result<Arc<render::viewport::Gpu>, String> {
    static GPU: OnceLock<Result<Arc<render::viewport::Gpu>, String>> = OnceLock::new();
    GPU.get_or_init(|| {
        render::viewport::Gpu::new_blocking(Backends::VULKAN)
            .or_else(|vk| {
                render::viewport::Gpu::new_blocking(Backends::GL)
                    .map_err(|gl| format!("no GPU: Vulkan: {vk}; OpenGL: {gl}"))
            })
            .map(Arc::new)
    })
    .clone()
}

/// Snapshots' renderer, on the viewports' device.
fn offscreen() -> Result<&'static Offscreen, String> {
    static DEVICE: OnceLock<Result<Offscreen, String>> = OnceLock::new();
    DEVICE
        .get_or_init(|| gpu().map(|g| Offscreen::on_gpu(&g)))
        .as_ref()
        .map_err(Clone::clone)
}

/// `secs` since the Unix epoch as `YYYY-MM-DDTHH:MM:SSZ` (the creation
/// date 3MF and PDF exports record).
pub fn iso8601(secs: i64) -> String {
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        rem / 60 % 60,
        rem % 60
    )
}

/// Now, as [`iso8601`].
pub fn iso8601_now() -> String {
    iso8601(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs()) as i64,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates_are_utc_calendar_dates() {
        assert_eq!(iso8601(0), "1970-01-01T00:00:00Z");
        assert_eq!(iso8601(951_782_400), "2000-02-29T00:00:00Z");
        assert_eq!(iso8601(1_790_000_000), "2026-09-21T14:13:20Z");
    }

    #[test]
    fn fonts_come_from_the_home_and_xdg_folders() {
        let home = Some(PathBuf::from("/home/u"));
        assert_eq!(
            user_font_dirs(home.clone(), None),
            [
                PathBuf::from("/home/u/.fonts"),
                PathBuf::from("/home/u/.local/share/fonts")
            ]
        );
        assert_eq!(
            user_font_dirs(home.clone(), Some("/x".into())),
            [PathBuf::from("/home/u/.fonts"), PathBuf::from("/x/fonts")]
        );
        // A relative XDG_DATA_HOME is invalid by the spec and ignored.
        assert_eq!(
            user_font_dirs(home, Some("rel".into()))[1],
            PathBuf::from("/home/u/.local/share/fonts")
        );
        assert!(user_font_dirs(None, None).is_empty());
    }
}
