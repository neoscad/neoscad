//! A small interpreter for the subset of CMake that OpenSCAD's
//! `tests/CMakeLists.txt` uses to register its regression tests.
//!
//! OpenSCAD's test list is not stored anywhere as data: it is computed by
//! CMake from globs, list arithmetic and helper functions
//! (`tests/cmake/TestFunctions.cmake`). Hand-copying the result would drift
//! the moment upstream adds a test, and CMake itself is not a dependency we
//! want (it would also require configuring OpenSCAD's whole build). So this
//! module evaluates the file directly: generic commands (`set`, `list`,
//! `file(GLOB)`, `if`, `foreach`) are interpreted, and the test-registration
//! helpers (`add_cmdline_test`, `add_failing_test`, `set_test_config`, ...)
//! are native ports of their CMake definitions, recording each registration
//! instead of calling `add_test`.
//!
//! Anything the interpreter does not understand is an error rather than a
//! silent skip, so an upstream change that needs new support shows up the
//! next time the manifest is regenerated instead of quietly dropping tests.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::path::Path;

/// One argument as written in the source; quoting changes how CMake expands
/// and splits it, so the distinction is kept until evaluation.
#[derive(Debug, Clone)]
enum RawArg {
    Unquoted(String),
    Quoted(String),
    Bracket(String),
}

#[derive(Debug, Clone)]
struct Command {
    name: String,
    args: Vec<RawArg>,
    line: usize,
}

/// How a test was registered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegKind {
    /// `add_cmdline_test`: run a tool, compare its output file.
    Cmdline,
    /// `add_failing_test`: run through `shouldfail.py`, check the exit code.
    Failing,
    /// A raw `add_test` (relative-output tests, harness self-tests).
    Raw,
}

/// A test registration, with every value CMake would have baked into the
/// ctest command line. Paths are absolute, as CMake produces them.
#[derive(Debug, Clone)]
pub struct Registration {
    pub kind: RegKind,
    pub name: String,
    pub group: String,
    /// Test input (`SCADFILE`), absent for raw tests.
    pub file: Option<String>,
    /// `-f` value: the input's name without extension, spaces replaced.
    pub basename: String,
    pub suffix: String,
    pub expected_dir: Option<String>,
    /// Python driver for SCRIPT tests (`export_import_pngtest.py`, ...).
    pub script: Option<String>,
    pub openscad: bool,
    pub stdio: bool,
    pub experimental: bool,
    /// Arguments after the input file: the 2D camera options (if any)
    /// followed by the registration's ARGS.
    pub args: Vec<String>,
    /// The registration's own ARGS only (no camera options).
    pub test_args: Vec<String>,
    pub configs: Vec<String>,
    /// `OPENSCAD_TEST_EXCLUDE_LINE` as the test's environment would carry it.
    pub exclude_line: Option<String>,
    /// Full command for raw tests.
    pub command: Vec<String>,
    pub line: usize,
    /// Line of the `set_tests_properties`/`disable_tests_safe` call that
    /// disabled this test, if any.
    pub disabled_at: Option<usize>,
}

/// Result of evaluating the test CMakeLists.
#[derive(Debug, Default)]
pub struct Evaluation {
    pub registrations: Vec<Registration>,
    /// `configure_file` outputs: (template, output, COPYONLY) with absolute
    /// paths. CMake writes these before globbing.
    pub configured_files: Vec<(String, String, bool)>,
    /// Structural problems worth reporting (duplicate names, disabling an
    /// unknown test, ...). These depend only on the CMake file.
    pub diagnostics: Vec<String>,
    /// `message(WARNING|FATAL_ERROR ...)` output. Kept apart from
    /// `diagnostics` because it can depend on local state (e.g. whether the
    /// MCAD submodule is checked out), which must not leak into the
    /// committed manifest.
    pub messages: Vec<String>,
    /// Final variable values, for callers that need e.g. OPENSCAD_BINPATH.
    pub vars: HashMap<String, String>,
}

pub struct Interpreter {
    vars: HashMap<String, String>,
    functions: BTreeSet<String>,
    /// Paths that exist only because `configure_file` would create them.
    virtual_files: BTreeSet<String>,
    ctest_env: BTreeMap<String, String>,
    per_test_exclude: HashMap<String, String>,
    out: Evaluation,
    index: HashMap<String, usize>,
}

impl std::fmt::Debug for Interpreter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Interpreter").finish_non_exhaustive()
    }
}

type Result<T> = std::result::Result<T, String>;

impl Interpreter {
    /// `vars` are the cache/platform variables a configured build would have
    /// (CMAKE_SOURCE_DIR, APPLE, EXPERIMENTAL, ...).
    pub fn new(vars: HashMap<String, String>) -> Self {
        Self {
            vars,
            functions: BTreeSet::new(),
            virtual_files: BTreeSet::new(),
            ctest_env: BTreeMap::new(),
            per_test_exclude: HashMap::new(),
            out: Evaluation::default(),
            index: HashMap::new(),
        }
    }

    pub fn run_file(mut self, path: &Path) -> Result<Evaluation> {
        let src = fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let cmds = parse(&src)?;
        self.exec_block(&cmds)?;
        // Per-test exclusions are merged after registration, as
        // test_env_append_value edits the test's ENVIRONMENT property.
        for (test, regex) in std::mem::take(&mut self.per_test_exclude) {
            if let Some(&i) = self.index.get(&test) {
                let r = &mut self.out.registrations[i];
                r.exclude_line = Some(regex_alt(r.exclude_line.as_deref().unwrap_or(""), &regex));
            } else {
                self.out
                    .diagnostics
                    .push(format!("line exclusion for unknown test {test}"));
            }
        }
        self.out.vars = self.vars;
        Ok(self.out)
    }

    fn exec_block(&mut self, cmds: &[Command]) -> Result<()> {
        let mut i = 0;
        while i < cmds.len() {
            let c = &cmds[i];
            match c.name.as_str() {
                "if" => {
                    let end = find_block_end(cmds, i, &["if"], "endif")?;
                    self.exec_if(&cmds[i..=end])?;
                    i = end + 1;
                }
                "foreach" => {
                    let end = find_block_end(cmds, i, &["foreach"], "endforeach")?;
                    self.exec_foreach(c, &cmds[i + 1..end])?;
                    i = end + 1;
                }
                "function" | "macro" => {
                    let endname = format!("end{}", c.name);
                    let end = find_block_end(cmds, i, &[c.name.as_str()], &endname)?;
                    let args = self.expand_args(&c.args);
                    if let Some(name) = args.first() {
                        self.functions.insert(name.to_lowercase());
                    }
                    i = end + 1;
                }
                _ => {
                    self.exec_command(c)
                        .map_err(|e| format!("CMakeLists.txt:{}: {}(): {e}", c.line, c.name))?;
                    i += 1;
                }
            }
        }
        Ok(())
    }

    fn exec_if(&mut self, block: &[Command]) -> Result<()> {
        // Split the block into branches at depth-0 elseif/else.
        let mut branches: Vec<(Option<&Command>, usize, usize)> = Vec::new();
        let mut depth = 0usize;
        let mut start = 1;
        let mut cond: Option<&Command> = Some(&block[0]);
        for (j, c) in block.iter().enumerate().skip(1) {
            match c.name.as_str() {
                "if" => depth += 1,
                "endif" if depth > 0 => depth -= 1,
                "endif" => {
                    branches.push((cond, start, j));
                }
                "elseif" | "else" if depth == 0 => {
                    branches.push((cond, start, j));
                    cond = if c.name == "else" { None } else { Some(c) };
                    start = j + 1;
                }
                _ => {}
            }
        }
        for (cond, s, e) in branches {
            let take = match cond {
                None => true,
                Some(c) => self
                    .eval_condition(&c.args)
                    .map_err(|err| format!("CMakeLists.txt:{}: if(): {err}", c.line))?,
            };
            if take {
                return self.exec_block(&block[s..e]);
            }
        }
        Ok(())
    }

    fn exec_foreach(&mut self, head: &Command, body: &[Command]) -> Result<()> {
        let args = self.expand_args(&head.args);
        let (var, rest) = args.split_first().ok_or("foreach without variable")?;
        let items: Vec<String> = match rest.first().map(String::as_str) {
            Some("IN") => {
                let mut items = Vec::new();
                let mut mode = "";
                for a in &rest[1..] {
                    match a.as_str() {
                        "LISTS" | "ITEMS" => mode = if a == "LISTS" { "L" } else { "I" },
                        _ if mode == "L" => items.extend(split_list(self.get(a))),
                        _ if mode == "I" => items.push(a.clone()),
                        _ => return Err(format!("unsupported foreach form: {args:?}")),
                    }
                }
                items
            }
            Some("RANGE") => return Err("foreach(RANGE) is not supported".into()),
            _ => rest.to_vec(),
        };
        let saved = self.vars.get(var).cloned();
        for item in items {
            self.vars.insert(var.clone(), item);
            self.exec_block(body)?;
        }
        match saved {
            Some(v) => self.vars.insert(var.clone(), v),
            None => self.vars.remove(var),
        };
        Ok(())
    }

    fn get(&self, name: &str) -> &str {
        self.vars.get(name).map(String::as_str).unwrap_or("")
    }

    fn exists(&self, path: &str) -> bool {
        !path.is_empty() && (Path::new(path).exists() || self.virtual_files.contains(path))
    }

    /// Expand `${VAR}`, `$ENV{VAR}` and `$CACHE{VAR}` references. Environment
    /// variables expand to nothing so the result does not depend on the
    /// machine the manifest is generated on.
    fn expand(&self, s: &str) -> String {
        let mut out = String::with_capacity(s.len());
        let b = s.as_bytes();
        let mut i = 0;
        while i < b.len() {
            if b[i] == b'\\' && i + 1 < b.len() {
                // Keep escapes for the later unescape step; skipping the next
                // byte stops `\${` from being treated as a reference.
                out.push_str(&s[i..i + 2]);
                i += 2;
                continue;
            }
            if b[i] == b'$' {
                let (prefix_len, kind) = if s[i..].starts_with("${") {
                    (2, "")
                } else if s[i..].starts_with("$ENV{") {
                    (5, "ENV")
                } else if s[i..].starts_with("$CACHE{") {
                    (7, "CACHE")
                } else {
                    (0, "")
                };
                if prefix_len > 0
                    && let Some(close) = matching_brace(s, i + prefix_len)
                {
                    let name = self.expand(&s[i + prefix_len..close]);
                    if kind != "ENV" {
                        out.push_str(self.get(&name));
                    }
                    i = close + 1;
                    continue;
                }
            }
            let ch = s[i..].chars().next().expect("in bounds");
            out.push(ch);
            i += ch.len_utf8();
        }
        out
    }

    /// Evaluate arguments into the flat list a command receives: unquoted
    /// arguments are split on `;` and empty elements dropped; quoted and
    /// bracket arguments stay single, as in CMake.
    fn expand_args(&self, raw: &[RawArg]) -> Vec<String> {
        let mut out = Vec::new();
        for a in raw {
            match a {
                RawArg::Unquoted(s) => {
                    let v = unescape(&self.expand(s));
                    out.extend(split_list(&v));
                }
                RawArg::Quoted(s) => out.push(unescape(&self.expand(s))),
                RawArg::Bracket(s) => out.push(s.clone()),
            }
        }
        out
    }

    /// Arguments as a CMake function sees them after `cmake_parse_arguments`:
    /// ARGN is a list, so a quoted argument containing `;` is split too.
    fn function_args(&self, raw: &[RawArg]) -> Vec<String> {
        self.expand_args(raw)
            .iter()
            .flat_map(|a| split_list(a))
            .map(|a| a.replace("$<SEMICOLON>", ";"))
            .collect()
    }

    fn exec_command(&mut self, c: &Command) -> Result<()> {
        let name = c.name.as_str();
        match name {
            // Build-system plumbing with no effect on which tests exist.
            "cmake_minimum_required"
            | "cmake_policy"
            | "project"
            | "enable_testing"
            | "include"
            | "find_package"
            | "pkg_check_modules"
            | "add_custom_target"
            | "set_property"
            | "set_directory_properties" => Ok(()),
            "message" => {
                let args = self.expand_args(&c.args);
                if let Some(level @ ("WARNING" | "FATAL_ERROR" | "SEND_ERROR")) =
                    args.first().map(String::as_str)
                {
                    self.out.messages.push(format!(
                        "CMakeLists.txt:{}: message({level}) {}",
                        c.line,
                        args[1..].join(" ")
                    ));
                }
                Ok(())
            }
            "set" => {
                let mut args = self.expand_args(&c.args);
                if let Some(p) = args.iter().position(|a| a == "CACHE") {
                    args.truncate(p);
                }
                if args.last().map(String::as_str) == Some("PARENT_SCOPE") {
                    args.pop();
                }
                let (var, vals) = args.split_first().ok_or("set without variable")?;
                if vals.is_empty() {
                    self.vars.remove(var);
                } else {
                    self.vars.insert(var.clone(), vals.join(";"));
                }
                Ok(())
            }
            "unset" => {
                let args = self.expand_args(&c.args);
                if let Some(v) = args.first() {
                    self.vars.remove(v);
                }
                Ok(())
            }
            "option" => {
                let args = self.expand_args(&c.args);
                if let Some(v) = args.first()
                    && !self.vars.contains_key(v)
                {
                    let val = args.get(2).cloned().unwrap_or_else(|| "OFF".into());
                    self.vars.insert(v.clone(), val);
                }
                Ok(())
            }
            "find_path" => {
                let args = self.expand_args(&c.args);
                if let Some(v) = args.first() {
                    self.vars.insert(v.clone(), format!("{v}-NOTFOUND"));
                }
                Ok(())
            }
            "get_filename_component" => {
                let args = self.expand_args(&c.args);
                let [var, path, mode, ..] = args.as_slice() else {
                    return Err("expected VAR FILE MODE".into());
                };
                let v = match mode.as_str() {
                    "NAME_WE" => name_we(path),
                    "NAME" => file_name(path).to_string(),
                    "DIRECTORY" | "PATH" => path
                        .rsplit_once('/')
                        .map(|(d, _)| d.to_string())
                        .unwrap_or_default(),
                    m => return Err(format!("unsupported mode {m}")),
                };
                self.vars.insert(var.clone(), v);
                Ok(())
            }
            "list" => self.cmd_list(c),
            "file" => self.cmd_file(c),
            "configure_file" => {
                let args = self.expand_args(&c.args);
                let [input, output, ..] = args.as_slice() else {
                    return Err("expected INPUT OUTPUT".into());
                };
                self.virtual_files.insert(output.clone());
                let copy_only = args.iter().any(|a| a == "COPYONLY");
                self.out
                    .configured_files
                    .push((input.clone(), output.clone(), copy_only));
                Ok(())
            }
            "ctest_env_append_value" => {
                let args = self.expand_args(&c.args);
                let [key, value, ..] = args.as_slice() else {
                    return Err("expected KEY VALUE".into());
                };
                let old = self.ctest_env.get(key).cloned().unwrap_or_default();
                self.ctest_env.insert(key.clone(), regex_alt(&old, value));
                Ok(())
            }
            "add_line_exclusion_for_test" => {
                let args = self.expand_args(&c.args);
                let [base, test, regex, ..] = args.as_slice() else {
                    return Err("expected TEST_BASENAME TEST_NAME REGEX".into());
                };
                let full = format!("{base}_{test}");
                let old = self.per_test_exclude.remove(&full).unwrap_or_default();
                self.per_test_exclude.insert(full, regex_alt(&old, regex));
                Ok(())
            }
            "set_test_config" => {
                let args = self.function_args(&c.args);
                let (config, rest) = args.split_first().ok_or("missing CONFIG")?;
                let p = parse_arguments(rest, &[], &[], &["FILES", "PREFIXES"]);
                let files = p.multi("FILES");
                let prefixes = p.multi("PREFIXES");
                let names: Vec<String> = if prefixes.is_empty() {
                    files.to_vec()
                } else {
                    prefixes
                        .iter()
                        .flat_map(|pre| files.iter().map(move |f| test_fullname(pre, f)))
                        .collect()
                };
                self.set_test_config(config, &names);
                Ok(())
            }
            "remove_test_config" => {
                let args = self.function_args(&c.args);
                let (config, rest) = args.split_first().ok_or("missing CONFIG")?;
                let p = parse_arguments(rest, &[], &[], &["FILES"]);
                let key = format!("{config}_TEST_CONFIG");
                let remove: BTreeSet<&String> = p.multi("FILES").iter().collect();
                let kept: Vec<String> = split_list(self.get(&key))
                    .into_iter()
                    .filter(|n| !remove.contains(n))
                    .collect();
                self.vars.insert(key, kept.join(";"));
                Ok(())
            }
            "add_cmdline_test" => self.add_cmdline_test(c),
            "add_failing_test" => self.add_failing_test(c),
            "add_output_file_test" => self.add_output_file_test(c),
            "add_test" => {
                let args = self.function_args(&c.args);
                let p = parse_arguments(&args, &[], &["NAME"], &["CONFIGURATIONS", "COMMAND"]);
                let name = p.one("NAME").ok_or("add_test without NAME")?.to_string();
                let reg = Registration {
                    kind: RegKind::Raw,
                    name: name.clone(),
                    group: name,
                    file: None,
                    basename: String::new(),
                    suffix: String::new(),
                    expected_dir: None,
                    script: None,
                    openscad: false,
                    stdio: false,
                    experimental: false,
                    args: Vec::new(),
                    test_args: Vec::new(),
                    configs: p.multi("CONFIGURATIONS").to_vec(),
                    exclude_line: None,
                    command: p.multi("COMMAND").to_vec(),
                    line: c.line,
                    disabled_at: None,
                };
                self.register(reg);
                Ok(())
            }
            "set_tests_properties" => {
                let args = self.function_args(&c.args);
                let p = args
                    .iter()
                    .position(|a| a == "PROPERTIES")
                    .ok_or("no PROPERTIES")?;
                let props = &args[p + 1..];
                let disables = props
                    .chunks(2)
                    .any(|kv| kv.len() == 2 && kv[0] == "DISABLED" && is_true_constant(&kv[1]));
                if disables {
                    for name in &args[..p] {
                        self.disable(name, c.line, true);
                    }
                }
                Ok(())
            }
            "disable_tests_safe" => {
                for name in self.function_args(&c.args) {
                    self.disable(&name, c.line, false);
                }
                Ok(())
            }
            other if self.functions.contains(other) => Err(format!(
                "call to CMake function '{other}' defined in the file, which has no native port"
            )),
            other => Err(format!("unsupported command '{other}'")),
        }
    }

    fn disable(&mut self, name: &str, line: usize, strict: bool) {
        match self.index.get(name) {
            Some(&i) => self.out.registrations[i].disabled_at = Some(line),
            None if strict => self.out.diagnostics.push(format!(
                "CMakeLists.txt:{line}: disabling unknown test {name}"
            )),
            None => {}
        }
    }

    fn register(&mut self, reg: Registration) {
        if self.index.contains_key(&reg.name) {
            // ctest refuses duplicate names; keep the first and report.
            self.out.diagnostics.push(format!(
                "CMakeLists.txt:{}: duplicate test name {}",
                reg.line, reg.name
            ));
            return;
        }
        self.index
            .insert(reg.name.clone(), self.out.registrations.len());
        self.out.registrations.push(reg);
    }

    fn set_test_config(&mut self, config: &str, names: &[String]) {
        let key = format!("{config}_TEST_CONFIG");
        let mut list = split_list(self.get(&key));
        list.extend(names.iter().cloned());
        self.vars.insert(key, list.join(";"));
        let mut configs = split_list(self.get("TEST_CONFIGS"));
        if !configs.iter().any(|c| c == config) {
            configs.push(config.to_string());
            configs.sort();
            self.vars.insert("TEST_CONFIGS".into(), configs.join(";"));
        }
    }

    fn get_test_config(&self, name: &str) -> Vec<String> {
        split_list(self.get("TEST_CONFIGS"))
            .into_iter()
            .filter(|c| {
                split_list(self.get(&format!("{c}_TEST_CONFIG")))
                    .iter()
                    .any(|n| n == name)
            })
            .collect()
    }

    /// Port of `add_cmdline_test` (tests/cmake/TestFunctions.cmake:393-494).
    fn add_cmdline_test(&mut self, c: &Command) -> Result<()> {
        let args = self.function_args(&c.args);
        let (group, rest) = args.split_first().ok_or("missing test basename")?;
        let p = parse_arguments(
            rest,
            &["OPENSCAD", "STDIO", "EXPERIMENTAL"],
            &["EXE", "SCRIPT", "SUFFIX", "KERNEL", "EXPECTEDDIR"],
            &["FILES", "ARGS"],
        );
        let openscad = p.flag("OPENSCAD");
        if openscad && (p.one("EXE").is_some() || p.one("SCRIPT").is_some()) {
            return Err("OPENSCAD flag alongside EXE or SCRIPT".into());
        }
        let suffix = p.one("SUFFIX").unwrap_or_default().to_string();
        let all_2d = split_list(self.get("ALL_2D_FILES"));
        let exclude_line = self.ctest_env.get("OPENSCAD_TEST_EXCLUDE_LINE").cloned();
        for file in p.multi("FILES") {
            let basename = name_we(file).replace(' ', "_");
            let fullname = format!("{group}_{basename}");
            let mut camera: Vec<String> = Vec::new();
            if all_2d.iter().any(|f| f == file) {
                camera = [
                    "--camera=0,0,100,0,0,0",
                    "--viewall",
                    "--autocenter",
                    "--projection=ortho",
                ]
                .map(String::from)
                .to_vec();
            }
            let found = self.get_test_config(&fullname);
            if found.is_empty() {
                self.set_test_config("Default", std::slice::from_ref(&fullname));
            }
            self.set_test_config("All", std::slice::from_ref(&fullname));
            if !found.iter().any(|c| c == "Bugs") {
                self.set_test_config("Good", std::slice::from_ref(&fullname));
            }
            let configs = self.get_test_config(&fullname);
            let test_args = p.multi("ARGS").to_vec();
            let mut all_args = camera;
            all_args.extend(test_args.iter().cloned());
            let reg = Registration {
                kind: RegKind::Cmdline,
                name: fullname,
                group: group.clone(),
                file: Some(file.clone()),
                basename,
                suffix: suffix.clone(),
                expected_dir: p.one("EXPECTEDDIR").map(String::from),
                script: p.one("SCRIPT").map(String::from),
                openscad,
                stdio: p.flag("STDIO"),
                experimental: p.flag("EXPERIMENTAL"),
                args: all_args,
                test_args,
                configs,
                exclude_line: exclude_line.clone(),
                command: Vec::new(),
                line: c.line,
                disabled_at: None,
            };
            // CMake skips experimental tests unless EXPERIMENTAL is on; we
            // register them anyway (flagged) so the manifest can list them
            // as skipped with a reason.
            self.register(reg);
        }
        Ok(())
    }

    /// Port of `add_failing_test` (tests/cmake/TestFunctions.cmake:497-536).
    fn add_failing_test(&mut self, c: &Command) -> Result<()> {
        let args = self.function_args(&c.args);
        let (group, rest) = args.split_first().ok_or("missing test basename")?;
        let p = parse_arguments(
            rest,
            &[],
            &["RETVAL", "EXE", "SCRIPT", "SUFFIX"],
            &["FILES", "ARGS"],
        );
        let suffix = p.one("SUFFIX").ok_or("SUFFIX is required")?.to_string();
        let script = p
            .one("SCRIPT")
            .map(String::from)
            .unwrap_or_else(|| self.get("SHOULDFAIL_PY").to_string());
        let exclude_line = self.ctest_env.get("OPENSCAD_TEST_EXCLUDE_LINE").cloned();
        for file in p.multi("FILES") {
            let basename = name_we(file).replace(' ', "_");
            let fullname = format!("{group}_{basename}");
            if self.get_test_config(&fullname).is_empty() {
                self.set_test_config("Default", std::slice::from_ref(&fullname));
            }
            self.set_test_config("All", std::slice::from_ref(&fullname));
            let configs = self.get_test_config(&fullname);
            let reg = Registration {
                kind: RegKind::Failing,
                name: fullname,
                group: group.clone(),
                file: Some(file.clone()),
                basename,
                suffix: suffix.clone(),
                expected_dir: None,
                script: Some(script.clone()),
                openscad: false,
                stdio: false,
                experimental: false,
                args: p.multi("ARGS").to_vec(),
                test_args: p.multi("ARGS").to_vec(),
                configs,
                exclude_line: exclude_line.clone(),
                command: Vec::new(),
                line: c.line,
                disabled_at: None,
            };
            self.register(reg);
        }
        Ok(())
    }

    /// Port of the `add_output_file_test` function defined in
    /// tests/CMakeLists.txt itself: a `_run` test that writes to a relative
    /// path and a `_check` test that reads it back.
    fn add_output_file_test(&mut self, c: &Command) -> Result<()> {
        let args = self.function_args(&c.args);
        let (base, rest) = args.split_first().ok_or("missing basename")?;
        let p = parse_arguments(rest, &[], &["FILE", "FORMAT"], &[]);
        let file = p.one("FILE").ok_or("FILE is required")?.to_string();
        let format = p.one("FORMAT").ok_or("FORMAT is required")?.to_string();
        let out = format!("{base}.{format}");
        let bin = self.get("OPENSCAD_BINPATH").to_string();
        for (step, command) in [
            ("run", vec![bin, file.clone(), "-o".into(), out.clone()]),
            (
                "check",
                vec!["cmake".into(), "-E".into(), "cat".into(), out.clone()],
            ),
        ] {
            let name = format!("{base}_{format}_{step}");
            let reg = Registration {
                kind: RegKind::Raw,
                name,
                group: base.clone(),
                file: Some(file.clone()),
                basename: name_we(&file),
                suffix: format.clone(),
                expected_dir: None,
                script: None,
                openscad: step == "run",
                stdio: false,
                experimental: false,
                args: Vec::new(),
                test_args: Vec::new(),
                configs: vec!["Default".into()],
                exclude_line: None,
                command,
                line: c.line,
                disabled_at: None,
            };
            self.register(reg);
        }
        Ok(())
    }

    fn cmd_list(&mut self, c: &Command) -> Result<()> {
        let args = self.expand_args(&c.args);
        let [op, var, rest @ ..] = args.as_slice() else {
            return Err("expected OPERATION VAR".into());
        };
        let mut list = split_list(self.get(var));
        match op.as_str() {
            "APPEND" => list.extend(rest.iter().flat_map(|a| split_list(a))),
            "PREPEND" => {
                let mut new: Vec<String> = rest.iter().flat_map(|a| split_list(a)).collect();
                new.extend(list);
                list = new;
            }
            "REMOVE_ITEM" => {
                let remove: BTreeSet<String> = rest.iter().flat_map(|a| split_list(a)).collect();
                list.retain(|x| !remove.contains(x));
            }
            "REMOVE_DUPLICATES" => {
                let mut seen = BTreeSet::new();
                list.retain(|x| seen.insert(x.clone()));
            }
            "SORT" => list.sort(),
            "GET" => {
                let (out, idx) = rest.split_last().ok_or("GET needs an output variable")?;
                let mut got = Vec::new();
                for i in idx {
                    let i: isize = i.parse().map_err(|_| format!("bad index {i}"))?;
                    let n = list.len() as isize;
                    let j = if i < 0 { n + i } else { i };
                    let item = list
                        .get(usize::try_from(j).map_err(|_| "index out of range")?)
                        .ok_or_else(|| format!("index {i} out of range ({n} items)"))?;
                    got.push(item.clone());
                }
                self.vars.insert(out.clone(), got.join(";"));
                return Ok(());
            }
            "LENGTH" => {
                let out = rest.first().ok_or("LENGTH needs an output variable")?;
                self.vars.insert(out.clone(), list.len().to_string());
                return Ok(());
            }
            "FIND" => {
                let [item, out, ..] = rest else {
                    return Err("FIND needs ITEM OUT".into());
                };
                let pos = list
                    .iter()
                    .position(|x| x == item)
                    .map_or(-1, |p| p as isize);
                self.vars.insert(out.clone(), pos.to_string());
                return Ok(());
            }
            other => return Err(format!("unsupported list({other})")),
        }
        self.vars.insert(var.clone(), list.join(";"));
        Ok(())
    }

    fn cmd_file(&mut self, c: &Command) -> Result<()> {
        let args = self.expand_args(&c.args);
        let (op, rest) = args.split_first().ok_or("file() without operation")?;
        match op.as_str() {
            "MAKE_DIRECTORY" => Ok(()),
            "GLOB" | "GLOB_RECURSE" => {
                let (var, patterns) = rest.split_first().ok_or("GLOB needs a variable")?;
                if patterns.iter().any(|p| {
                    matches!(
                        p.as_str(),
                        "RELATIVE" | "LIST_DIRECTORIES" | "FOLLOW_SYMLINKS"
                    )
                }) {
                    return Err(format!("unsupported GLOB option in {patterns:?}"));
                }
                let mut result = Vec::new();
                for pat in patterns
                    .iter()
                    .filter(|p| p.as_str() != "CONFIGURE_DEPENDS")
                {
                    result.extend(self.glob(pat, op == "GLOB_RECURSE"));
                }
                self.vars.insert(var.clone(), result.join(";"));
                Ok(())
            }
            other => Err(format!("unsupported file({other})")),
        }
    }

    /// `file(GLOB)` for the patterns the test file uses: wildcards only in
    /// the last path component. CMake returns each expression's matches in
    /// lexicographic order; generated (configure_file) outputs are included,
    /// because CMake creates them before globbing.
    fn glob(&self, pattern: &str, recurse: bool) -> Vec<String> {
        let (dir, pat) = pattern.rsplit_once('/').unwrap_or((".", pattern));
        if !pat.contains(['*', '?', '[']) && !dir.contains(['*', '?', '[']) {
            return if self.exists(pattern) {
                vec![pattern.to_string()]
            } else {
                Vec::new()
            };
        }
        let mut found = BTreeSet::new();
        let mut stack = vec![dir.to_string()];
        while let Some(d) = stack.pop() {
            let Ok(entries) = fs::read_dir(&d) else {
                continue;
            };
            for e in entries.flatten() {
                let name = e.file_name().to_string_lossy().into_owned();
                let path = format!("{d}/{name}");
                let is_dir = e.file_type().map(|t| t.is_dir()).unwrap_or(false);
                if recurse && is_dir {
                    stack.push(path.clone());
                    continue;
                }
                if wildcard_match(pat, &name) {
                    found.insert(path);
                }
            }
        }
        for v in &self.virtual_files {
            if let Some((vd, vn)) = v.rsplit_once('/') {
                let under = if recurse {
                    vd == dir || vd.starts_with(&format!("{dir}/"))
                } else {
                    vd == dir
                };
                if under && wildcard_match(pat, vn) {
                    found.insert(v.clone());
                }
            }
        }
        found.into_iter().collect()
    }

    /// Evaluate an `if()` condition. Supports the forms the test file uses:
    /// constants, variable truthiness, NOT/AND/OR, parentheses, EXISTS,
    /// DEFINED, and the common binary comparisons.
    fn eval_condition(&self, raw: &[RawArg]) -> Result<bool> {
        let mut toks: Vec<(String, bool)> = Vec::new();
        for a in raw {
            match a {
                RawArg::Unquoted(s) => {
                    for t in split_list(&unescape(&self.expand(s))) {
                        toks.push((t, false));
                    }
                }
                RawArg::Quoted(s) => toks.push((unescape(&self.expand(s)), true)),
                RawArg::Bracket(s) => toks.push((s.clone(), true)),
            }
        }
        let mut pos = 0;
        let v = self.cond_or(&toks, &mut pos)?;
        if pos != toks.len() {
            return Err(format!("unparsed condition tokens: {:?}", &toks[pos..]));
        }
        Ok(v)
    }

    fn cond_or(&self, t: &[(String, bool)], pos: &mut usize) -> Result<bool> {
        let mut v = self.cond_and(t, pos)?;
        while t.get(*pos).is_some_and(|(s, q)| !q && s == "OR") {
            *pos += 1;
            let r = self.cond_and(t, pos)?;
            v = v || r;
        }
        Ok(v)
    }

    fn cond_and(&self, t: &[(String, bool)], pos: &mut usize) -> Result<bool> {
        let mut v = self.cond_not(t, pos)?;
        while t.get(*pos).is_some_and(|(s, q)| !q && s == "AND") {
            *pos += 1;
            let r = self.cond_not(t, pos)?;
            v = v && r;
        }
        Ok(v)
    }

    fn cond_not(&self, t: &[(String, bool)], pos: &mut usize) -> Result<bool> {
        if t.get(*pos).is_some_and(|(s, q)| !q && s == "NOT") {
            *pos += 1;
            return Ok(!self.cond_not(t, pos)?);
        }
        self.cond_primary(t, pos)
    }

    fn cond_primary(&self, t: &[(String, bool)], pos: &mut usize) -> Result<bool> {
        let (tok, quoted) = t.get(*pos).ok_or("unexpected end of condition")?.clone();
        *pos += 1;
        if !quoted {
            match tok.as_str() {
                "(" => {
                    let v = self.cond_or(t, pos)?;
                    if t.get(*pos).map(|(s, _)| s.as_str()) != Some(")") {
                        return Err("missing ')'".into());
                    }
                    *pos += 1;
                    return Ok(v);
                }
                "EXISTS" | "DEFINED" | "COMMAND" | "TEST" | "IS_DIRECTORY" => {
                    let (arg, _) = t.get(*pos).ok_or("missing operand")?.clone();
                    *pos += 1;
                    return Ok(match tok.as_str() {
                        "EXISTS" => self.exists(&arg),
                        "IS_DIRECTORY" => Path::new(&arg).is_dir(),
                        "DEFINED" => self.vars.contains_key(&arg),
                        "COMMAND" => self.functions.contains(&arg.to_lowercase()),
                        _ => self.index.contains_key(&arg),
                    });
                }
                _ => {}
            }
        }
        if let Some((op, false)) = t.get(*pos)
            && matches!(
                op.as_str(),
                "STREQUAL" | "EQUAL" | "LESS" | "GREATER" | "MATCHES" | "IN_LIST"
            )
        {
            let op = op.clone();
            let (rhs, rq) = t.get(*pos + 1).ok_or("missing right operand")?.clone();
            *pos += 2;
            let lhs_v = self.operand(&tok, quoted);
            let rhs_v = self.operand(&rhs, rq);
            return Ok(match op.as_str() {
                "STREQUAL" => lhs_v == rhs_v,
                "EQUAL" | "LESS" | "GREATER" => {
                    let a: f64 = lhs_v
                        .parse()
                        .map_err(|_| format!("not a number: {lhs_v}"))?;
                    let b: f64 = rhs_v
                        .parse()
                        .map_err(|_| format!("not a number: {rhs_v}"))?;
                    match op.as_str() {
                        "EQUAL" => a == b,
                        "LESS" => a < b,
                        _ => a > b,
                    }
                }
                "IN_LIST" => split_list(self.get(&rhs)).contains(&lhs_v),
                _ => regex::Regex::new(&rhs_v)
                    .map_err(|e| e.to_string())?
                    .is_match(&lhs_v),
            });
        }
        if is_true_constant(&tok) {
            return Ok(true);
        }
        if is_false_constant(&tok) || quoted {
            return Ok(false);
        }
        Ok(self.vars.get(&tok).is_some_and(|v| !is_false_constant(v)))
    }

    /// An `if()` operand: an unquoted name of a defined variable means its
    /// value, anything else is the literal string.
    fn operand(&self, tok: &str, quoted: bool) -> String {
        if !quoted && let Some(v) = self.vars.get(tok) {
            return v.clone();
        }
        tok.to_string()
    }
}

/// Parsed `cmake_parse_arguments` result.
#[derive(Debug, Default)]
struct Parsed {
    flags: BTreeSet<String>,
    one: HashMap<String, String>,
    multi: HashMap<String, Vec<String>>,
}

impl Parsed {
    fn flag(&self, k: &str) -> bool {
        self.flags.contains(k)
    }
    fn one(&self, k: &str) -> Option<&str> {
        self.one.get(k).map(String::as_str)
    }
    fn multi(&self, k: &str) -> &[String] {
        self.multi.get(k).map(Vec::as_slice).unwrap_or(&[])
    }
}

/// `cmake_parse_arguments`: a keyword ends the previous keyword's values.
fn parse_arguments(args: &[String], options: &[&str], one: &[&str], multi: &[&str]) -> Parsed {
    let mut p = Parsed::default();
    let mut current: Option<(&str, bool)> = None; // (keyword, is_multi)
    for a in args {
        let s = a.as_str();
        if options.contains(&s) {
            p.flags.insert(s.to_string());
            current = None;
        } else if one.contains(&s) {
            current = Some((one[one.iter().position(|k| *k == s).expect("found")], false));
        } else if multi.contains(&s) {
            let k = multi[multi.iter().position(|k| *k == s).expect("found")];
            p.multi.entry(k.to_string()).or_default();
            current = Some((k, true));
        } else {
            match current {
                Some((k, true)) => p.multi.entry(k.to_string()).or_default().push(a.clone()),
                Some((k, false)) => {
                    p.one.insert(k.to_string(), a.clone());
                    current = None;
                }
                None => {} // unparsed arguments are ignored, as in CMake
            }
        }
    }
    p
}

fn regex_alt(old: &str, new: &str) -> String {
    match (old.is_empty(), new.is_empty()) {
        (true, _) => new.to_string(),
        (_, true) => old.to_string(),
        _ => format!("(?:{old})|(?:{new})"),
    }
}

/// `get_test_fullname`: `<command name>_<file NAME_WE>`, spaces replaced.
fn test_fullname(prefix: &str, file: &str) -> String {
    format!("{}_{}", name_we(prefix), name_we(file).replace(' ', "_"))
}

fn file_name(path: &str) -> &str {
    path.rsplit_once('/').map_or(path, |(_, n)| n)
}

/// CMake's NAME_WE: the file name up to its first dot.
pub fn name_we(path: &str) -> String {
    let n = file_name(path);
    n.split_once('.').map_or(n, |(stem, _)| stem).to_string()
}

fn is_true_constant(s: &str) -> bool {
    matches!(s.to_uppercase().as_str(), "1" | "ON" | "YES" | "TRUE" | "Y")
        || s.parse::<f64>().is_ok_and(|v| v != 0.0)
}

fn is_false_constant(s: &str) -> bool {
    let u = s.to_uppercase();
    matches!(
        u.as_str(),
        "" | "0" | "OFF" | "NO" | "FALSE" | "N" | "IGNORE" | "NOTFOUND"
    ) || u.ends_with("-NOTFOUND")
}

/// Split a CMake list on unescaped `;`, dropping empty elements.
fn split_list(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut chars = s.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\\' && chars.peek() == Some(&';') {
            cur.push(';');
            chars.next();
        } else if ch == ';' {
            if !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
            }
        } else {
            cur.push(ch);
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// Process CMake escape sequences other than `\;`, which list splitting
/// consumes.
fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(ch) = chars.next() {
        if ch != '\\' {
            out.push(ch);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some(';') => out.push_str("\\;"),
            Some(c) => out.push(c),
            None => out.push('\\'),
        }
    }
    out
}

fn matching_brace(s: &str, start: usize) -> Option<usize> {
    let b = s.as_bytes();
    let mut depth = 1;
    let mut i = start;
    while i < b.len() {
        match b[i] {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

/// Glob matching for one path component: `*`, `?`.
fn wildcard_match(pat: &str, name: &str) -> bool {
    fn go(p: &[char], n: &[char]) -> bool {
        match p.split_first() {
            None => n.is_empty(),
            Some(('*', rest)) => (0..=n.len()).any(|i| go(rest, &n[i..])),
            Some(('?', rest)) => !n.is_empty() && go(rest, &n[1..]),
            Some((c, rest)) => n.first() == Some(c) && go(rest, &n[1..]),
        }
    }
    let p: Vec<char> = pat.chars().collect();
    let n: Vec<char> = name.chars().collect();
    go(&p, &n)
}

fn find_block_end(cmds: &[Command], start: usize, open: &[&str], close: &str) -> Result<usize> {
    let mut depth = 0usize;
    for (j, c) in cmds.iter().enumerate().skip(start) {
        if open.contains(&c.name.as_str()) {
            depth += 1;
        } else if c.name == close {
            depth -= 1;
            if depth == 0 {
                return Ok(j);
            }
        }
    }
    Err(format!(
        "CMakeLists.txt:{}: {}() without {close}()",
        cmds[start].line, cmds[start].name
    ))
}

/// Tokenise a CMake file into commands.
fn parse(src: &str) -> Result<Vec<Command>> {
    let chars: Vec<char> = src.chars().collect();
    let mut i = 0;
    let mut line = 1;
    let mut cmds = Vec::new();
    let n = chars.len();

    // Skips a `#` comment starting at `i`; handles `#[[ ... ]]` blocks.
    fn skip_comment(chars: &[char], i: &mut usize, line: &mut usize) {
        *i += 1;
        if let Some(close) = bracket_open(chars, *i) {
            let (end, content_lines) = bracket_close(chars, close.0, close.1);
            *line += content_lines;
            *i = end;
            return;
        }
        while *i < chars.len() && chars[*i] != '\n' {
            *i += 1;
        }
    }

    while i < n {
        let c = chars[i];
        if c == '\n' {
            line += 1;
            i += 1;
        } else if c.is_whitespace() {
            i += 1;
        } else if c == '#' {
            skip_comment(&chars, &mut i, &mut line);
        } else if c.is_ascii_alphabetic() || c == '_' {
            let start = i;
            while i < n && (chars[i].is_ascii_alphanumeric() || chars[i] == '_') {
                i += 1;
            }
            let name: String = chars[start..i].iter().collect::<String>().to_lowercase();
            let cmd_line = line;
            while i < n && (chars[i] == ' ' || chars[i] == '\t') {
                i += 1;
            }
            if i >= n || chars[i] != '(' {
                return Err(format!("line {line}: expected '(' after {name}"));
            }
            i += 1;
            let mut args = Vec::new();
            let mut depth = 0;
            loop {
                if i >= n {
                    return Err(format!("line {cmd_line}: unterminated {name}("));
                }
                let ch = chars[i];
                if ch == '\n' {
                    line += 1;
                    i += 1;
                } else if ch.is_whitespace() {
                    i += 1;
                } else if ch == '#' {
                    skip_comment(&chars, &mut i, &mut line);
                } else if ch == '(' {
                    depth += 1;
                    args.push(RawArg::Unquoted("(".into()));
                    i += 1;
                } else if ch == ')' {
                    i += 1;
                    if depth == 0 {
                        break;
                    }
                    depth -= 1;
                    args.push(RawArg::Unquoted(")".into()));
                } else if ch == '"' {
                    i += 1;
                    let mut s = String::new();
                    while i < n && chars[i] != '"' {
                        if chars[i] == '\\' && i + 1 < n {
                            s.push(chars[i]);
                            i += 1;
                        }
                        if chars[i] == '\n' {
                            line += 1;
                        }
                        s.push(chars[i]);
                        i += 1;
                    }
                    i += 1;
                    args.push(RawArg::Quoted(s));
                } else if let Some((content_start, eqs)) = bracket_open(&chars, i) {
                    let (end, content_lines) = bracket_close(&chars, content_start, eqs);
                    let close_len = eqs + 2;
                    let mut content: String =
                        chars[content_start..end - close_len].iter().collect();
                    // CMake drops a newline immediately after the opening bracket.
                    if content.starts_with('\n') {
                        content.remove(0);
                    }
                    line += content_lines;
                    args.push(RawArg::Bracket(content));
                    i = end;
                } else {
                    let mut s = String::new();
                    while i < n {
                        let ch = chars[i];
                        if ch.is_whitespace() || ch == '(' || ch == ')' {
                            break;
                        }
                        if ch == '\\' && i + 1 < n {
                            s.push(ch);
                            i += 1;
                        }
                        s.push(chars[i]);
                        i += 1;
                    }
                    args.push(RawArg::Unquoted(s));
                }
            }
            cmds.push(Command {
                name,
                args,
                line: cmd_line,
            });
        } else {
            return Err(format!("line {line}: unexpected character {c:?}"));
        }
    }
    Ok(cmds)
}

/// If a bracket opener `[` `=`* `[` starts at `i`, return (content start,
/// number of `=`).
fn bracket_open(chars: &[char], i: usize) -> Option<(usize, usize)> {
    if chars.get(i) != Some(&'[') {
        return None;
    }
    let mut j = i + 1;
    while chars.get(j) == Some(&'=') {
        j += 1;
    }
    (chars.get(j) == Some(&'[')).then_some((j + 1, j - i - 1))
}

/// Find the matching `]` `=`* `]`; returns (index after it, newlines inside).
fn bracket_close(chars: &[char], start: usize, eqs: usize) -> (usize, usize) {
    let mut j = start;
    let mut lines = 0;
    while j < chars.len() {
        if chars[j] == '\n' {
            lines += 1;
        }
        if chars[j] == ']'
            && chars[j + 1..].iter().take(eqs).all(|&c| c == '=')
            && chars.get(j + 1 + eqs) == Some(&']')
        {
            return (j + 2 + eqs, lines);
        }
        j += 1;
    }
    (chars.len(), lines)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn eval(src: &str) -> Evaluation {
        let dir = std::env::temp_dir().join(format!("neoscad-cmake-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let f = dir.join(format!("t{n}.cmake"));
        fs::write(&f, src).unwrap();
        Interpreter::new(HashMap::new()).run_file(&f).unwrap()
    }

    #[test]
    fn lists_ifs_and_registration() {
        let e = eval(
            r#"
            set(D /x)
            list(APPEND A ${D}/a.scad ${D}/b.scad ${D}/c.scad) # comment
            list(REMOVE_ITEM A ${D}/b.scad)
            if(NOT UNDEFINED_VAR AND (D OR NOPE))
              add_cmdline_test(echo OPENSCAD SUFFIX echo FILES ${A} ARGS -D a=3$<SEMICOLON>)
            else()
              add_cmdline_test(never OPENSCAD SUFFIX echo FILES ${A})
            endif()
            foreach(T one two)
              add_cmdline_test(g-${T} EXPERIMENTAL OPENSCAD SUFFIX csg FILES "/y/my file.scad")
            endforeach()
            set_tests_properties(g-one_my_file PROPERTIES DISABLED TRUE)
            "#,
        );
        let names: Vec<_> = e.registrations.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(
            names,
            ["echo_a", "echo_c", "g-one_my_file", "g-two_my_file"]
        );
        assert_eq!(e.registrations[0].args, ["-D", "a=3;"]);
        assert_eq!(e.registrations[0].configs, ["All", "Default", "Good"]);
        assert!(e.registrations[2].experimental);
        assert!(e.registrations[2].disabled_at.is_some());
        assert!(e.registrations[3].disabled_at.is_none());
    }

    #[test]
    fn test_configs_follow_testfunctions() {
        let e = eval(
            r#"
            set_test_config(Bugs FILES /b/x.scad PREFIXES render)
            add_cmdline_test(render OPENSCAD SUFFIX png FILES /b/x.scad /b/y.scad)
            "#,
        );
        assert_eq!(e.registrations[0].configs, ["All", "Bugs"]);
        assert_eq!(e.registrations[1].configs, ["All", "Default", "Good"]);
    }

    #[test]
    fn exclusions_merge_like_regex_alt() {
        let e = eval(
            r#"
            ctest_env_append_value(OPENSCAD_TEST_EXCLUDE_LINE [[^A$]])
            add_cmdline_test(echo OPENSCAD SUFFIX echo FILES /t/r.scad)
            add_line_exclusion_for_test(echo r [[^B$]])
            "#,
        );
        assert_eq!(
            e.registrations[0].exclude_line.as_deref(),
            Some("(?:^A$)|(?:^B$)")
        );
    }

    #[test]
    fn wildcard() {
        assert!(wildcard_match("*.scad", "a-b.scad"));
        assert!(!wildcard_match("*.scad", "a.scadx"));
        assert_eq!(name_we("/p/utf8-☠-2D.scad"), "utf8-☠-2D");
        assert_eq!(name_we("/p/a.b.c"), "a");
    }
}
