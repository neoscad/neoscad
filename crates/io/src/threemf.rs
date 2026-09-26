//! 3MF: the reader as OpenSCAD's lib3mf v2 path sees files
//! (`src/io/import_3mf_v2.cc`), and the writer (`export_3mf_v2.cc`) with
//! lib3mf's layout.
//!
//! Reading, as lib3mf and OpenSCAD do it (each point checked against the
//! 2026.09.23 nightly on hand-made files):
//! - vertices and transforms are `float`s, so coordinates are rounded to
//!   `f32` before anything else;
//! - every build item contributes the meshes it reaches through
//!   components; a component's matrix is applied *after* its parent's
//!   (`cm * m`, the reverse of the spec), and a component without a
//!   transform, or with an identity one (lib3mf's `HasTransform` is false
//!   for both), gets OpenSCAD's default `{..., {0, 0, 1}}`, which moves it
//!   up by 1;
//! - a triangle without properties takes the object's `pid`/`pindex`, a
//!   missing `p2`/`p3` repeats `p1`; base materials give their display
//!   colour with alpha forced to 255, colour groups the mean of the three
//!   vertex colours (none if any is `#00000000`);
//! - a mesh with no vertices or no triangles stops the whole import with
//!   "Empty mesh";
//! - several meshes are returned separately; OpenSCAD unions them with the
//!   backend, which is the caller's job here.
//!
//! lib3mf's own error texts for malformed files are only reproduced for the
//! cases seen on the nightly (missing file, not a ZIP, empty file); other
//! problems report a description in the same frame.

use std::collections::HashMap;
use std::io::{Cursor, Read, Write};

use quick_xml::NsReader;
use quick_xml::events::{BytesStart, Event};
use quick_xml::name::ResolveResult;
use sha2::{Digest, Sha256};

use crate::mesh::{Mesh, MeshRef};
use crate::text::parse_f64;
use crate::{Color, Message, Severity};

const CORE: &str = "http://schemas.microsoft.com/3dmanufacturing/core/2015/02";
const MATERIAL: &str = "http://schemas.microsoft.com/3dmanufacturing/material/2015/02";
const MODEL_REL: &str = "http://schemas.microsoft.com/3dmanufacturing/2013/01/3dmodel";

/// A 3x4 transform as lib3mf's `sTransform::m_Fields[4][3]`: rows 0-2 the
/// linear part, row 3 the translation, in `float`.
type Fields = [[f32; 3]; 4];

const IDENTITY: Fields = [
    [1.0, 0.0, 0.0],
    [0.0, 1.0, 0.0],
    [0.0, 0.0, 1.0],
    [0.0, 0.0, 0.0],
];

#[derive(Debug, Default)]
struct Tri {
    v: [u32; 3],
    pid: Option<u32>,
    p: [Option<u32>; 3],
}

#[derive(Debug, Default)]
struct Object {
    vertices: Vec<[f32; 3]>,
    tris: Vec<Tri>,
    components: Vec<(u32, Option<Fields>)>,
    is_components: bool,
    pid: Option<u32>,
    pindex: u32,
}

#[derive(Debug)]
enum Resource {
    Object(Object),
    /// Display colours of a `basematerials` group.
    Base(Vec<[u8; 4]>),
    /// Colours of a `colorgroup`.
    Colors(Vec<[u8; 4]>),
}

#[derive(Debug, Default)]
struct Model {
    resources: HashMap<u32, Resource>,
    items: Vec<(u32, Option<Fields>)>,
    title: Option<String>,
}

/// Read a 3MF file (`None` when it could not be opened). `line` is the
/// `import()` call's line, which OpenSCAD's messages quote.
pub fn read(bytes: Option<&[u8]>, file: &str, line: u32, msgs: &mut Vec<Message>) -> Vec<Mesh> {
    let fail = |msgs: &mut Vec<Message>, why: &str| {
        msgs.push(Message::warning(format!(
            "Could not read file '{file}', import() at line {line}: Error: GENERICEXCEPTION: {why}"
        )));
        Vec::new()
    };
    let Some(bytes) = bytes else {
        return fail(msgs, "The specified file could not be opened");
    };
    if bytes.is_empty() {
        return fail(msgs, "Could not get stream position");
    }
    let Ok(mut zip) = zip::ZipArchive::new(Cursor::new(bytes)) else {
        return fail(msgs, "Could not read ZIP file");
    };
    let model_path = root_model_path(&mut zip).unwrap_or_else(|| "3D/3dmodel.model".to_string());
    let Some(text) = zip_text(&mut zip, &model_path) else {
        return fail(msgs, "Could not find 3D model part");
    };
    let model = match parse_model(&text) {
        Ok(m) => m,
        Err(e) => return fail(msgs, &e),
    };
    if let Some(t) = &model.title {
        msgs.push(Message {
            severity: None,
            text: format!("Reading 3MF with title '{t}'"),
            located: false,
        });
    }
    let mut meshes = Vec::new();
    for &(id, transform) in &model.items {
        let m = get_matrix(&transform.unwrap_or(IDENTITY));
        let mut objects = Vec::new();
        if let Err(e) = collect(&model, id, m, &mut objects, 0) {
            return fail(msgs, &e);
        }
        for (obj, m) in objects {
            if obj.vertices.is_empty() || obj.tris.is_empty() {
                msgs.push(Message::warning(format!(
                    "Empty mesh, import() at line {line}"
                )));
                return Vec::new();
            }
            match to_mesh(&model, obj, &m) {
                Ok(mesh) => meshes.push(mesh),
                Err(e) => return fail(msgs, &e),
            }
        }
    }
    meshes
}

fn zip_text(zip: &mut zip::ZipArchive<Cursor<&[u8]>>, name: &str) -> Option<String> {
    let name = name.trim_start_matches('/');
    let index = (0..zip.len())
        .find(|&i| zip.name_for_index(i).is_some_and(|n| n == name))
        .or_else(|| {
            (0..zip.len()).find(|&i| {
                zip.name_for_index(i)
                    .is_some_and(|n| n.eq_ignore_ascii_case(name))
            })
        })?;
    let mut f = zip.by_index(index).ok()?;
    let mut s = String::new();
    f.read_to_string(&mut s).ok()?;
    Some(s)
}

/// The part `_rels/.rels` names as the 3D model.
fn root_model_path(zip: &mut zip::ZipArchive<Cursor<&[u8]>>) -> Option<String> {
    let rels = zip_text(zip, "_rels/.rels")?;
    let mut r = NsReader::from_str(&rels);
    loop {
        match r.read_event().ok()? {
            Event::Start(e) | Event::Empty(e) if e.local_name().as_ref() == "Relationship" => {
                if attr(&e, "Type").as_deref() == Some(MODEL_REL) {
                    return attr(&e, "Target");
                }
            }
            Event::Eof => return None,
            _ => {}
        }
    }
}

fn attr(e: &BytesStart<'_>, key: &str) -> Option<String> {
    e.attributes()
        .flatten()
        .find(|a| a.key.as_ref() == key)
        .and_then(|a| a.normalized_value(quick_xml::XmlVersion::Implicit1_0).ok())
        .map(|v| v.into_owned())
}

fn num_attr<T: std::str::FromStr>(
    e: &BytesStart<'_>,
    key: &str,
    what: &str,
) -> Result<Option<T>, String> {
    match attr(e, key) {
        None => Ok(None),
        Some(v) => v
            .trim()
            .parse::<T>()
            .map(Some)
            .map_err(|_| format!("Invalid {what} '{v}'")),
    }
}

fn parse_transform(s: &str) -> Result<Fields, String> {
    let v: Vec<f32> = s
        .split_ascii_whitespace()
        .map(|w| parse_f64(w).map(|x| x as f32))
        .collect::<Option<_>>()
        .ok_or("Invalid transform")?;
    if v.len() != 12 {
        return Err(format!("Invalid transform '{s}'"));
    }
    Ok([
        [v[0], v[1], v[2]],
        [v[3], v[4], v[5]],
        [v[6], v[7], v[8]],
        [v[9], v[10], v[11]],
    ])
}

/// `#RRGGBB` or `#RRGGBBAA`.
fn parse_color(s: &str) -> Result<[u8; 4], String> {
    let h = s
        .trim()
        .strip_prefix('#')
        .filter(|h| (h.len() == 6 || h.len() == 8) && h.bytes().all(|b| b.is_ascii_hexdigit()));
    let Some(h) = h else {
        return Err(format!("Invalid color '{s}'"));
    };
    let byte = |i: usize| u8::from_str_radix(&h[i..i + 2], 16).unwrap_or(0);
    Ok([
        byte(0),
        byte(2),
        byte(4),
        if h.len() == 8 { byte(6) } else { 255 },
    ])
}

fn parse_model(text: &str) -> Result<Model, String> {
    let mut r = NsReader::from_str(text);
    let mut model = Model::default();
    let mut object: Option<(u32, Object)> = None;
    let mut group: Option<(u32, bool, Vec<[u8; 4]>)> = None;
    let mut in_build = false;
    let mut title_meta = false;
    loop {
        let (ns, ev) = r.read_resolved_event().map_err(|e| e.to_string())?;
        let ns: Option<String> = match ns {
            ResolveResult::Bound(n) => Some(n.as_ref().to_string()),
            _ => None,
        };
        let core = ns.as_deref() == Some(CORE);
        let material = ns.as_deref() == Some(MATERIAL);
        match ev {
            Event::Start(ref e) | Event::Empty(ref e) => {
                let empty = matches!(ev, Event::Empty(_));
                let name = e.local_name();
                match (core, material, name.as_ref()) {
                    (true, _, "object") => {
                        let id =
                            num_attr::<u32>(e, "id", "resource id")?.ok_or("Missing object id")?;
                        let o = Object {
                            pid: num_attr(e, "pid", "property id")?,
                            pindex: num_attr(e, "pindex", "property index")?.unwrap_or(0),
                            ..Default::default()
                        };
                        if empty {
                            model.resources.insert(id, Resource::Object(o));
                        } else {
                            object = Some((id, o));
                        }
                    }
                    (true, _, "vertex") => {
                        if let Some((_, o)) = object.as_mut() {
                            let c = |k: &str| -> Result<f32, String> {
                                let v = attr(e, k).ok_or("Missing vertex coordinate")?;
                                parse_f64(v.trim())
                                    .map(|x| x as f32)
                                    .ok_or(format!("Invalid vertex coordinate '{v}'"))
                            };
                            o.vertices.push([c("x")?, c("y")?, c("z")?]);
                        }
                    }
                    (true, _, "triangle") => {
                        if let Some((_, o)) = object.as_mut() {
                            let v = |k: &str| -> Result<u32, String> {
                                num_attr::<u32>(e, k, "vertex index")?
                                    .ok_or("Missing vertex index".into())
                            };
                            o.tris.push(Tri {
                                v: [v("v1")?, v("v2")?, v("v3")?],
                                pid: num_attr(e, "pid", "property id")?,
                                p: [
                                    num_attr(e, "p1", "property index")?,
                                    num_attr(e, "p2", "property index")?,
                                    num_attr(e, "p3", "property index")?,
                                ],
                            });
                        }
                    }
                    (true, _, "components") => {
                        if let Some((_, o)) = object.as_mut() {
                            o.is_components = true;
                        }
                    }
                    (true, _, "component") => {
                        if let Some((_, o)) = object.as_mut() {
                            let id = num_attr::<u32>(e, "objectid", "object id")?
                                .ok_or("Missing component object id")?;
                            let t = attr(e, "transform")
                                .map(|t| parse_transform(&t))
                                .transpose()?;
                            o.components.push((id, t));
                        }
                    }
                    (true, _, "basematerials") | (_, true, "colorgroup") => {
                        let id = num_attr::<u32>(e, "id", "resource id")?
                            .ok_or("Missing resource id")?;
                        let is_base = name.as_ref() == "basematerials";
                        if empty {
                            model.resources.insert(
                                id,
                                if is_base {
                                    Resource::Base(Vec::new())
                                } else {
                                    Resource::Colors(Vec::new())
                                },
                            );
                        } else {
                            group = Some((id, is_base, Vec::new()));
                        }
                    }
                    (true, _, "base") => {
                        if let Some((_, true, g)) = group.as_mut() {
                            let c = attr(e, "displaycolor").ok_or("Missing display color")?;
                            let mut c = parse_color(&c)?;
                            c[3] = 255;
                            g.push(c);
                        }
                    }
                    (_, true, "color") => {
                        if let Some((_, false, g)) = group.as_mut() {
                            let c = attr(e, "color").ok_or("Missing color")?;
                            g.push(parse_color(&c)?);
                        }
                    }
                    (true, _, "build") => in_build = !empty,
                    (true, _, "item") if in_build => {
                        let id = num_attr::<u32>(e, "objectid", "object id")?
                            .ok_or("Missing build item object id")?;
                        let t = attr(e, "transform")
                            .map(|t| parse_transform(&t))
                            .transpose()?;
                        model.items.push((id, t));
                    }
                    (true, _, "metadata") => {
                        title_meta = !empty && attr(e, "name").as_deref() == Some("Title")
                    }
                    _ => {}
                }
            }
            Event::Text(t) if title_meta => {
                let s = t.xml10_content().into_owned();
                model.title.get_or_insert_with(String::new).push_str(&s);
            }
            Event::GeneralRef(g) if title_meta => {
                let s = match g.resolve_char_ref() {
                    Ok(Some(c)) => c.to_string(),
                    _ => quick_xml::escape::resolve_predefined_entity(&g)
                        .unwrap_or("")
                        .to_string(),
                };
                model.title.get_or_insert_with(String::new).push_str(&s);
            }
            Event::End(ref e) => match (core, material, e.local_name().as_ref()) {
                (true, _, "object") => {
                    if let Some((id, o)) = object.take() {
                        model.resources.insert(id, Resource::Object(o));
                    }
                }
                (true, _, "basematerials") | (_, true, "colorgroup") => {
                    if let Some((id, is_base, g)) = group.take() {
                        model.resources.insert(
                            id,
                            if is_base {
                                Resource::Base(g)
                            } else {
                                Resource::Colors(g)
                            },
                        );
                    }
                }
                (true, _, "build") => in_build = false,
                (true, _, "metadata") => title_meta = false,
                _ => {}
            },
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(model)
}

/// `get_matrix`: lib3mf's row-vector fields as an Eigen column-vector
/// matrix (row `r` is `F[0][r], F[1][r], F[2][r], F[3][r]`).
fn get_matrix(t: &Fields) -> [[f64; 4]; 4] {
    let f = |i: usize, j: usize| f64::from(t[i][j]);
    [
        [f(0, 0), f(1, 0), f(2, 0), f(3, 0)],
        [f(0, 1), f(1, 1), f(2, 1), f(3, 1)],
        [f(0, 2), f(1, 2), f(2, 2), f(3, 2)],
        [0.0, 0.0, 0.0, 1.0],
    ]
}

fn mul(a: &[[f64; 4]; 4], b: &[[f64; 4]; 4]) -> [[f64; 4]; 4] {
    std::array::from_fn(|i| std::array::from_fn(|j| (0..4).map(|k| a[i][k] * b[k][j]).sum()))
}

/// `collect_mesh_objects`. Errors are problems lib3mf would have reported
/// while reading (a missing or recursive reference).
fn collect<'m>(
    model: &'m Model,
    id: u32,
    m: [[f64; 4]; 4],
    out: &mut Vec<(&'m Object, [[f64; 4]; 4])>,
    depth: u32,
) -> Result<(), String> {
    let Some(Resource::Object(o)) = model.resources.get(&id) else {
        return Err(format!("Could not find object resource {id}"));
    };
    if depth > 64 {
        return Err("Recursive components".into());
    }
    if !o.is_components {
        out.push((o, m));
        return Ok(());
    }
    for (child, t) in &o.components {
        // `HasTransform()` is false for an identity matrix, and OpenSCAD's
        // stand-in then has {0, 0, 1} as its translation row.
        let t = match t {
            Some(t) if *t != IDENTITY => *t,
            _ => [
                [1.0, 0.0, 0.0],
                [0.0, 1.0, 0.0],
                [0.0, 0.0, 1.0],
                [0.0, 0.0, 1.0],
            ],
        };
        collect(model, *child, mul(&get_matrix(&t), &m), out, depth + 1)?;
    }
    Ok(())
}

/// `import_3mf_mesh`: transformed vertices, triangles as they are, and
/// per-triangle colours.
fn to_mesh(model: &Model, o: &Object, m: &[[f64; 4]; 4]) -> Result<Mesh, String> {
    let mut mesh = Mesh::default();
    for v in &o.vertices {
        let p = [f64::from(v[0]), f64::from(v[1]), f64::from(v[2]), 1.0];
        let row = |r: usize| (0..4).map(|k| m[r][k] * p[k]).sum::<f64>();
        mesh.vertices.push([row(0), row(1), row(2)]);
    }
    let mut index: HashMap<[u32; 4], i32> = HashMap::new();
    for t in &o.tris {
        if t.v.iter().any(|&i| i as usize >= o.vertices.len()) {
            return Err("Invalid vertex index".into());
        }
        mesh.faces.push(t.v.to_vec());
        let c = triangle_color(model, o, t);
        match c {
            Some(c) => {
                let next = mesh.colors.len() as i32;
                let ci = *index.entry(c.0.map(f32::to_bits)).or_insert(next);
                if ci == next {
                    mesh.colors.push(c);
                }
                mesh.color_indices.push(ci);
            }
            None => mesh.color_indices.push(-1),
        }
    }
    if mesh.colors.is_empty() {
        mesh.color_indices.clear();
    }
    Ok(mesh)
}

fn triangle_color(model: &Model, o: &Object, t: &Tri) -> Option<Color> {
    let (pid, p) = match (t.pid, t.p[0]) {
        (Some(pid), p1) => (
            pid,
            [
                p1.unwrap_or(o.pindex),
                t.p[1].or(p1).unwrap_or(o.pindex),
                t.p[2].or(p1).unwrap_or(o.pindex),
            ],
        ),
        (None, Some(p1)) => (o.pid?, [p1, t.p[1].unwrap_or(p1), t.p[2].unwrap_or(p1)]),
        (None, None) => (o.pid?, [o.pindex; 3]),
    };
    let to_color =
        |c: &[u8; 4]| Color::from_ints(c[0].into(), c[1].into(), c[2].into(), c[3].into());
    match model.resources.get(&pid)? {
        Resource::Base(b) => b.get(p[0] as usize).map(to_color),
        Resource::Colors(g) => {
            let cs: Vec<Color> = p
                .iter()
                .map(|&i| g.get(i as usize).filter(|c| **c != [0; 4]).map(to_color))
                .collect::<Option<_>>()?;
            let mean = |k: usize| ((cs[0].0[k] + cs[1].0[k] + cs[2].0[k]) / 3.0).clamp(0.0, 1.0);
            Some(Color([mean(0), mean(1), mean(2), mean(3)]))
        }
        Resource::Object(_) => None,
    }
}

/// What a 3MF file records besides the meshes.
#[derive(Debug, Clone)]
pub struct WriteOptions<'a> {
    /// The input file's name, stored as the `Title` metadata.
    pub title: &'a str,
    /// ISO 8601 UTC time for `CreationDate`, e.g. `2026-09-26T07:51:02Z`.
    pub creation_date: &'a str,
    /// The colour scheme's face colour: the "Default" base material, which
    /// faces without a colour of their own (and faces of that colour) use.
    pub default_color: Color,
}

/// lib3mf's decimal output at `precision` places: the coordinate as a
/// `float`, times 10^precision in `float`, truncated to an integer, printed
/// with that many decimals, and `0` for zero. Large values show the `float`
/// rounding of the product (123456.789 prints as `123456.790528` at 6), as
/// lib3mf's do; at 16 places even `1` shows it (`1.0000000272564224`,
/// checked against the nightly), because 10^16 is not a `float`.
fn lib3mf_float(v: f64, precision: u32) -> String {
    // The scale as the nearest `float`, not a product of rounded powers.
    let scale: f32 = format!("1e{precision}").parse().unwrap_or(1.0e6);
    let n = ((v as f32) * scale) as i64;
    if n == 0 {
        return "0".into();
    }
    let sign = if n < 0 { "-" } else { "" };
    let a = n.unsigned_abs();
    let unit = 10u64.pow(precision);
    format!(
        "{sign}{}.{:0width$}",
        a / unit,
        a % unit,
        width = precision as usize
    )
}

fn xml_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            c => out.push(c),
        }
    }
    out
}

/// A version-4 UUID drawn from a hash, so the same model always gets the
/// same identifiers (lib3mf draws random ones).
fn uuid(seed: &[u8], n: u8) -> String {
    let mut h = Sha256::new();
    h.update(seed);
    h.update([n]);
    let mut b: [u8; 16] = h.finalize()[..16].try_into().expect("16 bytes");
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let x: String = b.iter().map(|v| format!("{v:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &x[..8],
        &x[8..12],
        &x[12..16],
        &x[16..20],
        &x[20..]
    )
}

fn hex_color(c: [i32; 4]) -> String {
    format!("#{:02X}{:02X}{:02X}{:02X}", c[0], c[1], c[2], c[3])
}

/// `export-3mf/color-mode`: which colours the file carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ColorMode {
    /// Face colours from the model, over the default colour.
    #[default]
    Model,
    /// No colour or material resources at all.
    None,
    /// Only [`Options::color`], for the whole object.
    SelectedOnly,
}

/// `export-3mf/material-type`: the resource the colours go into.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MaterialType {
    /// A `<m:colorgroup>`.
    Color,
    /// A `<basematerials>` group (lib3mf's default for OpenSCAD).
    #[default]
    BaseMaterial,
}

/// `export-3mf/unit`, as lib3mf writes the model's `unit` attribute.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Unit {
    Micron,
    #[default]
    Millimeter,
    Centimeter,
    Meter,
    Inch,
    Foot,
}

impl Unit {
    fn attr(self) -> &'static str {
        match self {
            Unit::Micron => "micron",
            Unit::Millimeter => "millimeter",
            Unit::Centimeter => "centimeter",
            Unit::Meter => "meter",
            Unit::Inch => "inch",
            Unit::Foot => "foot",
        }
    }
}

/// The `-O export-3mf/...` settings (`Export3mfOptions`, `io/export.h`),
/// already decoded; [`Options::default`] is OpenSCAD's defaults.
#[derive(Debug, Clone, PartialEq)]
pub struct Options {
    pub color_mode: ColorMode,
    /// The colour of `selected-only` mode, already resolved by the caller
    /// (`OpenSCAD::getColor(color, defaultColor)`, which prints its own
    /// warning for a name it cannot parse). Unused in the other modes.
    pub color: Option<Color>,
    pub material_type: MaterialType,
    pub unit: Unit,
    /// Passed to lib3mf as given: outside 1..=16 lib3mf refuses it, the
    /// export says so and keeps lib3mf's default of 6.
    pub decimal_precision: i32,
    pub add_meta_data: bool,
    /// Replaces the file-name `Title` when not empty.
    pub meta_data_title: String,
    pub meta_data_designer: String,
    pub meta_data_description: String,
    pub meta_data_copyright: String,
    pub meta_data_license_terms: String,
    pub meta_data_rating: String,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            color_mode: ColorMode::Model,
            color: None,
            material_type: MaterialType::BaseMaterial,
            unit: Unit::Millimeter,
            decimal_precision: 6,
            add_meta_data: true,
            meta_data_title: String::new(),
            meta_data_designer: String::new(),
            meta_data_description: String::new(),
            meta_data_copyright: String::new(),
            meta_data_license_terms: String::new(),
            meta_data_rating: String::new(),
        }
    }
}

/// `export_3mf` with OpenSCAD's default options (colour mode "model",
/// base materials, millimetres, precision 6, metadata on) for one
/// triangulated mesh. Returns the file and the messages OpenSCAD would
/// print; an empty file means the export failed.
pub fn write(mesh: MeshRef<'_>, opts: &WriteOptions<'_>) -> (Vec<u8>, Vec<Message>) {
    write_with(mesh, opts, &Options::default())
}

/// `export_3mf` with the `-O export-3mf/...` settings in `o`, for one
/// triangulated mesh. Each branch below was checked against the files the
/// 2026.09.23 nightly writes for the same settings.
pub fn write_with(
    mesh: MeshRef<'_>,
    opts: &WriteOptions<'_>,
    o: &Options,
) -> (Vec<u8>, Vec<Message>) {
    let mut msgs = Vec::new();
    let invalid = || Message::warning("Invalid color in 3MF export");
    let export_error = |t: String| Message {
        severity: Some(Severity::Error),
        text: format!("EXPORT-ERROR: {t}"),
        located: false,
    };
    // `NMR_MESH_MAXCOORDINATE`: lib3mf refuses a vertex beyond 1e9 mm.
    if mesh
        .vertices
        .iter()
        .flatten()
        .any(|c| (c.abs() as f32) > 1.0e9)
    {
        msgs.push(export_error(
            "Error: GENERICEXCEPTION: The coordinates exceed NMR_MESH_MAXCOORDINATE (= 1 billion mm)"
                .into(),
        ));
        msgs.push(export_error("Can't add vertex to 3MF model.".into()));
        return (Vec::new(), msgs);
    }

    // The colour resource: none, base materials or a colour group. Its
    // entries are (name, RGBA); a colour group's names are unused.
    let group: Option<MaterialType> = (o.color_mode != ColorMode::None).then_some(o.material_type);
    let color = match o.color_mode {
        ColorMode::SelectedOnly => o.color.unwrap_or(opts.default_color),
        _ => opts.default_color,
    };
    let mut entries: Vec<(String, [i32; 4])> = Vec::new();
    let mut by_color: HashMap<[u32; 4], usize> = HashMap::new();
    if let Some(kind) = group {
        let c = color.rgba_int().unwrap_or_else(|| {
            msgs.push(invalid());
            [0, 0, 0, 0]
        });
        match kind {
            // "Default", alpha forced opaque. The map starts with the colour
            // itself, so faces of exactly that colour use "Default".
            MaterialType::BaseMaterial => {
                entries.push(("Default".into(), [c[0], c[1], c[2], 255]));
                by_color.insert(color.0.map(f32::to_bits), 0);
            }
            // The colour as it is, and not in the map: a face of the same
            // colour adds a second entry, as the nightly's files show.
            MaterialType::Color => entries.push((String::new(), c)),
        }
    }
    // Per-face properties: only in "model" mode, one entry per new colour
    // in face order; faces without a colour keep the object's.
    let mut tri_prop: Vec<usize> = vec![0; mesh.faces.len()];
    if group.is_some() && o.color_mode == ColorMode::Model {
        for (i, prop) in tri_prop.iter_mut().enumerate() {
            if let Some(c) = mesh.face_color(i) {
                *prop = *by_color.entry(c.0.map(f32::to_bits)).or_insert_with(|| {
                    let rgba = c.rgba_int().unwrap_or_else(|| {
                        msgs.push(invalid());
                        [0, 0, 0, 0]
                    });
                    entries.push((format!("Color {}", entries.len()), rgba));
                    entries.len() - 1
                });
            }
        }
    }

    // `SetDecimalPrecision` rejects anything outside 1..=16; the writer then
    // keeps its default.
    let precision = match u32::try_from(o.decimal_precision) {
        Ok(p @ 1..=16) => p,
        _ => {
            msgs.push(export_error(
                "Error setting decimal precision for export: Error: GENERICEXCEPTION: The call parameter to the function was invalid"
                    .into(),
            ));
            6
        }
    };

    let mut body = String::with_capacity(mesh.vertices.len() * 64 + mesh.faces.len() * 64);
    match group {
        Some(MaterialType::BaseMaterial) => {
            body.push_str("\t\t<basematerials id=\"1\">\n");
            for (name, c) in &entries {
                body.push_str(&format!(
                    "\t\t\t<base name=\"{}\" displaycolor=\"{}\"/>\n",
                    xml_escape(name),
                    hex_color(*c)
                ));
            }
            body.push_str("\t\t</basematerials>\n");
        }
        Some(MaterialType::Color) => {
            body.push_str("\t\t<m:colorgroup id=\"1\">\n");
            for (_, c) in &entries {
                body.push_str(&format!("\t\t\t<m:color color=\"{}\"/>\n", hex_color(*c)));
            }
            body.push_str("\t\t</m:colorgroup>\n");
        }
        None => {}
    }
    let mut geometry = String::with_capacity(body.capacity());
    geometry.push_str("\t\t\t<mesh>\n\t\t\t\t<vertices>\n");
    for v in mesh.vertices {
        geometry.push_str(&format!(
            "\t\t\t\t\t<vertex x=\"{}\" y=\"{}\" z=\"{}\" />\n",
            lib3mf_float(v[0], precision),
            lib3mf_float(v[1], precision),
            lib3mf_float(v[2], precision)
        ));
    }
    geometry.push_str("\t\t\t\t</vertices>\n\t\t\t\t<triangles>\n");
    for (f, &prop) in mesh.faces.iter().zip(&tri_prop) {
        geometry.push_str(&format!(
            "\t\t\t\t\t<triangle v1=\"{}\" v2=\"{}\" v3=\"{}\"",
            f[0], f[1], f[2]
        ));
        // lib3mf leaves out properties equal to the object's default.
        if prop != 0 {
            geometry.push_str(&format!(" pid=\"1\" p1=\"{prop}\""));
        }
        geometry.push_str(" />\n");
    }
    geometry.push_str("\t\t\t\t</triangles>\n\t\t\t</mesh>\n");
    let seed = [body.as_bytes(), geometry.as_bytes()].concat();
    let mut xml = String::with_capacity(body.len() + geometry.len() + 2048);
    xml.push_str("<?xml version=\"1.0\" encoding=\"utf-8\"?>\n");
    xml.push_str(&format!("<model xmlns=\"http://schemas.microsoft.com/3dmanufacturing/core/2015/02\" unit=\"{}\" xml:lang=\"en-US\" xmlns:m=\"http://schemas.microsoft.com/3dmanufacturing/material/2015/02\" xmlns:p=\"http://schemas.microsoft.com/3dmanufacturing/production/2015/06\" xmlns:b=\"http://schemas.microsoft.com/3dmanufacturing/beamlattice/2017/02\" xmlns:s=\"http://schemas.microsoft.com/3dmanufacturing/slice/2015/07\" xmlns:t=\"http://schemas.microsoft.com/3dmanufacturing/trianglesets/2021/07\" xmlns:sc=\"http://schemas.microsoft.com/3dmanufacturing/securecontent/2019/04\" xmlns:v=\"http://schemas.3mf.io/3dmanufacturing/volumetric/2022/01\" xmlns:i=\"http://schemas.3mf.io/3dmanufacturing/implicit/2023/12\">\n", o.unit.attr()));
    if o.add_meta_data {
        // `add_meta_data`: the setting, else the fallback; empty is skipped.
        let title = if o.meta_data_title.is_empty() {
            opts.title
        } else {
            &o.meta_data_title
        };
        for (name, value) in [
            ("Title", title),
            ("Application", "OpenSCAD (https://www.openscad.org/)"),
            ("CreationDate", opts.creation_date),
            ("Designer", &o.meta_data_designer),
            ("Description", &o.meta_data_description),
            ("Copyright", &o.meta_data_copyright),
            ("LicenseTerms", &o.meta_data_license_terms),
            ("Rating", &o.meta_data_rating),
        ] {
            if !value.is_empty() {
                xml.push_str(&format!(
                    "\t<metadata name=\"{name}\" preserve=\"1\">{}</metadata>\n",
                    xml_escape(value)
                ));
            }
        }
    }
    xml.push_str("\t<resources>\n");
    xml.push_str(&body);
    // lib3mf numbers resources in creation order: the colour resource, if
    // any, is 1 and the object follows it.
    let (object_id, props) = if group.is_some() {
        (2, " pid=\"1\" pindex=\"0\"")
    } else {
        (1, "")
    };
    xml.push_str(&format!(
        "\t\t<object id=\"{object_id}\" name=\"OpenSCAD Model\" type=\"model\" p:UUID=\"{}\"{props}>\n",
        uuid(&seed, 0)
    ));
    xml.push_str(&geometry);
    xml.push_str("\t\t</object>\n\t</resources>\n");
    xml.push_str(&format!(
        "\t<build p:UUID=\"{}\">\n\t\t<item objectid=\"{object_id}\" p:UUID=\"{}\"/>\n\t</build>\n</model>\n",
        uuid(&seed, 1),
        uuid(&seed, 2)
    ));
    (package(&xml), msgs)
}

const CONTENT_TYPES: &str = "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\">\n\t<Default Extension=\"jpeg\" ContentType=\"image/jpeg\"/>\n\t<Default Extension=\"jpg\" ContentType=\"image/jpeg\"/>\n\t<Default Extension=\"model\" ContentType=\"application/vnd.ms-package.3dmanufacturing-3dmodel+xml\"/>\n\t<Default Extension=\"png\" ContentType=\"image/png\"/>\n\t<Default Extension=\"rels\" ContentType=\"application/vnd.openxmlformats-package.relationships+xml\"/>\n\t<Default Extension=\"texture\" ContentType=\"application/vnd.ms-package.3dmanufacturing-3dmodeltexture\"/>\n</Types>\n";

const RELS: &str = "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">\n\t<Relationship Type=\"http://schemas.microsoft.com/3dmanufacturing/2013/01/3dmodel\" Target=\"/3D/3dmodel.model\" Id=\"rel0\"/>\n</Relationships>\n";

/// The OPC package, with lib3mf's part order.
fn package(model: &str) -> Vec<u8> {
    let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let opts = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    for (name, text) in [
        ("3D/3dmodel.model", model),
        ("[Content_Types].xml", CONTENT_TYPES),
        ("_rels/.rels", RELS),
    ] {
        // Writing to memory cannot fail.
        zip.start_file(name, opts).expect("zip entry");
        zip.write_all(text.as_bytes()).expect("zip write");
    }
    zip.finish().expect("zip finish").into_inner()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn floats_print_like_lib3mf() {
        // Values checked against 3MF files the nightly wrote.
        for (v, s) in [
            (0.0, "0"),
            (1.0, "1.000000"),
            (0.1234567, "0.123456"),
            (123456.789, "123456.790528"),
            (-99999999.5, "-100000000.376832"),
            (1e-7, "0"),
            (-4e-7, "0"),
            (0.0000019, "0.000001"),
            (-0.0000019, "-0.000001"),
            (1234.5678, "1234.567808"),
        ] {
            assert_eq!(lib3mf_float(v, 6), s, "{v}");
        }
    }

    #[test]
    fn other_precisions_print_like_lib3mf() {
        // `cube(1/3)` and `cube(1)` exported by the 2026.09.23 nightly with
        // `-O export-3mf/decimal-precision=N`.
        let third = 1.0 / 3.0;
        for (p, s) in [
            (1, "0.3"),
            (2, "0.33"),
            (7, "0.3333333"),
            (10, "0.3333333504"),
            (16, "0.3333333513666560"),
        ] {
            assert_eq!(lib3mf_float(third, p), s, "{p}");
        }
        assert_eq!(lib3mf_float(1.0, 1), "1.0");
        assert_eq!(lib3mf_float(1.0, 3), "1.000");
        assert_eq!(lib3mf_float(1.0, 16), "1.0000000272564224");
    }

    /// The model part of a written file.
    fn model_xml(bytes: &[u8]) -> String {
        let mut zip = zip::ZipArchive::new(Cursor::new(bytes)).unwrap();
        zip_text(&mut zip, "3D/3dmodel.model").unwrap()
    }

    #[test]
    fn export_options_shape_the_file_as_the_nightly_does() {
        let red = Color::from_u8(255, 0, 0);
        let front = Color::from_u8(0xf9, 0xd7, 0x2c);
        let m = tet(vec![red, front], vec![0, 1, -1, 0]);
        let opts = WriteOptions {
            title: "t.scad",
            creation_date: "2026-09-26T00:00:00Z",
            default_color: front,
        };
        let write = |o: &Options| {
            let (bytes, msgs) = write_with(m.as_ref(), &opts, o);
            (model_xml(&bytes), msgs)
        };

        // color-mode=none: no colour resource, the object is resource 1.
        let (x, _) = write(&Options {
            color_mode: ColorMode::None,
            ..Options::default()
        });
        assert!(!x.contains("basematerials") && !x.contains("colorgroup"));
        assert!(x.contains("<object id=\"1\" name=\"OpenSCAD Model\" type=\"model\" p:UUID="));
        assert!(!x.contains("pid="));
        assert!(x.contains("<item objectid=\"1\""));

        // material-type=color: the default colour is not pre-mapped, so a
        // face of that colour gets its own entry.
        let (x, _) = write(&Options {
            material_type: MaterialType::Color,
            ..Options::default()
        });
        assert!(x.contains(
            "<m:colorgroup id=\"1\">\n\t\t\t<m:color color=\"#F9D72CFF\"/>\n\t\t\t<m:color color=\"#FF0000FF\"/>\n\t\t\t<m:color color=\"#F9D72CFF\"/>\n\t\t</m:colorgroup>"
        ));
        assert_eq!(x.matches("p1=\"1\"").count(), 2);
        assert_eq!(x.matches("p1=\"2\"").count(), 1);

        // selected-only: one material in the chosen colour, alpha forced
        // opaque, and no per-face properties.
        let (x, _) = write(&Options {
            color_mode: ColorMode::SelectedOnly,
            color: Some(Color([1.0, 0.0, 0.0, 0.5])),
            ..Options::default()
        });
        assert!(
            x.contains("<base name=\"Default\" displaycolor=\"#FF0000FF\"/>\n\t\t</basematerials>")
        );
        assert!(!x.contains("p1="));

        // Units, precision and metadata.
        let (x, msgs) = write(&Options {
            unit: Unit::Inch,
            decimal_precision: 3,
            add_meta_data: true,
            meta_data_title: "T&<x>".into(),
            meta_data_rating: "R".into(),
            ..Options::default()
        });
        assert!(msgs.is_empty());
        assert!(x.contains(" unit=\"inch\" "));
        assert!(x.contains("<vertex x=\"1.000\" y=\"0\" z=\"0\" />"));
        assert!(x.contains("<metadata name=\"Title\" preserve=\"1\">T&amp;&lt;x&gt;</metadata>"));
        assert!(x.contains("<metadata name=\"Rating\" preserve=\"1\">R</metadata>\n\t<resources>"));
        let (x, _) = write(&Options {
            add_meta_data: false,
            ..Options::default()
        });
        assert!(!x.contains("<metadata"));

        // An out-of-range precision is reported and 6 is used.
        let (x, msgs) = write(&Options {
            decimal_precision: 17,
            ..Options::default()
        });
        assert_eq!(
            msgs[0].text,
            "EXPORT-ERROR: Error setting decimal precision for export: Error: GENERICEXCEPTION: The call parameter to the function was invalid"
        );
        assert!(x.contains("<vertex x=\"1.000000\""));
    }

    fn tet(colors: Vec<Color>, color_indices: Vec<i32>) -> Mesh {
        Mesh {
            vertices: vec![
                [0.0, 0.0, 0.0],
                [1.0, 0.0, 0.0],
                [0.0, 1.0, 0.0],
                [0.0, 0.0, 1.0],
            ],
            faces: vec![vec![0, 2, 1], vec![0, 1, 3], vec![0, 3, 2], vec![1, 2, 3]],
            colors,
            color_indices,
        }
    }

    #[test]
    fn write_then_read_keeps_mesh_and_colours() {
        let red = Color::from_u8(255, 0, 0);
        let front = Color::from_u8(0xf9, 0xd7, 0x2c);
        let m = tet(vec![red, front], vec![0, 1, -1, 0]);
        let opts = WriteOptions {
            title: "t.scad",
            creation_date: "2026-09-26T00:00:00Z",
            default_color: front,
        };
        let (bytes, msgs) = write(m.as_ref(), &opts);
        assert!(msgs.is_empty());
        let mut msgs = Vec::new();
        let back = read(Some(&bytes), "t.3mf", 1, &mut msgs);
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].text, "Reading 3MF with title 't.scad'");
        assert_eq!(back.len(), 1);
        assert_eq!(back[0].vertices, m.vertices);
        assert_eq!(back[0].faces, m.faces);
        // Uncoloured faces read back with the default material's colour.
        let colors: Vec<Option<Color>> = (0..4)
            .map(|i| back[0].as_ref().face_color(i).copied())
            .collect();
        assert_eq!(colors, vec![Some(red), Some(front), Some(front), Some(red)]);
        // Deterministic output.
        assert_eq!(write(m.as_ref(), &opts).0, bytes);
    }

    #[test]
    fn unreadable_files() {
        let mut msgs = Vec::new();
        assert!(read(Some(b"garbage"), "g.3mf", 3, &mut msgs).is_empty());
        assert_eq!(
            msgs[0].text,
            "Could not read file 'g.3mf', import() at line 3: Error: GENERICEXCEPTION: Could not read ZIP file"
        );
        assert_eq!(msgs[0].severity, Some(Severity::Warning));
    }
}
