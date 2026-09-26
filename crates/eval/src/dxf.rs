//! Reading DXF files for `dxf_dim()` and `dxf_cross()`.
//!
//! A port of `DxfData`'s reader (io/DxfData.cc): it collects dimension
//! entities, and joins line-like entities (lines, polylines, circles, arcs,
//! ellipses and block inserts) into paths on OpenSCAD's coarse snapping
//! grid. The two builtin functions are evaluation-time features, which is
//! why this lives in the evaluator for now; the `import()` of DXF geometry
//! in the geometry phase should move this reader into the `io` crate and
//! share it.
//!
//! Quirks kept on purpose: the reader does not check which entity a group
//! code belongs to (an ellipse's ratio lands in `radius`, an insert's scale
//! in the ellipse angles), a missing coordinate aborts the entity with a
//! "Not enough input values" warning, and the grid inserts empty cells when
//! it is merely queried, which affects later snapping.

use std::collections::{BTreeMap, HashMap};

use crate::trig::{cos_degrees, sin_degrees};

const GRID_COARSE: f64 = 0.0009765625;
const GRID_FINE: f64 = 0.00000095367431640625;

/// `Grid2d<std::vector<int>>`: snaps points to a grid, preferring an
/// existing neighbouring cell.
struct Grid {
    res: f64,
    db: HashMap<(i64, i64), Vec<i32>>,
}

impl Grid {
    fn key(&mut self, x: f64, y: f64) -> (i64, i64) {
        let mut ix = (x / self.res).round() as i64;
        let mut iy = (y / self.res).round() as i64;
        if !self.db.contains_key(&(ix, iy)) {
            let mut dist = 10;
            let (cx, cy) = (ix, iy);
            for jx in cx - 1..=cx + 1 {
                for jy in cy - 1..=cy + 1 {
                    if !self.db.contains_key(&(jx, jy)) {
                        continue;
                    }
                    let d = (cx - jx).abs() + (cy - jy).abs();
                    if d < dist {
                        dist = d;
                        ix = jx;
                        iy = jy;
                    }
                }
            }
        }
        (ix, iy)
    }

    fn align(&mut self, x: &mut f64, y: &mut f64) -> &mut Vec<i32> {
        let k = self.key(*x, *y);
        *x = k.0 as f64 * self.res;
        *y = k.1 as f64 * self.res;
        self.db.entry(k).or_default()
    }

    fn data(&mut self, mut x: f64, mut y: f64) -> Vec<i32> {
        self.align(&mut x, &mut y).clone()
    }

    fn eq(&mut self, mut x1: f64, mut y1: f64, mut x2: f64, mut y2: f64) -> bool {
        self.align(&mut x1, &mut y1);
        self.align(&mut x2, &mut y2);
        (x1 - x2).abs() < self.res && (y1 - y2).abs() < self.res
    }
}

#[derive(Debug, Clone, Default)]
pub struct Dim {
    pub ty: i32,
    pub coords: [[f64; 2]; 7],
    pub angle: f64,
    pub name: String,
}

#[derive(Debug, Clone, Default)]
pub struct Path {
    pub indices: Vec<usize>,
    pub closed: bool,
    pub inner: bool,
}

#[derive(Debug, Default)]
pub struct DxfData {
    pub points: Vec<[f64; 2]>,
    pub paths: Vec<Path>,
    pub dims: Vec<Dim>,
}

#[derive(Clone, Copy)]
struct Line {
    idx: [usize; 2],
    disabled: bool,
}

/// `std::getline` over a byte buffer, with the stream's eof flag.
struct Lines<'a> {
    data: &'a [u8],
    pos: usize,
    eof: bool,
}

impl Lines<'_> {
    fn next(&mut self) -> String {
        if self.pos >= self.data.len() {
            self.eof = true;
            return String::new();
        }
        let rest = &self.data[self.pos..];
        let line = match rest.iter().position(|&b| b == b'\n') {
            Some(i) => {
                self.pos += i + 1;
                &rest[..i]
            }
            None => {
                self.pos = self.data.len();
                self.eof = true;
                rest
            }
        };
        String::from_utf8_lossy(line).trim().to_string()
    }
}

/// `CurveDiscretizer(36).getCircularSegmentCount(r, angle)`.
fn segments(r: f64, angle: f64) -> Option<i32> {
    if r < GRID_FINE || angle.is_nan() || angle.is_infinite() {
        return None;
    }
    let result = 36.0 * angle.abs() / 360.0;
    Some((result.ceil() as i32).max(1))
}

enum Abort {
    /// `boost::bad_lexical_cast`.
    Value,
    /// `std::out_of_range` from `vector::at`.
    Range,
}

fn num(s: &str) -> Result<f64, Abort> {
    s.parse::<f64>().map_err(|_| Abort::Value)
}

fn at(v: &[f64], i: usize) -> Result<f64, Abort> {
    v.get(i).copied().ok_or(Abort::Range)
}

/// What to read from a DXF file.
pub struct Request<'a> {
    /// The absolute path, as some messages print it.
    pub file: &'a str,
    /// The path relative to the main file's directory, as others do.
    pub display: &'a str,
    /// Keep only this layer, unless empty.
    pub layer: &'a str,
    pub origin: [f64; 2],
    pub scale: f64,
}

/// Read a DXF file's contents (`None` when it could not be opened).
pub fn read(bytes: Option<&[u8]>, req: &Request<'_>, warn: &mut dyn FnMut(String)) -> DxfData {
    let (file, display, layer_name, scale) = (req.file, req.display, req.layer, req.scale);
    let [xorigin, yorigin] = req.origin;
    let mut out = DxfData::default();
    let Some(bytes) = bytes else {
        warn(format!("Can't open DXF file '{file}'."));
        return out;
    };
    let mut grid = Grid { res: GRID_COARSE, db: HashMap::new() };
    let mut lines: Vec<Line> = Vec::new();
    let mut blockdata: HashMap<String, Vec<Line>> = HashMap::new();
    let mut in_entities = false;
    let mut in_blocks = false;
    let mut current_block = String::new();

    let (mut mode, mut layer, mut name, mut iddata) = (String::new(), String::new(), String::new(), String::new());
    let mut dimtype = 0i32;
    let mut coords = [[0.0f64; 2]; 7];
    let mut xverts: Vec<f64> = Vec::new();
    let mut yverts: Vec<f64> = Vec::new();
    let mut radius = 0.0;
    let (mut arc_start, mut arc_stop) = (0.0f64, 0.0f64);
    let (mut ell_start, mut ell_stop) = (0.0f64, 0.0f64);
    let mut unsupported: Vec<(String, i32)> = Vec::new();

    let mut reader = Lines { data: bytes, pos: 0, eof: false };
    while !reader.eof {
        let id_str = reader.next();
        let data = reader.next();
        let Ok(id) = id_str.parse::<i32>() else {
            if !reader.eof {
                warn(format!("Illegal ID '{id_str}' in `{file}'"));
            }
            break;
        };
        let r: Result<(), Abort> = (|| {
            let add_line = |out: &mut DxfData,
                                grid: &mut Grid,
                                lines: &mut Vec<Line>,
                                blockdata: &mut HashMap<String, Vec<Line>>,
                                layer: &str,
                                p: [f64; 4]| {
                let [mut x1, mut y1, mut x2, mut y2] = p;
                if !in_entities && !in_blocks {
                    return;
                }
                if in_entities && !(layer_name.is_empty() || layer_name == layer) {
                    return;
                }
                let n = lines.len() as i32;
                grid.align(&mut x1, &mut y1).push(n);
                grid.align(&mut x2, &mut y2).push(n);
                if in_entities {
                    out.points.push([x1, y1]);
                    out.points.push([x2, y2]);
                    let l = out.points.len();
                    lines.push(Line { idx: [l - 2, l - 1], disabled: false });
                }
                if in_blocks && !current_block.is_empty() {
                    out.points.push([x1, y1]);
                    out.points.push([x2, y2]);
                    let l = out.points.len();
                    blockdata.entry(current_block.clone()).or_default().push(Line { idx: [l - 2, l - 1], disabled: false });
                }
            };
            if (10..=16).contains(&id) {
                let v = num(&data)?;
                coords[(id - 10) as usize][0] = if in_blocks {
                    v
                } else if id == 11 || id == 12 || id == 16 {
                    v * scale
                } else {
                    (v - xorigin) * scale
                };
            }
            if (20..=26).contains(&id) {
                let v = num(&data)?;
                coords[(id - 20) as usize][1] = if in_blocks {
                    v
                } else if id == 21 || id == 22 || id == 26 {
                    v * scale
                } else {
                    (v - yorigin) * scale
                };
            }
            match id {
                0 => {
                    match mode.as_str() {
                        "SECTION" => {
                            in_entities = iddata == "ENTITIES";
                            in_blocks = iddata == "BLOCKS";
                        }
                        "LINE" => {
                            let p = [at(&xverts, 0)?, at(&yverts, 0)?, at(&xverts, 1)?, at(&yverts, 1)?];
                            add_line(&mut out, &mut grid, &mut lines, &mut blockdata, &layer, p);
                        }
                        "LWPOLYLINE" => {
                            let n = xverts.len().max(yverts.len());
                            for i in 1..n {
                                let p = [at(&xverts, i - 1)?, at(&yverts, i - 1)?, at(&xverts, i % n)?, at(&yverts, i % n)?];
                                add_line(&mut out, &mut grid, &mut lines, &mut blockdata, &layer, p);
                            }
                            if dimtype & 1 != 0 {
                                let p = [
                                    at(&xverts, n.wrapping_sub(1))?,
                                    at(&yverts, n.wrapping_sub(1))?,
                                    at(&xverts, 0)?,
                                    at(&yverts, 0)?,
                                ];
                                add_line(&mut out, &mut grid, &mut lines, &mut blockdata, &layer, p);
                            }
                        }
                        "CIRCLE" => {
                            let n = segments(radius, 360.0).unwrap_or(3);
                            let c = [at(&xverts, 0)?, at(&yverts, 0)?];
                            for i in 0..n {
                                let a1 = 360.0 * f64::from(i) / f64::from(n);
                                let a2 = 360.0 * f64::from(i + 1) / f64::from(n);
                                let p = [
                                    cos_degrees(a1) * radius + c[0],
                                    sin_degrees(a1) * radius + c[1],
                                    cos_degrees(a2) * radius + c[0],
                                    sin_degrees(a2) * radius + c[1],
                                ];
                                add_line(&mut out, &mut grid, &mut lines, &mut blockdata, &layer, p);
                            }
                        }
                        "ARC" => {
                            let c = [at(&xverts, 0)?, at(&yverts, 0)?];
                            // OpenSCAD loops (practically) forever on a
                            // non-finite or huge angle; stop instead.
                            let mut guard = 0;
                            while arc_start > arc_stop && guard < 1_000_000 {
                                arc_stop += 360.0;
                                guard += 1;
                            }
                            let angle = arc_stop - arc_start;
                            let n = segments(radius, angle).unwrap_or(1);
                            for i in 0..n {
                                let a1 = arc_start + angle * f64::from(i) / f64::from(n);
                                let a2 = arc_start + angle * f64::from(i + 1) / f64::from(n);
                                let p = [
                                    cos_degrees(a1) * radius + c[0],
                                    sin_degrees(a1) * radius + c[1],
                                    cos_degrees(a2) * radius + c[0],
                                    sin_degrees(a2) * radius + c[1],
                                ];
                                add_line(&mut out, &mut grid, &mut lines, &mut blockdata, &layer, p);
                            }
                        }
                        "ELLIPSE" => {
                            let mut guard = 0;
                            while ell_start > ell_stop && guard < 1_000_000 {
                                ell_stop += 2.0 * std::f64::consts::PI;
                                guard += 1;
                            }
                            let c = [at(&xverts, 0)?, at(&yverts, 0)?];
                            let ce = [at(&xverts, 1)?, at(&yverts, 1)?];
                            let r_major = (ce[0] * ce[0] + ce[1] * ce[1]).sqrt();
                            let mut rot = (ce[0] / r_major).clamp(-1.0, 1.0).acos();
                            if ce[1] < 0.0 {
                                rot = 2.0 * std::f64::consts::PI - rot;
                            }
                            let r_minor = r_major * radius;
                            let sweep = ell_stop - ell_start;
                            let n = segments(r_major, sweep / (2.0 * std::f64::consts::PI) * 360.0).unwrap_or(1);
                            let mut p1 = [0.0, 0.0];
                            for i in 0..=n {
                                let a = ell_start + sweep * f64::from(i) / f64::from(n);
                                let p2 = [a.cos() * r_major, a.sin() * r_minor];
                                let q = [
                                    rot.cos() * p2[0] - rot.sin() * p2[1] + c[0],
                                    rot.sin() * p2[0] + rot.cos() * p2[1] + c[1],
                                ];
                                if i > 0 {
                                    add_line(&mut out, &mut grid, &mut lines, &mut blockdata, &layer, [p1[0], p1[1], q[0], q[1]]);
                                }
                                p1 = q;
                            }
                        }
                        "INSERT" => {
                            let block = blockdata.get(&iddata).cloned().unwrap_or_default();
                            for l in &block {
                                let a = arc_start;
                                let lx1 = out.points[l.idx[0]][0] * ell_start;
                                let ly1 = out.points[l.idx[0]][1] * ell_stop;
                                let lx2 = out.points[l.idx[1]][0] * ell_start;
                                let ly2 = out.points[l.idx[1]][1] * ell_stop;
                                let (s, c) = (sin_degrees(a), cos_degrees(a));
                                let p = [
                                    (c * lx1 - s * ly1) * scale + at(&xverts, 0)?,
                                    (s * lx1 + c * ly1) * scale + at(&yverts, 0)?,
                                    (c * lx2 - s * ly2) * scale + at(&xverts, 0)?,
                                    (s * lx2 + c * ly2) * scale + at(&yverts, 0)?,
                                ];
                                add_line(&mut out, &mut grid, &mut lines, &mut blockdata, &layer, p);
                            }
                        }
                        "DIMENSION" if layer_name.is_empty() || layer_name == layer => {
                            out.dims.push(Dim { ty: dimtype, coords, angle: arc_start, name: name.clone() });
                        }
                        "BLOCK" => current_block = iddata.clone(),
                        "ENDBLK" => current_block.clear(),
                        "ENDSEC" => {}
                        _ => {
                            if in_blocks || (in_entities && (layer_name.is_empty() || layer_name == layer)) {
                                match unsupported.iter_mut().find(|(m, _)| *m == mode) {
                                    Some(e) => e.1 += 1,
                                    None => unsupported.push((mode.clone(), 1)),
                                }
                            }
                        }
                    }
                    mode = data.clone();
                    layer.clear();
                    name.clear();
                    iddata.clear();
                    dimtype = 0;
                    coords = [[0.0; 2]; 7];
                    xverts.clear();
                    yverts.clear();
                    radius = 0.0;
                    arc_start = 0.0;
                    arc_stop = 0.0;
                    ell_start = 0.0;
                    ell_stop = 0.0;
                    if mode == "INSERT" {
                        ell_start = 1.0;
                        ell_stop = 1.0;
                    }
                }
                1 => name = data.clone(),
                2 => iddata = data.clone(),
                8 => layer = data.clone(),
                10 | 11 => {
                    let v = num(&data)?;
                    xverts.push(if in_blocks { v } else { (v - xorigin) * scale });
                }
                20 | 21 => {
                    let v = num(&data)?;
                    yverts.push(if in_blocks { v } else { (v - yorigin) * scale });
                }
                40 => {
                    radius = num(&data)?;
                    if !in_blocks {
                        radius *= scale;
                    }
                }
                41 => ell_start = num(&data)?,
                50 => arc_start = num(&data)?,
                42 => ell_stop = num(&data)?,
                51 => arc_stop = num(&data)?,
                70 => dimtype = data.parse::<i32>().map_err(|_| Abort::Value)?,
                _ => {}
            }
            Ok(())
        })();
        match r {
            Ok(()) => {}
            Err(Abort::Value) => warn(format!("Illegal value '{data}'in `{file}'")),
            Err(Abort::Range) => warn(format!("Not enough input values for {data}. in '{file}'")),
        }
    }

    for (m, n) in &unsupported {
        if layer_name.is_empty() {
            let mut q = Vec::new();
            lang::dump::quoted(&mut q, display.as_bytes());
            warn(format!("Unsupported DXF Entity '{m}' ({n:x}) in {}.", String::from_utf8_lossy(&q)));
        } else {
            warn(format!("Unsupported DXF Entity '{m}' ({n:x}) in layer '{layer_name}' of {display}"));
        }
    }

    extract_paths(&mut out, &mut grid, &mut lines);
    out
}

/// Join lines into paths: open paths first (starting from free line ends),
/// then closed loops.
fn extract_paths(out: &mut DxfData, grid: &mut Grid, lines: &mut [Line]) {
    let mut enabled: BTreeMap<usize, usize> = (0..lines.len()).map(|i| (i, i)).collect();
    let follow = |out: &mut DxfData, grid: &mut Grid, lines: &mut [Line], enabled: &mut BTreeMap<usize, usize>, path: &mut Path, mut line: usize, mut point: usize| {
        path.indices.push(lines[line].idx[point]);
        loop {
            path.indices.push(lines[line].idx[1 - point]);
            let rp = out.points[lines[line].idx[1 - point]];
            lines[line].disabled = true;
            enabled.remove(&line);
            let lv = grid.data(rp[0], rp[1]);
            let mut next = None;
            for &k in &lv {
                let Ok(k) = usize::try_from(k) else { continue };
                if k >= lines.len() || lines[k].disabled {
                    continue;
                }
                let (i0, i1) = (lines[k].idx[0], lines[k].idx[1]);
                if grid.eq(rp[0], rp[1], out.points[i0][0], out.points[i0][1]) {
                    next = Some((k, 0));
                    break;
                }
                if grid.eq(rp[0], rp[1], out.points[i1][0], out.points[i1][1]) {
                    next = Some((k, 1));
                    break;
                }
            }
            match next {
                Some((k, p)) => {
                    line = k;
                    point = p;
                }
                None => break,
            }
        }
    };
    // Open paths.
    'open: while !enabled.is_empty() {
        let mut start = None;
        'search: for &idx in enabled.values() {
            for j in 0..2 {
                let p = out.points[lines[idx].idx[j]];
                let lv = grid.data(p[0], p[1]);
                let connected = lv.iter().any(|&k| {
                    usize::try_from(k).is_ok_and(|k| k < lines.len() && k != idx && !lines[k].disabled)
                });
                if !connected {
                    start = Some((idx, j));
                    break 'search;
                }
            }
        }
        let Some((line, point)) = start else { break 'open };
        let mut path = Path::default();
        follow(out, grid, lines, &mut enabled, &mut path, line, point);
        out.paths.push(path);
    }
    // Closed paths.
    while let Some((_, &line)) = enabled.iter().next() {
        let mut path = Path { closed: true, ..Default::default() };
        follow(out, grid, lines, &mut enabled, &mut path, line, 0);
        out.paths.push(path);
    }
    // fixup_path_direction.
    for path in out.paths.iter_mut() {
        if !path.closed {
            break;
        }
        path.inner = true;
        let pts = &out.points;
        let mut min_x = pts[path.indices[0]][0];
        let mut b = 0;
        for (j, &ix) in path.indices.iter().enumerate().skip(1) {
            if pts[ix][0] < min_x {
                min_x = pts[ix][0];
                b = j;
            }
        }
        let n = path.indices.len();
        let a = if b == 0 { n - 2 } else { b - 1 };
        let c = if b == n - 1 { 1 } else { b + 1 };
        let (pa, pb, pc) = (pts[path.indices[a]], pts[path.indices[b]], pts[path.indices[c]]);
        let (ax, ay) = (pa[0] - pb[0], pa[1] - pb[1]);
        let (cx, cy) = (pc[0] - pb[0], pc[1] - pb[1]);
        if ax.atan2(ay) < cx.atan2(cy) {
            path.indices.reverse();
        }
    }
}
