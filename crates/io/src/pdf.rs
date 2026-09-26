//! PDF export (`src/io/export_pdf.cc`).
//!
//! OpenSCAD draws the page with Cairo; this writes the same drawing as a
//! small hand-built PDF instead. The page is what OpenSCAD's regression
//! tests check: they rasterise the PDF with Ghostscript and compare the
//! image (`tests/export_pngtest.py`), so the file's bytes (Cairo's object
//! layout, its embedded font subsets) are not reproduced, only what the
//! page shows:
//!
//! - the page size and orientation, and the shape centred on it by the
//!   same integer arithmetic (`export_pdf`, including its truncations);
//! - one path of every outline, drawn as Cairo draws it: each outline ends
//!   with a line back to its first point rather than a close, filled with
//!   the non-zero rule, then stroked, with Cairo's defaults (butt caps,
//!   mitre joins, mitre limit 10), which are also PDF's;
//! - the scale, its message, the grid and the file name, with their
//!   alphas as extended graphics states.
//!
//! Cairo sets text in Liberation Sans, a font it embeds. The standard
//! Helvetica font has the same metrics (Liberation Sans was designed to
//! match it), so labels here use it unembedded, which every PDF reader
//! must supply; the glyph shapes differ slightly.

use crate::Outline;

/// `PTS_IN_MM` (`export_pdf.cc:25`).
const PTS_IN_MM: f64 = 2.834645656693;

/// `MARGIN` (`export_pdf.cc`), in points.
const MARGIN: f64 = 30.0;

/// `paperDimensions` (`export_pdf.cc:38-46`), width and height in points.
const PAPER: [(i32, i32); 7] = [
    (298, 420),  // A6
    (420, 595),  // A5
    (595, 842),  // A4
    (842, 1190), // A3
    (612, 792),  // Letter
    (612, 1008), // Legal
    (792, 1224), // Tabloid
];

/// `ExportPdfPaperSize`, in the order of [`PAPER`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaperSize {
    A6,
    A5,
    A4,
    A3,
    Letter,
    Legal,
    Tabloid,
}

impl PaperSize {
    /// The `export-pdf/paper-size` setting's names (`Settings.cc`).
    pub fn from_name(name: &str) -> Option<PaperSize> {
        Some(match name {
            "a6" => PaperSize::A6,
            "a5" => PaperSize::A5,
            "a4" => PaperSize::A4,
            "a3" => PaperSize::A3,
            "letter" => PaperSize::Letter,
            "legal" => PaperSize::Legal,
            "tabloid" => PaperSize::Tabloid,
            _ => return None,
        })
    }
}

/// `ExportPdfPaperOrientation`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Orientation {
    Portrait,
    Landscape,
    Auto,
}

impl Orientation {
    /// The `export-pdf/orientation` setting's names (`Settings.cc`).
    pub fn from_name(name: &str) -> Option<Orientation> {
        Some(match name {
            "portrait" => Orientation::Portrait,
            "landscape" => Orientation::Landscape,
            "auto" => Orientation::Auto,
            _ => return None,
        })
    }
}

/// `ExportPdfOptions`, with colours already resolved to RGBA (the caller
/// parses them, since colour names live in the evaluator). A `None` fill
/// or stroke is switched off. The defaults are the command line's, which
/// come from the settings (`Settings.cc`), not the struct's initialisers.
#[derive(Debug, Clone, PartialEq)]
pub struct PdfOptions {
    pub show_scale: bool,
    pub show_scale_message: bool,
    pub show_grid: bool,
    /// Millimetres.
    pub grid_size: f64,
    pub show_filename: bool,
    pub orientation: Orientation,
    pub paper_size: PaperSize,
    pub add_meta_data: bool,
    pub meta_data_title: String,
    pub meta_data_author: String,
    pub meta_data_subject: String,
    pub meta_data_keywords: String,
    pub fill: Option<[f32; 4]>,
    pub stroke: Option<[f32; 4]>,
    /// Millimetres.
    pub stroke_width: f64,
}

impl Default for PdfOptions {
    fn default() -> Self {
        PdfOptions {
            show_scale: true,
            show_scale_message: true,
            show_grid: false,
            grid_size: 10.0,
            show_filename: false,
            orientation: Orientation::Portrait,
            paper_size: PaperSize::A4,
            add_meta_data: true,
            meta_data_title: String::new(),
            meta_data_author: String::new(),
            meta_data_subject: String::new(),
            meta_data_keywords: String::new(),
            fill: None,
            stroke: Some([0.0, 0.0, 0.0, 1.0]),
            stroke_width: 0.35,
        }
    }
}

/// What the document is about (`ExportInfo`).
#[derive(Debug, Clone, Copy)]
pub struct PdfInfo<'a> {
    /// The input's file name, the default title.
    pub title: &'a str,
    /// The input path as given, printed by `show-filename`.
    pub source_path: &'a str,
    /// `YYYY-MM-DDTHH:MM:SSZ`, the creation date.
    pub creation_date: &'a str,
}

/// The text of OpenSCAD's `EXPORT-WARNING` when the shape is larger than
/// the page less its margin.
pub const TOO_LARGE: &str = "Geometry is too large to fit into selected size.";

fn mm_to_points(mm: f64) -> f64 {
    mm * PTS_IN_MM
}

fn points_to_mm(pts: f64) -> f64 {
    pts / PTS_IN_MM
}

/// A number as PDF content writes it: fixed point (PDF has no exponent
/// syntax), trailing zeros dropped.
fn num(v: f64) -> String {
    let v = if v == 0.0 { 0.0 } else { v };
    let s = format!("{v:.6}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s == "-0" { "0".into() } else { s.into() }
}

/// The drawing, with the graphics state Cairo would track.
struct Page {
    ops: String,
    /// Alphas used, each an `ExtGState` named `/a<i>`.
    alphas: Vec<(u32, u32)>,
    uses_font: bool,
}

impl Page {
    fn op(&mut self, s: &str) {
        self.ops.push_str(s);
        self.ops.push('\n');
    }

    /// `cairo_set_source_rgba`, for both filling and stroking.
    fn source(&mut self, c: [f32; 4]) {
        let [r, g, b, _] = c.map(|x| num(f64::from(x)));
        self.op(&format!("{r} {g} {b} rg {r} {g} {b} RG"));
        let a = f64::from(c[3]);
        // Cairo's source colour paints both fills and strokes.
        let alpha = (a.clamp(0.0, 1.0) * 1000.0).round() as u32;
        let key = (alpha, alpha);
        let i = match self.alphas.iter().position(|k| *k == key) {
            Some(i) => i,
            None => {
                self.alphas.push(key);
                self.alphas.len() - 1
            }
        };
        self.op(&format!("/a{i} gs"));
    }

    fn move_to(&mut self, x: f64, y: f64) {
        self.op(&format!("{} {} m", num(x), num(y)));
    }

    fn line_to(&mut self, x: f64, y: f64) {
        self.op(&format!("{} {} l", num(x), num(y)));
    }

    /// `cairo_set_line_width` and `cairo_stroke` of the current path.
    fn stroke(&mut self, width: f64) {
        self.op(&format!("{} w S", num(width)));
    }

    /// `draw_text`: Liberation Sans (here Helvetica) at `size`, starting
    /// at (`x`, `y`) in Cairo's y-down space. The text matrix flips the
    /// glyphs back up.
    fn text(&mut self, text: &str, x: f64, y: f64, size: f64) {
        self.uses_font = true;
        self.op(&format!(
            "BT /F1 1 Tf {} 0 0 {} {} {} Tm {} Tj ET",
            num(size),
            num(-size),
            num(x),
            num(y),
            pdf_string_winansi(text)
        ));
    }
}

/// `draw_grid` (`export_pdf.cc:66-110`).
fn draw_grid(p: &mut Page, left: f64, right: f64, bottom: f64, top: f64, grid_size: f64) {
    let grid_size = if grid_size < 1.0 { 2.0 } else { grid_size };
    let darker = 0.36;
    let lighter = 0.24;
    // `const int major = (gridSize > 10.0 ? gridSize : int(10.0 / gridSize));`
    let major = if grid_size > 10.0 {
        grid_size as i32
    } else {
        (10.0 / grid_size) as i32
    };
    let x_start = (points_to_mm(left) / grid_size).ceil() as i32;
    let x_stop = (points_to_mm(right) / grid_size).floor() as i32;
    for i in x_start..=x_stop {
        let (w, a) = if i % major != 0 {
            (lighter, 0.48)
        } else {
            (darker, 0.6)
        };
        p.source([0.0, 0.0, 0.0, a]);
        let pts = mm_to_points(f64::from(i) * grid_size);
        p.move_to(pts, top);
        p.line_to(pts, bottom);
        p.stroke(w);
    }
    let y_start = (points_to_mm(top) / grid_size).ceil() as i32;
    let y_stop = (points_to_mm(bottom) / grid_size).floor() as i32;
    for i in y_start..=y_stop {
        let (w, a) = if i % major != 0 {
            (lighter, 0.4)
        } else {
            (darker, 0.6)
        };
        p.source([0.0, 0.0, 0.0, a]);
        let pts = mm_to_points(f64::from(i) * grid_size);
        p.move_to(left, pts);
        p.line_to(right, pts);
        p.stroke(w);
    }
}

/// `draw_axes` (`export_pdf.cc:113-160`).
fn draw_axes(p: &mut Page, left: f64, right: f64, bottom: f64, top: f64) {
    let darker = 0.36;
    let offset = mm_to_points(5.0);
    p.source([0.0, 0.0, 0.0, 0.6]);
    p.move_to(left, top);
    p.line_to(left, bottom);
    p.stroke(darker);
    p.move_to(left, bottom);
    p.line_to(right, bottom);
    p.stroke(darker);
    let x_start = (points_to_mm(left) / 10.0).ceil() as i32;
    let x_stop = (points_to_mm(right) / 10.0).floor() as i32;
    for i in x_start..=x_stop {
        let pts = mm_to_points(f64::from(i) * 10.0);
        p.move_to(pts, bottom);
        p.line_to(pts, bottom + offset);
        p.stroke(darker);
        if i % 2 == 0 {
            p.text(&(i * 10).to_string(), pts + 1.0, bottom + offset - 2.0, 6.0);
        }
    }
    let y_start = (points_to_mm(top) / 10.0).ceil() as i32;
    let y_stop = (points_to_mm(bottom) / 10.0).floor() as i32;
    for i in y_start..=y_stop {
        let pts = mm_to_points(f64::from(i) * 10.0);
        p.move_to(left, pts);
        p.line_to(left - offset, pts);
        p.stroke(darker);
        if i % 2 == 0 {
            p.text(&(-i * 10).to_string(), left - offset, pts - 3.0, 6.0);
        }
    }
}

/// `export_pdf`: one page with `outlines` (in millimetres) centred on it.
/// Returns the file and the `EXPORT-WARNING` texts to print.
pub fn write(
    outlines: &[Outline],
    options: &PdfOptions,
    info: &PdfInfo<'_>,
) -> (Vec<u8>, Vec<String>) {
    let mut warnings = Vec::new();
    let mut it = outlines.iter().flat_map(|o| o.vertices.iter());
    let (lo, hi) = match it.next() {
        Some(&first) => it.fold((first, first), |(lo, hi), v| {
            (
                [lo[0].min(v[0]), lo[1].min(v[1])],
                [hi[0].max(v[0]), hi[1].max(v[1])],
            )
        }),
        None => ([0.0; 2], [0.0; 2]),
    };
    // The same integer steps as OpenSCAD: whole millimetres, then points
    // truncated to `int`.
    let minx = lo[0].floor() as i32;
    let maxy = hi[1].floor() as i32;
    let maxx = hi[0].ceil() as i32;
    let miny = lo[1].ceil() as i32;
    let span_x = mm_to_points(f64::from(maxx - minx)) as i32;
    let span_y = mm_to_points(f64::from(maxy - miny)) as i32;
    let center_x = (mm_to_points(f64::from(minx)) + f64::from(span_x) / 2.0) as i32;
    let center_y = (mm_to_points(f64::from(miny)) + f64::from(span_y) / 2.0) as i32;

    let (w, h) = PAPER[options.paper_size as usize];
    let landscape = (options.orientation == Orientation::Auto && span_x > span_y)
        || options.orientation == Orientation::Landscape;
    let (pdf_x, pdf_y) = if landscape { (h, w) } else { (w, h) };
    let fits = f64::from(span_x) <= f64::from(pdf_x) - MARGIN
        && f64::from(span_y) <= f64::from(pdf_y) - MARGIN;
    if !fits {
        warnings.push(TOO_LARGE.to_string());
    }

    let (pdf_xf, pdf_yf) = (f64::from(pdf_x), f64::from(pdf_y));
    let (cx, cy) = (f64::from(center_x), f64::from(center_y));
    let tc_x = pdf_xf / 2.0 - cx;
    let tc_y = pdf_yf / 2.0 + cy;
    let mlx = cx - pdf_xf / 2.0 + MARGIN;
    let mrx = cx + pdf_xf / 2.0 - MARGIN;
    let mty = -(cy - pdf_yf / 2.0 + MARGIN);
    let mby = -(cy + pdf_yf / 2.0 - MARGIN);

    let mut p = Page {
        ops: String::new(),
        alphas: Vec::new(),
        uses_font: false,
    };
    // Cairo's user space: y down from the top of the page, then
    // `cairo_translate(cr, tcX, tcY)`.
    p.op(&format!("1 0 0 -1 0 {pdf_y} cm"));
    p.op(&format!("1 0 0 1 {} {} cm", num(tc_x), num(tc_y)));

    // `draw_geom`: y is inverted, and each outline returns to its start
    // with a line, not a close, so the stroke has butt caps there. The one
    // exception is Cairo's: a path that is a single axis-aligned rectangle
    // is written as `re`, which PDF closes, so its first corner is mitred
    // like the others (see [`cairo_box`]).
    let mut path = String::new();
    if let Some([x, y, w, h]) = cairo_box(outlines, tc_x, tc_y) {
        path.push_str(&format!("{} {} {} {} re\n", num(x), num(y), num(w), num(h)));
    } else {
        for o in outlines {
            let Some(p0) = o.vertices.first() else {
                continue;
            };
            let (x0, y0) = (num(mm_to_points(p0[0])), num(mm_to_points(-p0[1])));
            path.push_str(&format!("{x0} {y0} m\n"));
            for v in &o.vertices[1..] {
                path.push_str(&format!(
                    "{} {} l\n",
                    num(mm_to_points(v[0])),
                    num(mm_to_points(-v[1]))
                ));
            }
            path.push_str(&format!("{x0} {y0} l\n"));
        }
    }
    let paint = |p: &mut Page, color: [f32; 4], op: &str| {
        p.source(color);
        p.ops.push_str(&path);
        p.op(op);
    };
    if let Some(c) = options.fill {
        paint(&mut p, c, "f");
    }
    if let Some(c) = options.stroke {
        p.op(&format!("{} w", num(mm_to_points(options.stroke_width))));
        paint(&mut p, c, "S");
    }

    let about = "Scale is to calibrate actual printed dimension. Check both X and Y. \
                 Measure between tick 0 and last tick";
    p.source([0.0, 0.0, 0.0, 0.48]);
    if options.show_filename {
        p.text(info.source_path, mlx, mby, 10.0);
    }
    if options.show_scale {
        draw_axes(&mut p, mlx, mrx, mty, mby);
        if options.show_scale_message {
            // `draw_axes` changed the source; OpenSCAD does not restore it.
            p.text(about, mlx + 1.0, mty - 1.0, 5.0);
        }
        if options.show_grid {
            draw_grid(&mut p, mlx, mrx, mty, mby, options.grid_size);
        }
    }

    let meta = if options.add_meta_data {
        let title = if options.meta_data_title.is_empty() {
            info.title
        } else {
            &options.meta_data_title
        };
        let mut m = Vec::new();
        let mut add = |k: &str, v: &str| {
            if !v.is_empty() {
                m.push(format!("/{k} {}", pdf_text_string(v)));
            }
        };
        add("Title", title);
        add("Author", &options.meta_data_author);
        add("Subject", &options.meta_data_subject);
        add("Keywords", &options.meta_data_keywords);
        add("Creator", "OpenSCAD (https://www.openscad.org/)");
        let date = pdf_date(info.creation_date);
        add("CreationDate", &date);
        m
    } else {
        Vec::new()
    };
    (assemble(&p, pdf_x, pdf_y, &meta), warnings)
}

/// Cairo's rectangle detection, in the user space before the page
/// translation: `Some([x, y, w, h])` when the whole path is one axis-aligned
/// rectangle.
///
/// Cairo stores a path in device space in 24.8 fixed point and simplifies
/// it as it is built (`_cairo_path_fixed_line_to`): a line to the current
/// point is dropped, and a line continuing the previous one in the same
/// direction replaces it. The PDF backend then writes a path that is a
/// single move and four lines back to the start, forming a box, as `re`
/// (`_cairo_pdf_operators_emit_path` via `_cairo_path_fixed_is_rectangle`).
fn cairo_box(outlines: &[Outline], tc_x: f64, tc_y: f64) -> Option<[f64; 4]> {
    let mut drawn = outlines.iter().filter(|o| !o.vertices.is_empty());
    let o = drawn.next()?;
    if drawn.next().is_some() {
        return None;
    }
    let fixed = |v: &[f64; 2]| {
        let x = tc_x + mm_to_points(v[0]);
        let y = tc_y + mm_to_points(-v[1]);
        ((x * 256.0).round() as i64, (y * 256.0).round() as i64)
    };
    let first = fixed(&o.vertices[0]);
    let mut pts = vec![first];
    for v in o.vertices[1..]
        .iter()
        .chain(std::iter::once(&o.vertices[0]))
    {
        let p = fixed(v);
        let cur = *pts.last().expect("starts with the move");
        if pts.len() > 1 {
            if p == cur {
                continue;
            }
            let prev = pts[pts.len() - 2];
            let (a, b) = ((cur.0 - prev.0, cur.1 - prev.1), (p.0 - cur.0, p.1 - cur.1));
            // `_cairo_slope_equal` and not `_cairo_slope_backwards`.
            if a.1 * b.0 == b.1 * a.0 && (a.0 * b.0 + a.1 * b.1) > 0 {
                pts.pop();
            }
        }
        pts.push(p);
    }
    // A move and four lines, the last back to the start.
    if pts.len() != 5 || pts[4] != pts[0] {
        return None;
    }
    let [p0, p1, p2, p3] = [pts[0], pts[1], pts[2], pts[3]];
    let rect = (p0.1 == p1.1 && p1.0 == p2.0 && p2.1 == p3.1 && p3.0 == p0.0)
        || (p0.0 == p1.0 && p1.1 == p2.1 && p2.0 == p3.0 && p3.1 == p0.1);
    if !rect {
        return None;
    }
    let user = |p: (i64, i64)| (p.0 as f64 / 256.0 - tc_x, p.1 as f64 / 256.0 - tc_y);
    let (a, c) = (user(p0), user(p2));
    Some([a.0, a.1, c.0 - a.0, c.1 - a.1])
}

/// `YYYY-MM-DDTHH:MM:SSZ` as a PDF date, `D:YYYYMMDDHHmmSSZ`.
fn pdf_date(iso: &str) -> String {
    let digits: String = iso.chars().filter(char::is_ascii_digit).collect();
    if digits.len() == 14 {
        format!("D:{digits}Z")
    } else {
        String::new()
    }
}

/// A PDF text string: literal when ASCII, else UTF-16BE with a byte order
/// mark, in hex.
fn pdf_text_string(s: &str) -> String {
    if s.is_ascii() {
        return pdf_literal(s.as_bytes());
    }
    let mut out = String::from("<FEFF");
    for u in s.encode_utf16() {
        out.push_str(&format!("{u:04X}"));
    }
    out.push('>');
    out
}

/// Text for a `Tj` in a WinAnsi-encoded standard font: Latin-1 characters
/// map to themselves, others become `?`.
fn pdf_string_winansi(s: &str) -> String {
    let bytes: Vec<u8> = s
        .chars()
        .map(|c| u8::try_from(u32::from(c)).unwrap_or(b'?'))
        .collect();
    pdf_literal(&bytes)
}

fn pdf_literal(bytes: &[u8]) -> String {
    let mut out = String::from("(");
    for &b in bytes {
        match b {
            b'(' | b')' | b'\\' => {
                out.push('\\');
                out.push(char::from(b));
            }
            0x20..=0x7e => out.push(char::from(b)),
            _ => out.push_str(&format!("\\{b:03o}")),
        }
    }
    out.push(')');
    out
}

/// The file: catalog, pages, page, content, font and graphics states, the
/// document information, and the cross-reference table.
fn assemble(p: &Page, w: i32, h: i32, meta: &[String]) -> Vec<u8> {
    let mut objects: Vec<Vec<u8>> = Vec::new();
    // Object numbers: 1 catalog, 2 pages, 3 page, 4 content, then the
    // font, the graphics states and the information dictionary.
    let font_id = 5;
    let gs_first = font_id + usize::from(p.uses_font);
    let info_id = gs_first + p.alphas.len();
    objects.push(b"<< /Type /Catalog /Pages 2 0 R >>".to_vec());
    objects.push(b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec());
    let mut resources = String::new();
    if p.uses_font {
        resources.push_str(&format!("/Font << /F1 {font_id} 0 R >> "));
    }
    if !p.alphas.is_empty() {
        resources.push_str("/ExtGState << ");
        for i in 0..p.alphas.len() {
            resources.push_str(&format!("/a{i} {} 0 R ", gs_first + i));
        }
        resources.push_str(">> ");
    }
    objects.push(
        format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {w} {h}] /Contents 4 0 R /Resources << {resources}>> >>"
        )
        .into_bytes(),
    );
    let mut content = format!("<< /Length {} >>\nstream\n", p.ops.len()).into_bytes();
    content.extend_from_slice(p.ops.as_bytes());
    content.extend_from_slice(b"endstream");
    objects.push(content);
    if p.uses_font {
        objects.push(
            b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>"
                .to_vec(),
        );
    }
    for &(fill, stroke) in &p.alphas {
        objects.push(
            format!(
                "<< /Type /ExtGState /ca {} /CA {} >>",
                num(f64::from(fill) / 1000.0),
                num(f64::from(stroke) / 1000.0)
            )
            .into_bytes(),
        );
    }
    let has_info = !meta.is_empty();
    if has_info {
        objects.push(format!("<< {} >>", meta.join(" ")).into_bytes());
    }

    let mut out = b"%PDF-1.5\n%\xb5\xed\xae\xfb\n".to_vec();
    let mut offsets = Vec::with_capacity(objects.len());
    for (i, o) in objects.iter().enumerate() {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n", i + 1).as_bytes());
        out.extend_from_slice(o);
        out.extend_from_slice(b"\nendobj\n");
    }
    let xref = out.len();
    out.extend_from_slice(
        format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes(),
    );
    for off in offsets {
        out.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
    }
    let info = if has_info {
        format!(" /Info {info_id} 0 R")
    } else {
        String::new()
    };
    out.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R{info} >>\nstartxref\n{xref}\n%%EOF\n",
            objects.len() + 1
        )
        .as_bytes(),
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn square(x: f64, y: f64, s: f64) -> Outline {
        Outline::new(vec![[x, y], [x + s, y], [x + s, y + s], [x, y + s]])
    }

    #[test]
    fn numbers_are_fixed_point() {
        assert_eq!(num(1.0), "1");
        assert_eq!(num(-0.0), "0");
        assert_eq!(num(2.5e-7), "0");
        assert_eq!(num(1e20), "100000000000000000000");
        assert_eq!(num(-3.125), "-3.125");
    }

    #[test]
    fn centres_like_openscad() {
        // A 40 mm square at (-70, -70): minx = -70, maxy = -30, maxx = -30,
        // miny = -70; spans 113 pt; centre (-198 + 56.5) truncated = -141.
        let opts = PdfOptions {
            show_scale: false,
            ..Default::default()
        };
        let info = PdfInfo {
            title: "t.scad",
            source_path: "t.scad",
            creation_date: "2026-01-02T03:04:05Z",
        };
        let (pdf, warnings) = write(&[square(-70.0, -70.0, 40.0)], &opts, &info);
        assert!(warnings.is_empty());
        let text = String::from_utf8_lossy(&pdf);
        assert!(text.contains("/MediaBox [0 0 595 842]"), "{text}");
        // tcX = 297.5 + 141, tcY = 421 - 141.
        assert!(text.contains("1 0 0 1 438.5 280 cm"), "{text}");
        assert!(text.contains("/CreationDate (D:20260102030405Z)"));
        assert!(text.ends_with("%%EOF\n"));
    }

    #[test]
    fn warns_when_too_large() {
        let (_, warnings) = write(
            &[square(0.0, 0.0, 400.0)],
            &PdfOptions::default(),
            &PdfInfo {
                title: "",
                source_path: "",
                creation_date: "",
            },
        );
        assert_eq!(warnings, vec![TOO_LARGE.to_string()]);
    }

    #[test]
    fn non_ascii_titles_are_utf16() {
        assert_eq!(pdf_text_string("a☠"), "<FEFF00612620>");
        assert_eq!(pdf_text_string("a(b)"), "(a\\(b\\))");
    }
}
