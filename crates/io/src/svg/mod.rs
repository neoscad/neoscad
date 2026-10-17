//! SVG: `import()` of SVG files (`src/io/import_svg.cc` over OpenSCAD's
//! own `libsvg`, ported in [`libsvg`]) and the writer (`export_svg.cc`).
//!
//! Why a port and not `usvg`: `usvg` normalises every shape into `f32`
//! cubic Béziers (circles, ellipses and arcs become four or more cubic
//! segments through `kurbo`) and keeps strokes as paint, where libsvg
//! flattens circles and arcs straight into `$fn`/`$fa`/`$fs`-dependent
//! point counts and turns strokes into Clipper offsets. Neither the vertex
//! counts nor the stroke outlines of OpenSCAD's results can be recovered
//! from `usvg`'s output.
//!
//! Page handling (`import_svg`): lengths go to millimetres (user units at
//! `dpi`, `px` at 96, `pt` at 72, `pc` at 6), a valid `viewBox` scales the
//! content to the page, `preserveAspectRatio` picks the smaller (`meet`) or
//! larger (`slice`) scale and aligns within the page, y is flipped so the
//! page's top edge is at the page height, and `center` moves the centre of
//! the shapes' scaled bounding box to the origin. Shapes are returned
//! separately; OpenSCAD unions them with Clipper, which is the caller's
//! step.

pub mod libsvg;

use crate::text::fmt_g;
use crate::{Curves, Message, Outline};
use libsvg::{Align, Length, Selector, Unit};

/// `import()`'s SVG parameters.
#[derive(Debug, Clone)]
pub struct Options<'a> {
    pub id: Option<&'a str>,
    pub layer: Option<&'a str>,
    pub dpi: f64,
    pub center: bool,
}

const INCH_TO_MM: f64 = 25.4;

fn to_mm(l: Length, viewbox: f64, valid: bool, dpi: f64) -> f64 {
    match l.unit {
        Unit::None => INCH_TO_MM * l.number / dpi,
        Unit::Px => INCH_TO_MM * l.number / 96.0,
        Unit::Pt => INCH_TO_MM * l.number / 72.0,
        Unit::Pc => INCH_TO_MM * l.number / 6.0,
        Unit::In => INCH_TO_MM * l.number,
        Unit::Cm => 10.0 * l.number,
        Unit::Mm => l.number,
        Unit::Percent => {
            if valid {
                INCH_TO_MM * l.number / 100.0 * viewbox / dpi
            } else {
                0.0
            }
        }
        // "If no width/height given, but viewbox is set, then rely on the
        // DPI value (e.g. Adobe Illustrator does that in older versions)".
        Unit::Undefined => {
            if valid {
                INCH_TO_MM * viewbox / dpi
            } else {
                0.0
            }
        }
        Unit::Em | Unit::Ex => {
            if valid {
                viewbox
            } else {
                0.0
            }
        }
    }
}

fn alignment(a: Align, page_mm: f64, scale: f64, viewbox: f64) -> f64 {
    match a {
        Align::Mid => page_mm / 2.0 - scale * viewbox / 2.0,
        Align::Max => page_mm - scale * viewbox,
        _ => 0.0,
    }
}

/// Read an SVG file (`None` when it could not be opened) into one list of
/// outlines per shape, in millimetres. `file` is the path as messages
/// print it, `line` the `import()` call's line.
pub fn read(
    bytes: Option<&[u8]>,
    file: &str,
    line: u32,
    opts: &Options<'_>,
    curves: &dyn Curves,
    msgs: &mut Vec<Message>,
) -> Vec<Vec<Outline>> {
    let Some(bytes) = bytes else {
        msgs.push(Message::error(format!(
            "Can't open file '{file}', import() at line {line}"
        )));
        return Vec::new();
    };
    let selector = match (opts.id, opts.layer) {
        (Some(id), layer) => Selector::Id {
            id: id.to_string(),
            layer: layer.map(str::to_string),
        },
        (None, Some(layer)) => Selector::Layer(layer.to_string()),
        (None, None) => Selector::Root,
    };
    let text = String::from_utf8_lossy(bytes);
    let doc = match libsvg::read(&text, &selector, curves) {
        Ok(d) => d,
        Err(libsvg::ParseError) => {
            msgs.push(Message::error(format!(
                "Error parsing file '{file}', import() at line {line}"
            )));
            return Vec::new();
        }
    };
    let mut match_args = String::new();
    if let Some(id) = opts.id {
        match_args.push_str(&format!("id = \"{id}\""));
    }
    if let Some(layer) = opts.layer {
        if opts.id.is_some() {
            match_args.push_str(", ");
        }
        match_args.push_str(&format!("layer = \"{layer}\""));
    }
    if !match_args.is_empty() && doc.matches == 0 {
        msgs.push(
            Message::warning(format!(
                "import() filter {match_args} did not match anything"
            ))
            .at_call(),
        );
    }

    let (mut height_mm, mut scale, mut align, mut viewbox) =
        (0.0, [1.0, 1.0], [0.0, 0.0], [0.0, 0.0]);
    let (mut lo, mut hi) = ([f64::MAX; 2], [f64::MIN; 2]);
    for item in &doc.items {
        if let Some(page) = &item.page {
            let vb = page.viewbox;
            let width_mm = to_mm(page.width, vb.width, vb.valid, opts.dpi);
            height_mm = to_mm(page.height, vb.height, vb.valid, opts.dpi);
            if vb.valid {
                let px = if page.width.unit == Unit::Percent {
                    page.width.number / 100.0
                } else {
                    1.0
                };
                let py = if page.height.unit == Unit::Percent {
                    page.height.number / 100.0
                } else {
                    1.0
                };
                viewbox = [px * vb.x, py * vb.y];
                scale = [width_mm / vb.width, height_mm / vb.height];
                let a = page.alignment;
                if a.x != Align::None {
                    // `meet` fits the page (the smaller scale), `slice`
                    // fills it (the larger).
                    let s = if a.meet {
                        if scale[0] < scale[1] {
                            scale[0]
                        } else {
                            scale[1]
                        }
                    } else if scale[0] > scale[1] {
                        scale[0]
                    } else {
                        scale[1]
                    };
                    scale = [s, s];
                    align = [
                        alignment(a.x, width_mm, s, vb.width),
                        alignment(a.y, height_mm, s, vb.height),
                    ];
                }
            }
        }
        if !item.excluded {
            for v in item.paths.iter().flatten() {
                let p = [scale[0] * v[0], scale[1] * v[1]];
                for k in 0..2 {
                    lo[k] = lo[k].min(p[k]);
                    hi[k] = hi[k].max(p[k]);
                }
            }
        }
    }
    // Eigen's empty box has min = DBL_MAX and max = lowest, so its centre
    // is 0.
    let centre = |k: usize| {
        if lo[k] > hi[k] {
            0.0
        } else {
            (lo[k] + hi[k]) / 2.0
        }
    };
    let cx = if opts.center { centre(0) } else { -align[0] };
    let cy = if opts.center {
        centre(1)
    } else {
        height_mm - align[1]
    };
    doc.items
        .iter()
        .filter(|item| !item.excluded)
        .map(|item| {
            item.paths
                .iter()
                .map(|p| {
                    Outline::new(
                        p.iter()
                            .map(|v| {
                                [
                                    scale[0] * (-viewbox[0] + v[0]) - cx,
                                    scale[1] * (-viewbox[1] - v[1]) + cy,
                                ]
                            })
                            .collect(),
                    )
                })
                .collect::<Vec<_>>()
        })
        .filter(|o: &Vec<Outline>| !o.is_empty())
        .collect()
}

/// `ExportSvgOptions`: the paint of the exported path. Colours are
/// written as given (OpenSCAD does not parse them for SVG).
#[derive(Debug, Clone, PartialEq)]
pub struct SvgStyle {
    /// `fill-color` when `fill` is on.
    pub fill: Option<String>,
    /// `stroke-color` when `stroke` is on.
    pub stroke: Option<String>,
    pub stroke_width: f64,
}

impl Default for SvgStyle {
    /// The settings' defaults (`Settings.cc`): no fill, a black stroke
    /// 0.35 wide.
    fn default() -> Self {
        SvgStyle {
            fill: None,
            stroke: Some("black".into()),
            stroke_width: 0.35,
        }
    }
}

/// `export_svg` with the default options.
pub fn write(outlines: &[Outline]) -> Vec<u8> {
    write_styled(outlines, &SvgStyle::default())
}

/// `export_svg`: a view box of whole millimetres around the shape, padded
/// by half the stroke when there is one, then one path with every outline,
/// y flipped (so `0` prints as `-0`), six points to a line.
pub fn write_styled(outlines: &[Outline], style: &SvgStyle) -> Vec<u8> {
    let mut out = header(outlines.iter(), style);
    push_path(&mut out, outlines, style, None);
    out.push_str("</svg>\n");
    out.into_bytes()
}

/// [`write_styled`] with the model's parts kept apart (NeoSCAD's
/// `--enable part`): the same view box around every layer's outlines,
/// then the geometry outside every part as the plain path, then each part
/// as `<g id="NAME">` around its own path, in the order given. A part's
/// colour replaces the style's stroke and fill colours (whichever are on),
/// with its alpha as `stroke-opacity`/`fill-opacity` when below 1.
pub fn write_layers(layers: &[crate::Layer<'_>], style: &SvgStyle) -> Vec<u8> {
    let mut out = header(layers.iter().flat_map(|l| l.outlines.iter()), style);
    for l in layers {
        match l.name {
            None => push_path(&mut out, l.outlines, style, l.paint()),
            Some(name) => {
                out.push_str(&format!("<g id=\"{}\">\n", xml_attr(name)));
                push_path(&mut out, l.outlines, style, l.paint());
                out.push_str("</g>\n");
            }
        }
    }
    out.push_str("</svg>\n");
    out.into_bytes()
}

/// `s` escaped for a double-quoted XML attribute. Tab, newline and
/// carriage return become character references, which a reader keeps
/// (written raw, attribute normalisation would turn them into spaces);
/// the other control characters, and U+FFFE and U+FFFF, are left out,
/// since XML 1.0 cannot hold them at all and one would make the file
/// unreadable.
fn xml_attr(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => o.push_str("&amp;"),
            '<' => o.push_str("&lt;"),
            '>' => o.push_str("&gt;"),
            '"' => o.push_str("&quot;"),
            '\t' | '\n' | '\r' => o.push_str(&format!("&#{};", u32::from(c))),
            '\u{0}'..='\u{1f}' | '\u{fffe}' | '\u{ffff}' => {}
            _ => o.push(c),
        }
    }
    o
}

/// The XML prolog, the `<svg>` element sized to `outlines` and the title.
fn header<'a>(outlines: impl Iterator<Item = &'a Outline>, style: &SvgStyle) -> String {
    let pad = if style.stroke.is_some() {
        style.stroke_width / 2.0
    } else {
        0.0
    };
    let mut it = outlines.flat_map(|o| o.vertices.iter());
    let (lo, hi) = match it.next() {
        Some(&first) => it.fold((first, first), |(lo, hi), v| {
            (
                [lo[0].min(v[0]), lo[1].min(v[1])],
                [hi[0].max(v[0]), hi[1].max(v[1])],
            )
        }),
        None => ([f64::MAX; 2], [-f64::MAX; 2]),
    };
    let minx = (lo[0] - pad).floor() as i32;
    let miny = (-hi[1] - pad).floor() as i32;
    let maxx = (hi[0] + pad).ceil() as i32;
    let maxy = (-lo[1] + pad).ceil() as i32;
    let (width, height) = (maxx - minx, maxy - miny);
    let mut out = String::new();
    out.push_str("<?xml version=\"1.0\" standalone=\"no\"?>\n");
    out.push_str("<!DOCTYPE svg PUBLIC \"-//W3C//DTD SVG 1.1//EN\" \"http://www.w3.org/Graphics/SVG/1.1/DTD/svg11.dtd\">\n");
    out.push_str(&format!(
        "<svg width=\"{width}mm\" height=\"{height}mm\" viewBox=\"{minx} {miny} {width} {height}\" xmlns=\"http://www.w3.org/2000/svg\" version=\"1.1\">\n"
    ));
    // OpenSCAD's title, kept: its export tests compare the file as text.
    out.push_str("<title>OpenSCAD Model</title>\n");
    out
}

/// One `<path>` with every outline of `outlines`, painted by `style`, or
/// in `color` where the style paints at all.
fn push_path(out: &mut String, outlines: &[Outline], style: &SvgStyle, color: Option<[f32; 4]>) {
    out.push_str("<path d=\"\n");
    for o in outlines {
        let Some(p0) = o.vertices.first() else {
            continue;
        };
        out.push_str(&format!("M {},{}", fmt_g(p0[0]), fmt_g(-p0[1])));
        for (idx, v) in o.vertices.iter().enumerate().skip(1) {
            out.push_str(&format!(" L {},{}", fmt_g(v[0]), fmt_g(-v[1])));
            if idx % 6 == 5 {
                out.push('\n');
            }
        }
        out.push_str(" z\n");
    }
    let paint = |p: &Option<String>| match (p, color) {
        (None, _) => "none".to_string(),
        (Some(_), Some(c)) => crate::hex_rgb(c),
        (Some(p), None) => p.clone(),
    };
    out.push_str(&format!(
        "\" stroke=\"{}\" fill=\"{}\" stroke-width=\"{}\"",
        paint(&style.stroke),
        paint(&style.fill),
        fmt_g(style.stroke_width)
    ));
    if let Some(c) = color.filter(|c| c[3] < 1.0) {
        let a = fmt_g(f64::from(c[3].max(0.0)));
        if style.stroke.is_some() {
            out.push_str(&format!(" stroke-opacity=\"{a}\""));
        }
        if style.fill.is_some() {
            out.push_str(&format!(" fill-opacity=\"{a}\""));
        }
    }
    out.push_str("/>\n");
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fn0;
    impl Curves for Fn0 {
        fn circular_segments(&self, r: f64, angle: f64) -> Option<i32> {
            // `$fn = 0, $fa = 12, $fs = 2`.
            let n = (360.0f64 / 12.0)
                .min(r * 2.0 * std::f64::consts::PI / 2.0)
                .max(5.0)
                .ceil();
            Some(((n * angle.abs() / 360.0).ceil() as i32).max(1))
        }
        fn path_segments(&self) -> i32 {
            3
        }
    }

    fn read_str(s: &str, opts: &Options<'_>) -> (Vec<Vec<Outline>>, Vec<Message>) {
        let mut msgs = Vec::new();
        let out = read(Some(s.as_bytes()), "f.svg", 1, opts, &Fn0, &mut msgs);
        (out, msgs)
    }

    const DEFAULT: Options<'static> = Options {
        id: None,
        layer: None,
        dpi: 72.0,
        center: false,
    };

    #[test]
    fn rect_in_millimetres_is_flipped() {
        let (out, msgs) = read_str(
            r#"<svg width="10mm" height="20mm" viewBox="0 0 10 20"><rect x="1" y="2" width="3" height="4"/></svg>"#,
            &DEFAULT,
        );
        assert!(msgs.is_empty());
        assert_eq!(out.len(), 1);
        assert_eq!(
            out[0][0].vertices,
            vec![
                [1.0, 18.0],
                [4.0, 18.0],
                [4.0, 14.0],
                [1.0, 14.0],
                [1.0, 18.0]
            ]
        );
    }

    #[test]
    fn circles_have_at_least_40_points() {
        let (out, _) = read_str(r#"<svg><circle cx="0" cy="0" r="1"/></svg>"#, &DEFAULT);
        assert_eq!(out[0][0].vertices.len(), 40);
    }

    #[test]
    fn selection_and_parse_errors() {
        let opts = Options {
            id: Some("nope"),
            ..DEFAULT
        };
        let (out, msgs) = read_str(r#"<svg><rect id="a" width="1" height="1"/></svg>"#, &opts);
        assert!(out.is_empty());
        assert_eq!(
            msgs[0].text,
            "import() filter id = \"nope\" did not match anything"
        );
        assert!(msgs[0].located);
        let (_, msgs) = read_str("hello world", &DEFAULT);
        assert_eq!(
            msgs[0].text,
            "Error parsing file 'f.svg', import() at line 1"
        );
    }

    #[test]
    fn open_paths_are_stroked_closed_ones_filled() {
        let (out, _) = read_str(
            r#"<svg><path d="M 0 0 L 10 0" stroke-width="2"/></svg>"#,
            &DEFAULT,
        );
        let v = &out[0][0].vertices;
        let ys: Vec<f64> = v.iter().map(|p| p[1]).collect();
        let span = ys.iter().cloned().fold(f64::MIN, f64::max)
            - ys.iter().cloned().fold(f64::MAX, f64::min);
        // Without a viewBox, user units are millimetres.
        assert!((span - 2.0).abs() < 1e-6, "{span}");
        let (out, _) = read_str(r#"<svg><path d="M 0 0 L 10 0 L 0 10 z"/></svg>"#, &DEFAULT);
        assert_eq!(out[0][0].vertices.len(), 4);
    }

    #[test]
    fn layers_are_groups_with_their_colours() {
        let sq = |x: f64| Outline::new(vec![[x, 0.0], [x + 1.0, 0.0], [x + 1.0, 1.0], [x, 1.0]]);
        let (rest, stud) = ([sq(0.0)], [sq(2.0)]);
        let layers = [
            crate::Layer {
                name: None,
                color: None,
                outlines: &rest,
            },
            crate::Layer {
                name: Some("wall.\"stud\""),
                color: Some([1.0, 0.0, 0.0, 0.5]),
                outlines: &stud,
            },
        ];
        let svg = String::from_utf8(write_layers(&layers, &SvgStyle::default())).unwrap();
        // One view box around both, as for the union of the two.
        let both = String::from_utf8(write(&[sq(0.0), sq(2.0)])).unwrap();
        let head = |s: &str| s.lines().take(4).collect::<Vec<_>>().join("\n");
        assert_eq!(head(&svg), head(&both));
        assert!(svg.contains(
            "\" stroke=\"black\" fill=\"none\" stroke-width=\"0.35\"/>\n<g id=\"wall.&quot;stud&quot;\">\n<path d=\"\nM 2,-0"
        ));
        assert!(svg.ends_with(
            "\" stroke=\"#ff0000\" fill=\"none\" stroke-width=\"0.35\" stroke-opacity=\"0.5\"/>\n</g>\n</svg>\n"
        ));
        // The exported groups are what `import(id = ...)` selects.
        let opts = Options {
            id: Some("wall.\"stud\""),
            layer: None,
            dpi: 96.0,
            center: false,
        };
        let (shapes, msgs) = read_str(&svg, &opts);
        assert!(msgs.is_empty(), "{msgs:?}");
        assert_eq!(shapes.len(), 1);
    }

    #[test]
    fn group_ids_stay_valid_xml() {
        assert_eq!(xml_attr("a\tb\nc\rd"), "a&#9;b&#10;c&#13;d");
        assert_eq!(xml_attr("a\u{1}b\u{1f}c\u{ffff}"), "abc");
        assert_eq!(xml_attr("<&>\""), "&lt;&amp;&gt;&quot;");
        // A "no colour" layer is painted in the style's colours.
        let sq = [Outline::new(vec![[0.0, 0.0], [1.0, 0.0], [1.0, 1.0]])];
        let layers = [crate::Layer {
            name: Some("u"),
            color: Some([-1.0; 4]),
            outlines: &sq,
        }];
        let svg = String::from_utf8(write_layers(&layers, &SvgStyle::default())).unwrap();
        assert!(svg.contains("stroke=\"black\""), "{svg}");
        assert!(!svg.contains("opacity"), "{svg}");
    }
}
