//! A NeoSCAD-only hint for a trap of `use <file>`: the file's top-level
//! `$fn`, `$fa` and `$fs` do not apply to its modules.
//!
//! Special variables are scoped by the call, not by the file: a module
//! from a `use`d file sees the caller's `$fn`, and the assignments at the
//! top of its own file are never in effect for it. OpenSCAD does this
//! (checked against the 2026.09.23 nightly: a used file with `$fn = 64`
//! and `w = 7` at its top echoes `$fn = 0, w = 7` from its module), and so
//! does NeoSCAD, silently. In the T2 transcript audit every NeoSCAD run of
//! a two-part enclosure hit it: a harness file `use`d both parts, measured
//! their default-tessellated circles, and reported a 0.0056 mm³ "clash"
//! that the agent spent 139 s chasing.
//!
//! The hint is recorded with [`eval::Console::note`]: in the tool view
//! (JSON, MCP, the LSP) and never on the console, since OpenSCAD prints
//! nothing here and the conformance suite compares the console word for
//! word.

use std::collections::HashMap;
use std::path::Path;

use eval::Node;
use lang::Program;
use lang::ast::{Ast, Scope};
use lang::diag::{DiagCode, Diagnostic, Hint, PathBase, Severity};
use lang::source::Span;

/// The special variables whose top-level value a used file loses.
const SPECIALS: [&str; 3] = ["$fn", "$fa", "$fs"];

/// A top-level special-variable assignment of a used file, as written.
struct Setting {
    name: &'static str,
    value: String,
}

/// The first call into each used file whose top level sets `$fn`, `$fa`
/// or `$fs`, logged as a warning at that call with the fix. `program`
/// gives each evaluation unit's program (0 the main one, `1 + i` the
/// i-th used file).
///
/// A variable is left out when the call, or a call it is made from,
/// passes it, and when a program the call is made from assigns it
/// anywhere (at its top, in a module): the caller's value is then likely
/// the one meant, and a hint there would be noise. The second test is
/// conservative, so a hint can be missed but not given for a variable
/// the caller sets.
pub fn report<'a, W: std::io::Write>(
    con: &mut eval::Console<W>,
    top: &Node,
    units: usize,
    program: &dyn Fn(u32) -> Option<&'a Program>,
    cwd: &Path,
) {
    // Used files that set a special variable at their top: most set none,
    // and then the tree is not walked.
    let mut settings: HashMap<u32, Vec<Setting>> = HashMap::new();
    for u in 1..=units as u32 {
        let Some(p) = program(u) else { continue };
        let found: Vec<Setting> = p
            .ast
            .root
            .assignments
            .iter()
            .filter_map(|a| {
                let name = SPECIALS.into_iter().find(|s| p.ast.name(a.name) == *s)?;
                let span = p.ast.expr(a.expr).span;
                let value = String::from_utf8_lossy(p.sources.text(span)).into_owned();
                Some(Setting { name, value })
            })
            .collect();
        if !found.is_empty() {
            settings.insert(u, found);
        }
    }
    if settings.is_empty() {
        return;
    }
    let mut calls: Vec<Call> = Vec::new();
    let mut chain: Vec<&Node> = Vec::new();
    walk(top, &settings, &mut chain, &mut calls);
    for call in calls {
        let (Some(lib), Some(caller)) = (program(call.lib), program(call.unit)) else {
            continue;
        };
        let set: Vec<&Setting> = settings[&call.lib]
            .iter()
            .filter(|s| {
                !call.chain.iter().any(|&(u, span)| {
                    program(u).is_some_and(|p| passes(p, span, s.name) || assigns(&p.ast, s.name))
                })
            })
            .collect();
        if set.is_empty() {
            continue;
        }
        let file = lib.sources.path(lib.main);
        let file = file
            .file_name()
            .map_or(file.to_string_lossy(), |n| n.to_string_lossy());
        let written: Vec<String> = set
            .iter()
            .map(|s| format!("`{} = {}`", s.name, s.value))
            .collect();
        let names: Vec<String> = set.iter().map(|s| format!("`{}`", s.name)).collect();
        let one = set.len() == 1;
        let message = format!(
            "{} at the top of {file} {} apply to its modules when the file is used (OpenSCAD \
             behaviour: special variables come from the caller)",
            join(&written),
            if one { "doesn't" } else { "don't" },
        );
        let fix = format!(
            "pass {} in the call (`{}({} = ...)`) or set {} in this file",
            join(&names),
            call.module,
            set[0].name,
            if one { "it" } else { "them" },
        );
        let mut d = Diagnostic::new(DiagCode::UseSpecialVariables, Severity::Warning, message)
            .at(call.span, call.line)
            .with_base(PathBase::MainFileDir);
        d.hints.push(Hint {
            message: fix,
            replacement: None,
        });
        con.note(&d, &caller.sources, cwd);
    }
}

/// A call into a used file: the file's unit, the call's unit, span, line
/// and module name, and the calls it is made from (unit and span each).
struct Call {
    lib: u32,
    unit: u32,
    span: Span,
    line: u32,
    module: String,
    chain: Vec<(u32, Span)>,
}

/// Find the first call into each used file of `settings`, in tree order.
/// A node built by the file's code whose parent was built elsewhere marks
/// the parent as the call.
fn walk<'n>(
    n: &'n Node,
    settings: &HashMap<u32, Vec<Setting>>,
    chain: &mut Vec<&'n Node>,
    calls: &mut Vec<Call>,
) {
    let unit = n.origin.as_ref().map(|o| o.unit);
    for c in &n.children {
        if let (Some(o), Some(parent)) = (&c.origin, n.origin.as_ref())
            && Some(o.unit) != unit
            && settings.contains_key(&o.unit)
            && !calls.iter().any(|k| k.lib == o.unit)
        {
            let mut path: Vec<(u32, Span)> = chain
                .iter()
                .filter_map(|a| a.origin.as_ref())
                .map(|a| (a.unit, a.span))
                .filter(|&(u, _)| u != o.unit)
                .collect();
            path.push((parent.unit, parent.span));
            calls.push(Call {
                lib: o.unit,
                unit: parent.unit,
                span: parent.span,
                line: parent.line,
                module: parent.name.clone(),
                chain: path,
            });
        }
        chain.push(n);
        walk(c, settings, chain, calls);
        chain.pop();
    }
}

/// Whether the call at `span` in `p` passes `name` as an argument.
fn passes(p: &Program, span: Span, name: &str) -> bool {
    fn find(s: &Scope, ast: &Ast, span: Span, name: &str) -> Option<bool> {
        for i in &s.instantiations {
            if i.span == span {
                return Some(
                    i.args
                        .iter()
                        .any(|a| a.name.is_some_and(|n| ast.name(n) == name)),
                );
            }
            if let Some(r) = find(&i.children, ast, span, name) {
                return Some(r);
            }
        }
        s.modules
            .iter()
            .find_map(|m| find(&m.body, ast, span, name))
    }
    find(&p.ast.root, &p.ast, span, name).unwrap_or(false)
}

/// Whether `name` is assigned anywhere in the program: at its top, in a
/// module's body or in a child block.
fn assigns(ast: &Ast, name: &str) -> bool {
    fn scope(s: &Scope, ast: &Ast, name: &str) -> bool {
        s.assignments.iter().any(|a| ast.name(a.name) == name)
            || s.modules.iter().any(|m| scope(&m.body, ast, name))
            || s.instantiations
                .iter()
                .any(|i| scope(&i.children, ast, name))
    }
    scope(&ast.root, ast, name)
}

/// `a`, `a and b`, `a, b and c`.
fn join(items: &[String]) -> String {
    match items {
        [] => String::new(),
        [a] => a.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}
