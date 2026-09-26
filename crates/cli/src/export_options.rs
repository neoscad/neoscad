//! `-O section/key=value` export settings (`convert_export_options` and
//! `set_cmd_line_option` in OpenSCAD, `openscad.cc` and `io/export.h`).
//!
//! Only settings named on the command line change anything; every other
//! one takes its default from `Settings.cc` (the command line never reads
//! the GUI's saved settings). A value that does not decode also falls back
//! to the default, silently, as `SettingsEntry*::decode` does.

use std::collections::HashMap;

/// The parsed `-O` arguments: section, then key, then the raw value. A
/// later `-O` for the same key replaces an earlier one (`map[s][n] = v`).
#[derive(Debug, Default, Clone)]
pub struct ExportOptions {
    map: HashMap<String, HashMap<String, String>>,
}

/// `simple_split`: both halves empty when `sep` is missing.
fn split(s: &str, sep: char) -> (&str, &str) {
    s.split_once(sep).unwrap_or(("", ""))
}

impl ExportOptions {
    pub fn parse(args: &[String]) -> ExportOptions {
        let mut map: HashMap<String, HashMap<String, String>> = HashMap::new();
        for a in args {
            let (key, value) = split(a, '=');
            let (section, name) = split(key, '/');
            map.entry(section.to_string())
                .or_default()
                .insert(name.to_string(), value.to_string());
        }
        ExportOptions { map }
    }

    fn raw(&self, section: &str, name: &str) -> Option<&str> {
        self.map.get(section)?.get(name).map(String::as_str)
    }

    /// `SettingsEntryString::decode`: the value as given.
    fn string(&self, section: &str, name: &str, default: &str) -> String {
        self.raw(section, name).unwrap_or(default).to_string()
    }

    /// `SettingsEntryBool::decode`: `true`/`false` after trimming, else
    /// `boost::lexical_cast<bool>`, which accepts only `1` and `0`.
    fn bool(&self, section: &str, name: &str, default: bool) -> bool {
        match self.raw(section, name).map(str::trim) {
            Some("true" | "1") => true,
            Some("false" | "0") => false,
            _ => default,
        }
    }

    /// `SettingsEntryDouble::decode`: `boost::lexical_cast<double>` of the
    /// trimmed value, which must be a number with nothing after it. The
    /// entry's range is not applied.
    fn double(&self, section: &str, name: &str, default: f64) -> f64 {
        self.raw(section, name)
            .and_then(|v| lexical_double(v.trim()))
            .unwrap_or(default)
    }

    /// `ExportSvgOptions::withOptions`.
    pub fn svg(&self) -> io::svg::SvgStyle {
        const S: &str = "export-svg";
        let fill = self.bool(S, "fill", false);
        let stroke = self.bool(S, "stroke", true);
        io::svg::SvgStyle {
            fill: fill.then(|| self.string(S, "fill-color", "white")),
            stroke: stroke.then(|| self.string(S, "stroke-color", "black")),
            stroke_width: self.double(S, "stroke-width", 0.35),
        }
    }

    /// `ExportPdfOptions::withOptions`, with the fill and stroke colours
    /// still as text: [`PdfColors::resolve`] parses them.
    pub fn pdf(&self) -> (io::pdf::PdfOptions, PdfColors) {
        const S: &str = "export-pdf";
        let d = io::pdf::PdfOptions::default();
        let options = io::pdf::PdfOptions {
            show_scale: self.bool(S, "show-scale", d.show_scale),
            show_scale_message: self.bool(S, "show-scale-message", d.show_scale_message),
            show_grid: self.bool(S, "show-grid", d.show_grid),
            grid_size: self.double(S, "grid-size", d.grid_size),
            show_filename: self.bool(S, "show-filename", d.show_filename),
            orientation: self
                .raw(S, "orientation")
                .and_then(io::pdf::Orientation::from_name)
                .unwrap_or(d.orientation),
            paper_size: self
                .raw(S, "paper-size")
                .and_then(io::pdf::PaperSize::from_name)
                .unwrap_or(d.paper_size),
            add_meta_data: self.bool(S, "add-meta-data", d.add_meta_data),
            meta_data_title: self.string(S, "meta-data-title", ""),
            meta_data_author: self.string(S, "meta-data-author", ""),
            meta_data_subject: self.string(S, "meta-data-subject", ""),
            meta_data_keywords: self.string(S, "meta-data-keywords", ""),
            fill: None,
            stroke: None,
            stroke_width: self.double(S, "stroke-width", d.stroke_width),
        };
        let colors = PdfColors {
            fill: self
                .bool(S, "fill", false)
                .then(|| self.string(S, "fill-color", "black")),
            stroke: self
                .bool(S, "stroke", true)
                .then(|| self.string(S, "stroke-color", "black")),
        };
        (options, colors)
    }
}

impl ExportOptions {
    /// `SettingsEntryInt::decode`: `boost::lexical_cast<int>` of the
    /// trimmed value (digits with an optional sign, nothing else). The
    /// entry's range is not applied.
    fn int(&self, section: &str, name: &str, default: i32) -> i32 {
        self.raw(section, name)
            .map(str::trim)
            .filter(|v| {
                let d = v.strip_prefix(['+', '-']).unwrap_or(v);
                !d.is_empty() && d.bytes().all(|b| b.is_ascii_digit())
            })
            .and_then(|v| v.parse().ok())
            .unwrap_or(default)
    }

    /// `SettingsEntryEnum::decode`: the item whose name is exactly the
    /// value, else the default.
    fn choice<T: Copy>(&self, section: &str, name: &str, items: &[(&str, T)], default: T) -> T {
        self.raw(section, name)
            .and_then(|v| items.iter().find(|(n, _)| *n == v).map(|(_, t)| *t))
            .unwrap_or(default)
    }

    /// `Export3mfOptions::withOptions`, with the colour resolved as
    /// `export_3mf` resolves it (`OpenSCAD::getColor(color, default)`, only
    /// in `selected-only` mode). Returns the warning a colour name that
    /// does not parse produces.
    pub fn threemf(&self, default_color: io::Color) -> (io::threemf::Options, Option<String>) {
        use io::threemf::{ColorMode, MaterialType, Unit};
        const S: &str = "export-3mf";
        let d = io::threemf::Options::default();
        let color_mode = self.choice(
            S,
            "color-mode",
            &[
                ("model", ColorMode::Model),
                ("none", ColorMode::None),
                ("selected-only", ColorMode::SelectedOnly),
            ],
            d.color_mode,
        );
        let mut warning = None;
        let color = (color_mode == ColorMode::SelectedOnly).then(|| {
            let name = self.string(S, "color", "#f9d72c");
            match eval::parse_color(&name) {
                Some(c) => io::Color(c),
                None => {
                    warning = Some(format!(
                        "Unable to parse color \"{name}\", reverting to default color."
                    ));
                    default_color
                }
            }
        });
        let options = io::threemf::Options {
            color_mode,
            color,
            material_type: self.choice(
                S,
                "material-type",
                &[
                    ("color", MaterialType::Color),
                    ("basematerial", MaterialType::BaseMaterial),
                ],
                d.material_type,
            ),
            unit: self.choice(
                S,
                "unit",
                &[
                    ("micron", Unit::Micron),
                    ("millimeter", Unit::Millimeter),
                    ("centimeter", Unit::Centimeter),
                    ("meter", Unit::Meter),
                    ("inch", Unit::Inch),
                    ("foot", Unit::Foot),
                ],
                d.unit,
            ),
            decimal_precision: self.int(S, "decimal-precision", d.decimal_precision),
            add_meta_data: self.bool(S, "add-meta-data", d.add_meta_data),
            meta_data_title: self.string(S, "meta-data-title", ""),
            meta_data_designer: self.string(S, "meta-data-designer", ""),
            meta_data_description: self.string(S, "meta-data-description", ""),
            meta_data_copyright: self.string(S, "meta-data-copyright", ""),
            meta_data_license_terms: self.string(S, "meta-data-license-terms", ""),
            meta_data_rating: self.string(S, "meta-data-rating", ""),
        };
        (options, warning)
    }
}

/// The PDF paint colours as given, for the enabled paints only.
#[derive(Debug, Clone)]
pub struct PdfColors {
    pub fill: Option<String>,
    pub stroke: Option<String>,
}

impl PdfColors {
    /// `OpenSCAD::getColor(color, black)` for the fill, then the stroke,
    /// the order `export_pdf` asks for them. Each unparsable name falls
    /// back to black and yields OpenSCAD's warning text.
    pub fn resolve(&self, options: &mut io::pdf::PdfOptions) -> Vec<String> {
        let mut warnings = Vec::new();
        let mut get = |name: &String| {
            eval::parse_color(name).unwrap_or_else(|| {
                warnings.push(format!(
                    "Unable to parse color \"{name}\", reverting to default color."
                ));
                [0.0, 0.0, 0.0, 1.0]
            })
        };
        options.fill = self.fill.as_ref().map(&mut get);
        options.stroke = self.stroke.as_ref().map(&mut get);
        warnings
    }
}

/// `boost::lexical_cast<double>`: an optional sign, digits with an
/// optional point, an optional exponent, and nothing else; also `inf`,
/// `infinity` and `nan` in any case.
fn lexical_double(s: &str) -> Option<f64> {
    let body = s.strip_prefix(['+', '-']).unwrap_or(s);
    let word = body.to_ascii_lowercase();
    if matches!(word.as_str(), "inf" | "infinity" | "nan") {
        return s.to_ascii_lowercase().parse().ok();
    }
    let bytes = body.as_bytes();
    let mut i = 0;
    let mut digits = 0;
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        i += 1;
        digits += 1;
    }
    if i < bytes.len() && bytes[i] == b'.' {
        i += 1;
        while i < bytes.len() && bytes[i].is_ascii_digit() {
            i += 1;
            digits += 1;
        }
    }
    if digits == 0 {
        return None;
    }
    if i < bytes.len() && (bytes[i] == b'e' || bytes[i] == b'E') {
        let mut j = i + 1;
        if j < bytes.len() && (bytes[j] == b'+' || bytes[j] == b'-') {
            j += 1;
        }
        let start = j;
        while j < bytes.len() && bytes[j].is_ascii_digit() {
            j += 1;
        }
        if j == start {
            return None;
        }
        i = j;
    }
    if i != bytes.len() {
        return None;
    }
    s.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opts(args: &[&str]) -> ExportOptions {
        ExportOptions::parse(&args.iter().map(|s| s.to_string()).collect::<Vec<_>>())
    }

    #[test]
    fn svg_options_as_the_tests_pass_them() {
        let s = opts(&[
            "export-svg/fill=true",
            "export-svg/fill-color=cyan",
            "export-svg/stroke-color=magenta",
            "export-svg/stroke-width=3",
        ])
        .svg();
        assert_eq!(s.fill.as_deref(), Some("cyan"));
        assert_eq!(s.stroke.as_deref(), Some("magenta"));
        assert_eq!(s.stroke_width, 3.0);
        let s = opts(&["export-svg/stroke=false", "export-svg/fill=1"]).svg();
        assert_eq!(s.stroke, None);
        assert_eq!(s.fill.as_deref(), Some("white"));
        assert_eq!(opts(&[]).svg(), io::svg::SvgStyle::default());
    }

    #[test]
    fn bad_values_fall_back_to_defaults() {
        let s = opts(&["export-svg/stroke=yes", "export-svg/stroke-width=2mm"]).svg();
        assert!(s.stroke.is_some());
        assert_eq!(s.stroke_width, 0.35);
        let (p, _) = opts(&["export-pdf/paper-size=A3", "export-pdf/orientation=auto"]).pdf();
        assert_eq!(p.paper_size, io::pdf::PaperSize::A4);
        assert_eq!(p.orientation, io::pdf::Orientation::Auto);
    }

    #[test]
    fn pdf_colours_resolve_with_a_warning_for_unknown_names() {
        let (mut p, c) = opts(&[
            "export-pdf/fill=true",
            "export-pdf/fill-color=cyan",
            "export-pdf/stroke-color=nope",
        ])
        .pdf();
        let w = c.resolve(&mut p);
        assert_eq!(p.fill, Some([0.0, 1.0, 1.0, 1.0]));
        assert_eq!(p.stroke, Some([0.0, 0.0, 0.0, 1.0]));
        assert_eq!(
            w,
            ["Unable to parse color \"nope\", reverting to default color."]
        );
    }

    #[test]
    fn threemf_options_as_the_nightly_decodes_them() {
        use io::threemf::{ColorMode, MaterialType, Unit};
        let front = io::Color::from_u8(0xf9, 0xd7, 0x2c);
        let (o, w) = opts(&[]).threemf(front);
        assert_eq!(o, io::threemf::Options::default());
        assert!(w.is_none());
        let (o, w) = opts(&[
            "export-3mf/color-mode=selected-only",
            "export-3mf/color=blue",
            "export-3mf/material-type=color",
            "export-3mf/unit=inch",
            "export-3mf/decimal-precision= 4",
            "export-3mf/add-meta-data=false",
            "export-3mf/meta-data-designer=D",
        ])
        .threemf(front);
        assert!(w.is_none());
        assert_eq!(o.color_mode, ColorMode::SelectedOnly);
        assert_eq!(o.color, Some(io::Color([0.0, 0.0, 1.0, 1.0])));
        assert_eq!(o.material_type, MaterialType::Color);
        assert_eq!(o.unit, Unit::Inch);
        assert_eq!(o.decimal_precision, 4);
        assert!(!o.add_meta_data);
        assert_eq!(o.meta_data_designer, "D");
        // Unknown enum names fall back to the default; an out-of-range
        // precision is kept for lib3mf to refuse, as the nightly does.
        let (o, _) =
            opts(&["export-3mf/unit=bogus", "export-3mf/decimal-precision=99"]).threemf(front);
        assert_eq!(o.unit, Unit::Millimeter);
        assert_eq!(o.decimal_precision, 99);
        let (o, _) = opts(&["export-3mf/decimal-precision=4x"]).threemf(front);
        assert_eq!(o.decimal_precision, 6);
        // An unparsable colour warns and uses the default colour.
        let (o, w) = opts(&[
            "export-3mf/color-mode=selected-only",
            "export-3mf/color=notacolor",
        ])
        .threemf(front);
        assert_eq!(o.color, Some(front));
        assert_eq!(
            w.as_deref(),
            Some("Unable to parse color \"notacolor\", reverting to default color.")
        );
    }

    #[test]
    fn doubles_parse_like_lexical_cast() {
        assert_eq!(lexical_double("1"), Some(1.0));
        assert_eq!(lexical_double("-.5e1"), Some(-5.0));
        assert_eq!(lexical_double("1."), Some(1.0));
        assert_eq!(lexical_double("1e"), None);
        assert_eq!(lexical_double("0x10"), None);
        assert_eq!(lexical_double(""), None);
    }
}
