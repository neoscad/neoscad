//! `-d` dependency files and `-m` make commands (`handle_dep.cc`).
//!
//! OpenSCAD records a dependency whenever it opens a file on the
//! program's behalf: the input (as given on the command line), every
//! `include`d and `use`d file it found (as full paths), and every file an
//! `import()`, `surface()` or `dxf_dim()`/`dxf_cross()` names, found or
//! not. `-d` writes them as a make rule after all exports. `-m CMD` runs
//! `CMD 'file'` for an imported file that does not exist yet, before it is
//! read, so `make` can build it.
//!
//! OpenSCAD keeps the dependencies in an `unordered_set`, so its file lists
//! them in hash order; here they are in the order first seen (input,
//! includes, used libraries, imported files in tree order), which is
//! stable from run to run. The set is process-wide, as upstream's is,
//! because one `-d` file covers every `-o` and every animation frame.

use std::path::Path;
use std::sync::Mutex;

use eval::node::{Node, NodeKind};

static DEPS: Mutex<Vec<String>> = Mutex::new(Vec::new());
static MAKE_COMMAND: Mutex<Option<String>> = Mutex::new(None);

/// `-m`: the command to run for missing imported files.
pub fn set_make_command(cmd: Option<String>) {
    *MAKE_COMMAND.lock().expect("make command") = cmd;
}

/// `handle_dep`: record `path` once. Returns whether it was new.
pub fn add(path: &str) -> bool {
    // make treats a space as a separator, so OpenSCAD escapes it (and only
    // it) as `\ `.
    let dep = path.replace(' ', "\\ ");
    let mut deps = DEPS.lock().expect("dependency list");
    if deps.contains(&dep) {
        return false;
    }
    deps.push(dep);
    true
}

/// The files a parsed program read: its own sources after the main file
/// (which the caller records as given), then each used library's.
pub fn add_sources(sources: &lang::source::SourceMap) {
    for (id, f) in sources.iter() {
        if id.0 != 0 {
            add(&f.path.to_string_lossy());
        }
    }
}

/// The files `import()` and `surface()` nodes name, in tree order. For one
/// that does not exist, `-m` runs first, the moment OpenSCAD's
/// instantiation does it (`ImportNode.cc:85`, `SurfaceNode.cc:75`).
pub fn add_node_files(root: &Node, fs: &dyn lang::loader::FileSystem) {
    // An explicit stack: trees from recursive modules can be as deep as
    // the evaluator allowed.
    let mut stack = vec![root];
    while let Some(n) = stack.pop() {
        let file = match &n.kind {
            NodeKind::Import(i) => Some(&i.file),
            NodeKind::Surface { file, .. } => Some(file),
            _ => None,
        };
        if let Some(file) = file.filter(|f| !f.is_empty())
            && add(file)
            && !fs.exists(Path::new(file))
        {
            run_make(file);
        }
        stack.extend(n.children.iter().rev());
    }
}

/// `make_cmd 'file'` through the shell, reporting failures as
/// `handle_dep` does; the export goes on either way.
fn run_make(file: &str) {
    let Some(cmd) = MAKE_COMMAND.lock().expect("make command").clone() else {
        return;
    };
    let line = format!("{cmd} '{}'", file.replace('\'', "'\\''"));
    match std::process::Command::new("sh")
        .arg("-c")
        .arg(&line)
        .status()
    {
        Err(e) => eprintln!("ERROR: system(make_cmd) failed: {e}"),
        Ok(s) => match s.code() {
            None => eprintln!("ERROR: {line}: Process terminated abnormally!"),
            Some(0) => {}
            Some(c) => eprintln!("ERROR: {line}: Exit status {c}"),
        },
    }
}

/// `write_deps`: `out1 out2: \` then one dependency per tab-indented
/// continuation line.
pub fn render(outputs: &[String]) -> String {
    let mut s = outputs.join(" ");
    s.push(':');
    for d in DEPS.lock().expect("dependency list").iter() {
        s.push_str(" \\\n\t");
        s.push_str(d);
    }
    s.push('\n');
    s
}

/// Write the `-d` file. On failure prints OpenSCAD's messages and returns
/// false.
pub fn write(file: &str, outputs: &[String]) -> bool {
    match std::fs::write(file, render(outputs)) {
        Ok(()) => true,
        Err(_) => {
            eprintln!("Can't open dependencies file `{file}' for writing!");
            eprintln!("Error writing deps");
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rule_layout_and_escaping() {
        // One test owns the process-wide list.
        DEPS.lock().unwrap().clear();
        assert!(add("a.scad"));
        assert!(add("/x/sub/u 1.scad"));
        assert!(!add("/x/sub/u 1.scad"));
        assert_eq!(
            render(&["a.stl".into(), "a.off".into()]),
            "a.stl a.off: \\\n\ta.scad \\\n\t/x/sub/u\\ 1.scad\n"
        );
        DEPS.lock().unwrap().clear();
        assert_eq!(render(&["a.stl".into()]), "a.stl:\n");
    }
}
