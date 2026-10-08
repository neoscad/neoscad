//! The edge selector language of `fillet_edges()` and `chamfer_edges()`
//! (`docs/fillets.md`, section 5.2): parsing and printing only. Which
//! edges a selector matches is decided on the child's B-rep, in
//! `geom::fillet`; the evaluator parses once and stores the result on the
//! node, so geometry never parses strings.
//!
//! The strings are a subset of CadQuery's selector strings with the same
//! meaning, plus NeoSCAD's own atoms (`convex`, `new`, `child(i, j)`,
//! `part(name)`, `@anchor`, `box(...)`). Precedence follows CadQuery's
//! grammar as written (`cadquery/selectors.py`, `_makeExpressionGrammar`,
//! retrieved 2026-10-08), which is not the order its documentation
//! suggests: `and` binds tightest, then `or`, then `exc`/`except`, and
//! `not` is the loosest, negating everything to its right. So
//! `not convex and |z` is `not (convex and |z)`, as in CadQuery. CadQuery
//! rejects `not` after a binary operator; here `|z and not >x` is
//! accepted, with the same rule (`not` takes everything to its right up to
//! the closing parenthesis), so every string CadQuery accepts means the
//! same here and the extension cannot change that.
//!
//! Keywords and atoms are case-insensitive (CadQuery's `>Z` is `>z`); part
//! and anchor names keep their case, since part names are case-sensitive
//! everywhere else.

use std::fmt;

/// A direction: an axis, or any non-zero vector.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Dir {
    X,
    Y,
    Z,
    Vector([f64; 3]),
}

impl Dir {
    pub fn vector(self) -> [f64; 3] {
        match self {
            Dir::X => [1.0, 0.0, 0.0],
            Dir::Y => [0.0, 1.0, 0.0],
            Dir::Z => [0.0, 0.0, 1.0],
            Dir::Vector(v) => v,
        }
    }
}

/// An edge's curve kind (`%line`, ...).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Curve {
    Line,
    Circle,
    Ellipse,
    BSpline,
}

impl Curve {
    const ALL: [Curve; 4] = [Curve::Line, Curve::Circle, Curve::Ellipse, Curve::BSpline];

    pub fn name(self) -> &'static str {
        match self {
            Curve::Line => "line",
            Curve::Circle => "circle",
            Curve::Ellipse => "ellipse",
            Curve::BSpline => "bspline",
        }
    }
}

/// One selector atom (`docs/fillets.md`, the table in 5.2).
#[derive(Debug, Clone, PartialEq)]
pub enum Atom {
    /// Every selectable edge (`all`; `none` is `not all`).
    All,
    Convex,
    Concave,
    /// `%line`, `%circle`, `%ellipse`, `%bspline`.
    Curve(Curve),
    /// `|d`, and a bare axis (`x`, CadQuery's `X`).
    Parallel(Dir),
    /// `#d`.
    Perpendicular(Dir),
    /// `>d` (`max`) and `<d`: the edges whose centre is farthest along
    /// or against the direction.
    Farthest {
        max: bool,
        dir: Dir,
    },
    /// `>>d[i]` (`max`) and `<<d[i]`: the i-th group by centre. Without an
    /// index, -1, as CadQuery's `_chooseSelector` makes it.
    Nth {
        max: bool,
        dir: Dir,
        index: i64,
    },
    /// `new`: edges between faces of different leaves.
    New,
    /// `child(i)` and `child(i, j)`.
    Child(u32, Option<u32>),
    /// `part(name)`, the full dotted name (needs `--enable part`).
    Part(String),
    /// `@name` (needs `--enable query`).
    Anchor(String),
    /// `box(x0, y0, z0, x1, y1, z1)`: edges wholly inside.
    Box {
        min: [f64; 3],
        max: [f64; 3],
    },
}

/// A selector expression.
#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    Atom(Atom),
    Not(Box<Expr>),
    And(Box<Expr>, Box<Expr>),
    Or(Box<Expr>, Box<Expr>),
    /// Set difference (`exc`, `except`).
    Exc(Box<Expr>, Box<Expr>),
}

/// One item of an `edges` or `except` argument.
#[derive(Debug, Clone, PartialEq)]
pub enum Item {
    /// A selector string.
    Expr(Expr),
    /// A BOSL2-style direction vector on the child's bounding box, each
    /// entry -1, 0 or 1, not all 0.
    Descriptor([i8; 3]),
}

/// An `edges` or `except` argument: the union of its items.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Selector {
    pub items: Vec<Item>,
}

impl Selector {
    /// `edges = "all"`, the default.
    pub fn all() -> Selector {
        Selector {
            items: vec![Item::Expr(Expr::Atom(Atom::All))],
        }
    }

    /// Whether this is `"all"` (written or by default): unsupported edges
    /// it matches are warnings, not errors (`docs/fillets.md`, section 8).
    pub fn is_all(&self) -> bool {
        matches!(self.items.as_slice(), [Item::Expr(Expr::Atom(Atom::All))])
    }

    /// Every atom of every string item, in order.
    pub fn atoms(&self) -> Vec<&Atom> {
        let mut out = Vec::new();
        for item in &self.items {
            if let Item::Expr(e) = item {
                e.atoms(&mut out);
            }
        }
        out
    }
}

impl Expr {
    /// The atoms of the expression, left to right.
    pub fn atoms<'a>(&'a self, out: &mut Vec<&'a Atom>) {
        let mut stack = vec![self];
        while let Some(e) = stack.pop() {
            match e {
                Expr::Atom(a) => out.push(a),
                Expr::Not(x) => stack.push(x),
                Expr::And(a, b) | Expr::Or(a, b) | Expr::Exc(a, b) => {
                    stack.push(b);
                    stack.push(a);
                }
            }
        }
    }
}

/// Which atoms need an extension the run has not turned on.
#[derive(Debug, Clone, Copy, Default)]
pub struct Allowed {
    /// `part(name)`: `--enable part`.
    pub part: bool,
    /// `@name`: `--enable query`.
    pub anchor: bool,
}

/// A selector string that does not parse: the byte range in the string
/// (`start == end` for "something is missing here"), the message, and a
/// replacement for that range when one fix is clear ("did you mean").
#[derive(Debug, Clone, PartialEq)]
pub struct ParseError {
    pub start: usize,
    pub end: usize,
    pub message: String,
    pub suggestion: Option<String>,
}

type R<T> = Result<T, ParseError>;

/// Parse a selector string.
pub fn parse(text: &str, allowed: Allowed) -> R<Expr> {
    let mut p = Parser {
        s: text.as_bytes(),
        pos: 0,
        allowed,
        opens: Vec::new(),
    };
    p.ws();
    if p.at_end() {
        return Err(p.err(
            0,
            text.len(),
            "the selector is empty; write \"all\" for every edge",
            None,
        ));
    }
    let e = p.expr()?;
    p.ws();
    if !p.at_end() {
        return Err(p.trailing());
    }
    Ok(e)
}

/// Words the atoms and operators use, for "did you mean".
const WORDS: [&str; 15] = [
    "all", "none", "convex", "concave", "new", "child", "part", "box", "x", "y", "z", "and", "or",
    "not", "exc",
];

struct Parser<'a> {
    s: &'a [u8],
    pos: usize,
    allowed: Allowed,
    /// Positions of the open parentheses being parsed, for the message
    /// when one is never closed.
    opens: Vec<usize>,
}

impl Parser<'_> {
    fn at_end(&self) -> bool {
        self.pos >= self.s.len()
    }

    fn peek(&self) -> Option<u8> {
        self.s.get(self.pos).copied()
    }

    fn ws(&mut self) {
        while self.peek().is_some_and(|c| c.is_ascii_whitespace()) {
            self.pos += 1;
        }
    }

    fn err(
        &self,
        start: usize,
        end: usize,
        m: impl Into<String>,
        fix: Option<String>,
    ) -> ParseError {
        ParseError {
            start,
            end,
            message: m.into(),
            suggestion: fix,
        }
    }

    fn text(&self, a: usize, b: usize) -> String {
        String::from_utf8_lossy(&self.s[a..b]).into_owned()
    }

    /// The word at the position (letters, digits, `_`), without consuming
    /// it: its end and its lower-case form.
    fn word_at(&self, at: usize) -> Option<(usize, String)> {
        let s = self.s;
        if !s
            .get(at)
            .is_some_and(|c| c.is_ascii_alphabetic() || *c == b'_')
        {
            return None;
        }
        let mut end = at;
        while s
            .get(end)
            .is_some_and(|c| c.is_ascii_alphanumeric() || *c == b'_')
        {
            end += 1;
        }
        Some((end, self.text(at, end).to_ascii_lowercase()))
    }

    /// Consume keyword `k` (after whitespace) if it is the next word.
    fn keyword(&mut self, k: &[&str]) -> bool {
        self.ws();
        match self.word_at(self.pos) {
            Some((end, w)) if k.contains(&w.as_str()) => {
                self.pos = end;
                true
            }
            _ => false,
        }
    }

    /// `exc` level: `or`-expressions joined by `exc`/`except`. This is the
    /// top of the grammar; `not` (looser still) is handled where an
    /// operand starts, taking everything to its right.
    fn expr(&mut self) -> R<Expr> {
        let mut l = self.or()?;
        while self.keyword(&["exc", "except"]) {
            let r = self.or()?;
            l = Expr::Exc(Box::new(l), Box::new(r));
        }
        Ok(l)
    }

    fn or(&mut self) -> R<Expr> {
        let mut l = self.and()?;
        while self.keyword(&["or"]) {
            let r = self.and()?;
            l = Expr::Or(Box::new(l), Box::new(r));
        }
        Ok(l)
    }

    fn and(&mut self) -> R<Expr> {
        let mut l = self.operand()?;
        while self.keyword(&["and"]) {
            let r = self.operand()?;
            l = Expr::And(Box::new(l), Box::new(r));
        }
        Ok(l)
    }

    fn operand(&mut self) -> R<Expr> {
        if self.keyword(&["not"]) {
            return Ok(Expr::Not(Box::new(self.expr()?)));
        }
        self.ws();
        let start = self.pos;
        let Some(c) = self.peek() else {
            return Err(self.err(start, start, "expected a selector at the end", None));
        };
        match c {
            b'(' => {
                self.pos += 1;
                self.opens.push(start);
                let e = self.expr()?;
                self.ws();
                if self.peek() != Some(b')') {
                    if self.at_end() {
                        return Err(self.err(
                            start,
                            start + 1,
                            "unbalanced parentheses: this '(' is never closed",
                            None,
                        ));
                    }
                    return Err(self.trailing());
                }
                self.pos += 1;
                self.opens.pop();
                Ok(e)
            }
            b')' => Err(self.err(start, start + 1, "expected a selector before ')'", None)),
            b'%' => {
                self.pos += 1;
                let Some((end, w)) = self.word_at(self.pos) else {
                    return Err(self.err(
                        start,
                        self.pos,
                        "'%' needs a curve kind: %line, %circle, %ellipse or %bspline",
                        None,
                    ));
                };
                let at = self.pos;
                self.pos = end;
                match Curve::ALL.into_iter().find(|k| k.name() == w) {
                    Some(k) => Ok(Expr::Atom(Atom::Curve(k))),
                    None => {
                        let fix = closest(&w, Curve::ALL.iter().map(|k| k.name()));
                        Err(self.unknown(
                            at,
                            end,
                            &format!("curve kind '%{}'", self.text(at, end)),
                            fix,
                            "line, circle, ellipse, bspline",
                        ))
                    }
                }
            }
            b'|' | b'#' => {
                self.pos += 1;
                let d = self.dir(start)?;
                Ok(Expr::Atom(if c == b'|' {
                    Atom::Parallel(d)
                } else {
                    Atom::Perpendicular(d)
                }))
            }
            b'>' | b'<' => {
                let max = c == b'>';
                self.pos += 1;
                if self.peek() == Some(c) {
                    self.pos += 1;
                    let dir = self.dir(start)?;
                    let index = self.index()?.unwrap_or(-1);
                    return Ok(Expr::Atom(Atom::Nth { max, dir, index }));
                }
                let dir = self.dir(start)?;
                let after = self.pos;
                if let Some(i) = self.index()? {
                    let op = if max { ">>" } else { "<<" };
                    let written = self.text(start, self.pos);
                    let fix = format!("{op}{}", &written[1..]);
                    return Err(self.err(
                        start,
                        self.pos,
                        format!(
                            "'{written}' (CadQuery's n-th parallel edge) is not supported; \
                             '{op}…[{i}]' picks the n-th group of edges by centre"
                        ),
                        Some(fix),
                    ));
                }
                self.pos = after;
                Ok(Expr::Atom(Atom::Farthest { max, dir }))
            }
            b'+' | b'-' => {
                self.pos += 1;
                self.dir(start)?;
                let written = self.text(start, self.pos);
                Err(self.err(
                    start,
                    self.pos,
                    format!(
                        "'{written}' selects edges by their orientation, which an OpenSCAD \
                         model does not let you choose; use '|{}' for edges parallel to it",
                        &written[1..]
                    ),
                    Some(format!("|{}", &written[1..])),
                ))
            }
            b'@' => {
                self.pos += 1;
                let name = self.name_chars();
                if name.is_empty() {
                    return Err(self.err(start, self.pos, "'@' needs an anchor name", None));
                }
                if !self.allowed.anchor {
                    return Err(self.err(
                        start,
                        self.pos,
                        format!("'@{name}' selects by anchor, which needs --enable query"),
                        None,
                    ));
                }
                Ok(Expr::Atom(Atom::Anchor(name)))
            }
            _ => self.word_atom(start),
        }
    }

    /// The characters of an anchor or part name: letters, digits, `_`,
    /// `.` and `-`.
    fn name_chars(&mut self) -> String {
        let a = self.pos;
        while self
            .peek()
            .is_some_and(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'.' | b'-'))
        {
            self.pos += 1;
        }
        self.text(a, self.pos)
    }

    fn word_atom(&mut self, start: usize) -> R<Expr> {
        let Some((end, w)) = self.word_at(start) else {
            let c = self.text(start, next_char(self.s, start));
            return Err(self.err(
                start,
                next_char(self.s, start),
                format!("unexpected '{c}': expected a selector"),
                None,
            ));
        };
        self.pos = end;
        let atom = match w.as_str() {
            "all" => Atom::All,
            "none" => return Ok(Expr::Not(Box::new(Expr::Atom(Atom::All)))),
            "convex" => Atom::Convex,
            "concave" => Atom::Concave,
            "new" => Atom::New,
            "x" => Atom::Parallel(Dir::X),
            "y" => Atom::Parallel(Dir::Y),
            "z" => Atom::Parallel(Dir::Z),
            "child" => {
                self.open(start, "child(i) or child(i, j)")?;
                let i = self.uint()?;
                self.ws();
                let j = if self.peek() == Some(b',') {
                    self.pos += 1;
                    Some(self.uint()?)
                } else {
                    None
                };
                self.close("child(i) or child(i, j)")?;
                Atom::Child(i, j)
            }
            "part" => {
                self.open(start, "part(name)")?;
                self.ws();
                let name = if self.peek() == Some(b'\'') {
                    let q = self.pos;
                    self.pos += 1;
                    let a = self.pos;
                    while self.peek().is_some_and(|c| c != b'\'') {
                        self.pos += 1;
                    }
                    if self.at_end() {
                        return Err(self.err(q, self.pos, "this quote is never closed", None));
                    }
                    let n = self.text(a, self.pos);
                    self.pos += 1;
                    n
                } else {
                    self.name_chars()
                };
                if name.is_empty() {
                    return Err(self.err(start, self.pos, "part() needs a part name", None));
                }
                self.close("part(name)")?;
                if !self.allowed.part {
                    return Err(self.err(
                        start,
                        self.pos,
                        format!("'part({name})' selects by part, which needs --enable part"),
                        None,
                    ));
                }
                Atom::Part(name)
            }
            "box" => {
                let shape = "box(x0, y0, z0, x1, y1, z1)";
                self.open(start, shape)?;
                let mut v = [0.0; 6];
                for (k, x) in v.iter_mut().enumerate() {
                    if k > 0 {
                        self.ws();
                        if self.peek() != Some(b',') {
                            return Err(self.err(
                                self.pos,
                                next_char(self.s, self.pos),
                                format!("expected ',': {shape} takes six numbers"),
                                None,
                            ));
                        }
                        self.pos += 1;
                    }
                    *x = self.number()?;
                }
                self.close(shape)?;
                let (min, max) = ([v[0], v[1], v[2]], [v[3], v[4], v[5]]);
                if (0..3).any(|k| min[k] > max[k]) {
                    return Err(self.err(
                        start,
                        self.pos,
                        format!("{shape}: each of x0, y0, z0 must not exceed x1, y1, z1"),
                        None,
                    ));
                }
                Atom::Box { min, max }
            }
            "and" | "or" | "exc" | "except" => {
                return Err(self.err(
                    start,
                    end,
                    format!("expected a selector before '{}'", self.text(start, end)),
                    None,
                ));
            }
            "xy" | "xz" | "yz" => {
                return Err(self.err(
                    start,
                    end,
                    format!(
                        "plane directions ('{}') are not supported; write a vector such as (1, 1, 0)",
                        self.text(start, end)
                    ),
                    None,
                ));
            }
            "line" | "circle" | "ellipse" | "bspline" => {
                let fix = format!("%{w}");
                return Err(self.err(
                    start,
                    end,
                    format!(
                        "unknown selector '{}': did you mean '{fix}'?",
                        self.text(start, end)
                    ),
                    Some(fix),
                ));
            }
            "front" | "back" | "left" | "right" | "top" | "bottom" => {
                return Err(self.err(
                    start,
                    end,
                    format!(
                        "named views ('{}') are not supported; use a direction such as '>z' or '<y'",
                        self.text(start, end)
                    ),
                    None,
                ));
            }
            _ => {
                let fix = closest(&w, WORDS.into_iter());
                return Err(self.unknown(
                    start,
                    end,
                    &format!("selector '{}'", self.text(start, end)),
                    fix,
                    "all, none, convex, concave, %line, %circle, |z, #z, >z, <z, >>z[i], new, child(i), part(name), @anchor, box(...)",
                ));
            }
        };
        Ok(Expr::Atom(atom))
    }

    fn unknown(
        &self,
        start: usize,
        end: usize,
        what: &str,
        fix: Option<&str>,
        known: &str,
    ) -> ParseError {
        match fix {
            Some(f) => self.err(
                start,
                end,
                format!("unknown {what}: did you mean '{f}'?"),
                Some(f.to_string()),
            ),
            None => self.err(start, end, format!("unknown {what}; known: {known}"), None),
        }
    }

    fn open(&mut self, start: usize, shape: &str) -> R<()> {
        self.ws();
        if self.peek() != Some(b'(') {
            return Err(self.err(
                start,
                self.pos,
                format!("expected '(': write {shape}"),
                None,
            ));
        }
        self.pos += 1;
        Ok(())
    }

    fn close(&mut self, shape: &str) -> R<()> {
        self.ws();
        if self.peek() != Some(b')') {
            let end = if self.at_end() {
                self.pos
            } else {
                next_char(self.s, self.pos)
            };
            return Err(self.err(self.pos, end, format!("expected ')': write {shape}"), None));
        }
        self.pos += 1;
        Ok(())
    }

    /// A direction after an operator that began at `op`.
    fn dir(&mut self, op: usize) -> R<Dir> {
        self.ws();
        let at = self.pos;
        if self.peek() == Some(b'(') {
            self.pos += 1;
            let mut v = [0.0; 3];
            for (k, x) in v.iter_mut().enumerate() {
                if k > 0 {
                    self.ws();
                    if self.peek() != Some(b',') {
                        return Err(self.err(
                            self.pos,
                            next_char(self.s, self.pos),
                            "expected ',': a direction is (a, b, c)",
                            None,
                        ));
                    }
                    self.pos += 1;
                }
                *x = self.number()?;
            }
            self.ws();
            if self.peek() != Some(b')') {
                return Err(self.err(
                    self.pos,
                    next_char(self.s, self.pos),
                    "expected ')': a direction is (a, b, c)",
                    None,
                ));
            }
            self.pos += 1;
            if v == [0.0; 3] {
                return Err(self.err(
                    at,
                    self.pos,
                    "the direction (0, 0, 0) has no direction",
                    None,
                ));
            }
            return Ok(Dir::Vector(v));
        }
        match self.word_at(at) {
            Some((end, w)) => {
                self.pos = end;
                match w.as_str() {
                    "x" => Ok(Dir::X),
                    "y" => Ok(Dir::Y),
                    "z" => Ok(Dir::Z),
                    "xy" | "xz" | "yz" => Err(self.err(
                        at,
                        end,
                        format!(
                            "plane directions ('{}') are not supported; write a vector such as (1, 1, 0)",
                            self.text(at, end)
                        ),
                        None,
                    )),
                    _ => Err(self.err(
                        at,
                        end,
                        format!(
                            "'{}' is not a direction: write x, y, z or (a, b, c)",
                            self.text(at, end)
                        ),
                        None,
                    )),
                }
            }
            None => {
                let op_text = self.text(op, at);
                Err(self.err(
                    op,
                    at.max(op + 1),
                    format!(
                        "'{}' needs a direction: x, y, z or (a, b, c)",
                        op_text.trim()
                    ),
                    None,
                ))
            }
        }
    }

    /// An optional `[i]` index, signed.
    fn index(&mut self) -> R<Option<i64>> {
        let save = self.pos;
        self.ws();
        if self.peek() != Some(b'[') {
            self.pos = save;
            return Ok(None);
        }
        let at = self.pos;
        self.pos += 1;
        self.ws();
        let neg = self.peek() == Some(b'-');
        if neg {
            self.pos += 1;
        }
        let d = self.pos;
        while self.peek().is_some_and(|c| c.is_ascii_digit()) {
            self.pos += 1;
        }
        let digits = self.text(d, self.pos);
        self.ws();
        if digits.is_empty() || self.peek() != Some(b']') {
            let end = if self.at_end() {
                self.pos
            } else {
                next_char(self.s, self.pos)
            };
            return Err(self.err(
                at,
                end,
                "an index is a whole number in brackets, such as [0] or [-1]",
                None,
            ));
        }
        self.pos += 1;
        let n: i64 = digits
            .parse()
            .map_err(|_| self.err(at, self.pos, "the index is too large", None))?;
        Ok(Some(if neg { -n } else { n }))
    }

    /// A non-negative whole number (a child index).
    fn uint(&mut self) -> R<u32> {
        self.ws();
        let a = self.pos;
        while self.peek().is_some_and(|c| c.is_ascii_digit()) {
            self.pos += 1;
        }
        if a == self.pos {
            let end = if self.at_end() {
                a
            } else {
                next_char(self.s, a)
            };
            return Err(self.err(a, end, "expected a child index (0, 1, ...)", None));
        }
        self.text(a, self.pos)
            .parse()
            .map_err(|_| self.err(a, self.pos, "the child index is too large", None))
    }

    /// A number: optional sign, digits with an optional fraction, and an
    /// optional exponent (CadQuery's are without the exponent).
    fn number(&mut self) -> R<f64> {
        self.ws();
        let a = self.pos;
        if matches!(self.peek(), Some(b'+' | b'-')) {
            self.pos += 1;
        }
        let digits = |p: &mut Self| {
            let d = p.pos;
            while p.peek().is_some_and(|c| c.is_ascii_digit()) {
                p.pos += 1;
            }
            p.pos - d
        };
        let mut n = digits(self);
        if self.peek() == Some(b'.') {
            self.pos += 1;
            n += digits(self);
        }
        if n > 0 && matches!(self.peek(), Some(b'e' | b'E')) {
            let save = self.pos;
            self.pos += 1;
            if matches!(self.peek(), Some(b'+' | b'-')) {
                self.pos += 1;
            }
            if digits(self) == 0 {
                self.pos = save;
            }
        }
        let t = self.text(a, self.pos);
        match t.parse::<f64>() {
            Ok(x) if n > 0 && x.is_finite() => Ok(x),
            _ => {
                let end = if self.pos > a {
                    self.pos
                } else if self.at_end() {
                    a
                } else {
                    next_char(self.s, a)
                };
                Err(self.err(a, end, "expected a number", None))
            }
        }
    }

    /// What follows a complete expression where the end, an operator or a
    /// `)` was expected.
    fn trailing(&self) -> ParseError {
        let at = self.pos;
        if self.peek() == Some(b')') && self.opens.is_empty() {
            return self.err(
                at,
                at + 1,
                "unbalanced parentheses: this ')' closes nothing",
                None,
            );
        }
        let end = match self.word_at(at) {
            Some((e, _)) => e,
            None => {
                // The next token, roughly: up to whitespace.
                let mut e = next_char(self.s, at);
                while e < self.s.len() && !self.s[e].is_ascii_whitespace() && self.s[e] != b')' {
                    e += 1;
                }
                e
            }
        };
        let found = self.text(at, end);
        if let Some((_, w)) = self.word_at(at)
            && let Some(f) = closest(&w, ["and", "or", "exc", "except", "not"].into_iter())
        {
            return self.err(
                at,
                end,
                format!("unknown operator '{found}': did you mean '{f}'?"),
                Some(f.to_string()),
            );
        }
        let expected = if self.opens.is_empty() {
            "'and', 'or', 'exc' or the end"
        } else {
            "'and', 'or', 'exc' or ')'"
        };
        self.err(
            at,
            end,
            format!("expected {expected}, found '{found}'"),
            None,
        )
    }
}

/// The byte after the character starting at `at` (selector strings are
/// UTF-8, so an error never splits a character).
fn next_char(s: &[u8], at: usize) -> usize {
    let mut e = (at + 1).min(s.len());
    while e < s.len() && (s[e] & 0xC0) == 0x80 {
        e += 1;
    }
    e
}

/// The closest of `candidates` to `name` within a third of its length (at
/// least one edit), as the other "did you mean" hints choose; ties go to
/// the first.
fn closest<'a>(name: &str, candidates: impl Iterator<Item = &'a str>) -> Option<&'a str> {
    let limit = (name.chars().count() / 3).max(1);
    let mut best: Option<(usize, &str)> = None;
    for c in candidates {
        if c == name {
            continue;
        }
        let d = distance(name, c);
        if d <= limit && best.is_none_or(|(b, _)| d < b) {
            best = Some((d, c));
        }
    }
    best.map(|(_, c)| c)
}

/// Edit distance by characters, counting a swap of two neighbours as one
/// edit (optimal string alignment), so `adn` is one edit from `and`.
fn distance(a: &str, b: &str) -> usize {
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    let w = b.len() + 1;
    let mut d = vec![0usize; (a.len() + 1) * w];
    for i in 0..=a.len() {
        for j in 0..=b.len() {
            d[i * w + j] = if i == 0 || j == 0 {
                i + j
            } else {
                let cost = usize::from(a[i - 1] != b[j - 1]);
                let mut v = (d[(i - 1) * w + j] + 1)
                    .min(d[i * w + j - 1] + 1)
                    .min(d[(i - 1) * w + j - 1] + cost);
                if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                    v = v.min(d[(i - 2) * w + j - 2] + 1);
                }
                v
            };
        }
    }
    d[a.len() * w + b.len()]
}

// --- printing ----------------------------------------------------------------

/// How tightly an expression binds, loosest first.
fn level(e: &Expr) -> u8 {
    match e {
        Expr::Not(_) => 0,
        Expr::Exc(..) => 1,
        Expr::Or(..) => 2,
        Expr::And(..) => 3,
        Expr::Atom(_) => 4,
    }
}

fn write_at(f: &mut fmt::Formatter<'_>, e: &Expr, min: u8) -> fmt::Result {
    if level(e) < min {
        f.write_str("(")?;
        write_at(f, e, 0)?;
        return f.write_str(")");
    }
    match e {
        Expr::Atom(a) => write!(f, "{a}"),
        Expr::Not(x) => {
            f.write_str("not ")?;
            write_at(f, x, 0)
        }
        Expr::And(l, r) => {
            write_at(f, l, 3)?;
            f.write_str(" and ")?;
            write_at(f, r, 4)
        }
        Expr::Or(l, r) => {
            write_at(f, l, 2)?;
            f.write_str(" or ")?;
            write_at(f, r, 3)
        }
        Expr::Exc(l, r) => {
            write_at(f, l, 1)?;
            f.write_str(" exc ")?;
            write_at(f, r, 2)
        }
    }
}

/// The canonical form: lower case, single spaces, parentheses only where
/// precedence needs them, numbers in Rust's shortest round-trip form. It
/// parses back to the same expression, so the `.csg` export can print it
/// and the cache key can hash it.
impl fmt::Display for Expr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write_at(f, self, 0)
    }
}

impl fmt::Display for Dir {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Dir::X => f.write_str("x"),
            Dir::Y => f.write_str("y"),
            Dir::Z => f.write_str("z"),
            Dir::Vector([a, b, c]) => write!(f, "({a}, {b}, {c})"),
        }
    }
}

/// Whether a part name prints bare inside `part(...)`, or needs quotes.
fn bare_name(n: &str) -> bool {
    n.bytes()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'.' | b'-'))
}

impl fmt::Display for Atom {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Atom::All => f.write_str("all"),
            Atom::Convex => f.write_str("convex"),
            Atom::Concave => f.write_str("concave"),
            Atom::Curve(c) => write!(f, "%{}", c.name()),
            Atom::Parallel(d) => write!(f, "|{d}"),
            Atom::Perpendicular(d) => write!(f, "#{d}"),
            Atom::Farthest { max, dir } => write!(f, "{}{dir}", if *max { ">" } else { "<" }),
            Atom::Nth { max, dir, index } => {
                write!(f, "{}{dir}[{index}]", if *max { ">>" } else { "<<" })
            }
            Atom::New => f.write_str("new"),
            Atom::Child(i, None) => write!(f, "child({i})"),
            Atom::Child(i, Some(j)) => write!(f, "child({i}, {j})"),
            Atom::Part(n) if bare_name(n) => write!(f, "part({n})"),
            Atom::Part(n) => write!(f, "part('{n}')"),
            Atom::Anchor(n) => write!(f, "@{n}"),
            Atom::Box { min, max } => write!(
                f,
                "box({}, {}, {}, {}, {}, {})",
                min[0], min[1], min[2], max[0], max[1], max[2]
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ON: Allowed = Allowed {
        part: true,
        anchor: true,
    };

    fn p(s: &str) -> Expr {
        parse(s, ON).unwrap_or_else(|e| panic!("{s}: {e:?}"))
    }

    fn e(s: &str) -> ParseError {
        parse(s, ON).expect_err(s)
    }

    fn atom(a: Atom) -> Expr {
        Expr::Atom(a)
    }

    #[test]
    fn atoms() {
        assert_eq!(p("all"), atom(Atom::All));
        assert_eq!(p("ALL"), atom(Atom::All));
        assert_eq!(p("none"), Expr::Not(Box::new(atom(Atom::All))));
        assert_eq!(p("convex"), atom(Atom::Convex));
        assert_eq!(p("Concave"), atom(Atom::Concave));
        assert_eq!(p("%Circle"), atom(Atom::Curve(Curve::Circle)));
        assert_eq!(p("%bspline"), atom(Atom::Curve(Curve::BSpline)));
        assert_eq!(p("|Z"), atom(Atom::Parallel(Dir::Z)));
        assert_eq!(p("Z"), atom(Atom::Parallel(Dir::Z)));
        assert_eq!(p("| x"), atom(Atom::Parallel(Dir::X)));
        assert_eq!(
            p("#(1, -1, 0)"),
            atom(Atom::Perpendicular(Dir::Vector([1.0, -1.0, 0.0])))
        );
        assert_eq!(
            p(">(-1,1,0.5)"),
            atom(Atom::Farthest {
                max: true,
                dir: Dir::Vector([-1.0, 1.0, 0.5])
            })
        );
        assert_eq!(
            p("<y"),
            atom(Atom::Farthest {
                max: false,
                dir: Dir::Y
            })
        );
        assert_eq!(
            p(">>Y[-2]"),
            atom(Atom::Nth {
                max: true,
                dir: Dir::Y,
                index: -2
            })
        );
        // CadQuery's default index.
        assert_eq!(
            p("<<z"),
            atom(Atom::Nth {
                max: false,
                dir: Dir::Z,
                index: -1
            })
        );
        assert_eq!(p("new"), atom(Atom::New));
        assert_eq!(p("child(0)"), atom(Atom::Child(0, None)));
        assert_eq!(p("child( 0 , 1 )"), atom(Atom::Child(0, Some(1))));
        assert_eq!(p("part(lid.Hinge)"), atom(Atom::Part("lid.Hinge".into())));
        assert_eq!(p("part('a b')"), atom(Atom::Part("a b".into())));
        assert_eq!(p("@Lip"), atom(Atom::Anchor("Lip".into())));
        assert_eq!(
            p("box(0, 0, 1e1, 10, 10.5, 20)"),
            atom(Atom::Box {
                min: [0.0, 0.0, 10.0],
                max: [10.0, 10.5, 20.0]
            })
        );
    }

    /// CadQuery's precedence (`_makeExpressionGrammar`): `and` tightest,
    /// then `or`, then `exc`, then `not`.
    #[test]
    fn precedence_is_cadquerys() {
        let (a, b, c) = (
            || atom(Atom::Parallel(Dir::Z)),
            || atom(Atom::Convex),
            || atom(Atom::New),
        );
        let bx = Box::new;
        assert_eq!(
            p("|z and convex or new"),
            Expr::Or(bx(Expr::And(bx(a()), bx(b()))), bx(c()))
        );
        assert_eq!(
            p("|z or convex and new"),
            Expr::Or(bx(a()), bx(Expr::And(bx(b()), bx(c()))))
        );
        assert_eq!(
            p("|z or convex exc new"),
            Expr::Exc(bx(Expr::Or(bx(a()), bx(b()))), bx(c()))
        );
        assert_eq!(
            p("|z exc convex exc new"),
            Expr::Exc(bx(Expr::Exc(bx(a()), bx(b()))), bx(c()))
        );
        assert_eq!(
            p("not |z and convex"),
            Expr::Not(bx(Expr::And(bx(a()), bx(b()))))
        );
        assert_eq!(
            p("(not |z) and convex"),
            Expr::And(bx(Expr::Not(bx(a()))), bx(b()))
        );
        // An extension of CadQuery's grammar: `not` after an operator
        // takes everything to its right.
        assert_eq!(
            p("|z and not convex or new"),
            Expr::And(bx(a()), bx(Expr::Not(bx(Expr::Or(bx(b()), bx(c()))))))
        );
        assert_eq!(p("all except <z"), p("all exc <z"));
    }

    #[test]
    fn printing_round_trips() {
        for s in [
            "all",
            "not all",
            "|z and >x",
            "%circle and >z",
            "child(0, 1)",
            "all exc <z",
            "(not |z) and convex",
            "|z and (not convex)",
            "not |z or convex exc new",
            "|z exc (convex exc new)",
            "(|z or convex) and new",
            "#(1, -1, 0.25) or <<y[3]",
            ">>z[-1] and box(-1, -2.5, 0, 1, 2, 3)",
            "part(lid.hinge) or part('a b') or @tip",
        ] {
            let e = p(s);
            assert_eq!(e.to_string(), s, "canonical form of {s}");
            assert_eq!(p(&e.to_string()), e, "{s}");
        }
        assert_eq!(p("  |Z   AND  >X ").to_string(), "|z and >x");
        assert_eq!(p("((|z))").to_string(), "|z");
        assert_eq!(p("none").to_string(), "not all");
    }

    /// Each error points at the offending text in the string.
    #[test]
    fn errors_have_spans_and_fixes() {
        let x = e("|z and convx");
        assert_eq!((x.start, x.end), (7, 12));
        assert_eq!(x.suggestion.as_deref(), Some("convex"));
        assert!(x.message.contains("did you mean 'convex'"), "{}", x.message);

        let x = e("%circel");
        assert_eq!((x.start, x.end), (1, 7));
        assert_eq!(x.suggestion.as_deref(), Some("circle"));

        let x = e("circle and >z");
        assert_eq!((x.start, x.end), (0, 6));
        assert_eq!(x.suggestion.as_deref(), Some("%circle"));

        let x = e("(|z and >x");
        assert_eq!((x.start, x.end), (0, 1));
        assert!(x.message.contains("never closed"), "{}", x.message);

        let x = e("|z) and >x");
        assert_eq!((x.start, x.end), (2, 3));
        assert!(x.message.contains("closes nothing"), "{}", x.message);

        let x = e("|z adn >x");
        assert_eq!((x.start, x.end), (3, 6));
        assert_eq!(x.suggestion.as_deref(), Some("and"));

        let x = e("|z >x");
        assert_eq!((x.start, x.end), (3, 5));
        assert!(x.message.contains("expected 'and'"), "{}", x.message);

        let x = e("+z");
        assert_eq!((x.start, x.end), (0, 2));
        assert_eq!(x.suggestion.as_deref(), Some("|z"));

        let x = e("convex and >y[1]");
        assert_eq!((x.start, x.end), (11, 16));
        assert_eq!(x.suggestion.as_deref(), Some(">>y[1]"));

        let x = e("|q");
        assert_eq!((x.start, x.end), (1, 2));

        let x = e("|");
        assert_eq!((x.start, x.end), (0, 1));

        let x = e("#(0, 0, 0)");
        assert_eq!((x.start, x.end), (1, 10));

        let x = e("child(a)");
        assert_eq!((x.start, x.end), (6, 7));

        let x = e("box(1, 2, 3)");
        assert_eq!((x.start, x.end), (11, 12));

        let x = e("box(1, 0, 0, 0, 1, 1)");
        assert!(x.message.contains("must not exceed"), "{}", x.message);

        let x = e("|z and");
        assert_eq!((x.start, x.end), (6, 6));

        let x = e("and |z");
        assert_eq!((x.start, x.end), (0, 3));

        let x = e("   ");
        assert!(x.message.contains("empty"), "{}", x.message);

        let x = e("|z and top");
        assert!(x.message.contains("named views"), "{}", x.message);

        let x = e("|z and é");
        assert_eq!((x.start, x.end), (7, 9));
    }

    #[test]
    fn extension_atoms_need_their_flags() {
        let off = Allowed::default();
        let x = parse("convex and part(lid)", off).unwrap_err();
        assert_eq!((x.start, x.end), (11, 20));
        assert!(x.message.contains("--enable part"), "{}", x.message);
        let x = parse("@tip", off).unwrap_err();
        assert_eq!((x.start, x.end), (0, 4));
        assert!(x.message.contains("--enable query"), "{}", x.message);
        assert!(parse("convex or child(1)", off).is_ok());
    }
}
