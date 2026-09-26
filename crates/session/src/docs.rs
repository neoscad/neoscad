//! `neoscad docs` as a session operation: builtins from the `docs`
//! crate's reference data, and with a file the modules and functions it
//! defines, includes or `use`s, with their comment blocks.

use std::path::{Path, PathBuf};

use docs::{Entry, Kind, UserDoc};
use lang::loader::FileSystem;
use serde_json::{Value, json};

use crate::Session;

/// A docs query.
#[derive(Debug, Clone, Default)]
pub struct DocsRequest {
    /// The name to look up; `None` for an index.
    pub name: Option<String>,
    /// A file whose own (and included and `use`d) definitions are
    /// searched first, absolute or relative to `cwd`.
    pub file: Option<String>,
    pub cwd: Option<PathBuf>,
    /// The whole comment block of a user definition, not the compact
    /// form.
    pub full: bool,
}

/// A docs answer: text for people, JSON for agents.
#[derive(Debug, Clone)]
pub struct DocsResult {
    /// 0, or 1 when the name is unknown or the file cannot be read.
    pub exit_code: u8,
    pub text: String,
    pub json: Value,
}

fn kind_name(k: Kind) -> &'static str {
    k.name()
}

fn entry_json(e: &Entry) -> Value {
    json!({
        "source": "builtin",
        "kind": kind_name(e.kind),
        "name": e.name,
        "signature": e.signature,
        "summary": e.summary,
        "params": e.params.iter().map(|p| json!({
            "name": p.name, "type": p.ty, "default": p.default, "doc": p.doc,
        })).collect::<Vec<_>>(),
        "returns": e.returns,
        "example": e.example,
        "notes": e.notes,
    })
}

fn user_json(d: &UserDoc, location: &str, full: bool) -> Value {
    let mut v = json!({
        "source": "user",
        "kind": kind_name(d.kind),
        "name": d.name,
        "signature": d.signature,
        "file": location,
        "line": d.line,
    });
    if d.sections.is_empty() || full {
        v["comment"] = json!(d.comment);
    }
    if !d.sections.is_empty() {
        v["sections"] = d
            .sections
            .iter()
            .filter(|s| full || matches!(s.title.as_str(), "Synopsis" | "Usage" | "Arguments"))
            .map(|s| json!({"title": s.title, "text": s.text, "lines": s.lines}))
            .collect();
    }
    v
}

impl Session {
    /// Look up a builtin or a user definition (see [`DocsRequest`]).
    pub fn docs(&self, req: &DocsRequest) -> DocsResult {
        let cwd = req.cwd.clone().unwrap_or_else(|| self.cfg.work_dir.clone());
        let mut user: Vec<UserDoc> = Vec::new();
        if let Some(f) = &req.file {
            match self.user_docs(&cwd.join(f)) {
                Ok(u) => user = u,
                Err(e) => {
                    return DocsResult {
                        exit_code: 1,
                        text: format!("neoscad docs: {e}\n"),
                        json: json!({"schema": 1, "exit_code": 1, "error": e}),
                    };
                }
            }
        }
        let rel = |p: &Path| lang::diag::relative_path(p, &cwd).display().to_string();
        let Some(name) = &req.name else {
            return self.docs_index(&user, &rel);
        };
        let found_user: Vec<&UserDoc> = user.iter().filter(|d| d.name == *name).collect();
        let found_builtin = docs::builtin(name);
        if found_user.is_empty() && found_builtin.is_empty() {
            let experimental = eval::builtins()
                .into_iter()
                .find(|b| b.name == *name && b.status == eval::BuiltinStatus::Experimental);
            let mut msg = match experimental {
                Some(b) => format!(
                    "'{name}' is an experimental OpenSCAD builtin {}, not enabled in neoscad",
                    match b.kind {
                        eval::BuiltinKind::Module => "module",
                        eval::BuiltinKind::Function => "function",
                        eval::BuiltinKind::Variable => "variable",
                    }
                ),
                None => format!(
                    "no builtin{} named '{name}'",
                    if req.file.is_some() {
                        " or definition"
                    } else {
                        ""
                    }
                ),
            };
            let names = docs::builtins()
                .iter()
                .map(|e| e.name.as_str())
                .chain(user.iter().map(|d| d.name.as_str()));
            let hint = crate::diag::did_you_mean(name, names).map(str::to_string);
            if let Some(h) = &hint {
                msg.push_str(&format!("; did you mean '{h}'?"));
            } else if req.file.is_none() {
                msg.push_str("; for your own or a library's code add --in FILE");
            }
            return DocsResult {
                exit_code: 1,
                text: format!("neoscad docs: {msg}\n"),
                json: json!({"schema": 1, "exit_code": 1, "name": name, "error": msg,
                             "did_you_mean": hint, "entries": []}),
            };
        }
        let mut text = String::new();
        let mut entries = Vec::new();
        for d in &found_user {
            let loc = rel(&d.file);
            text.push_str(&docs::user::render(d, &loc, req.full));
            entries.push(user_json(d, &loc, req.full));
        }
        for e in &found_builtin {
            if !text.is_empty() {
                text.push('\n');
            }
            text.push_str(&docs::entry_text(e));
            entries.push(entry_json(e));
        }
        DocsResult {
            exit_code: 0,
            text,
            json: json!({"schema": 1, "exit_code": 0, "name": name, "entries": entries}),
        }
    }

    fn docs_index(&self, user: &[UserDoc], rel: &dyn Fn(&Path) -> String) -> DocsResult {
        let mut text = String::new();
        let mut defs = Vec::new();
        if !user.is_empty() {
            let mut by_file: Vec<(String, Vec<String>)> = Vec::new();
            for d in user {
                let f = rel(&d.file);
                let label = format!(
                    "{}{}",
                    d.name,
                    if d.kind == Kind::Function { "()" } else { "" }
                );
                match by_file.last_mut() {
                    Some((lf, v)) if *lf == f => v.push(label),
                    _ => by_file.push((f, vec![label])),
                }
                defs.push(json!({"kind": kind_name(d.kind), "name": d.name, "file": rel(&d.file), "line": d.line}));
            }
            for (f, names) in by_file {
                text.push_str(&format!("{f}: {}\n", names.join(" ")));
            }
            text.push_str("(names ending in () are functions)\n");
        } else {
            text = docs::index_text();
        }
        let builtins: Vec<Value> = docs::builtins()
            .iter()
            .map(|e| json!({"kind": kind_name(e.kind), "name": e.name, "summary": e.summary}))
            .collect();
        DocsResult {
            exit_code: 0,
            text,
            json: json!({"schema": 1, "exit_code": 0, "builtins": builtins, "definitions": defs}),
        }
    }

    /// The definitions of a file, its includes and the libraries it
    /// `use`s (one level: their own `use`s are not followed).
    fn user_docs(&self, path: &Path) -> Result<Vec<UserDoc>, String> {
        let (fs, libs) = (&*self.fs, &self.cfg.libs);
        let path = crate::normal(path);
        let text = fs
            .read(&path)
            .map_err(|e| format!("cannot read '{}': {e}", path.display()))?;
        let program = lang::parse_program(path.clone(), text, fs, libs);
        let mut out = docs::definitions(&program);
        for key in lang::deps::resolve_uses(&program, fs, libs) {
            let p = PathBuf::from(&key);
            if let Ok(t) = fs.read(&p) {
                let lib = lang::parse_library(p, t, &path, fs, libs);
                out.extend(docs::definitions(&lib));
            }
        }
        Ok(out)
    }
}
