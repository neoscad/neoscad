//! The lossless concrete syntax tree.
//!
//! Representation: one flat arena per parse. `tokens` holds every token of
//! the input in order (trivia included), and `entries` holds the tree in
//! preorder, one entry per node or token. A node entry stores the index one
//! past its last descendant, so a subtree is a contiguous slice and walking
//! children is a loop of index jumps: no per-node allocation, no pointers,
//! and the whole tree is two `Vec`s that are cheap to build, drop and send
//! between threads.
//!
//! Why not a rowan-style green/red tree: its main advantage, structural
//! sharing for incremental reparsing, pays off for large files. OpenSCAD
//! files are small and a full reparse runs at tens of MB/s, so a fresh flat
//! tree per edit is simpler and faster to build and walk. Rowan also wants
//! `str` text, while OpenSCAD's lexer is byte-based (see `source`).
//!
//! When the parse splices `include`d files into the stream, tokens from
//! several files appear in one tree; each token carries its [`FileId`].

use std::fmt::Write as _;

use crate::source::{FileId, SourceMap, Span};
use crate::syntax::SyntaxKind;
use crate::syntax::lexer::Token;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Entry {
    kind: SyntaxKind,
    /// Entry index of the parent node (`u32::MAX` for the root).
    parent: u32,
    /// Node: entry index one past its subtree. Token: index into `tokens`.
    data: u32,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Cst {
    tokens: Vec<Token>,
    entries: Vec<Entry>,
}

/// A node in a [`Cst`].
#[derive(Debug, Clone, Copy)]
pub struct Node<'a> {
    cst: &'a Cst,
    idx: u32,
}

/// A token in a [`Cst`], with its position in the tree.
#[derive(Debug, Clone, Copy)]
pub struct TokenRef<'a> {
    cst: &'a Cst,
    idx: u32,
}

impl PartialEq for Node<'_> {
    fn eq(&self, other: &Self) -> bool {
        std::ptr::eq(self.cst, other.cst) && self.idx == other.idx
    }
}
impl Eq for Node<'_> {}

impl PartialEq for TokenRef<'_> {
    fn eq(&self, other: &Self) -> bool {
        std::ptr::eq(self.cst, other.cst) && self.idx == other.idx
    }
}
impl Eq for TokenRef<'_> {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Element<'a> {
    Node(Node<'a>),
    Token(TokenRef<'a>),
}

impl Cst {
    pub fn root(&self) -> Node<'_> {
        Node { cst: self, idx: 0 }
    }

    pub fn tokens(&self) -> &[Token] {
        &self.tokens
    }

    /// Number of nodes and tokens in the tree.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The source text of the whole tree (every byte, in order).
    pub fn text(&self, sources: &SourceMap) -> Vec<u8> {
        self.root().text(sources)
    }

    /// The deepest token covering `offset` in `file`.
    pub fn token_at(&self, file: FileId, offset: u32) -> Option<TokenRef<'_>> {
        self.entries.iter().enumerate().find_map(|(i, e)| {
            let t = self
                .tokens
                .get(e.data as usize)
                .filter(|_| e.kind.is_token())?;
            (t.file == file && t.start <= offset && offset < t.end()).then_some(TokenRef {
                cst: self,
                idx: i as u32,
            })
        })
    }

    /// An indented outline, for tests and debugging.
    pub fn debug_dump(&self, sources: &SourceMap) -> String {
        let mut s = String::new();
        dump(self.root(), sources, 0, &mut s);
        s
    }
}

fn dump(n: Node<'_>, sources: &SourceMap, depth: usize, out: &mut String) {
    let _ = writeln!(out, "{:indent$}{:?}", "", n.kind(), indent = depth * 2);
    for c in n.children_with_tokens() {
        match c {
            Element::Node(c) => dump(c, sources, depth + 1, out),
            Element::Token(t) => {
                let _ = writeln!(
                    out,
                    "{:indent$}{:?} {:?}",
                    "",
                    t.kind(),
                    String::from_utf8_lossy(t.text(sources)),
                    indent = (depth + 1) * 2
                );
            }
        }
    }
}

impl<'a> Node<'a> {
    /// The node's entry index in its tree (its preorder position).
    pub(crate) fn index(&self) -> u32 {
        self.idx
    }

    fn entry(&self) -> Entry {
        self.cst.entries[self.idx as usize]
    }

    pub fn kind(&self) -> SyntaxKind {
        self.entry().kind
    }

    pub fn parent(&self) -> Option<Node<'a>> {
        let p = self.entry().parent;
        (p != u32::MAX).then_some(Node {
            cst: self.cst,
            idx: p,
        })
    }

    pub fn children_with_tokens(&self) -> impl Iterator<Item = Element<'a>> + 'a {
        let cst = self.cst;
        let end = self.entry().data;
        let mut i = self.idx + 1;
        std::iter::from_fn(move || {
            if i >= end {
                return None;
            }
            let e = cst.entries[i as usize];
            let here = i;
            if e.kind.is_token() {
                i += 1;
                Some(Element::Token(TokenRef { cst, idx: here }))
            } else {
                i = e.data;
                Some(Element::Node(Node { cst, idx: here }))
            }
        })
    }

    pub fn children(&self) -> impl Iterator<Item = Node<'a>> + 'a {
        self.children_with_tokens().filter_map(|e| match e {
            Element::Node(n) => Some(n),
            Element::Token(_) => None,
        })
    }

    /// Direct child tokens that are not trivia.
    pub fn tokens(&self) -> impl Iterator<Item = TokenRef<'a>> + 'a {
        self.children_with_tokens().filter_map(|e| match e {
            Element::Token(t) if !t.kind().is_trivia() => Some(t),
            _ => None,
        })
    }

    /// First direct child token of `kind`.
    pub fn token(&self, kind: SyntaxKind) -> Option<TokenRef<'a>> {
        self.tokens().find(|t| t.kind() == kind)
    }

    /// Every token in the subtree, trivia included.
    pub fn descendant_tokens(&self) -> impl Iterator<Item = &'a Token> + 'a {
        let cst = self.cst;
        let (start, end) = (self.idx as usize + 1, self.entry().data as usize);
        cst.entries[start..end]
            .iter()
            .filter(|e| e.kind.is_token())
            .map(move |e| &cst.tokens[e.data as usize])
    }

    /// Span from the first to the last significant token, in the file of the
    /// first. `None` for a node without tokens.
    pub fn span(&self) -> Option<Span> {
        // Scan from both ends: walking the whole subtree for every node
        // would make lowering quadratic in nesting depth.
        let cst = self.cst;
        let sub = &cst.entries[self.idx as usize + 1..self.entry().data as usize];
        let sig = |e: &Entry| {
            cst.tokens
                .get(e.data as usize)
                .filter(|_| e.kind.is_token() && !e.kind.is_trivia())
                .copied()
        };
        let first = sub.iter().find_map(sig)?;
        let last = sub
            .iter()
            .rev()
            .filter_map(sig)
            .find(|t| t.file == first.file)
            .unwrap_or(first);
        Some(Span::new(first.file, first.start, last.end()))
    }

    /// First significant token in the subtree.
    pub fn first_token(&self) -> Option<&'a Token> {
        self.descendant_tokens().find(|t| !t.kind.is_trivia())
    }

    pub fn text(&self, sources: &SourceMap) -> Vec<u8> {
        let mut out = Vec::new();
        for t in self.descendant_tokens() {
            out.extend_from_slice(sources.get(t.file).slice(t.start, t.end()));
        }
        out
    }
}

impl<'a> TokenRef<'a> {
    pub fn token(&self) -> &'a Token {
        let e = self.cst.entries[self.idx as usize];
        &self.cst.tokens[e.data as usize]
    }

    pub fn kind(&self) -> SyntaxKind {
        self.token().kind
    }

    /// Index of the token in [`Cst::tokens`].
    pub fn index(&self) -> u32 {
        self.cst.entries[self.idx as usize].data
    }

    pub fn span(&self) -> Span {
        let t = self.token();
        Span::new(t.file, t.start, t.end())
    }

    pub fn text<'s>(&self, sources: &'s SourceMap) -> &'s [u8] {
        let t = self.token();
        sources.get(t.file).slice(t.start, t.end())
    }

    pub fn parent(&self) -> Node<'a> {
        Node {
            cst: self.cst,
            idx: self.cst.entries[self.idx as usize].parent,
        }
    }
}

/// One included file's parse for [`Cst::splice`] to put into a tree.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Insert<'a> {
    /// Index in the host tree's tokens of the `include` directive the
    /// file's parse goes right after. It must be a child of the root.
    pub at: u32,
    pub cst: &'a Cst,
    /// Added to the file id of every inserted token.
    pub file_base: u32,
}

impl Cst {
    /// Token indices of the tokens that are direct children of the root:
    /// the trivia between top-level statements.
    pub(crate) fn root_tokens(&self) -> Vec<u32> {
        self.root()
            .children_with_tokens()
            .filter_map(|e| match e {
                Element::Token(t) => Some(t.index()),
                Element::Node(_) => None,
            })
            .collect()
    }

    /// The first token index of each direct child node of the root, in
    /// order (`u32::MAX` for a node without tokens).
    pub(crate) fn root_node_starts(&self) -> Vec<u32> {
        self.root()
            .children()
            .map(|n| {
                let sub = &self.entries[n.idx as usize + 1..n.entry().data as usize];
                sub.iter()
                    .find(|e| e.kind.is_token())
                    .map_or(u32::MAX, |e| e.data)
            })
            .collect()
    }

    /// The tree the parser builds when each insert's tokens follow its
    /// directive in the stream, provided that the inserted file parses on
    /// its own, without errors, as whole statements and that its directive
    /// sits between top-level statements. Then the parser flushes the
    /// directive, the file's leading trivia, its statements and its
    /// trailing trivia into the root one after the other (`parser::build`
    /// flushes pending trivia into the parent when a node starts, and into
    /// the root at the end), so the file's root children go right after
    /// the directive, unchanged but renumbered. `inserts` are in token
    /// order; returns the tree and the entry range each insert took.
    pub(crate) fn splice(self, inserts: &[Insert<'_>]) -> (Cst, Vec<std::ops::Range<u32>>) {
        let Cst { tokens, entries } = self;
        let added_tokens: usize = inserts.iter().map(|i| i.cst.tokens.len()).sum();
        let added_entries: usize = inserts
            .iter()
            .map(|i| i.cst.entries.len().saturating_sub(1))
            .sum();
        // The host's tokens: old index -> new.
        let mut tok_map = Vec::with_capacity(tokens.len() + 1);
        let mut new_tokens = Vec::with_capacity(tokens.len() + added_tokens);
        let mut tok_base = Vec::with_capacity(inserts.len());
        let mut next = inserts.iter().peekable();
        for (i, t) in tokens.into_iter().enumerate() {
            tok_map.push(new_tokens.len() as u32);
            new_tokens.push(t);
            while let Some(ins) = next.next_if(|ins| ins.at as usize == i) {
                tok_base.push(new_tokens.len() as u32);
                let base = ins.file_base;
                new_tokens.extend(ins.cst.tokens.iter().map(|t| Token {
                    file: FileId(t.file.0 + base),
                    ..*t
                }));
            }
        }
        tok_map.push(new_tokens.len() as u32);
        // The host's entries: each keeps its place, shifted by the entries
        // inserted before it.
        let mut entry_map = Vec::with_capacity(entries.len() + 1);
        let mut after_entry = Vec::with_capacity(inserts.len());
        let mut shift = 0u32;
        for (i, e) in entries.iter().enumerate() {
            entry_map.push(i as u32 + shift);
            if let Some(ins) = inserts.get(after_entry.len())
                && e.kind.is_token()
                && e.parent == 0
                && e.data == ins.at
            {
                after_entry.push(i);
                shift += ins.cst.entries.len().saturating_sub(1) as u32;
            }
        }
        entry_map.push(entries.len() as u32 + shift);
        assert_eq!(
            after_entry.len(),
            inserts.len(),
            "an included file's parse can only follow a directive between top-level statements"
        );
        let mut new_entries = Vec::with_capacity(entries.len() + added_entries);
        let mut ranges = Vec::with_capacity(inserts.len());
        let mut k = 0usize;
        for (i, e) in entries.iter().enumerate() {
            let parent = match e.parent {
                u32::MAX => u32::MAX,
                p => entry_map[p as usize],
            };
            let data = if e.kind.is_token() {
                tok_map[e.data as usize]
            } else {
                entry_map[e.data as usize]
            };
            new_entries.push(Entry {
                kind: e.kind,
                parent,
                data,
            });
            if after_entry.get(k) == Some(&i) {
                let (ins, tb) = (&inserts[k], tok_base[k]);
                let start = new_entries.len() as u32;
                // Entry `j >= 1` of the file's tree lands at `start + j - 1`,
                // and its root is the host's root (entry 0).
                let off = start - 1;
                new_entries.extend(ins.cst.entries.iter().skip(1).map(|f| Entry {
                    kind: f.kind,
                    parent: if f.parent == 0 { 0 } else { f.parent + off },
                    data: if f.kind.is_token() {
                        f.data + tb
                    } else {
                        f.data + off
                    },
                }));
                ranges.push(start..new_entries.len() as u32);
                k += 1;
            }
        }
        (
            Cst {
                tokens: new_tokens,
                entries: new_entries,
            },
            ranges,
        )
    }
}

/// Builds a [`Cst`] in preorder; used by the parser.
#[derive(Debug)]
pub(crate) struct Builder {
    cst: Cst,
    stack: Vec<u32>,
}

impl Builder {
    pub(crate) fn new(tokens: Vec<Token>) -> Self {
        let n = tokens.len();
        let mut cst = Cst {
            tokens,
            entries: Vec::new(),
        };
        cst.entries.reserve(n + n / 2);
        Self {
            cst,
            stack: Vec::new(),
        }
    }

    fn parent(&self) -> u32 {
        self.stack.last().copied().unwrap_or(u32::MAX)
    }

    pub(crate) fn start(&mut self, kind: SyntaxKind) {
        let idx = self.cst.entries.len() as u32;
        let parent = self.parent();
        self.cst.entries.push(Entry {
            kind,
            parent,
            data: 0,
        });
        self.stack.push(idx);
    }

    pub(crate) fn finish(&mut self) {
        let idx = self.stack.pop().expect("unbalanced finish");
        let end = self.cst.entries.len() as u32;
        self.cst.entries[idx as usize].data = end;
    }

    pub(crate) fn token(&mut self, token_index: u32) {
        let kind = self.cst.tokens[token_index as usize].kind;
        let parent = self.parent();
        self.cst.entries.push(Entry {
            kind,
            parent,
            data: token_index,
        });
    }

    pub(crate) fn depth(&self) -> usize {
        self.stack.len()
    }

    pub(crate) fn token_kind(&self, token_index: usize) -> Option<SyntaxKind> {
        self.cst.tokens.get(token_index).map(|t| t.kind)
    }

    pub(crate) fn finish_tree(self) -> Cst {
        debug_assert!(self.stack.is_empty());
        self.cst
    }
}
