//! Render colour schemes (`src/glview/ColorMap.{h,cc}`): OpenSCAD's
//! built-in "Cornfield" plus the JSON files it ships in
//! `color-schemes/render/`, vendored in `assets/color-schemes/render` and
//! compiled in, so the renderer needs no file system (the WASM build has
//! none).
//!
//! OpenSCAD also reads schemes from the user's configuration directory;
//! that belongs to the app, which can hand extra JSON to [`parse`].

use geom::color::Color;

/// The scheme OpenSCAD uses when `--colorscheme` is not given.
pub const DEFAULT_NAME: &str = "Cornfield";

/// The vendored scheme files, by file name.
const FILES: &[(&str, &str)] = &[
    (
        "beforedawn.json",
        include_str!("../../../assets/color-schemes/render/beforedawn.json"),
    ),
    (
        "clearsky.json",
        include_str!("../../../assets/color-schemes/render/clearsky.json"),
    ),
    (
        "daylight-gem.json",
        include_str!("../../../assets/color-schemes/render/daylight-gem.json"),
    ),
    (
        "deepocean.json",
        include_str!("../../../assets/color-schemes/render/deepocean.json"),
    ),
    (
        "metallic.json",
        include_str!("../../../assets/color-schemes/render/metallic.json"),
    ),
    (
        "monotone.json",
        include_str!("../../../assets/color-schemes/render/monotone.json"),
    ),
    (
        "nature.json",
        include_str!("../../../assets/color-schemes/render/nature.json"),
    ),
    (
        "nocturnal-gem.json",
        include_str!("../../../assets/color-schemes/render/nocturnal-gem.json"),
    ),
    (
        "nord-dark.json",
        include_str!("../../../assets/color-schemes/render/nord-dark.json"),
    ),
    (
        "nord-light.json",
        include_str!("../../../assets/color-schemes/render/nord-light.json"),
    ),
    (
        "solarized.json",
        include_str!("../../../assets/color-schemes/render/solarized.json"),
    ),
    (
        "starnight.json",
        include_str!("../../../assets/color-schemes/render/starnight.json"),
    ),
    (
        "sunset.json",
        include_str!("../../../assets/color-schemes/render/sunset.json"),
    ),
    (
        "tomorrow-night.json",
        include_str!("../../../assets/color-schemes/render/tomorrow-night.json"),
    ),
    (
        "tomorrow.json",
        include_str!("../../../assets/color-schemes/render/tomorrow.json"),
    ),
];

/// One render colour scheme (`RenderColorScheme`): every `RenderColor`.
#[derive(Debug, Clone, PartialEq)]
pub struct ColorScheme {
    pub name: String,
    /// Sort key of the scheme list (`index` in the JSON).
    pub index: i32,
    pub show_in_gui: bool,
    /// The top of the background gradient.
    pub background: Color,
    /// The bottom of the background gradient; the same as `background`
    /// when the file has no `background-stop` (a flat background).
    pub background_stop: Color,
    pub axes: Color,
    /// Preview colours, which render mode also uses for meshes without
    /// colours of their own (`Renderer::setColorScheme` maps them to the
    /// `MATERIAL` and `CUTOUT` colour modes).
    pub opencsg_face_front: Color,
    pub opencsg_face_back: Color,
    /// The colours the Manifold backend paints on its result's faces.
    pub cgal_face_front: Color,
    pub cgal_face_back: Color,
    /// 2D shapes and their outlines in render mode.
    pub cgal_face_2d: Color,
    pub cgal_edge_front: Color,
    pub cgal_edge_back: Color,
    pub cgal_edge_2d: Color,
    pub crosshair: Color,
}

impl ColorScheme {
    /// The built-in "Cornfield" (`RenderColorScheme::RenderColorScheme()`,
    /// `ColorMap.cc:21-51`).
    pub fn cornfield() -> ColorScheme {
        let c = Color::from_u8;
        ColorScheme {
            name: DEFAULT_NAME.to_string(),
            index: 1000,
            show_in_gui: true,
            background: c(0xff, 0xff, 0xe5),
            background_stop: c(0xff, 0xff, 0xe5),
            axes: c(0x00, 0x00, 0x00),
            opencsg_face_front: c(0xf9, 0xd7, 0x2c),
            opencsg_face_back: c(0x9d, 0xcb, 0x51),
            cgal_face_front: c(0xf9, 0xd7, 0x2c),
            cgal_face_2d: c(0x00, 0xbf, 0x99),
            cgal_face_back: c(0x9d, 0xcb, 0x51),
            cgal_edge_front: c(0xff, 0xec, 0x5e),
            cgal_edge_back: c(0xab, 0xd8, 0x56),
            cgal_edge_2d: c(0xff, 0x00, 0x00),
            crosshair: c(0x80, 0x00, 0x00),
        }
    }

    /// The two face colours the geometry kernel paints into Manifold
    /// results, which exported meshes carry too.
    pub fn geometry_scheme(&self) -> geom::color::Scheme {
        geom::color::Scheme {
            face_front: self.cgal_face_front,
            face_back: self.cgal_face_back,
        }
    }
}

/// Parse a scheme file as `RenderColorScheme(path)` does: `name`, `index`,
/// `show-in-gui` and every colour are required, as `#rrggbb`; a missing
/// `background-stop` falls back to `background`. Any problem rejects the
/// whole file, with the reason.
pub fn parse(json: &str) -> Result<ColorScheme, String> {
    let v: serde_json::Value = serde_json::from_str(json).map_err(|e| e.to_string())?;
    let name = v
        .get("name")
        .and_then(|n| n.as_str())
        .ok_or("no 'name'")?
        .to_string();
    let index = v
        .get("index")
        .and_then(|n| n.as_i64())
        .and_then(|n| i32::try_from(n).ok())
        .ok_or("no integer 'index'")?;
    let show_in_gui = v
        .get("show-in-gui")
        .and_then(|b| b.as_bool())
        .ok_or("no boolean 'show-in-gui'")?;
    let colors = v.get("colors").ok_or("no 'colors'")?;
    let color = |key: &str| -> Result<Color, String> {
        let s = colors
            .get(key)
            .and_then(|c| c.as_str())
            .ok_or_else(|| format!("no color '{key}'"))?;
        parse_hex(s).ok_or_else(|| format!("invalid color value for key '{key}': '{s}'"))
    };
    let background = color("background")?;
    Ok(ColorScheme {
        name,
        index,
        show_in_gui,
        background,
        background_stop: color("background-stop").unwrap_or(background),
        axes: color("axes-color")?,
        opencsg_face_front: color("opencsg-face-front")?,
        opencsg_face_back: color("opencsg-face-back")?,
        cgal_face_front: color("cgal-face-front")?,
        cgal_face_2d: color("cgal-face-2d")?,
        cgal_face_back: color("cgal-face-back")?,
        cgal_edge_front: color("cgal-edge-front")?,
        cgal_edge_back: color("cgal-edge-back")?,
        cgal_edge_2d: color("cgal-edge-2d")?,
        crosshair: color("crosshair")?,
    })
}

/// `#rrggbb` (`RenderColorScheme::addColor`, which takes exactly seven
/// characters and parses the rest with `strtol` base 16).
fn parse_hex(s: &str) -> Option<Color> {
    let hex = s.strip_prefix('#').filter(|h| h.len() == 6)?;
    let val = u32::from_str_radix(hex, 16).ok()?;
    Some(Color::from_u8(
        (val >> 16) as u8,
        (val >> 8) as u8,
        val as u8,
    ))
}

/// Every scheme in `ColorMap`'s order: by `index`, the built-in Cornfield
/// first among equals, then the vendored files in name order (OpenSCAD's
/// order among equal indices follows its directory listing). A file whose
/// name is already taken is skipped, as `enumerateColorSchemesInPath` does.
pub fn all() -> Vec<ColorScheme> {
    let mut out = vec![ColorScheme::cornfield()];
    for (_, json) in FILES {
        if let Ok(s) = parse(json)
            && !out.iter().any(|o| o.name == s.name)
        {
            out.push(s);
        }
    }
    out.sort_by_key(|s| s.index);
    out
}

/// `ColorMap::findColorScheme`: an exact, case-sensitive name match.
pub fn find(name: &str) -> Option<ColorScheme> {
    all().into_iter().find(|s| s.name == name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_vendored_scheme_parses() {
        for (file, json) in FILES {
            parse(json).unwrap_or_else(|e| panic!("{file}: {e}"));
        }
        assert_eq!(all().len(), FILES.len() + 1);
    }

    #[test]
    fn schemes_used_by_the_tests_resolve() {
        for name in [
            "Cornfield",
            "Metallic",
            "Sunset",
            "Starnight",
            "Monotone",
            "ClearSky",
        ] {
            assert!(find(name).is_some(), "{name}");
        }
        assert!(find("cornfield").is_none(), "names are case-sensitive");
        let sky = find("ClearSky").unwrap();
        assert_eq!(sky.background.rgba_int(), Some([0x87, 0xce, 0xeb, 255]));
        assert_eq!(
            sky.background_stop.rgba_int(),
            Some([0xc9, 0xe9, 0xf6, 255])
        );
        let mono = find("Monotone").unwrap();
        assert_eq!(mono.background, mono.background_stop);
    }

    #[test]
    fn bad_colors_reject_the_file() {
        let json = FILES[0].1.replace("#333333", "333333");
        assert!(parse(&json).unwrap_err().contains("background"));
    }

    /// The vendored files are the reference checkout's, byte for byte.
    #[test]
    fn identical_to_the_reference_checkout() {
        let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../.reference/openscad/color-schemes/render");
        let Ok(entries) = std::fs::read_dir(&dir) else {
            eprintln!("skipped: no reference checkout");
            return;
        };
        let mut theirs: Vec<String> = entries
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.ends_with(".json"))
            .collect();
        theirs.sort();
        let ours: Vec<&str> = FILES.iter().map(|(n, _)| *n).collect();
        assert_eq!(theirs, ours);
        for (name, json) in FILES {
            let theirs = std::fs::read_to_string(dir.join(name)).unwrap();
            assert!(theirs == *json, "{name} differs from the reference");
        }
    }
}
