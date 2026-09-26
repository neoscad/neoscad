//! A port of OpenSCAD's `libsvg` (`src/libsvg/`): it reads an SVG file into
//! a flat list of shapes, each already flattened into point paths in the
//! document's user units with its transforms applied.
//!
//! What it does, and so what this does (it is deliberately not a general
//! SVG renderer):
//! - `circle`, `ellipse`, `rect`, `polygon` and closed `path` subpaths are
//!   filled outlines; `line`, `polyline` and open `path` subpaths become the
//!   outline of their stroke, offset by Clipper with the stroke width, cap
//!   and join (`bevel` maps to Clipper's square join);
//! - circles and ellipses get `max(segments, 40)` points; arcs get
//!   `max(segments(r, sweep), |sweep| * 10 / 180 + 4)` steps; Béziers get
//!   `max($fn, 20)` steps;
//! - only `transform`, `display` (attribute or style), the stroke
//!   properties, `id` and Inkscape layers are read; fill rules, CSS
//!   `<style>` sheets, `symbol`, text and everything else are ignored;
//! - `<use>` can only refer to shapes inside `<defs>` defined before it;
//! - the numbers in a `rect` with rounded corners go through a 6-digit
//!   string on their way to the path parser, as in the C++.

use std::collections::{BTreeMap, HashMap};

use clipper2_rust::{ClipperOffset, EndType, JoinType, Path64, Paths64, Point64};
use quick_xml::events::{BytesStart, Event};

use crate::text::fmt_g;
use crate::trig::{atan2_degrees, cos_degrees, sin_degrees, tan_degrees};
use crate::Curves;

/// A path of points (`path_t`, which holds `Vector3d`s with z = 0).
pub type Path = Vec<[f64; 2]>;

type Mat3 = [[f64; 3]; 3];

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Unit {
    Undefined,
    None,
    Percent,
    Em,
    Ex,
    Px,
    In,
    Cm,
    Mm,
    Pt,
    Pc,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Length {
    pub number: f64,
    pub unit: Unit,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ViewBox {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
    pub valid: bool,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Align {
    None,
    Min,
    Mid,
    Max,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Alignment {
    pub x: Align,
    pub y: Align,
    pub meet: bool,
}

/// An `<svg>` element's page geometry.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Page {
    pub width: Length,
    pub height: Length,
    pub viewbox: ViewBox,
    pub alignment: Alignment,
}

#[derive(Debug, Clone)]
struct Shape {
    name: String,
    parent: Option<usize>,
    children: Vec<usize>,
    id: Option<String>,
    layer: Option<String>,
    transform: String,
    stroke_width: String,
    stroke_linecap: String,
    stroke_linejoin: String,
    style: String,
    excluded: bool,
    selected: bool,
    paths: Vec<Path>,
    page: Option<Page>,
    href: String,
}

/// What `import()` asked for: `fnContext::selector`.
#[derive(Debug, Clone)]
pub enum Selector {
    /// No `id` or `layer`: the root is selected.
    Root,
    /// `id`, optionally within a `layer`.
    Id { id: String, layer: Option<String> },
    Layer(String),
}

/// A shape as the reader hands it on.
#[derive(Debug, Clone)]
pub struct Item {
    pub paths: Vec<Path>,
    pub excluded: bool,
    /// For `<svg>` elements, their page geometry.
    pub page: Option<Page>,
}

/// The reader's result: shapes in `shape_list` order, and how many shapes
/// the selector matched.
#[derive(Debug, Clone)]
pub struct Document {
    pub items: Vec<Item>,
    pub matches: usize,
}

struct Reader<'a> {
    shapes: Vec<Shape>,
    curves: &'a dyn Curves,
    selector: &'a Selector,
    matches: usize,
}

const CONTAINERS: [&str; 4] = ["svg", "g", "text", "tspan"];
const KNOWN: [&str; 13] = ["circle", "ellipse", "line", "text", "tspan", "data", "polygon", "polyline", "rect", "svg", "path", "g", "use"];

/// The file is not well-formed XML (libxml's reader failed, which
/// `libsvg` reports as `SvgException("Error parsing file ...")`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParseError;

/// `libsvg_read_file`.
pub fn read(text: &str, selector: &Selector, curves: &dyn Curves) -> Result<Document, ParseError> {
    let mut r = Reader { shapes: Vec::new(), curves, selector, matches: 0 };
    let mut stack: Vec<usize> = Vec::new();
    let mut list: Vec<usize> = Vec::new();
    let mut defs: HashMap<String, usize> = HashMap::new();
    let mut entities: HashMap<String, String> = HashMap::new();
    let mut in_defs = false;
    let mut xml = quick_xml::Reader::from_str(text);
    xml.config_mut().check_end_names = true;
    // Well-formedness that libxml checks and quick-xml leaves to the
    // caller: exactly one root element, nothing but markup around it.
    let mut depth = 0usize;
    let mut roots = 0usize;
    loop {
        let ev = xml.read_event().map_err(|_| ParseError)?;
        match ev {
            Event::DocType(d) => parse_entities(&d.xml10_content(), &mut entities),
            Event::Start(ref e) | Event::Empty(ref e) => {
                let empty = matches!(ev, Event::Empty(_));
                if depth == 0 {
                    roots += 1;
                    if roots > 1 {
                        return Err(ParseError);
                    }
                }
                if !empty {
                    depth += 1;
                }
                let name = e.name().as_ref().to_string();
                if name == "defs" {
                    in_defs = true;
                }
                if KNOWN.contains(&name.as_str()) {
                    let attrs = attributes(e, &entities)?;
                    let idx = r.shapes.len();
                    r.shapes.push(Shape {
                        name: name.clone(),
                        parent: None,
                        children: Vec::new(),
                        id: None,
                        layer: None,
                        transform: String::new(),
                        stroke_width: String::new(),
                        stroke_linecap: String::new(),
                        stroke_linejoin: String::new(),
                        style: String::new(),
                        excluded: false,
                        selected: false,
                        paths: Vec::new(),
                        page: None,
                        href: String::new(),
                    });
                    if let Some(&top) = stack.last() {
                        r.shapes[top].children.push(idx);
                        r.shapes[idx].parent = Some(top);
                    }
                    r.set_attrs(idx, attrs);
                    if CONTAINERS.contains(&name.as_str()) {
                        stack.push(idx);
                    }
                    if name == "use" {
                        let href = r.shapes[idx].href.clone();
                        if let Some(id) = href.strip_prefix('#')
                            && let Some(&orig) = defs.get(id)
                        {
                            let mut clones = Vec::new();
                            r.clone_tree(orig, idx, &mut clones);
                            list.extend(clones);
                        }
                    }
                    if !in_defs {
                        list.push(idx);
                    } else if let Some(id) = r.shapes[idx].id.clone().filter(|i| !i.is_empty()) {
                        defs.entry(id).or_insert(idx);
                    }
                }
                if empty {
                    end_element(&name, &mut in_defs, &mut stack);
                }
            }
            Event::End(e) => {
                depth = depth.saturating_sub(1);
                let name = e.name().as_ref().to_string();
                end_element(&name, &mut in_defs, &mut stack);
            }
            Event::Text(t) if depth == 0 => {
                if !t.xml10_content().trim_matches(crate::text::is_space).is_empty() {
                    return Err(ParseError);
                }
            }
            Event::CData(_) | Event::GeneralRef(_) if depth == 0 => return Err(ParseError),
            Event::Eof => break,
            _ => {}
        }
    }
    if roots == 0 || depth != 0 {
        return Err(ParseError);
    }
    for &i in &list {
        r.apply_transform(i);
    }
    let items = list
        .iter()
        .map(|&i| Item { paths: r.shapes[i].paths.clone(), excluded: r.is_excluded(i), page: r.shapes[i].page })
        .collect();
    Ok(Document { items, matches: r.matches })
}

fn end_element(name: &str, in_defs: &mut bool, stack: &mut Vec<usize>) {
    if name == "defs" {
        *in_defs = false;
    }
    if CONTAINERS.contains(&name) {
        stack.pop();
    }
}

/// `<!ENTITY name "value">` declarations of the internal subset.
fn parse_entities(doctype: &str, out: &mut HashMap<String, String>) {
    let mut rest = doctype;
    while let Some(i) = rest.find("<!ENTITY") {
        rest = &rest[i + 8..];
        let t = rest.trim_start();
        if t.starts_with('%') {
            continue;
        }
        let name_end = t.find(crate::text::is_space).unwrap_or(t.len());
        let name = &t[..name_end];
        let v = t[name_end..].trim_start();
        let Some(q) = v.chars().next().filter(|c| *c == '"' || *c == '\'') else { continue };
        let Some(end) = v[1..].find(q) else { continue };
        out.entry(name.to_string()).or_insert_with(|| v[1..1 + end].to_string());
    }
}

/// `read_attributes`, by qualified name, with entities expanded and the
/// value normalised (tabs and newlines become spaces) as libxml does.
fn attributes(e: &BytesStart<'_>, entities: &HashMap<String, String>) -> Result<BTreeMap<String, String>, ParseError> {
    let mut out = BTreeMap::new();
    for a in e.attributes() {
        let a = a.map_err(|_| ParseError)?;
        let v = a
            .normalized_value_with(quick_xml::XmlVersion::Implicit1_0, 128, |name| {
                quick_xml::escape::resolve_predefined_entity(name).or_else(|| entities.get(name).map(String::as_str))
            })
            .map_err(|_| ParseError)?;
        out.insert(a.key.as_ref().to_string(), v.into_owned());
    }
    Ok(out)
}

/// `parse_double`: the whole string must be a number, else 0.
pub fn parse_double(s: &str) -> f64 {
    if s.is_empty() || s.starts_with(crate::text::is_space) || s.ends_with(crate::text::is_space) {
        return 0.0;
    }
    s.parse::<f64>().unwrap_or(0.0)
}

/// The longest prefix of `s` that Spirit's `qi::double_` accepts, and its
/// value.
fn double_prefix(s: &str) -> Option<(f64, usize)> {
    let b = s.as_bytes();
    let mut i = 0;
    if i < b.len() && (b[i] == b'+' || b[i] == b'-') {
        i += 1;
    }
    let lower = s[i..].to_ascii_lowercase();
    for word in ["infinity", "inf", "nan"] {
        if lower.starts_with(word) {
            let end = i + word.len();
            return s[..end].parse::<f64>().ok().map(|v| (v, end));
        }
    }
    let start = i;
    while i < b.len() && b[i].is_ascii_digit() {
        i += 1;
    }
    let int_digits = i - start;
    let mut frac_digits = 0;
    if i < b.len() && b[i] == b'.' {
        let j = i + 1;
        let mut k = j;
        while k < b.len() && b[k].is_ascii_digit() {
            k += 1;
        }
        frac_digits = k - j;
        if int_digits > 0 || frac_digits > 0 {
            i = k;
        }
    }
    if int_digits == 0 && frac_digits == 0 {
        return None;
    }
    if i < b.len() && (b[i] == b'e' || b[i] == b'E') {
        let mut k = i + 1;
        if k < b.len() && (b[k] == b'+' || b[k] == b'-') {
            k += 1;
        }
        let ds = k;
        while k < b.len() && b[k].is_ascii_digit() {
            k += 1;
        }
        if k > ds {
            i = k;
        }
    }
    s[..i].parse::<f64>().ok().map(|v| (v, i))
}

/// Skip the space skipper (`qi::space`).
fn skip_space(s: &str) -> &str {
    s.trim_start_matches(crate::text::is_space)
}

/// `parse_length`: a number and an optional unit, with space allowed
/// around and between them; anything else is `{0, UNDEFINED}`.
pub fn parse_length(value: &str) -> Length {
    let undefined = Length { number: 0.0, unit: Unit::Undefined };
    let s = skip_space(value);
    let Some((number, n)) = double_prefix(s) else { return undefined };
    let rest = skip_space(&s[n..]);
    let units = [("em", Unit::Em), ("ex", Unit::Ex), ("px", Unit::Px), ("in", Unit::In), ("cm", Unit::Cm), ("mm", Unit::Mm), ("pt", Unit::Pt), ("pc", Unit::Pc), ("%", Unit::Percent)];
    let (unit, rest) = match units.iter().find(|(u, _)| rest.starts_with(u)) {
        Some((u, unit)) => (*unit, &rest[u.len()..]),
        None => (Unit::None, rest),
    };
    if !skip_space(rest).is_empty() {
        return undefined;
    }
    Length { number, unit }
}

/// `parse_viewbox`: four numbers separated by space or one comma each, the
/// last two not negative.
pub fn parse_viewbox(value: &str) -> ViewBox {
    let invalid = ViewBox { x: 0.0, y: 0.0, width: 0.0, height: 0.0, valid: false };
    let mut s = skip_space(value);
    let mut v = [0.0; 4];
    for (k, slot) in v.iter_mut().enumerate() {
        if k > 0 {
            s = skip_space(s);
            if let Some(r) = s.strip_prefix(',') {
                s = skip_space(r);
            }
        }
        let Some((x, n)) = double_prefix(s) else { return invalid };
        *slot = x;
        s = &s[n..];
    }
    if !skip_space(s).is_empty() || v[2] < 0.0 || v[3] < 0.0 {
        return invalid;
    }
    ViewBox { x: v[0], y: v[1], width: v[2], height: v[3], valid: true }
}

/// `parse_alignment`: `[defer] <align> [meet|slice]`; the default (also
/// for anything unparsable) is `xMidYMid meet`.
pub fn parse_alignment(value: &str) -> Alignment {
    let default = Alignment { x: Align::Mid, y: Align::Mid, meet: true };
    let mut s = skip_space(value);
    if let Some(r) = s.strip_prefix("defer") {
        s = skip_space(r);
    }
    let aligns = [
        ("none", Align::None, Align::None),
        ("xMinYMin", Align::Min, Align::Min),
        ("xMidYMin", Align::Mid, Align::Min),
        ("xMaxYMin", Align::Max, Align::Min),
        ("xMinYMid", Align::Min, Align::Mid),
        ("xMidYMid", Align::Mid, Align::Mid),
        ("xMaxYMid", Align::Max, Align::Mid),
        ("xMinYMax", Align::Min, Align::Max),
        ("xMidYMax", Align::Mid, Align::Max),
        ("xMaxYMax", Align::Max, Align::Max),
    ];
    let Some((word, x, y)) = aligns.iter().find(|(w, _, _)| s.starts_with(w)) else { return default };
    s = skip_space(&s[word.len()..]);
    let mut meet = true;
    if let Some(r) = s.strip_prefix("meet") {
        s = r;
    } else if let Some(r) = s.strip_prefix("slice") {
        meet = false;
        s = r;
    }
    if !skip_space(s).is_empty() {
        return default;
    }
    Alignment { x: *x, y: *y, meet }
}

/// `boost::tokenizer<char_separator>` with dropped and kept delimiters.
fn tokenize<'s>(s: &'s str, dropped: &str, kept: &str) -> Vec<&'s str> {
    let mut out = Vec::new();
    let mut start = None;
    for (i, c) in s.char_indices() {
        let d = dropped.contains(c);
        let k = kept.contains(c);
        if d || k {
            if let Some(st) = start.take() {
                out.push(&s[st..i]);
            }
            if k {
                out.push(&s[i..i + c.len_utf8()]);
            }
        } else if start.is_none() {
            start = Some(i);
        }
    }
    if let Some(st) = start {
        out.push(&s[st..]);
    }
    out
}

impl Reader<'_> {
    fn select(&mut self, idx: usize) -> bool {
        let s = &self.shapes[idx];
        let hit = match self.selector {
            Selector::Root => s.parent.is_none(),
            Selector::Layer(l) => s.layer.as_deref() == Some(l.as_str()),
            Selector::Id { id, layer } => {
                let mut layer_match = true;
                if let Some(l) = layer {
                    layer_match = false;
                    let mut cur = idx;
                    while let Some(p) = self.shapes[cur].parent {
                        if self.shapes[cur].layer.as_deref() == Some(l.as_str()) {
                            layer_match = true;
                            break;
                        }
                        cur = p;
                    }
                }
                layer_match && s.id.as_deref() == Some(id.as_str())
            }
        };
        if hit {
            self.matches += 1;
        }
        hit
    }

    fn style(&self, idx: usize, name: &str) -> String {
        for style in self.shapes[idx].style.split(';') {
            let v: Vec<&str> = style.split(':').collect();
            if v.len() != 2 {
                continue;
            }
            if crate::text::trim(v[0]) == name {
                return crate::text::trim(v[1]).to_string();
            }
        }
        String::new()
    }

    fn stroke_width(&self, idx: usize) -> f64 {
        let s = &self.shapes[idx].stroke_width;
        let w = if s.is_empty() { parse_double(&self.style(idx, "stroke-width")) } else { parse_double(s) };
        if w < 0.01 { 1.0 } else { w }
    }

    fn linecap(&self, idx: usize) -> EndType {
        let s = &self.shapes[idx].stroke_linecap;
        let cap = if s.is_empty() { self.style(idx, "stroke-linecap") } else { s.clone() };
        match cap.as_str() {
            "round" => EndType::Round,
            "square" => EndType::Square,
            _ => EndType::Butt,
        }
    }

    fn linejoin(&self, idx: usize) -> JoinType {
        let s = &self.shapes[idx].stroke_linejoin;
        let join = if s.is_empty() { self.style(idx, "stroke-linejoin") } else { s.clone() };
        match join.as_str() {
            "bevel" => JoinType::Square,
            "round" => JoinType::Round,
            _ => JoinType::Miter,
        }
    }

    /// `shape::set_attrs` and each shape's own.
    fn set_attrs(&mut self, idx: usize, mut attrs: BTreeMap<String, String>) {
        let name = self.shapes[idx].name.clone();
        let get = |k: &str| attrs.get(k).cloned().unwrap_or_default();
        if name == "svg" {
            self.shapes[idx].page = Some(Page {
                width: parse_length(&get("width")),
                height: parse_length(&get("height")),
                viewbox: parse_viewbox(&get("viewBox")),
                alignment: parse_alignment(&get("preserveAspectRatio")),
            });
            self.shapes[idx].selected = self.select(idx);
            return;
        }
        self.base_attrs(idx, &attrs);
        let d = |k: &str| parse_double(&get(k));
        match name.as_str() {
            "circle" => {
                let r = d("r");
                let p = self.ellipse(d("cx"), d("cy"), r, r);
                self.shapes[idx].paths.push(p);
            }
            "ellipse" => {
                let p = self.ellipse(d("cx"), d("cy"), d("rx"), d("ry"));
                self.shapes[idx].paths.push(p);
            }
            "line" => {
                let path = vec![[d("x1"), d("y1")], [d("x2"), d("y2")]];
                self.stroke(idx, &path);
            }
            "polyline" | "polygon" => {
                let pts = get("points");
                let mut path = Vec::new();
                let mut x = 0.0;
                for (k, t) in tokenize(&pts, " ,", "").into_iter().enumerate() {
                    let p = parse_double(t);
                    if k % 2 == 0 {
                        x = p;
                    } else {
                        path.push([x, p]);
                    }
                }
                if name == "polyline" {
                    self.stroke(idx, &path);
                } else {
                    if let Some(&first) = path.first() {
                        path.push(first);
                    }
                    self.shapes[idx].paths.push(path);
                }
            }
            "rect" => {
                let (x, y, w, h) = (d("x"), d("y"), d("width"), d("height"));
                let (mut rx, mut ry) = (d("rx"), d("ry"));
                let has_rx = rx.abs() >= 1e-8;
                let has_ry = ry.abs() >= 1e-8;
                if has_rx || has_ry {
                    if !has_rx {
                        rx = ry;
                    } else if !has_ry {
                        ry = rx;
                    }
                    if rx > w / 2.0 {
                        rx = w / 2.0;
                    }
                    if ry > h / 2.0 {
                        ry = h / 2.0;
                    }
                    let g = fmt_g;
                    let path = format!(
                        "M {},{} H {} A {},{} 0 0,1 {},{} V {} A {},{} 0 0,1 {},{} H {} A {},{} 0 0,1 {},{} V {} A {},{} 0 0,1 {},{} z",
                        g(x + rx), g(y), g(x + w - rx), g(rx), g(ry), g(x + w), g(y + ry), g(y + h - ry),
                        g(rx), g(ry), g(x + w - rx), g(y + h), g(x + rx), g(rx), g(ry), g(x), g(y + h - ry),
                        g(y + ry), g(rx), g(ry), g(x + rx), g(y)
                    );
                    attrs.insert("d".into(), path);
                    // `path::set_attrs`, which runs `shape::set_attrs` again.
                    self.base_attrs(idx, &attrs);
                    self.path(idx, &attrs["d"]);
                } else {
                    self.shapes[idx].paths.push(vec![[x, y], [x + w, y], [x + w, y + h], [x, y + h], [x, y]]);
                }
            }
            "path" => {
                let data = get("d");
                self.path(idx, &data);
            }
            "use" => {
                let (x, y) = (d("x"), d("y"));
                let mut href = get("href");
                let xlink = get("xlink:href");
                if href.is_empty() && !xlink.is_empty() {
                    href = xlink;
                }
                self.shapes[idx].href = href;
                // "apply the x/y coordinates to all the children by using a
                // transform", printed with the stream's six digits.
                let t = format!("{} translate({},{})", self.shapes[idx].transform, fmt_g(x), fmt_g(y));
                self.shapes[idx].transform = t;
            }
            _ => {}
        }
    }

    /// `shape::set_attrs`.
    fn base_attrs(&mut self, idx: usize, attrs: &BTreeMap<String, String>) {
        let get = |k: &str| attrs.get(k).cloned().unwrap_or_default();
        {
            let s = &mut self.shapes[idx];
            if let Some(id) = attrs.get("id") {
                s.id = Some(id.clone());
            }
            s.transform = get("transform");
            s.stroke_width = get("stroke-width");
            s.stroke_linecap = get("stroke-linecap");
            s.stroke_linejoin = get("stroke-linejoin");
            s.style = get("style");
        }
        let mut display = self.style(idx, "display");
        if display.is_empty()
            && let Some(d) = attrs.get("display")
        {
            display = d.clone();
        }
        let s = &mut self.shapes[idx];
        if display == "none" {
            s.excluded = true;
        }
        if get("inkscape:groupmode") == "layer"
            && let Some(l) = attrs.get("inkscape:label")
        {
            s.layer = Some(l.clone());
        }
        let sel = self.select(idx);
        self.shapes[idx].selected = sel;
    }

    /// `draw_ellipse`: at least 40 points, starting one step past the top.
    fn ellipse(&self, x: f64, y: f64, rx: f64, ry: f64) -> Path {
        let rmax = rx.max(ry);
        let mut n = self.curves.circular_segments(rmax, 360.0).unwrap_or(3) as u64;
        if n < 40 {
            n = 40;
        }
        (1..=n)
            .map(|i| {
                let a = i as f64 * 360.0 / n as f64;
                [rx * sin_degrees(a) + x, ry * cos_degrees(a) + y]
            })
            .collect()
    }

    /// `offset_path`: the outline of a stroke, closed by repeating its first
    /// point.
    fn stroke(&mut self, idx: usize, path: &Path) {
        let out = offset_stroke(path, self.stroke_width(idx), self.linejoin(idx), self.linecap(idx));
        self.shapes[idx].paths.extend(out);
    }

    /// `path::set_attrs`: SVG path data.
    fn path(&mut self, idx: usize, data: &str) {
        const COMMANDS: &str = "-zmlcqahvstZMLCQAHVST";
        let mut tokens: Vec<String> = Vec::new();
        for t in tokenize(data, " ,", COMMANDS) {
            tokens.extend(split_dots(t));
        }
        let (mut x, mut y, mut xx) = (0.0f64, 0.0f64, 0.0f64);
        let mut yy: f64;
        let (mut rx, mut ry, mut cx1, mut cy1, mut cx2, mut cy2, mut angle) = (0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0);
        let (mut large, mut sweep) = (false, false);
        let (mut last_cubic, mut last_quad) = (false, false);
        let mut cmd = ' ';
        let mut point: i32 = 0;
        let mut negate = false;
        let mut closed = false;
        let mut pre_exp = String::new();
        // An open subpath is replaced by its stroke outline where it ends,
        // so the outlines keep the C++ order.
        let (width, join, cap) = (self.stroke_width(idx), self.linejoin(idx), self.linecap(idx));
        let mut list: Vec<Path> = vec![Vec::new()];
        for v in &tokens {
            let mut p = 0.0;
            if v.len() == 1 && COMMANDS.contains(v.as_str()) {
                if v == "-" {
                    negate = true;
                    continue;
                }
                point = -1;
                cmd = v.chars().next().unwrap_or(' ');
            } else {
                if v.chars().last().is_some_and(|c| c.eq_ignore_ascii_case(&'e')) {
                    pre_exp = if negate { format!("-{v}") } else { v.clone() };
                    negate = false;
                    continue;
                }
                if pre_exp.is_empty() {
                    p = parse_double(v);
                    if negate {
                        p = -p;
                    }
                } else {
                    let s = format!("{pre_exp}{}{v}", if negate { "-" } else { "" });
                    p = parse_double(&s);
                    pre_exp.clear();
                }
                negate = false;
            }
            let rel = cmd.is_ascii_lowercase();
            let back = list.last_mut().expect("path list");
            match cmd.to_ascii_lowercase() {
                'a' => match point {
                    0 => rx = p.abs(),
                    1 => ry = p.abs(),
                    2 => angle = p,
                    3 => large = p > 0.5,
                    4 => sweep = p > 0.5,
                    5 => xx = if rel { x + p } else { p },
                    6 => {
                        yy = if rel { y + p } else { p };
                        arc_to(back, x, y, rx, ry, xx, yy, angle, large, sweep, self.curves);
                        x = xx;
                        y = yy;
                        point = -1;
                        last_cubic = false;
                        last_quad = false;
                    }
                    _ => {}
                },
                'l' => match point {
                    0 => xx = if rel { x + p } else { p },
                    1 => {
                        yy = if rel { y + p } else { p };
                        back.push([xx, yy]);
                        x = xx;
                        y = yy;
                        point = -1;
                        last_cubic = false;
                        last_quad = false;
                    }
                    _ => {}
                },
                'c' => match point {
                    0 => cx1 = p,
                    1 => cy1 = p,
                    2 => cx2 = p,
                    3 => cy2 = p,
                    4 => xx = if rel { x + p } else { p },
                    5 => {
                        yy = if rel { y + p } else { p };
                        if rel {
                            cx1 += x;
                            cy1 += y;
                            cx2 += x;
                            cy2 += y;
                        }
                        cubic_to(back, x, y, cx1, cy1, cx2, cy2, xx, yy, self.curves);
                        x = xx;
                        y = yy;
                        point = -1;
                        last_cubic = true;
                        last_quad = false;
                    }
                    _ => {}
                },
                's' => match point {
                    0 => {
                        if last_cubic {
                            cx1 = x + (x - cx2);
                            cy1 = y + (y - cy2);
                        } else {
                            cx1 = x;
                            cy1 = y;
                        }
                        cx2 = p;
                    }
                    1 => cy2 = p,
                    2 => xx = if rel { x + p } else { p },
                    3 => {
                        yy = if rel { y + p } else { p };
                        if rel {
                            cx2 += x;
                            cy2 += y;
                        }
                        cubic_to(back, x, y, cx1, cy1, cx2, cy2, xx, yy, self.curves);
                        x = xx;
                        y = yy;
                        point = -1;
                        last_cubic = true;
                        last_quad = false;
                    }
                    _ => {}
                },
                'q' => match point {
                    0 => cx1 = p,
                    1 => cy1 = p,
                    2 => xx = if rel { x + p } else { p },
                    3 => {
                        yy = if rel { y + p } else { p };
                        if rel {
                            cx1 += x;
                            cy1 += y;
                        }
                        quad_to(back, x, y, cx1, cy1, xx, yy, self.curves);
                        x = xx;
                        y = yy;
                        point = -1;
                        last_cubic = false;
                        last_quad = true;
                    }
                    _ => {}
                },
                't' => match point {
                    0 => {
                        if last_quad {
                            cx1 = x + (x - cx1);
                            cy1 = y + (y - cy1);
                        } else {
                            cx1 = x;
                            cy1 = y;
                        }
                        xx = if rel { x + p } else { p };
                    }
                    1 => {
                        yy = if rel { y + p } else { p };
                        quad_to(back, x, y, cx1, cy1, xx, yy, self.curves);
                        x = xx;
                        y = yy;
                        point = -1;
                        last_cubic = false;
                        last_quad = true;
                    }
                    _ => {}
                },
                'm' => match point {
                    0 => xx = if rel { x + p } else { p },
                    1 => {
                        yy = if rel { y + p } else { p };
                        cmd = if rel { 'l' } else { 'L' };
                        if !back.is_empty() {
                            if is_open(back) {
                                let open = list.pop().expect("path list");
                                list.extend(offset_stroke(&open, width, join, cap));
                            }
                            list.push(Vec::new());
                        }
                        list.last_mut().expect("path list").push([xx, yy]);
                        x = xx;
                        y = yy;
                        point = -1;
                        last_cubic = false;
                        last_quad = false;
                    }
                    _ => {}
                },
                'v' => {
                    if point == 0 {
                        y = if rel { y + p } else { p };
                        back.push([x, y]);
                        point = -1;
                        last_cubic = false;
                        last_quad = false;
                    }
                }
                'h' => {
                    if point == 0 {
                        x = if rel { x + p } else { p };
                        back.push([x, y]);
                        point = -1;
                        last_cubic = false;
                        last_quad = false;
                    }
                }
                'z' => {
                    if let Some(&first) = back.first() {
                        back.push(first);
                        x = first[0];
                        y = first[1];
                    }
                    list.push(Vec::new());
                    closed = true;
                    last_cubic = false;
                    last_quad = false;
                }
                _ => {}
            }
            point += 1;
        }
        while list.last().is_some_and(Vec::is_empty) {
            list.pop();
        }
        if !closed
            && let Some(last) = list.last()
            && is_open(last)
        {
            let open = list.pop().expect("path list");
            list.extend(offset_stroke(&open, width, join, cap));
        }
        self.shapes[idx].paths.extend(list);
    }

    fn clone_tree(&mut self, src: usize, parent: usize, out: &mut Vec<usize>) {
        let mut copy = self.shapes[src].clone();
        copy.parent = Some(parent);
        let children = std::mem::take(&mut copy.children);
        let idx = self.shapes.len();
        self.shapes.push(copy);
        out.push(idx);
        for c in children {
            // `clone_children`: each clone, then its own clones.
            let before = out.len();
            self.clone_tree(c, idx, out);
            let child = out[before];
            self.shapes[idx].children.push(child);
        }
    }

    /// `is_excluded`: the nearest selected or excluded shape up the tree
    /// decides; with neither, the shape is excluded.
    fn is_excluded(&self, idx: usize) -> bool {
        let mut cur = Some(idx);
        while let Some(i) = cur {
            if self.shapes[i].selected {
                return false;
            }
            if self.shapes[i].excluded {
                return true;
            }
            cur = self.shapes[i].parent;
        }
        true
    }

    /// `apply_transform`: every ancestor's transform except the root's,
    /// outermost first.
    fn apply_transform(&mut self, idx: usize) {
        let mut matrices: Vec<Mat3> = Vec::new();
        let mut cur = idx;
        while let Some(p) = self.shapes[cur].parent {
            let mut m = transform_matrices(&self.shapes[cur].transform);
            m.append(&mut matrices);
            matrices = m;
            cur = p;
        }
        if matrices.is_empty() {
            return;
        }
        for path in &mut self.shapes[idx].paths {
            for v in path.iter_mut() {
                let mut r = [v[0], v[1], 1.0];
                for m in matrices.iter().rev() {
                    r = [
                        m[0][0] * r[0] + m[0][1] * r[1] + m[0][2] * r[2],
                        m[1][0] * r[0] + m[1][1] * r[1] + m[1][2] * r[2],
                        m[2][0] * r[0] + m[2][1] * r[1] + m[2][2] * r[2],
                    ];
                }
                *v = [r[0], r[1]];
            }
        }
    }
}

/// `split_dots`: "1.5.5" is two numbers, "1.5" and ".5".
fn split_dots(s: &str) -> Vec<String> {
    if s.matches('.').count() < 2 {
        return vec![s.to_string()];
    }
    let mut out = Vec::new();
    let mut text = String::new();
    let mut dot_seen = false;
    for t in tokenize(s, "", ".") {
        text.push_str(t);
        if t == "." {
            dot_seen = true;
            continue;
        } else if dot_seen {
            out.push(std::mem::take(&mut text));
        }
    }
    out
}

/// `is_open_path`: the ends are more than 0.1 apart.
fn is_open(path: &Path) -> bool {
    let (p1, p2) = (path[0], path[path.len() - 1]);
    ((p1[0] - p2[0]).powf(2.0) + (p1[1] - p2[1]).powf(2.0) + 0.0f64.powf(2.0)).powf(0.5) > 0.1
}

/// `path::arc_to`, after the SVG implementation notes (F.6.5).
#[allow(clippy::too_many_arguments)]
fn arc_to(path: &mut Path, x1: f64, y1: f64, mut rx: f64, mut ry: f64, x2: f64, y2: f64, angle: f64, large: bool, sweep: bool, curves: &dyn Curves) {
    let cos_rad = cos_degrees(angle);
    let sin_rad = sin_degrees(angle);
    let dx = (x1 - x2) / 2.0;
    let dy = (y1 - y2) / 2.0;
    let x1_ = cos_rad * dx + sin_rad * dy;
    let y1_ = -sin_rad * dx + cos_rad * dy;
    let d = (x1_ * x1_) / (rx * rx) + (y1_ * y1_) / (ry * ry);
    if d > 1.0 {
        rx = (d.sqrt() * rx).abs();
        ry = (d.sqrt() * ry).abs();
    }
    let mut t1 = rx * rx * ry * ry - rx * rx * y1_ * y1_ - ry * ry * x1_ * x1_;
    let t2 = rx * rx * y1_ * y1_ + ry * ry * x1_ * x1_;
    if t1 < 0.0 {
        t1 = 0.0;
    }
    let mut t3 = (t1 / t2).sqrt();
    if large == sweep {
        t3 = -t3;
    }
    let cx_ = t3 * rx * y1_ / ry;
    let cy_ = t3 * -ry * x1_ / rx;
    let cx = cos_rad * cx_ - sin_rad * cy_ + (x1 + x2) / 2.0;
    let cy = sin_rad * cx_ + cos_rad * cy_ + (y1 + y2) / 2.0;
    let ux = (x1_ - cx_) / rx;
    let uy = (y1_ - cy_) / ry;
    let vx = (-x1_ - cx_) / rx;
    let vy = (-y1_ - cy_) / ry;
    let theta = vector_angle(1.0, 0.0, ux, uy);
    let mut delta = vector_angle(ux, uy, vx, vy);
    if !sweep {
        delta -= 360.0;
    }
    let rmax = rx.max(ry);
    // `unsigned int fn = ...value_or(3)`, then the larger of that and
    // `(unsigned)(|delta| * 10 / 180 + 4)`.
    let fn_ = curves.circular_segments(rmax, delta).unwrap_or(3) as u32;
    let steps = fn_.max((delta.abs() * 10.0 / 180.0 + 4.0) as u32);
    for a in 0..=steps {
        let phi = theta + delta * f64::from(a) / f64::from(steps);
        let xx = cos_rad * cos_degrees(phi) * rx - sin_rad * sin_degrees(phi) * ry;
        let yy = sin_rad * cos_degrees(phi) * rx + cos_rad * sin_degrees(phi) * ry;
        path.push([xx + cx, yy + cy]);
    }
}

fn vector_angle(ux: f64, uy: f64, vx: f64, vy: f64) -> f64 {
    let mut a = atan2_degrees(vy, vx) - atan2_degrees(uy, ux);
    if a < 0.0 {
        a += 360.0;
    }
    a
}

/// `t(a, n)`: `std::pow(1 - a, n)`.
fn t(a: f64, n: i32) -> f64 {
    (1.0 - a).powf(f64::from(n))
}

/// Béziers get `max($fn, 20)` steps, the start point excluded.
fn bezier_steps(curves: &dyn Curves) -> i32 {
    curves.path_segments().max(20)
}

#[allow(clippy::too_many_arguments)]
fn quad_to(path: &mut Path, x: f64, y: f64, cx1: f64, cy1: f64, x2: f64, y2: f64, curves: &dyn Curves) {
    let n = bezier_steps(curves);
    for i in 1..=n {
        let a = f64::from(i) * (1.0 / f64::from(n));
        let xx = x * t(a, 2) + cx1 * 2.0 * t(a, 1) * a + x2 * a * a;
        let yy = y * t(a, 2) + cy1 * 2.0 * t(a, 1) * a + y2 * a * a;
        path.push([xx, yy]);
    }
}

#[allow(clippy::too_many_arguments)]
fn cubic_to(path: &mut Path, x: f64, y: f64, cx1: f64, cy1: f64, cx2: f64, cy2: f64, x2: f64, y2: f64, curves: &dyn Curves) {
    let n = bezier_steps(curves);
    for i in 1..=n {
        let a = f64::from(i) * (1.0 / f64::from(n));
        let xx = x * t(a, 3) + cx1 * 3.0 * t(a, 2) * a + cx2 * 3.0 * t(a, 1) * a * a + x2 * a * a * a;
        let yy = y * t(a, 3) + cy1 * 3.0 * t(a, 2) * a + cy2 * 3.0 * t(a, 1) * a * a + y2 * a * a * a;
        path.push([xx, yy]);
    }
}

/// `shape::offset_path`: Clipper's offset of an open path by half the
/// stroke width, at OpenSCAD's 2^27 scale, with Clipper's default miter
/// limit and arc tolerance; each result closed by repeating its first
/// point.
pub fn offset_stroke(path: &Path, width: f64, join: JoinType, cap: EndType) -> Vec<Path> {
    let scale = 2f64.powi(27);
    let line: Path64 = path.iter().map(|v| Point64::new((v[0] * scale).round() as i64, (v[1] * scale).round() as i64)).collect();
    let mut co = ClipperOffset::new_default();
    co.add_path(&line, join, cap);
    let mut result = Paths64::new();
    co.execute(width * scale / 2.0, &mut result);
    result
        .iter()
        .filter(|p| !p.is_empty())
        .map(|p| {
            let mut out: Path = p.iter().map(|q| [q.x as f64 / scale, q.y as f64 / scale]).collect();
            out.push([p[0].x as f64 / scale, p[0].y as f64 / scale]);
            out
        })
        .collect()
}

/// `collect_transform_matrices` for one shape's `transform` attribute.
fn transform_matrices(transform: &str) -> Vec<Mat3> {
    if transform.is_empty() {
        return Vec::new();
    }
    let s = transform
        .replace("matrix", "m")
        .replace("translate", "t")
        .replace("scale", "s")
        .replace("rotate", "r")
        .replace("skewX", "x")
        .replace("skewY", "y");
    let mut ops: Vec<(char, Vec<f64>)> = Vec::new();
    let mut cur: Option<(char, Vec<f64>)> = None;
    for tok in tokenize(&s, " ,()", "mtsrxy") {
        if tok.len() == 1 && "mtsrxy".contains(tok) {
            if let Some(c) = cur.take() {
                ops.push(c);
            }
            cur = Some((tok.chars().next().unwrap_or('m'), Vec::new()));
        } else if let Some((_, args)) = cur.as_mut() {
            args.push(parse_double(tok));
        }
    }
    if let Some(c) = cur {
        ops.push(c);
    }
    let mut out = Vec::new();
    for (op, a) in ops {
        // Invalid argument counts give no matrix (the C++ also prints a
        // note to stdout).
        match (op, a.len()) {
            ('m', 6) => out.push([[a[0], a[2], a[4]], [a[1], a[3], a[5]], [0.0, 0.0, 1.0]]),
            ('t', 1 | 2) => out.push([[1.0, 0.0, a[0]], [0.0, 1.0, a.get(1).copied().unwrap_or(0.0)], [0.0, 0.0, 1.0]]),
            ('s', 1 | 2) => out.push([[a[0], 0.0, 0.0], [0.0, a.get(1).copied().unwrap_or(a[0]), 0.0], [0.0, 0.0, 1.0]]),
            ('r', 1 | 3) => {
                let (c, s) = (cos_degrees(a[0]), sin_degrees(a[0]));
                let r = [[c, -s, 0.0], [s, c, 0.0], [0.0, 0.0, 1.0]];
                if a.len() == 3 {
                    out.push([[1.0, 0.0, a[1]], [0.0, 1.0, a[2]], [0.0, 0.0, 1.0]]);
                    out.push(r);
                    out.push([[1.0, 0.0, -a[1]], [0.0, 1.0, -a[2]], [0.0, 0.0, 1.0]]);
                } else {
                    out.push(r);
                }
            }
            ('x', 1) => out.push([[1.0, tan_degrees(a[0]), 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]]),
            ('y', 1) => out.push([[1.0, 0.0, 0.0], [tan_degrees(a[0]), 1.0, 0.0], [0.0, 0.0, 1.0]]),
            _ => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokenizer_and_dots() {
        assert_eq!(tokenize("M1,2-3 4", " ,", "-M"), vec!["M", "1", "2", "-", "3", "4"]);
        assert_eq!(split_dots("1.5.5"), vec!["1.5", ".5"]);
        assert_eq!(split_dots("-1.5"), vec!["-1.5"]);
    }

    #[test]
    fn lengths_viewboxes_alignment() {
        assert_eq!(parse_length(" 10 mm "), Length { number: 10.0, unit: Unit::Mm });
        assert_eq!(parse_length("10em"), Length { number: 10.0, unit: Unit::Em });
        assert_eq!(parse_length("50%"), Length { number: 50.0, unit: Unit::Percent });
        assert_eq!(parse_length("7"), Length { number: 7.0, unit: Unit::None });
        assert_eq!(parse_length("x").unit, Unit::Undefined);
        assert!(parse_viewbox("0 0 10,20").valid);
        assert!(!parse_viewbox("0 0 -1 2").valid);
        assert_eq!(parse_alignment("xMaxYMin slice"), Alignment { x: Align::Max, y: Align::Min, meet: false });
        assert_eq!(parse_alignment(""), Alignment { x: Align::Mid, y: Align::Mid, meet: true });
    }

    #[test]
    fn transforms_compose_in_document_order() {
        let m = transform_matrices("translate(10) scale(2)");
        assert_eq!(m.len(), 2);
        let m = transform_matrices("rotate(90, 1, 1)");
        assert_eq!(m.len(), 3);
        assert!(transform_matrices("matrix(1 2 3)").is_empty());
    }
}
