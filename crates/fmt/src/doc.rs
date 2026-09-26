//! A small Wadler-style document and its printer (the algorithm Prettier
//! uses): groups print on one line when they fit and break every line of
//! their own otherwise, deciding outermost first.
//!
//! Additions for source code with comments:
//!
//! - [`Doc::Space`] collapses (two in a row print one, none at the start
//!   of a line), so the builder can ask for a space around a comment
//!   without tracking what came before;
//! - [`Doc::Blank`] asks for an empty line before what follows, at most
//!   one, and never straight after an opening bracket ([`Doc::NoBlank`]).

/// A layout document.
#[derive(Debug, Clone)]
pub enum Doc {
    /// Printed as is. May contain line breaks (a multi-line comment or
    /// string), which force the enclosing groups to break.
    Text(String),
    /// One space, unless the output is at the start of a line or already
    /// ends with one.
    Space,
    /// A space when the group is flat, a line break when it breaks.
    Line,
    /// Nothing when flat, a line break when broken.
    SoftLine,
    /// Always a line break; the enclosing groups break.
    Hard,
    /// A line break unless the output is at the start of a line.
    EnsureLine,
    /// An empty line before the next text (implies [`Doc::EnsureLine`]).
    Blank,
    /// The next [`Doc::Blank`] is dropped: nothing but text may follow an
    /// opening bracket before its first item.
    NoBlank,
    Indent(Box<Doc>),
    /// Indented by this many spaces rather than a level (to line up with
    /// the text after `? ` or `: `).
    Align(usize, Box<Doc>),
    Group(Box<Doc>, bool),
    Concat(Vec<Doc>),
    /// Contents and separators alternating (`c, s, c, s, c`): each
    /// separator breaks only when the next content would not fit.
    Fill(Vec<Doc>),
}

impl Default for Doc {
    fn default() -> Doc {
        Doc::Concat(Vec::new())
    }
}

impl Doc {
    pub fn group(d: Doc) -> Doc {
        Doc::Group(Box::new(d), false)
    }

    pub fn indent(d: Doc) -> Doc {
        Doc::Indent(Box::new(d))
    }

    /// Mark every group that contains a forced break as broken. Returns
    /// whether `self` contains one.
    pub fn propagate(&mut self) -> bool {
        match self {
            Doc::Text(s) => s.contains('\n'),
            Doc::Hard | Doc::EnsureLine | Doc::Blank => true,
            Doc::Space | Doc::Line | Doc::SoftLine | Doc::NoBlank => false,
            Doc::Indent(d) | Doc::Align(_, d) => d.propagate(),
            Doc::Group(d, b) => {
                let inner = d.propagate();
                *b |= inner;
                inner
            }
            Doc::Concat(v) | Doc::Fill(v) => {
                let mut any = false;
                for d in v {
                    any |= d.propagate();
                }
                any
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Flat,
    Break,
}

#[derive(Debug, Clone, Copy)]
enum Cmd<'a> {
    Doc(&'a Doc),
    /// The rest of a [`Doc::Fill`], from a separator.
    Fill(&'a [Doc]),
}

/// Print `doc` at `width` columns with `indent` spaces per level, lines
/// ending in `newline`. `doc` must have been [`Doc::propagate`]d.
pub fn print(doc: &Doc, width: usize, indent: usize, newline: &str) -> String {
    let mut p = Printer {
        out: String::new(),
        width: width as isize,
        step: indent,
        newline,
        pos: 0,
        pending_indent: Some(0),
        pending_space: false,
        no_blank: true,
        blank: false,
    };
    let mut cmds: Vec<(usize, Mode, Cmd<'_>)> = vec![(0, Mode::Break, Cmd::Doc(doc))];
    while let Some((ind, mode, cmd)) = cmds.pop() {
        let d = match cmd {
            Cmd::Doc(d) => d,
            Cmd::Fill(rest) => {
                p.fill_rest(ind, mode, rest, &mut cmds);
                continue;
            }
        };
        match d {
            Doc::Text(s) => p.text(s),
            Doc::Space => {
                if p.pending_indent.is_none() {
                    p.pending_space = true;
                }
            }
            Doc::Line => match mode {
                Mode::Flat => {
                    if p.pending_indent.is_none() {
                        p.pending_space = true;
                    }
                }
                Mode::Break => p.newline(ind),
            },
            Doc::SoftLine => {
                if mode == Mode::Break {
                    p.newline(ind);
                }
            }
            Doc::Hard => p.newline(ind),
            Doc::EnsureLine => {
                if p.pending_indent.is_none() {
                    p.newline(ind);
                }
            }
            Doc::Blank => {
                if p.pending_indent.is_none() {
                    p.newline(ind);
                }
                if !p.no_blank {
                    p.blank = true;
                }
            }
            Doc::NoBlank => p.no_blank = true,
            Doc::Indent(inner) => cmds.push((ind + p.step, mode, Cmd::Doc(inner))),
            Doc::Align(n, inner) => cmds.push((ind + n, mode, Cmd::Doc(inner))),
            Doc::Group(inner, broken) => {
                let flat = !broken && (mode == Mode::Flat || p.fits(ind, inner, &cmds));
                let m = if flat { Mode::Flat } else { Mode::Break };
                cmds.push((ind, m, Cmd::Doc(inner)));
            }
            Doc::Concat(v) => {
                for d in v.iter().rev() {
                    cmds.push((ind, mode, Cmd::Doc(d)));
                }
            }
            Doc::Fill(parts) => {
                if let Some((first, rest)) = parts.split_first() {
                    let m = if mode == Mode::Flat || p.fits_flat(ind, &[first], &cmds) {
                        Mode::Flat
                    } else {
                        Mode::Break
                    };
                    if !rest.is_empty() {
                        cmds.push((ind, mode, Cmd::Fill(rest)));
                    }
                    cmds.push((ind, m, Cmd::Doc(first)));
                }
            }
        }
    }
    p.out
}

struct Printer<'n> {
    out: String,
    width: isize,
    step: usize,
    newline: &'n str,
    /// Column after the last output (counting a pending indent).
    pos: usize,
    /// At the start of a line: the indent to write before the next text.
    pending_indent: Option<usize>,
    pending_space: bool,
    /// A [`Doc::NoBlank`] is in effect (until the next text).
    no_blank: bool,
    /// An empty line is owed before the next text.
    blank: bool,
}

impl Printer<'_> {
    fn text(&mut self, s: &str) {
        if s.is_empty() {
            return;
        }
        if let Some(ind) = self.pending_indent.take() {
            if self.blank && !self.out.is_empty() {
                self.out.push_str(self.newline);
            }
            self.blank = false;
            for _ in 0..ind {
                self.out.push(' ');
            }
            self.pos = ind;
        } else if self.pending_space {
            self.out.push(' ');
            self.pos += 1;
        }
        self.pending_space = false;
        self.no_blank = false;
        self.out.push_str(s);
        match s.rfind('\n') {
            Some(i) => self.pos = s.len() - i - 1,
            None => self.pos += s.chars().count(),
        }
    }

    fn newline(&mut self, ind: usize) {
        if self.pending_indent.is_some() && !self.out.is_empty() {
            // Already at the start of a line: a second break is an empty
            // line, which only [`Doc::Blank`] may ask for.
            self.pending_indent = Some(ind);
            self.pos = ind;
            return;
        }
        if !self.out.is_empty() {
            self.out.push_str(self.newline);
        }
        self.pending_indent = Some(ind);
        self.pending_space = false;
        self.pos = ind;
    }

    fn remaining(&self) -> isize {
        self.width - self.pos as isize - isize::from(self.pending_space)
    }

    /// Whether `doc` fits flat on the rest of the line, followed by what
    /// the enclosing commands print up to their next line break.
    fn fits(&self, ind: usize, doc: &Doc, rest: &[(usize, Mode, Cmd<'_>)]) -> bool {
        let mut stack: Vec<(Mode, &Doc)> = vec![(Mode::Flat, doc)];
        let _ = ind;
        let mut width = self.remaining();
        let mut rest_idx = rest.len();
        loop {
            let (mode, d) = match stack.pop() {
                Some(x) => x,
                None => {
                    if rest_idx == 0 {
                        return true;
                    }
                    rest_idx -= 1;
                    match rest[rest_idx] {
                        (_, m, Cmd::Doc(d)) => (m, d),
                        (_, m, Cmd::Fill(parts)) => {
                            for d in parts.iter().rev() {
                                stack.push((m, d));
                            }
                            continue;
                        }
                    }
                }
            };
            match d {
                Doc::Text(s) => {
                    match s.find('\n') {
                        Some(i) => return width >= s[..i].chars().count() as isize,
                        None => width -= s.chars().count() as isize,
                    }
                    if width < 0 {
                        return false;
                    }
                }
                Doc::Space => width -= 1,
                Doc::Line => {
                    if mode == Mode::Break {
                        return true;
                    }
                    width -= 1;
                }
                Doc::SoftLine => {
                    if mode == Mode::Break {
                        return true;
                    }
                }
                Doc::Hard | Doc::EnsureLine | Doc::Blank => return true,
                Doc::NoBlank => {}
                Doc::Indent(inner) | Doc::Align(_, inner) => stack.push((mode, inner)),
                Doc::Group(inner, broken) => {
                    stack.push((if *broken { Mode::Break } else { mode }, inner));
                }
                Doc::Concat(v) | Doc::Fill(v) => {
                    for d in v.iter().rev() {
                        stack.push((mode, d));
                    }
                }
            }
            if width < 0 {
                return false;
            }
        }
    }

    fn fits_flat(&self, ind: usize, docs: &[&Doc], rest: &[(usize, Mode, Cmd<'_>)]) -> bool {
        let c = Doc::Concat(docs.iter().map(|d| (*d).clone()).collect());
        self.fits(ind, &c, rest)
    }

    /// Continue a fill at a separator: `rest` is `[sep, content, sep, ...]`.
    fn fill_rest<'a>(
        &mut self,
        ind: usize,
        mode: Mode,
        rest: &'a [Doc],
        cmds: &mut Vec<(usize, Mode, Cmd<'a>)>,
    ) {
        let [sep, content, more @ ..] = rest else {
            for d in rest.iter().rev() {
                cmds.push((ind, mode, Cmd::Doc(d)));
            }
            return;
        };
        let flat = mode == Mode::Flat || self.fits_flat(ind, &[sep, content], cmds);
        if !more.is_empty() {
            cmds.push((ind, mode, Cmd::Fill(more)));
        }
        let cm = if flat || self.fits_flat(ind, &[content], cmds) {
            Mode::Flat
        } else {
            Mode::Break
        };
        cmds.push((ind, cm, Cmd::Doc(content)));
        cmds.push((
            ind,
            if flat { Mode::Flat } else { Mode::Break },
            Cmd::Doc(sep),
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn list(items: &[&str]) -> Doc {
        let mut v = Vec::new();
        for (i, s) in items.iter().enumerate() {
            if i > 0 {
                v.push(Doc::Text(String::from(",")));
                v.push(Doc::Line);
            }
            v.push(Doc::Text(String::from(*s)));
        }
        Doc::group(Doc::Concat(vec![
            Doc::Text(String::from("[")),
            Doc::indent(Doc::Concat(vec![Doc::SoftLine, Doc::Concat(v)])),
            Doc::SoftLine,
            Doc::Text(String::from("]")),
        ]))
    }

    #[test]
    fn groups_break_when_too_long() {
        let mut d = list(&["aaa", "bbb", "ccc"]);
        d.propagate();
        assert_eq!(print(&d, 80, 4, "\n"), "[aaa, bbb, ccc]");
        assert_eq!(print(&d, 10, 4, "\n"), "[\n    aaa,\n    bbb,\n    ccc\n]");
    }

    #[test]
    fn blank_lines_collapse_and_skip_after_open() {
        let d = Doc::Concat(vec![
            Doc::Text(String::from("{")),
            Doc::NoBlank,
            Doc::Blank,
            Doc::Text(String::from("x")),
            Doc::Blank,
            Doc::Blank,
            Doc::Text(String::from("y")),
        ]);
        assert_eq!(print(&d, 80, 4, "\n"), "{\nx\n\ny");
    }
}
