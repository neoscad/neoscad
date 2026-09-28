//! `.nef3` import: the text form of CGAL's `Nef_polyhedron_3` ("Selective
//! Nef Complex"), as OpenSCAD's `import_nef3` reads it
//! (`src/io/import_nef.cc`, which hands the stream to CGAL's
//! `SNC_io_parser::read`, `CGAL/Nef_3/SNC_io_parser.h` in CGAL 6.1), and
//! the faces its Manifold backend makes of it
//! (`CGALUtils::createPolySetFromNefPolyhedron3`, `cgalutils.cc:288`).
//!
//! The file is a header of seven counts followed by one line per vertex,
//! halfedge, halffacet, volume, shalfedge, shalfloop and sface, each
//! `index { fields } mark`, with coordinates as homogeneous integers of
//! any size (`x y z w`). Only a few fields matter for the mesh: a vertex's
//! point, a halfedge's vertex, a halffacet's boundary shalfedges, volume
//! and mark, a volume's mark, and a shalfedge's halfedge and facet-cycle
//! successor. Everything else is still read with CGAL's syntax and index
//! checks, because a file CGAL rejects must be rejected here too, with the
//! same message: OpenSCAD's result for it is empty.
//!
//! No CGAL and no rational arithmetic: a vertex's coordinate is only ever
//! used as `CGAL::to_double(hx / hw)`, which is exact division truncated
//! to a `double` ([`ratio_to_f64`]), then rounded to the nearest `float`
//! (`vector_convert<Vector3f>`). Checked bit for bit against GMP on every
//! vertex of two nightly exports with rotated, 80-digit coordinates.

use std::collections::HashMap;

use crate::Message;

/// The path of `SNC_io_parser.h` compiled into the nightly OpenSCAD, which
/// CGAL's exception text names. Kept verbatim so a failed import prints
/// what the nightly prints and echo diffs against it stay clean.
const CGAL_HEADER: &str = "/Users/distiller/libraries/install/include/CGAL/Nef_3/SNC_io_parser.h";

/// One halffacet that faces empty space: its boundary cycles as indices
/// into [`Faces::vertices`]. CGAL does not order the cycles (the outer one
/// need not come first); holes run the other way round from the outer
/// boundary, so an odd-winding tessellation of all of them fills the face.
#[derive(Debug, Clone, PartialEq)]
pub struct Facet {
    /// The halffacet's own mark, which picks the colour the Manifold
    /// backend paints it with: front for marked, back for unmarked.
    pub mark: bool,
    pub cycles: Vec<Vec<u32>>,
}

/// The polygons `createPolySetFromNefPolyhedron3` collects before it
/// tessellates them.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Faces {
    /// Every vertex a cycle reached, in first-seen order, at `float`
    /// precision (stored as `f64`). Vertices that coincide in `float` are
    /// one, as OpenSCAD's `Reindexer<Vector3f>` makes them.
    pub vertices: Vec<[f64; 3]>,
    pub facets: Vec<Facet>,
}

/// Read a `.nef3` file's bytes. `file` and `line` name the `import()` in
/// messages. A file CGAL rejects gives OpenSCAD's two messages (a warning,
/// then CGAL's exception text) and no faces.
pub fn read(bytes: &[u8], file: &str, line: u32, msgs: &mut Vec<Message>) -> Faces {
    match parse(bytes) {
        Ok(snc) => snc.faces(file, msgs),
        Err(f) => {
            msgs.push(Message::warning(format!(
                "Failure trying to import '{file}', import() at line {line}"
            )));
            msgs.push(Message {
                severity: None,
                text: f.what(),
                located: false,
            });
            Faces::default()
        }
    }
}

/// `GeometryUtils::findUnconnectedEdges` over faces given as index lists:
/// each directed edge cancels one copy of its reverse, and what is left is
/// counted once per distinct directed edge.
pub fn unconnected_edges<'a>(faces: impl IntoIterator<Item = &'a [u32]>) -> usize {
    let mut edges: HashMap<(u32, u32), u32> = HashMap::new();
    for f in faces {
        for i in 0..f.len() {
            let (a, b) = (f[i], f[(i + 1) % f.len()]);
            match edges.get_mut(&(b, a)) {
                Some(n) => {
                    *n -= 1;
                    if *n == 0 {
                        edges.remove(&(b, a));
                    }
                }
                None => *edges.entry((a, b)).or_insert(0) += 1,
            }
        }
    }
    edges.len()
}

/// Why CGAL gave up, as the `CGAL::Failure_exception` OpenSCAD catches.
#[derive(Debug, PartialEq)]
enum Failure {
    /// `CGAL_warning_msg(false, msg)`, which OpenSCAD's CGAL error
    /// behaviour turns into an exception. `line` is its line in the header.
    Warning { line: u32, msg: &'static str },
    /// `CGAL_assertion(expr)`.
    Assertion { line: u32, expr: &'static str },
    /// Not a CGAL message: a vertex with homogeneous weight 0, where CGAL
    /// divides by zero and OpenSCAD dies on GMP's division-by-zero abort.
    Infinite { vertex: usize },
}

impl Failure {
    /// `Failure_exception::what()`.
    fn what(&self) -> String {
        match self {
            Failure::Warning { line, msg } => format!(
                "CGAL ERROR: warning condition failed!\nExpr: false\nFile: {CGAL_HEADER}\n\
                 Line: {line}\nExplanation: {msg}"
            ),
            Failure::Assertion { line, expr } => format!(
                "CGAL ERROR: assertion violation!\nExpr: {expr}\nFile: {CGAL_HEADER}\nLine: {line}"
            ),
            Failure::Infinite { vertex } => {
                format!("Vertex {vertex} is at infinity (homogeneous weight 0)")
            }
        }
    }
}

const fn warn(line: u32, msg: &'static str) -> Failure {
    Failure::Warning { line, msg }
}

/// The parts of the complex the mesh needs.
#[derive(Debug, Default)]
struct Snc {
    /// Each vertex's point, `CGAL::to_double` of each coordinate.
    points: Vec<[f64; 3]>,
    /// Each halfedge's (svertex's) vertex.
    edge_vertex: Vec<u32>,
    facets: Vec<RawFacet>,
    volume_marks: Vec<bool>,
    /// Each shalfedge's halfedge (`source()`) and facet-cycle successor
    /// (`next()`).
    sedges: Vec<(u32, u32)>,
}

#[derive(Debug)]
struct RawFacet {
    /// Boundary cycles that start at a shalfedge. Cycles that are a
    /// shalfloop (an isolated vertex on the facet) have no edges and give
    /// no polygon.
    entries: Vec<u32>,
    volume: u32,
    mark: bool,
}

impl Snc {
    /// Step 1 of `createPolySetFromNefPolyhedron3`: every halffacet whose
    /// volume is unmarked (so the facet faces empty space and its cycles
    /// run counter-clockwise seen from outside) becomes a polygon with its
    /// cycles as contours. Consecutive vertices equal in `float` merge, a
    /// cycle closing on its first vertex drops the repeat, and cycles left
    /// with fewer than three vertices go.
    fn faces(&self, file: &str, msgs: &mut Vec<Message>) -> Faces {
        let f32s: Vec<[f32; 3]> = self.points.iter().map(|p| p.map(|c| c as f32)).collect();
        let mut index: HashMap<[u32; 3], u32> = HashMap::new();
        let mut vertices: Vec<[f64; 3]> = Vec::new();
        let mut lookup = |v: [f32; 3]| -> u32 {
            // `-0` and `0` are one position, as `==` has them.
            let key = v.map(|c| if c == 0.0 { 0 } else { c.to_bits() });
            *index.entry(key).or_insert_with(|| {
                vertices.push(v.map(f64::from));
                (vertices.len() - 1) as u32
            })
        };
        let mut facets = Vec::new();
        let mut open = 0usize;
        for f in &self.facets {
            if self.volume_marks[f.volume as usize] {
                continue;
            }
            let mut cycles = Vec::new();
            for &start in &f.entries {
                let mut cur: Vec<u32> = Vec::new();
                let mut e = start;
                let mut closed = false;
                // A cycle has at most one step per shalfedge. The reader
                // only checks that `next` is in range, so a file can make
                // the walk miss its start; CGAL would circle forever.
                for _ in 0..self.sedges.len() {
                    let (edge, next) = self.sedges[e as usize];
                    let v = self.edge_vertex[edge as usize] as usize;
                    let idx = lookup(f32s[v]);
                    if cur.last() != Some(&idx) {
                        cur.push(idx);
                    }
                    e = next;
                    if e == start {
                        closed = true;
                        break;
                    }
                }
                if !closed {
                    open += 1;
                    continue;
                }
                if cur.len() > 1 && cur.first() == cur.last() {
                    cur.pop();
                }
                if cur.len() >= 3 {
                    cycles.push(cur);
                }
            }
            if !cycles.is_empty() {
                facets.push(Facet {
                    mark: f.mark,
                    cycles,
                });
            }
        }
        if open > 0 {
            msgs.push(Message::warning(format!(
                "Skipped {open} facet cycles that do not close in '{file}'"
            )));
        }
        let unconnected = unconnected_edges(
            facets
                .iter()
                .flat_map(|f| f.cycles.iter().map(Vec::as_slice)),
        );
        if unconnected > 0 {
            msgs.push(Message::error(format!(
                "Non-manifold mesh encountered: {unconnected} unconnected edges"
            )));
        }
        Faces { vertices, facets }
    }
}

/// An `std::istream` over the file, with the few extractions the parser
/// uses. Once an extraction fails the stream stays failed and every later
/// one fails too, which is how an error on one line can surface as CGAL's
/// complaint about the next.
struct In<'a> {
    b: &'a [u8],
    p: usize,
    fail: bool,
}

/// `isspace` in the C locale.
fn is_space(c: u8) -> bool {
    matches!(c, b' ' | b'\t' | b'\n' | b'\x0b' | b'\x0c' | b'\r')
}

impl<'a> In<'a> {
    fn skip_ws(&mut self) {
        while self.p < self.b.len() && is_space(self.b[self.p]) {
            self.p += 1;
        }
    }

    /// `check_sep`: skip whitespace, then match `sep` byte for byte.
    fn sep(&mut self, sep: &str) -> bool {
        if self.fail {
            return false;
        }
        self.skip_ws();
        if self.b[self.p..].starts_with(sep.as_bytes()) {
            self.p += sep.len();
            true
        } else {
            if self.p >= self.b.len() {
                self.fail = true;
            }
            false
        }
    }

    /// `in >> std::string`: the next run of non-whitespace.
    fn word(&mut self) -> Option<&'a [u8]> {
        if self.fail {
            return None;
        }
        self.skip_ws();
        let start = self.p;
        while self.p < self.b.len() && !is_space(self.b[self.p]) {
            self.p += 1;
        }
        if start == self.p {
            self.fail = true;
            return None;
        }
        Some(&self.b[start..self.p])
    }

    /// `test_string(s)`.
    fn is(&mut self, s: &str) -> bool {
        self.word() == Some(s.as_bytes())
    }

    /// `in >> c` for a `char`.
    fn char(&mut self) -> Option<u8> {
        if self.fail {
            return None;
        }
        self.skip_ws();
        match self.b.get(self.p) {
            Some(&c) => {
                self.p += 1;
                Some(c)
            }
            None => {
                self.fail = true;
                None
            }
        }
    }

    fn unget(&mut self) {
        self.p -= 1;
    }

    /// Optional sign and decimal digits, as `num_get` takes them; `None`
    /// (and a failed stream) without digits.
    fn signed_digits(&mut self) -> Option<(bool, &'a [u8])> {
        if self.fail {
            return None;
        }
        self.skip_ws();
        let mut neg = false;
        if let Some(&c @ (b'+' | b'-')) = self.b.get(self.p) {
            neg = c == b'-';
            self.p += 1;
        }
        let start = self.p;
        while self.p < self.b.len() && self.b[self.p].is_ascii_digit() {
            self.p += 1;
        }
        if start == self.p {
            self.fail = true;
            return None;
        }
        Some((neg, &self.b[start..self.p]))
    }

    /// `in >> int`; out of `int` range fails like `num_get`.
    fn int(&mut self) -> Option<i64> {
        let (neg, d) = self.signed_digits()?;
        let mut v: i64 = 0;
        for &c in d {
            v = v * 10 + i64::from(c - b'0');
            if v > i64::from(i32::MAX) + 1 {
                self.fail = true;
                return None;
            }
        }
        let v = if neg { -v } else { v };
        if v > i64::from(i32::MAX) {
            self.fail = true;
            return None;
        }
        Some(v)
    }

    /// `in >> size_t`, for the header counts.
    fn count(&mut self) -> Option<usize> {
        let (neg, d) = self.signed_digits()?;
        let mut v: usize = 0;
        for &c in d {
            match v
                .checked_mul(10)
                .and_then(|v| v.checked_add(usize::from(c - b'0')))
            {
                Some(n) => v = n,
                None => {
                    self.fail = true;
                    return None;
                }
            }
        }
        // `num_get` negates a leading `-` in unsigned arithmetic; no file
        // CGAL writes has one, and a count that large could never be read.
        if neg && v != 0 {
            self.fail = true;
            return None;
        }
        Some(v)
    }

    /// An index checked against `lo..n`: `lo` is 0 in every reader but
    /// the vertex's, which checks only the upper bound (it writes -2 for
    /// "none").
    fn index(&mut self, lo: i64, n: usize) -> Option<i64> {
        let v = self.int()?;
        (v >= lo && v < n as i64).then_some(v)
    }

    /// `in >> bool` (a mark): 0 or 1.
    fn mark(&mut self) -> Option<bool> {
        match self.int() {
            Some(0) => Some(false),
            Some(1) => Some(true),
            Some(_) => {
                self.fail = true;
                None
            }
            None => None,
        }
    }

    /// `gmpz_new_read`: whitespace, an optional sign, more whitespace,
    /// then digits.
    fn gmpz(&mut self) -> Option<(bool, &'a [u8])> {
        if self.fail {
            return None;
        }
        self.skip_ws();
        let mut neg = false;
        if let Some(&c @ (b'+' | b'-')) = self.b.get(self.p) {
            neg = c == b'-';
            self.p += 1;
            self.skip_ws();
        }
        let start = self.p;
        while self.p < self.b.len() && self.b[self.p].is_ascii_digit() {
            self.p += 1;
        }
        if start == self.p {
            self.fail = true;
            return None;
        }
        Some((neg, &self.b[start..self.p]))
    }

    /// Four integers (a point, vector or plane) read only for their syntax.
    fn skip4(&mut self) -> bool {
        (0..4).all(|_| self.gmpz().is_some())
    }

    /// A digit-led list ended by any other character, which is consumed
    /// (the facet, volume and sface lists). Each entry is checked against
    /// `0..n`; `None` when one is out of range.
    fn list(&mut self, n: usize, out: &mut Vec<u32>) -> Option<()> {
        while let Some(c) = self.char() {
            if !c.is_ascii_digit() {
                return Some(());
            }
            self.unget();
            out.push(self.index(0, n)? as u32);
        }
        Some(())
    }
}

/// The seven counts of the header.
struct Counts {
    vn: usize,
    en: usize,
    fn_: usize,
    cn: usize,
    sen: usize,
    sln: usize,
    sfn: usize,
}

/// `SNC_io_parser::read`. Line numbers are those of the CGAL 6.1 header,
/// which match what the nightly prints.
fn parse(bytes: &[u8]) -> Result<Snc, Failure> {
    let mut i = In {
        b: bytes,
        p: 0,
        fail: false,
    };
    if !i.sep("Selective Nef Complex") {
        return Err(warn(1406, "SNC_io_parser::read: no SNC header."));
    }
    let kernel = i.word().unwrap_or_default();
    // "extended" files are read as standard ones: OpenSCAD's kernel is not
    // an extended one, so CGAL adds no infimaximal box and reads the points
    // as plain homogeneous integers.
    if kernel != b"standard" && kernel != b"extended" {
        return Err(Failure::Assertion {
            line: 1411,
            expr: "kernel_type == \"standard\" || kernel_type == \"extended\"",
        });
    }
    let mut header = |name: &str, line: u32, msg: &'static str, even: bool| {
        let n = if i.sep(name) { i.count() } else { None };
        match n {
            Some(n) if !even || n % 2 == 0 => Ok(n),
            _ => Err(warn(line, msg)),
        }
    };
    let c = Counts {
        vn: header(
            "vertices",
            1414,
            "SNC_io_parser::read: wrong vertex line.",
            false,
        )?,
        en: header(
            "halfedges",
            1419,
            "SNC_io_parser::read: wrong edge line.",
            true,
        )?,
        fn_: header(
            "facets",
            1424,
            "SNC_io_parser::read: wrong facet line.",
            true,
        )?,
        cn: header(
            "volumes",
            1428,
            "SNC_io_parser::read: wrong volume line.",
            false,
        )?,
        sen: header(
            "shalfedges",
            1433,
            "SNC_io_parser::read: wrong sedge line.",
            false,
        )?,
        sln: header(
            "shalfloops",
            1438,
            "SNC_io_parser::read: wrong sloop line.",
            false,
        )?,
        sfn: header(
            "sfaces",
            1443,
            "SNC_io_parser::read: wrong sface line.",
            false,
        )?,
    };
    // Capacities come from the file, so bound them by what the file could
    // hold (every line is longer than 8 bytes) rather than trust a count.
    let cap = |n: usize| n.min(bytes.len() / 8);
    let mut snc = Snc {
        points: Vec::with_capacity(cap(c.vn)),
        edge_vertex: Vec::with_capacity(cap(c.en)),
        facets: Vec::with_capacity(cap(c.fn_)),
        volume_marks: Vec::with_capacity(cap(c.cn)),
        sedges: Vec::with_capacity(cap(c.sen)),
    };
    for _ in 0..c.vn {
        match read_vertex(&mut i, &c) {
            Some(Some(p)) => snc.points.push(p),
            Some(None) => {
                return Err(Failure::Infinite {
                    vertex: snc.points.len(),
                });
            }
            None => return Err(warn(1473, "SNC_io_parser::read: error in node line")),
        }
    }
    for _ in 0..c.en {
        let v =
            read_edge(&mut i, &c).ok_or(warn(1482, "SNC_io_parser::read: error in edge line"))?;
        snc.edge_vertex.push(v);
    }
    for _ in 0..c.fn_ {
        let f =
            read_facet(&mut i, &c).ok_or(warn(1492, "SNC_io_parser::read: error in facet line"))?;
        snc.facets.push(f);
    }
    for _ in 0..c.cn {
        let m = read_volume(&mut i, &c)
            .ok_or(warn(1500, "SNC_io_parser::read: error in volume line"))?;
        snc.volume_marks.push(m);
    }
    for _ in 0..c.sen {
        let s =
            read_sedge(&mut i, &c).ok_or(warn(1508, "SNC_io_parser::read: error in sedge line"))?;
        snc.sedges.push(s);
    }
    for _ in 0..c.sln {
        read_sloop(&mut i, &c).ok_or(warn(1516, "SNC_io_parser::read: error in sloop line"))?;
    }
    for _ in 0..c.sfn {
        read_sface(&mut i, &c).ok_or(warn(1524, "SNC_io_parser::read: error in sface line"))?;
    }
    Ok(snc)
}

/// Each reader returns `None` where CGAL's returns false: a separator that
/// does not match (which includes any read from a failed stream) or an
/// index out of range. The mark is read after the last check, so a bad
/// mark only fails the stream, and the next line reports it.
fn end(i: &mut In, ok: bool) -> Option<bool> {
    if !ok {
        return None;
    }
    // A failed mark read is the next line's error, as in CGAL.
    Some(i.mark().unwrap_or(false))
}

/// `read_vertex`: `index { svs sve, ses see, sfs sfe, sl | point } mark`.
/// The inner `None` is a point at infinity.
fn read_vertex(i: &mut In, c: &Counts) -> Option<Option<[f64; 3]>> {
    i.int()?;
    let mut ok = i.is("{");
    for (n, sep) in [(c.en, ","), (c.sen, ","), (c.sfn, ",")] {
        i.index(i64::MIN, n)?;
        i.index(i64::MIN, n)?;
        ok = ok && i.is(sep);
    }
    i.index(i64::MIN, c.sln)?;
    ok = ok && i.is("|");
    let mut h = [(false, &b""[..]); 4];
    for x in &mut h {
        *x = i.gmpz().unwrap_or((false, b"0"));
    }
    ok = ok && i.is("}");
    end(i, ok)?;
    let (wneg, w) = h[3];
    Some(
        (0..3)
            .map(|k| ratio_to_f64(h[k].0 != wneg, h[k].1, w))
            .collect::<Option<Vec<f64>>>()
            .map(|v| [v[0], v[1], v[2]]),
    )
}

/// `read_edge`: `index { twin, vertex, isolated object | vector } mark`.
fn read_edge(i: &mut In, c: &Counts) -> Option<u32> {
    i.int()?;
    let mut ok = i.is("{");
    i.index(0, c.en)?;
    ok = ok && i.is(",");
    let v = i.index(0, c.vn)?;
    ok = ok && i.is(",");
    let isolated = i.int()? != 0;
    i.index(0, if isolated { c.sfn } else { c.sen })?;
    ok = ok && i.is("|");
    ok = ok && i.skip4();
    ok = ok && i.is("}");
    end(i, ok)?;
    Some(v as u32)
}

/// `read_facet`: `index { twin, sedges , sloops , volume | plane } mark`.
fn read_facet(i: &mut In, c: &Counts) -> Option<RawFacet> {
    i.int()?;
    let mut ok = i.is("{");
    i.index(0, c.fn_)?;
    ok = ok && i.is(",");
    let mut entries = Vec::new();
    i.list(c.sen, &mut entries)?;
    i.list(c.sln, &mut Vec::new())?;
    let volume = i.index(0, c.cn)? as u32;
    ok = ok && i.is("|");
    ok = ok && i.skip4();
    ok = ok && i.is("}");
    let mark = end(i, ok)?;
    Some(RawFacet {
        entries,
        volume,
        mark,
    })
}

/// `read_volume`: `index { sfaces } mark`. CGAL checks nothing after the
/// opening brace but the list's indices.
fn read_volume(i: &mut In, c: &Counts) -> Option<bool> {
    i.int()?;
    let ok = i.is("{");
    i.list(c.sfn, &mut Vec::new())?;
    end(i, ok)
}

/// `read_sedge`: `index { twin, sprev, snext, source, sface, prev, next,
/// facet | circle } mark`. Returns the source halfedge and `next`.
fn read_sedge(i: &mut In, c: &Counts) -> Option<(u32, u32)> {
    i.int()?;
    let mut ok = i.is("{");
    let mut f = [0i64; 8];
    let ranges = [c.sen, c.sen, c.sen, c.en, c.sfn, c.sen, c.sen, c.fn_];
    for (k, n) in ranges.into_iter().enumerate() {
        f[k] = i.index(0, n)?;
        ok = ok && i.is(if k == 7 { "|" } else { "," });
    }
    ok = ok && i.skip4();
    ok = ok && i.is("}");
    end(i, ok)?;
    Some((f[3] as u32, f[6] as u32))
}

/// `read_sloop`: `index { twin, sface, facet | circle } mark`.
fn read_sloop(i: &mut In, c: &Counts) -> Option<()> {
    i.int()?;
    let mut ok = i.is("{");
    for (n, sep) in [(c.sln, ","), (c.sfn, ","), (c.fn_, "|")] {
        i.index(0, n)?;
        ok = ok && i.is(sep);
    }
    ok = ok && i.skip4();
    ok = ok && i.is("}");
    end(i, ok).map(|_| ())
}

/// `read_sface`: `index { vertex, sedges svertices sloops volume } mark`,
/// each list ended by the next separator.
fn read_sface(i: &mut In, c: &Counts) -> Option<()> {
    i.int()?;
    let mut ok = i.is("{");
    i.index(0, c.vn)?;
    ok = ok && i.is(",");
    let mut sink = Vec::new();
    i.list(c.sen, &mut sink)?;
    i.list(c.en, &mut sink)?;
    i.list(c.sln, &mut sink)?;
    i.index(0, c.cn)?;
    ok = ok && i.is("}");
    end(i, ok).map(|_| ())
}

/// `CGAL::to_double` of the rational `num / den` given as decimal digit
/// strings (`neg` is the sign of the quotient): GMP's `mpq_get_d`, which
/// truncates toward zero rather than rounding to nearest (1/10 gives
/// 0.09999999999999999167, one ulp below the nearest double). `None` when
/// `den` is zero.
pub fn ratio_to_f64(neg: bool, num: &[u8], den: &[u8]) -> Option<f64> {
    let d = big::Nat::from_decimal(den);
    if d.is_zero() {
        return None;
    }
    let n = big::Nat::from_decimal(num);
    let v = big::trunc_div_f64(&n, &d);
    Some(if neg { -v } else { v })
}

/// Just enough unsigned big-integer arithmetic for [`ratio_to_f64`].
mod big {
    use std::cmp::Ordering;

    /// A magnitude in base 2^64, least significant limb first, without
    /// high zero limbs.
    #[derive(Debug, Clone, PartialEq)]
    pub struct Nat(Vec<u64>);

    impl Nat {
        pub fn from_decimal(d: &[u8]) -> Nat {
            let mut v: Vec<u64> = Vec::with_capacity(d.len() / 19 + 1);
            for chunk in d.chunks(19) {
                let mut c: u64 = 0;
                for &x in chunk {
                    c = c * 10 + u64::from(x - b'0');
                }
                let mul = 10u128.pow(chunk.len() as u32);
                let mut carry = u128::from(c);
                for l in &mut v {
                    let t = u128::from(*l) * mul + carry;
                    *l = t as u64;
                    carry = t >> 64;
                }
                if carry != 0 {
                    v.push(carry as u64);
                }
            }
            let mut n = Nat(v);
            n.trim();
            n
        }

        fn trim(&mut self) {
            while self.0.last() == Some(&0) {
                self.0.pop();
            }
        }

        pub fn is_zero(&self) -> bool {
            self.0.is_empty()
        }

        pub fn bits(&self) -> u64 {
            match self.0.last() {
                None => 0,
                Some(&top) => 64 * (self.0.len() as u64 - 1) + u64::from(64 - top.leading_zeros()),
            }
        }

        fn bit(&self, i: u64) -> bool {
            self.0
                .get((i / 64) as usize)
                .is_some_and(|l| (l >> (i % 64)) & 1 == 1)
        }

        fn shl(&self, s: u64) -> Nat {
            let (limbs, bits) = ((s / 64) as usize, (s % 64) as u32);
            let mut v = vec![0u64; limbs];
            let mut carry = 0u64;
            for &l in &self.0 {
                v.push(if bits == 0 { l } else { (l << bits) | carry });
                carry = if bits == 0 { 0 } else { l >> (64 - bits) };
            }
            v.push(carry);
            let mut n = Nat(v);
            n.trim();
            n
        }

        fn shr(&self, s: u64) -> Nat {
            let (limbs, bits) = ((s / 64) as usize, (s % 64) as u32);
            if limbs >= self.0.len() {
                return Nat(Vec::new());
            }
            let src = &self.0[limbs..];
            let mut v: Vec<u64> = (0..src.len())
                .map(|k| {
                    let lo = src[k] >> bits;
                    let hi = if bits == 0 {
                        0
                    } else {
                        src.get(k + 1).map_or(0, |h| h << (64 - bits))
                    };
                    lo | hi
                })
                .collect();
            while v.last() == Some(&0) {
                v.pop();
            }
            Nat(v)
        }

        fn cmp(&self, o: &Nat) -> Ordering {
            self.0
                .len()
                .cmp(&o.0.len())
                .then_with(|| self.0.iter().rev().cmp(o.0.iter().rev()))
        }

        /// `self -= o`, for `self >= o`.
        fn sub(&mut self, o: &Nat) {
            let mut borrow = false;
            for k in 0..self.0.len() {
                let b = o.0.get(k).copied().unwrap_or(0);
                let (r1, o1) = self.0[k].overflowing_sub(b);
                let (r2, o2) = r1.overflowing_sub(u64::from(borrow));
                self.0[k] = r2;
                borrow = o1 || o2;
            }
            self.trim();
        }

        /// `self = 2 * self + bit`.
        fn push_bit(&mut self, bit: bool) {
            let mut carry = u64::from(bit);
            for l in &mut self.0 {
                let c = *l >> 63;
                *l = (*l << 1) | carry;
                carry = c;
            }
            if carry != 0 {
                self.0.push(carry);
            }
        }
    }

    /// 2^e as a double, for `e` in -1074..=1023.
    fn pow2(e: i64) -> f64 {
        if e >= -1022 {
            f64::from_bits(((e + 1023) as u64) << 52)
        } else {
            f64::from_bits(1u64 << (e + 1074))
        }
    }

    /// `n / d` for `d > 0`, truncated to a double.
    pub fn trunc_div_f64(n: &Nat, d: &Nat) -> f64 {
        if n.is_zero() {
            return 0.0;
        }
        // Integers and dyadic fractions with a short numerator: the
        // division is exact, so no truncation to do.
        if n.0.len() == 1 && n.0[0] < 1 << 53 && d.0.len() == 1 && d.0[0].is_power_of_two() {
            return n.0[0] as f64 / d.0[0] as f64;
        }
        // q = floor(n * 2^s / d), with s chosen so that q has 55 or 56
        // bits. Shifting n right instead of d left when s < 0 is exact:
        // floor(floor(a / b) / c) = floor(a / (b c)).
        let s = 55 - n.bits() as i64 + d.bits() as i64;
        let big = if s >= 0 {
            n.shl(s as u64)
        } else {
            n.shr((-s) as u64)
        };
        // Restoring division, one quotient bit at a time. The remainder
        // starts as the top bits of `big` that are shorter than `d`.
        let m = big.bits() + 1 - d.bits();
        let mut r = big.shr(m);
        let mut q: u64 = 0;
        for k in (0..m).rev() {
            r.push_bit(big.bit(k));
            if r.cmp(d) != Ordering::Less {
                r.sub(d);
                q |= 1 << k;
            }
        }
        // Keep the top 53 bits (truncating) and scale back.
        let t = i64::from(64 - q.leading_zeros()) - 53;
        let mut q = q >> t;
        let mut e = t - s;
        let top = e + 52;
        if top > 1023 {
            return f64::INFINITY;
        }
        if top < -1022 {
            // Subnormal: fewer bits are left, truncated too.
            let shift = -1074 - e;
            if shift >= 53 {
                return 0.0;
            }
            q >>= shift;
            e = -1074;
        }
        q as f64 * pow2(e)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CUBE: &str = include_str!("../tests/data/cube.nef3");
    const HOLE: &str = include_str!("../tests/data/square-hole.nef3");
    const CAVITY: &str = include_str!("../tests/data/cavity.nef3");

    fn faces(text: &str) -> (Faces, Vec<Message>) {
        let mut msgs = Vec::new();
        let f = read(text.as_bytes(), "t.nef3", 1, &mut msgs);
        (f, msgs)
    }

    /// Twice the signed area vector of a cycle.
    fn area(f: &Faces, c: &[u32]) -> [f64; 3] {
        let mut n = [0.0; 3];
        for k in 0..c.len() {
            let a = f.vertices[c[k] as usize];
            let b = f.vertices[c[(k + 1) % c.len()] as usize];
            n[0] += a[1] * b[2] - a[2] * b[1];
            n[1] += a[2] * b[0] - a[0] * b[2];
            n[2] += a[0] * b[1] - a[1] * b[0];
        }
        n
    }

    /// The enclosed volume of closed, outward-facing polygons with holes.
    fn volume(f: &Faces) -> f64 {
        let mut v = 0.0;
        for facet in &f.facets {
            for c in &facet.cycles {
                let n = area(f, c);
                let p = f.vertices[c[0] as usize];
                v += (n[0] * p[0] + n[1] * p[1] + n[2] * p[2]) / 6.0;
            }
        }
        v
    }

    #[test]
    fn cube_gives_six_outward_quads() {
        // cube(2, center = true), written by the nightly with --backend=cgal.
        let (f, msgs) = faces(CUBE);
        assert!(msgs.is_empty(), "{msgs:?}");
        assert_eq!(f.vertices.len(), 8);
        assert_eq!(f.facets.len(), 6);
        assert!(f.facets.iter().all(|x| x.mark && x.cycles.len() == 1));
        assert!(f.facets.iter().all(|x| x.cycles[0].len() == 4));
        assert!((volume(&f) - 8.0).abs() < 1e-12);
        for v in &f.vertices {
            assert!(v.iter().all(|c| c.abs() == 1.0));
        }
    }

    #[test]
    fn facets_with_holes_keep_their_hole_cycles() {
        // difference() { cube(4, center = true); cube([2, 2, 6], center = true); }
        let (f, msgs) = faces(HOLE);
        assert!(msgs.is_empty(), "{msgs:?}");
        assert_eq!(f.vertices.len(), 16);
        let holed: Vec<&Facet> = f.facets.iter().filter(|x| x.cycles.len() == 2).collect();
        assert_eq!(holed.len(), 2);
        for h in holed {
            // Outer and hole wind opposite ways.
            let (a, b) = (area(&f, &h.cycles[0]), area(&f, &h.cycles[1]));
            assert!(a[2] * b[2] < 0.0);
        }
        assert!((volume(&f) - (64.0 - 16.0)).abs() < 1e-9);
    }

    #[test]
    fn an_empty_cavity_is_a_second_inward_shell() {
        // difference() { cube(3); translate([1, 1, 1]) cube(1); }
        let (f, msgs) = faces(CAVITY);
        assert!(msgs.is_empty(), "{msgs:?}");
        assert_eq!(f.facets.len(), 12);
        assert!((volume(&f) - 26.0).abs() < 1e-9);
    }

    #[test]
    fn unmarking_the_solid_keeps_both_sides_of_every_facet() {
        // The nightly makes 24 triangles of cube.nef3 with its inner
        // volume's mark cleared: every halffacet then faces empty space.
        let text = CUBE.replace("\n1 { 1 } 1\n", "\n1 { 1 } 0\n");
        let (f, _) = faces(&text);
        assert_eq!(f.facets.len(), 12);
        assert!(volume(&f).abs() < 1e-12);
    }

    fn failure(text: &str) -> Vec<String> {
        let (f, msgs) = faces(text);
        assert!(f.facets.is_empty());
        msgs.into_iter().map(|m| m.text).collect()
    }

    fn explanation(msgs: &[String]) -> &str {
        let last = msgs[1].rsplit('\n').next().unwrap_or_default();
        last.strip_prefix("Explanation: ").unwrap_or(last)
    }

    /// The fixture with line `k` (0-based) replaced.
    fn with_line(k: usize, line: &str) -> String {
        let mut lines: Vec<&str> = CUBE.lines().collect();
        lines[k] = line;
        lines.join("\n") + "\n"
    }

    // Line numbers in cube.nef3: 9 header lines, then 8 vertices, 24
    // halfedges, 12 facets, 2 volumes, 48 shalfedges and 16 sfaces.
    const FIRST_VERTEX: usize = 9;
    const FIRST_EDGE: usize = 17;
    const LAST_EDGE: usize = 40;

    #[test]
    fn broken_files_fail_on_the_line_cgal_names() {
        // Like tests/data/nef3/broken.nef3: a letter in a halfedge's vector.
        let broken = with_line(FIRST_EDGE, "0 { 5, 0, 0 1 | 1 a 0 1 } 1");
        let m = failure(&broken);
        assert_eq!(
            m[0],
            "Failure trying to import 't.nef3', import() at line 1"
        );
        assert!(m[1].starts_with("CGAL ERROR: warning condition failed!\nExpr: false\n"));
        assert!(m[1].contains("\nLine: 1482\n"));
        assert_eq!(explanation(&m), "SNC_io_parser::read: error in edge line");
        // Cases checked against the nightly: a bad mark is the next line's
        // error, a bad header count its own, and a truncated file fails in
        // the section it stops in.
        let mark = with_line(LAST_EDGE, "23 { 2, 7, 0 45 | 0 0 1 1 } x");
        assert_eq!(
            explanation(&failure(&mark)),
            "SNC_io_parser::read: error in facet line"
        );
        let odd = CUBE.replace("facets     12", "facets     11");
        assert_eq!(
            explanation(&failure(&odd)),
            "SNC_io_parser::read: wrong facet line."
        );
        let truncated = &CUBE[..2000];
        assert_eq!(
            explanation(&failure(truncated)),
            "SNC_io_parser::read: error in sedge line"
        );
        assert_eq!(
            explanation(&failure("")),
            "SNC_io_parser::read: no SNC header."
        );
        let m = failure(&CUBE.replace("standard", "bogus"));
        assert!(m[1].starts_with("CGAL ERROR: assertion violation!\nExpr: kernel_type =="));
        assert!(m[1].ends_with("Line: 1411"));
        // An "extended" file reads like a standard one, as in the nightly.
        let (f, _) = faces(&CUBE.replace("standard", "extended"));
        assert_eq!(f.facets.len(), 6);
    }

    #[test]
    fn degenerate_input_is_rejected_or_skipped() {
        // An index past the end is an error in its line.
        let bad = with_line(FIRST_EDGE, "0 { 5, 99, 0 1 | 1 0 0 1 } 1");
        assert_eq!(
            explanation(&failure(&bad)),
            "SNC_io_parser::read: error in edge line"
        );
        // A vertex at infinity.
        let inf = with_line(FIRST_VERTEX, "0 { 0 2, 0 5, 0 1, -2 | -1 1 1 0 } 1");
        assert_eq!(
            failure(&inf)[1],
            "Vertex 0 is at infinity (homogeneous weight 0)"
        );
        // A facet cycle whose `next` never comes back to its start (0 -> 1
        // -> 2 -> 1 ...): skipped with a warning, where CGAL would hang.
        let snc = Snc {
            points: vec![[0.0; 3], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
            edge_vertex: vec![0, 1, 2],
            facets: vec![RawFacet {
                entries: vec![0],
                volume: 0,
                mark: true,
            }],
            volume_marks: vec![false],
            sedges: vec![(0, 1), (1, 2), (2, 1)],
        };
        let mut msgs = Vec::new();
        let f = snc.faces("t.nef3", &mut msgs);
        assert!(f.facets.is_empty());
        assert!(msgs[0].text.contains("do not close"), "{msgs:?}");
        // Header and the outer volume only: an empty polyhedron.
        let empty = "Selective Nef Complex\nstandard\nvertices 0\nhalfedges 0\nfacets 0\n\
                     volumes 1\nshalfedges 0\nshalfloops 0\nsfaces 0\n0 { } 0\n";
        let (f, msgs) = faces(empty);
        assert!(f.facets.is_empty() && msgs.is_empty(), "{msgs:?}");
    }

    #[test]
    fn to_double_truncates_like_mpq_get_d() {
        // Values from GMP's mpq_get_d on this machine.
        let r = |n: &str, d: &str| ratio_to_f64(false, n.as_bytes(), d.as_bytes());
        assert_eq!(r("1", "10"), Some(0.09999999999999999));
        assert_eq!(r("7", "10"), Some(0.7));
        assert_eq!(r("1", "3"), Some(1.0 / 3.0));
        assert_eq!(r("3", "1"), Some(3.0));
        assert_eq!(r("5", "4"), Some(1.25));
        assert_eq!(r("1", "0"), None);
        assert_eq!(r("0", "7"), Some(0.0));
        // Many limbs: 10^40 / (3 * 10^39) = 3.33..., truncated.
        let n = format!("1{}", "0".repeat(40));
        let d = format!("3{}", "0".repeat(39));
        assert_eq!(r(&n, &d), Some(10.0 / 3.0 - 4.440892098500626e-16));
        // 2^53 + 1 is not a double: truncation keeps 2^53.
        assert_eq!(r("9007199254740993", "1"), Some(9007199254740992.0));
        assert_eq!(r(&"9".repeat(400), "1"), Some(f64::INFINITY));
        assert_eq!(ratio_to_f64(true, b"1", b"4"), Some(-0.25));
    }
}
