//! The fonts `text()` can use, and the lookup of a font name among them
//! (OpenSCAD's `FontCache`).
//!
//! OpenSCAD asks fontconfig, which sees the fonts bundled with OpenSCAD
//! (`<resources>/fonts`: Liberation Sans, Serif and Mono), `~/.fonts`, each
//! directory in `OPENSCAD_FONT_PATH`, the files registered with
//! `use <font.ttf>`, and whatever the system configuration lists. Here the
//! host adds directories, files or font data explicitly: nothing is found
//! by searching the system, so the same inputs give the same fonts on every
//! machine and in WASM. Directories and files are read through the
//! database's [`FileSystem`], so a WASM host can serve them from memory.
//!
//! Font files are read and indexed on the first lookup, not when they are
//! added, so a run that draws no text never touches them.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use lang::loader::FileSystem;
use lang::vfs::NoFs;
use skrifa::raw::{FileRef, FontRef, TableProvider};
use skrifa::{MetadataProvider, string::StringId};

use crate::pattern::{self, Pattern};

/// Where a font comes from, in the order fonts were added.
#[derive(Debug, Clone)]
enum Source {
    /// A directory, searched recursively as fontconfig does.
    Dir(PathBuf),
    File(PathBuf),
    Data(FontData),
}

/// A font file's bytes: owned, or compiled into the binary (the bundled
/// fonts, which are then never copied).
#[derive(Debug, Clone)]
pub enum FontData {
    Owned(Arc<Vec<u8>>),
    Static(&'static [u8]),
}

impl std::ops::Deref for FontData {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        match self {
            FontData::Owned(d) => d,
            FontData::Static(d) => d,
        }
    }
}

/// One face of a font file, with the properties fontconfig matches on.
#[derive(Debug)]
pub struct Face {
    pub data: FontData,
    /// The face's index in a collection (`FC_INDEX`).
    pub index: u32,
    /// Family names in every language, as keys ([`pattern::family_key`]).
    families: Vec<String>,
    /// Style names in every language, lower case.
    styles: Vec<String>,
    weight: f64,
    slant: f64,
    width: f64,
    /// Filled in by [`crate::shape`] the first time the face is used.
    pub(crate) state: OnceLock<crate::shape::FaceState>,
}

impl Face {
    pub fn font(&self) -> Option<FontRef<'_>> {
        FontRef::from_index(&self.data, self.index).ok()
    }
}

/// The fonts available to `text()`.
#[derive(Debug)]
pub struct FontDb {
    fs: Arc<dyn FileSystem + Send + Sync>,
    sources: Vec<Source>,
    faces: OnceLock<Vec<Arc<Face>>>,
    /// Lookups by font name, as `FontCache::get_font` caches them. Failures
    /// are not cached, so their warnings repeat, as in OpenSCAD.
    lookups: Mutex<HashMap<String, Arc<Face>>>,
}

impl Default for FontDb {
    fn default() -> Self {
        FontDb::with_fs(Arc::new(NoFs))
    }
}

impl FontDb {
    /// An empty database with no file system: fonts come only from
    /// [`FontDb::add_static`] and the like. A host that reads font files
    /// uses [`FontDb::with_fs`] (the disk is `lang::loader::StdFs`).
    pub fn new() -> FontDb {
        FontDb::default()
    }

    /// An empty database reading directories and files through `fs`.
    pub fn with_fs(fs: Arc<dyn FileSystem + Send + Sync>) -> FontDb {
        FontDb {
            fs,
            sources: Vec::new(),
            faces: OnceLock::new(),
            lookups: Mutex::new(HashMap::new()),
        }
    }

    /// Add every font file under `dir` (`FcConfigAppFontAddDir`). A
    /// directory that does not exist is ignored, as OpenSCAD ignores it.
    pub fn add_dir(&mut self, dir: impl Into<PathBuf>) {
        self.sources.push(Source::Dir(dir.into()));
    }

    /// Add one font file (`use <font.ttf>`, `FcConfigAppFontAddFile`).
    pub fn add_file(&mut self, file: impl Into<PathBuf>) {
        self.sources.push(Source::File(file.into()));
    }

    /// Add a font from memory (for WASM, or fonts embedded by the host).
    pub fn add_data(&mut self, data: Vec<u8>) {
        self.sources
            .push(Source::Data(FontData::Owned(Arc::new(data))));
    }

    /// Add a font compiled into the binary, without copying it.
    pub fn add_static(&mut self, data: &'static [u8]) {
        self.sources.push(Source::Data(FontData::Static(data)));
    }

    /// Every face, indexed on first use.
    pub fn faces(&self) -> &[Arc<Face>] {
        self.faces.get_or_init(|| {
            let mut out = Vec::new();
            for s in &self.sources {
                match s {
                    Source::Dir(d) => scan_dir(&*self.fs, d, &mut out),
                    Source::File(f) => {
                        if let Ok(data) = self.fs.read(f) {
                            index_data(FontData::Owned(Arc::new(data)), &mut out);
                        }
                    }
                    Source::Data(d) => index_data(d.clone(), &mut out),
                }
            }
            out
        })
    }

    /// `FontCache::get_font`: the face a font name selects. An empty name
    /// means OpenSCAD's default, `Liberation Sans:style=Regular`.
    pub fn lookup(&self, name: &str) -> Result<Arc<Face>, LookupError> {
        if let Some(f) = self
            .lookups
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(name)
        {
            return Ok(f.clone());
        }
        let trimmed = name.trim();
        let query = if trimmed.is_empty() {
            "Liberation Sans:style=Regular"
        } else {
            trimmed
        };
        let pat = pattern::parse(query).ok_or(LookupError::Parse)?;
        let face = best_match(self.faces(), &pat).ok_or(LookupError::NotFound)?;
        self.lookups
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(name.to_string(), face.clone());
        Ok(face)
    }
}

/// Why a font name selected no face.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LookupError {
    /// Fontconfig cannot parse the name ("Could not parse font").
    Parse,
    /// No fonts at all.
    NotFound,
}

fn scan_dir(fs: &dyn FileSystem, dir: &Path, out: &mut Vec<Arc<Face>>) {
    let Ok(mut entries) = fs.read_dir(dir) else {
        return;
    };
    // Sorted, so the order (the last tie-break in matching) does not depend
    // on the file system.
    entries.sort();
    for p in entries {
        if fs.is_dir(&p) {
            scan_dir(fs, &p, out);
            continue;
        }
        let ext = p
            .extension()
            .map(|e| e.to_string_lossy().to_ascii_lowercase())
            .unwrap_or_default();
        if !matches!(ext.as_str(), "ttf" | "otf" | "ttc" | "otc") {
            continue;
        }
        if let Ok(data) = fs.read(&p) {
            index_data(FontData::Owned(Arc::new(data)), out);
        }
    }
}

fn index_data(data: FontData, out: &mut Vec<Arc<Face>>) {
    let count = match FileRef::new(&data) {
        Ok(FileRef::Font(_)) => 1,
        Ok(FileRef::Collection(c)) => c.len(),
        Err(_) => return,
    };
    for index in 0..count {
        let Ok(font) = FontRef::from_index(&data, index) else {
            continue;
        };
        // Fontconfig only offers scalable outline fonts to OpenSCAD
        // (`init_pattern` asks for `outline` and `scalable`).
        if font.glyf().is_err() && font.cff().is_err() && font.cff2().is_err() {
            continue;
        }
        let names = |ids: &[StringId]| -> Vec<String> {
            let mut v: Vec<String> = Vec::new();
            for &id in ids {
                for s in font.localized_strings(id) {
                    let s: String = s.chars().collect();
                    if !s.is_empty() && !v.contains(&s) {
                        v.push(s);
                    }
                }
            }
            v
        };
        let families: Vec<String> = names(&[
            StringId::WWS_FAMILY_NAME,
            StringId::TYPOGRAPHIC_FAMILY_NAME,
            StringId::FAMILY_NAME,
        ])
        .iter()
        .map(|s| pattern::family_key(s))
        .collect();
        let styles: Vec<String> = names(&[
            StringId::WWS_SUBFAMILY_NAME,
            StringId::TYPOGRAPHIC_SUBFAMILY_NAME,
            StringId::SUBFAMILY_NAME,
        ])
        .iter()
        .map(|s| s.to_lowercase())
        .collect();
        let (mut weight, mut slant, mut width) = (80.0, 0.0, 100.0);
        if let Ok(os2) = font.os2() {
            weight = weight_from_opentype(f64::from(os2.us_weight_class()));
            width = match os2.us_width_class() {
                1 => 50.0,
                2 => 63.0,
                3 => 75.0,
                4 => 87.0,
                6 => 113.0,
                7 => 125.0,
                8 => 150.0,
                9 => 200.0,
                _ => 100.0,
            };
            let sel = os2.fs_selection().bits();
            if sel & 1 != 0 {
                slant = 100.0;
            } else if sel & (1 << 9) != 0 {
                slant = 110.0;
            }
        }
        // Fontconfig also reads the style name (`FcFreeTypeQuery`).
        if slant == 0.0 {
            if styles.iter().any(|s| s.contains("italic")) {
                slant = 100.0;
            } else if styles.iter().any(|s| s.contains("oblique")) {
                slant = 110.0;
            }
        }
        out.push(Arc::new(Face {
            data: data.clone(),
            index,
            families,
            styles,
            weight,
            slant,
            width,
            state: OnceLock::new(),
        }));
    }
}

/// `FcWeightFromOpenTypeDouble`: OpenType weights 100..900 onto
/// fontconfig's scale, piecewise linear.
fn weight_from_opentype(ot: f64) -> f64 {
    const MAP: &[(f64, f64)] = &[
        (0.0, 0.0),
        (100.0, 0.0),
        (200.0, 40.0),
        (300.0, 50.0),
        (350.0, 55.0),
        (380.0, 75.0),
        (400.0, 80.0),
        (500.0, 100.0),
        (600.0, 180.0),
        (700.0, 200.0),
        (800.0, 205.0),
        (900.0, 210.0),
        (1000.0, 215.0),
    ];
    let ot = ot.clamp(1.0, 1000.0);
    for w in MAP.windows(2) {
        let ((a, fa), (b, fb)) = (w[0], w[1]);
        if ot <= b {
            return fa + (ot - a) * (fb - fa) / (b - a);
        }
    }
    215.0
}

/// Code points the face's character map covers, for `charset=` matching.
fn missing(face: &Face, ranges: &[(u32, u32)]) -> u64 {
    let Some(font) = face.font() else {
        return u64::MAX;
    };
    let cmap = font.charmap();
    let mut n = 0;
    for &(a, b) in ranges {
        for c in a..=b {
            if cmap.map(c).is_none() {
                n += 1;
            }
        }
    }
    n
}

/// `FcFontMatch`, reduced: the face with the lowest score in fontconfig's
/// priority order; ties go to the face added first.
fn best_match(faces: &[Arc<Face>], p: &Pattern) -> Option<Arc<Face>> {
    let families = pattern::family_candidates(p);
    let styles: Vec<String> = p.styles.iter().map(|s| s.to_lowercase()).collect();
    let weight = p.weight.unwrap_or(80.0);
    let slant = p.slant.unwrap_or(0.0);
    let width = p.width.unwrap_or(100.0);
    let score = |f: &Face| -> [f64; 6] {
        let charset = p
            .charsets
            .iter()
            .enumerate()
            .map(|(j, r)| missing(f, r) as f64 * 1000.0 + j as f64)
            .reduce(f64::min)
            .unwrap_or(0.0);
        let family = families
            .iter()
            .position(|k| f.families.contains(k))
            .map_or(1e9, |j| j as f64);
        let style = if styles.is_empty() || styles.iter().any(|s| f.styles.contains(s)) {
            0.0
        } else {
            1.0
        };
        [
            charset,
            family,
            style,
            (f.slant - slant).abs(),
            (f.weight - weight).abs(),
            (f.width - width).abs(),
        ]
    };
    let mut best: Option<(&Arc<Face>, [f64; 6])> = None;
    for f in faces {
        let s = score(f);
        if best.as_ref().is_none_or(|(_, b)| s < *b) {
            best = Some((f, s));
        }
    }
    best.map(|(f, _)| f.clone())
}
