//! The render summary after a geometry export (`RenderStatistic::printAll`,
//! `src/RenderStatistic.cc`): as log lines on stderr, or with
//! `--summary-file` as one JSON object and nothing on stderr.
//!
//! `--summary` picks the optional parts: `cache`, `time`, `camera`,
//! `geometry`, `bounding-box`, `area`, or `all`. Unknown names are ignored,
//! as OpenSCAD ignores them. In the log the cache counts, the time and the
//! object description always print; bounding box, area and camera are
//! optional. In the JSON every section is optional, and with none selected
//! the file holds `null`.
//!
//! The JSON is the start of neoscad's machine-readable surface; its schema
//! is documented, and kept stable, in `docs/cli-json.md`. Keys and layout
//! are the nightly's: nlohmann's compact dump, keys sorted, no trailing
//! newline, numbers as nlohmann prints them.

use std::collections::BTreeMap;
use std::io::Write;

use eval::Console;
use geom::Geometry;

/// `--summary` and `--summary-file` as given.
#[derive(Debug, Clone, Default)]
pub struct Request {
    pub options: Vec<String>,
    /// `-` is stdout.
    pub file: Option<String>,
}

impl Request {
    fn enabled(&self, name: &str) -> bool {
        self.options.iter().any(|o| o == "all" || o == name)
    }
}

/// What a render leaves to summarise.
#[derive(Debug)]
pub struct Facts<'a> {
    pub cache_entries: usize,
    pub elapsed_ms: u128,
    /// The top-level result; `None` or empty prints no object section.
    pub geometry: Option<&'a Geometry>,
    pub camera: &'a eval::Camera,
}

/// Print the summary. Returns false when `--summary-file` could not be
/// written (OpenSCAD's `ofstream` fails silently; neoscad says so and the
/// command fails, since a caller asking for the file relies on it).
pub fn emit<W: Write>(req: &Request, facts: &Facts<'_>, con: &mut Console<W>) -> bool {
    match &req.file {
        None => {
            for l in log_lines(req, facts) {
                con.print(None, l.as_bytes());
            }
            true
        }
        Some(target) => {
            let text = json(req, facts);
            let r = if target == "-" {
                let mut out = std::io::stdout().lock();
                out.write_all(text.as_bytes()).and_then(|_| out.flush())
            } else {
                std::fs::write(target, text.as_bytes())
            };
            r.map_err(|e| eprintln!("ERROR: Can't write summary file '{target}': {e}"))
                .is_ok()
        }
    }
}

/// `h:mm:ss.mmm`, as both the log and the JSON print the time.
fn clock(ms: u128) -> String {
    format!(
        "{}:{:02}:{:02}.{:03}",
        ms / 3_600_000,
        ms / 60_000 % 60,
        ms / 1000 % 60,
        ms % 1000
    )
}

/// The log form (`LogVisitor`).
pub fn log_lines(req: &Request, f: &Facts<'_>) -> Vec<String> {
    // `GeometryCache::print`. The cache size in bytes and the CGAL cache
    // lines that follow it in OpenSCAD are not printed: `geom` does not
    // report its cache's size (docs/followups.md).
    let mut l = vec![
        format!("Geometries in cache: {}", f.cache_entries),
        format!("Total rendering time: {}", clock(f.elapsed_ms)),
    ];
    if let Some(g) = f.geometry.filter(|g| !g.is_empty()) {
        l.extend(geom::export::summary(g));
        let bb = req.enabled("bounding-box");
        match g {
            Geometry::Polygon2d(p) => {
                if bb && let Some((min, max)) = p.bounds() {
                    l.push("Bounding box:".into());
                    l.push(format!("   Min:  {:.2}, {:.2}", min[0], min[1]));
                    l.push(format!("   Max:  {:.2}, {:.2}", max[0], max[1]));
                    l.push(format!(
                        "   Size: {:.2}, {:.2}",
                        max[0] - min[0],
                        max[1] - min[1]
                    ));
                }
                if req.enabled("area") {
                    l.push("Measurements:".into());
                    l.push(format!("   Area: {:.2}", area(p)));
                }
            }
            _ => {
                if bb && let Some((min, max)) = bounds3(g) {
                    l.push("Bounding box:".into());
                    l.push(format!(
                        "   Min:  {:.2}, {:.2}, {:.2}",
                        min[0], min[1], min[2]
                    ));
                    l.push(format!(
                        "   Max:  {:.2}, {:.2}, {:.2}",
                        max[0], max[1], max[2]
                    ));
                    l.push(format!(
                        "   Size: {:.2}, {:.2}, {:.2}",
                        max[0] - min[0],
                        max[1] - min[1],
                        max[2] - min[2]
                    ));
                }
            }
        }
    }
    if req.enabled("camera") {
        let c = f.camera;
        l.push("Camera:".into());
        l.push(format!(
            "   Translation: {:.2}, {:.2}, {:.2}",
            c.vpt[0], c.vpt[1], c.vpt[2]
        ));
        l.push(format!(
            "   Rotation:    {:.2}, {:.2}, {:.2}",
            c.vpr[0], c.vpr[1], c.vpr[2]
        ));
        l.push(format!("   Distance:    {:.2}", c.vpd));
        l.push(format!("   FOV:         {:.2}", c.vpf));
    }
    l
}

fn bounds3(g: &Geometry) -> Option<([f64; 3], [f64; 3])> {
    match g {
        Geometry::PolySet(p) => p.bounds(),
        Geometry::Manifold(m) => m.bounds(),
        Geometry::Polygon2d(_) => None,
    }
}

/// `Polygon2d::area`: the signed areas of the tessellation's triangles.
fn area(p: &geom::polygon2d::Polygon2d) -> f64 {
    let ps = p.tessellate();
    let mut a = 0.0;
    for f in &ps.faces {
        let v = |k: usize| ps.vertices[f[k] as usize];
        for k in 1..f.len().saturating_sub(1) {
            let (v1, v2, v3) = (v(0), v(k), v(k + 1));
            a +=
                0.5 * (v1[0] * (v2[1] - v3[1]) + v2[0] * (v3[1] - v1[1]) + v3[0] * (v1[1] - v2[1]));
        }
    }
    a
}

/// A JSON value, printed as nlohmann's `dump()` prints it.
#[derive(Debug, Clone)]
enum Json {
    Null,
    Bool(bool),
    Int(i128),
    Float(f64),
    Str(String),
    Array(Vec<Json>),
    /// Sorted keys, as nlohmann's default `std::map` keeps them.
    Object(BTreeMap<&'static str, Json>),
}

impl Json {
    fn write(&self, out: &mut String) {
        match self {
            Json::Null => out.push_str("null"),
            Json::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
            Json::Int(i) => out.push_str(&i.to_string()),
            Json::Float(x) => out.push_str(&nlohmann_double(*x)),
            Json::Str(s) => {
                out.push('"');
                for c in s.chars() {
                    match c {
                        '"' => out.push_str("\\\""),
                        '\\' => out.push_str("\\\\"),
                        '\n' => out.push_str("\\n"),
                        c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
                        c => out.push(c),
                    }
                }
                out.push('"');
            }
            Json::Array(v) => {
                out.push('[');
                for (i, x) in v.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    x.write(out);
                }
                out.push(']');
            }
            Json::Object(m) => {
                out.push('{');
                for (i, (k, v)) in m.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    out.push('"');
                    out.push_str(k);
                    out.push_str("\":");
                    v.write(out);
                }
                out.push('}');
            }
        }
    }
}

/// nlohmann's `dump` of a double: the shortest digits that read back
/// exactly, laid out by its `format_buffer` (plain notation for decimal
/// exponents from -4 to 15, `.0` on whole numbers, otherwise `1e+20` style
/// with at least two exponent digits), and `null` for NaN and infinities.
fn nlohmann_double(x: f64) -> String {
    if !x.is_finite() {
        return "null".into();
    }
    if x == 0.0 {
        return if x.is_sign_negative() { "-0.0" } else { "0.0" }.into();
    }
    // Rust's `{:e}` gives the same shortest round-trip digits.
    let sci = format!("{:e}", x.abs());
    let (mantissa, exp) = sci.split_once('e').unwrap_or((&sci, "0"));
    let digits: String = mantissa.chars().filter(|c| *c != '.').collect();
    let exp: i32 = exp.parse().unwrap_or(0);
    let k = digits.len() as i32;
    // `n`: where the decimal point goes, counted from the first digit.
    let n = exp + 1;
    let sign = if x < 0.0 { "-" } else { "" };
    let body = if k <= n && n <= 15 {
        format!("{digits}{}.0", "0".repeat((n - k) as usize))
    } else if 0 < n && n <= 15 {
        format!("{}.{}", &digits[..n as usize], &digits[n as usize..])
    } else if -4 < n && n <= 0 {
        format!("0.{}{digits}", "0".repeat((-n) as usize))
    } else {
        let e = n - 1;
        let m = if k == 1 {
            digits.clone()
        } else {
            format!("{}.{}", &digits[..1], &digits[1..])
        };
        let es = if e < 0 { '-' } else { '+' };
        format!("{m}e{es}{:02}", e.abs())
    };
    format!("{sign}{body}")
}

fn floats(v: &[f64]) -> Json {
    Json::Array(v.iter().map(|x| Json::Float(*x)).collect())
}

fn bbox_json(min: &[f64], max: &[f64]) -> Json {
    let size: Vec<f64> = min.iter().zip(max).map(|(a, b)| b - a).collect();
    Json::Object(BTreeMap::from([
        ("min", floats(min)),
        ("max", floats(max)),
        ("size", floats(&size)),
    ]))
}

/// The `--summary-file` form (`StreamVisitor`).
pub fn json(req: &Request, f: &Facts<'_>) -> String {
    let mut top: BTreeMap<&'static str, Json> = BTreeMap::new();
    if req.enabled("cache") {
        // neoscad has one geometry cache and no CGAL cache. Its size in
        // bytes and its budget are not reported by `geom` yet, so they are
        // null; the CGAL cache is always empty (docs/cli-json.md).
        let cache = |entries: usize, bytes: Option<i128>, max: Option<i128>| {
            Json::Object(BTreeMap::from([
                ("entries", Json::Int(entries as i128)),
                ("bytes", bytes.map_or(Json::Null, Json::Int)),
                ("max_size", max.map_or(Json::Null, Json::Int)),
            ]))
        };
        top.insert(
            "cache",
            Json::Object(BTreeMap::from([
                ("geometry_cache", cache(f.cache_entries, None, None)),
                ("cgal_cache", cache(0, Some(0), Some(0))),
            ])),
        );
    }
    if req.enabled("time") {
        let ms = f.elapsed_ms as i128;
        top.insert(
            "time",
            Json::Object(BTreeMap::from([
                ("time", Json::Str(clock(f.elapsed_ms))),
                ("total", Json::Int(ms)),
                ("milliseconds", Json::Int(ms % 1000)),
                ("seconds", Json::Int(ms / 1000 % 60)),
                ("minutes", Json::Int(ms / 60_000 % 60)),
                ("hours", Json::Int(ms / 3_600_000)),
            ])),
        );
    }
    if req.enabled("geometry")
        && let Some(g) = f.geometry.filter(|g| !g.is_empty())
    {
        let bb = req.enabled("bounding-box");
        let mut m: BTreeMap<&'static str, Json> = BTreeMap::new();
        match g {
            Geometry::Polygon2d(p) => {
                m.insert("dimensions", Json::Int(2));
                m.insert("convex", Json::Bool(p.is_convex()));
                m.insert("contours", Json::Int(p.outlines.len() as i128));
                if bb && let Some((min, max)) = p.bounds() {
                    m.insert("bounding_box", bbox_json(&min, &max));
                }
            }
            Geometry::PolySet(ps) => {
                m.insert("dimensions", Json::Int(3));
                m.insert("convex", Json::Bool(ps.is_convex()));
                m.insert("triangular", Json::Bool(ps.triangular));
                m.insert("facets", Json::Int(ps.faces.len() as i128));
                if bb && let Some((min, max)) = ps.bounds() {
                    m.insert("bounding_box", bbox_json(&min, &max));
                }
            }
            Geometry::Manifold(mg) => {
                let simple = geom::manifold_geom::status_name(mg.manifold.status()) == "NoError";
                m.insert("dimensions", Json::Int(3));
                m.insert("simple", Json::Bool(simple));
                m.insert("vertices", Json::Int(mg.manifold.num_vert() as i128));
                m.insert("facets", Json::Int(mg.manifold.num_tri() as i128));
                if bb && let Some((min, max)) = mg.bounds() {
                    m.insert("bounding_box", bbox_json(&min, &max));
                }
            }
        }
        top.insert("geometry", Json::Object(m));
    }
    if req.enabled("camera") {
        let c = f.camera;
        top.insert(
            "camera",
            Json::Object(BTreeMap::from([
                ("translation", floats(&c.vpt)),
                ("rotation", floats(&c.vpr)),
                ("distance", Json::Float(c.vpd)),
                ("fov", Json::Float(c.vpf)),
            ])),
        );
    }
    if top.is_empty() {
        // nlohmann's default-constructed value dumps as `null`.
        return "null".into();
    }
    let mut out = String::new();
    Json::Object(top).write(&mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn doubles_print_like_nlohmann() {
        for (x, s) in [
            (0.0, "0.0"),
            (1.0, "1.0"),
            (140.0, "140.0"),
            (22.5, "22.5"),
            (-0.8090169943749475, "-0.8090169943749475"),
            (1.9510565162951536, "1.9510565162951536"),
            (1e14, "100000000000000.0"),
            (1e15, "1e+15"),
            (1e16, "1e+16"),
            (1.5e20, "1.5e+20"),
            (0.0001, "0.0001"),
            (0.00001234, "1.234e-05"),
            (123456.789, "123456.789"),
            (f64::NAN, "null"),
        ] {
            assert_eq!(nlohmann_double(x), s, "{x}");
        }
    }

    fn cube_facts<'a>(g: &'a Geometry, camera: &'a eval::Camera) -> Facts<'a> {
        Facts {
            cache_entries: 1,
            elapsed_ms: 0,
            geometry: Some(g),
            camera,
        }
    }

    #[test]
    fn json_matches_the_nightly_layout() {
        let g = Geometry::PolySet(std::sync::Arc::new(geom::primitives::cube([1.0; 3], false)));
        let cam = eval::Camera::default();
        let req = |o: &[&str]| Request {
            options: o.iter().map(|s| s.to_string()).collect(),
            file: Some("-".into()),
        };
        // `--summary geometry --summary-file -` on `cube(1);`, nightly.
        assert_eq!(
            json(&req(&["geometry"]), &cube_facts(&g, &cam)),
            r#"{"geometry":{"convex":true,"dimensions":3,"facets":6,"triangular":false}}"#
        );
        // Nothing selected: `null`. `bounding-box` alone adds nothing,
        // since it only extends the geometry section.
        assert_eq!(json(&req(&[]), &cube_facts(&g, &cam)), "null");
        assert_eq!(
            json(&req(&["bounding-box", "bogus"]), &cube_facts(&g, &cam)),
            "null"
        );
        // `--summary all`, apart from the cache byte counts neoscad does
        // not know (null here, 856 and 104857600 in the nightly).
        assert_eq!(
            json(&req(&["all"]), &cube_facts(&g, &cam)),
            concat!(
                r#"{"cache":{"cgal_cache":{"bytes":0,"entries":0,"max_size":0},"geometry_cache":{"bytes":null,"entries":1,"max_size":null}},"#,
                r#""camera":{"distance":140.0,"fov":22.5,"rotation":[55.0,0.0,25.0],"translation":[0.0,0.0,0.0]},"#,
                r#""geometry":{"bounding_box":{"max":[1.0,1.0,1.0],"min":[0.0,0.0,0.0],"size":[1.0,1.0,1.0]},"convex":true,"dimensions":3,"facets":6,"triangular":false},"#,
                r#""time":{"hours":0,"milliseconds":0,"minutes":0,"seconds":0,"time":"0:00:00.000","total":0}}"#
            )
        );
    }

    #[test]
    fn log_lines_match_the_nightly() {
        let g = Geometry::PolySet(std::sync::Arc::new(geom::primitives::cube([1.0; 3], false)));
        let cam = eval::Camera::default();
        let req = Request {
            options: vec!["all".into()],
            file: None,
        };
        // `--summary all -o x.stl` on `cube(1);`, nightly, less the cache
        // byte lines.
        assert_eq!(
            log_lines(&req, &cube_facts(&g, &cam)),
            [
                "Geometries in cache: 1",
                "Total rendering time: 0:00:00.000",
                "Top level object is a 3D object (PolySet):",
                "   Convex:       yes",
                "   Facets:         6",
                "Bounding box:",
                "   Min:  0.00, 0.00, 0.00",
                "   Max:  1.00, 1.00, 1.00",
                "   Size: 1.00, 1.00, 1.00",
                "Camera:",
                "   Translation: 0.00, 0.00, 0.00",
                "   Rotation:    55.00, 0.00, 25.00",
                "   Distance:    140.00",
                "   FOV:         22.50",
            ]
        );
    }
}
