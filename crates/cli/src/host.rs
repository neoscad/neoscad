//! What the command line supplies from the machine it runs on: the file
//! system (with the bundled libraries mounted), the library path, the fonts
//! and the seed of unseeded `rands()`. The library crates never read the
//! disk, the environment or the clock themselves; this is where those come
//! from, as OpenSCAD's `parser_init()`, `FontCache` and `initialize_rng()`
//! take them.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use lang::loader::{FileSystem, LibraryPath, StdFs};

/// The environment variable naming a directory of fonts that replaces the
/// bundled ones, the counterpart of OpenSCAD's `<resources>/fonts`.
/// Without the `bundled-assets` feature, `fonts/` next to the executable
/// is used instead when the variable is unset.
const FONT_DIR_VAR: &str = "NEOSCAD_FONT_DIR";

/// The file system and library path of one run.
#[derive(Clone)]
pub struct Host {
    pub fs: Arc<dyn FileSystem + Send + Sync>,
    pub libs: LibraryPath,
}

impl std::fmt::Debug for Host {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Host").field("libs", &self.libs).finish()
    }
}

/// OpenSCAD's resource directory: the executable's (neoscad installs no
/// separate resources, so the bundled ones are mounted there).
fn resource_dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(Path::to_path_buf))
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_default()
}

impl Host {
    /// The disk, `OPENSCADPATH` and the user library directory, then the
    /// bundled libraries last, as `parser_init()` appends
    /// `<resources>/libraries` (`parsersettings.cc:166-171`). They are
    /// mounted in memory at `libraries/` next to the executable, so
    /// messages from them name a plausible path, and a library of the same
    /// name earlier in the path wins as it does in OpenSCAD.
    pub fn from_env() -> Host {
        #[allow(unused_mut)]
        let mut libs = LibraryPath::from_env();
        let fs: Arc<dyn FileSystem + Send + Sync> = Arc::new(StdFs);
        #[cfg(feature = "bundled-assets")]
        let fs: Arc<dyn FileSystem + Send + Sync> = {
            let root = resource_dir().join("libraries");
            libs.0.push(root.clone());
            Arc::new(assets::libraries(fs, root))
        };
        Host { fs, libs }
    }

    /// The fonts `text()` sees, in the order `FontCache::FontCache` adds
    /// them: the bundled fonts, `~/.fonts`, each directory in
    /// `OPENSCAD_FONT_PATH`, then `used` (the `use <font.ttf>` files of the
    /// program and its libraries, `SourceFile::registerUse`). Fontconfig's
    /// system configuration is not consulted, so only these fonts exist.
    /// Nothing is read until a `text()` needs a font.
    pub fn fonts<'u>(&self, used: impl Iterator<Item = &'u String>) -> text::FontDb {
        let mut db = text::FontDb::with_fs(self.fs.clone());
        for source in self.font_sources() {
            match source {
                #[cfg(feature = "bundled-assets")]
                FontSource::Bundled => assets::add_fonts(&mut db),
                #[cfg(not(feature = "bundled-assets"))]
                FontSource::Bundled => {}
                FontSource::Dir(d) => db.add_dir(d),
            }
        }
        for u in used {
            let p = Path::new(u);
            let is_font = p
                .extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("ttf") || e.eq_ignore_ascii_case("otf"));
            if is_font && self.fs.exists(p) && !self.fs.is_dir(p) {
                db.add_file(p);
            }
        }
        db
    }

    /// Where [`Host::fonts`] looks, in order; `--info` lists the same.
    ///
    /// (See also [`Host::session_config`].)
    pub fn font_sources(&self) -> Vec<FontSource> {
        let mut out = Vec::new();
        match std::env::var_os(FONT_DIR_VAR) {
            Some(d) => out.push(FontSource::Dir(PathBuf::from(d))),
            #[cfg(feature = "bundled-assets")]
            None => out.push(FontSource::Bundled),
            #[cfg(not(feature = "bundled-assets"))]
            None => out.push(FontSource::Dir(resource_dir().join("fonts"))),
        }
        if let Some(home) = std::env::var_os("HOME") {
            out.push(FontSource::Dir(PathBuf::from(home).join(".fonts")));
        }
        if let Some(paths) = std::env::var_os("OPENSCAD_FONT_PATH") {
            let sep = if cfg!(windows) { ';' } else { ':' };
            let cwd = std::env::current_dir().unwrap_or_default();
            for p in paths.to_string_lossy().split(sep) {
                let p = cwd.join(p);
                if self.fs.is_dir(&p) {
                    out.push(FontSource::Dir(p));
                }
            }
        }
        out
    }
}

impl Host {
    /// A session over this host: its files, library path and fonts, the
    /// working directory, a monotonic clock for timings and the GPU.
    /// `seed` is the seed of unseeded `rands()`.
    pub fn session_config(&self, seed: u32) -> session::Config {
        let mut cfg = session::Config::new(self.fs.clone(), self.libs.clone());
        let host = self.clone();
        cfg.fonts = Arc::new(move |used: &[String]| host.fonts(used.iter()));
        cfg.work_dir = std::env::current_dir().unwrap_or_default();
        let t0 = std::time::Instant::now();
        cfg.clock = Some(Arc::new(move || t0.elapsed().as_secs_f64() * 1000.0));
        cfg.rng_seed = seed;
        cfg.gpu = Some(Arc::new(crate::png::offscreen));
        cfg
    }
}

/// One place fonts come from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FontSource {
    /// The Liberation fonts compiled into the binary.
    Bundled,
    Dir(PathBuf),
}

/// The directory OpenSCAD calls its resource path: where the bundled
/// libraries are mounted.
pub fn resource_path() -> PathBuf {
    resource_dir()
}

/// The environment variable that replaces the bundled fonts.
pub const FONT_DIR_ENV: &str = FONT_DIR_VAR;

/// The seed of unseeded `rands()`, as OpenSCAD makes it: the time in
/// seconds plus the process ID (`builtin_functions.cc:69`, truncated to
/// 32 bits by `std::mt19937`'s seed).
pub fn entropy_seed() -> u32 {
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    (t as u32).wrapping_add(std::process::id())
}
