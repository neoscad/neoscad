//! Running a program through the front end and evaluator, the way
//! `openscad.cc`'s `cmdline()` does, and the `.ast`, `.echo`, `.csg`,
//! `.term` and mesh (`.stl`, `.off`, `.obj`) exports.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use eval::{Console, Options};
use lang::customizer::{Parameters, read_parameter_sets};
use lang::deps::{Library, load_dependencies, resolve_uses};
use lang::diag::Severity;
use lang::loader::{LibraryPath, StdFs};
use lang::{Program, parse_program};

/// OpenSCAD's general failure exit code.
const EXIT_ERROR: u8 = 1;

/// A feature of a later phase (see `main.rs`).
const EXIT_NOT_IMPLEMENTED: u8 = 3;

/// One command-line invocation's inputs.
#[derive(Debug)]
pub struct Job<'a> {
    /// The input path as given, or `-` for stdin.
    pub input: &'a str,
    pub outputs: &'a [String],
    pub defines: &'a [String],
    pub parameter_file: Option<&'a str>,
    pub parameter_set: Option<&'a str>,
    /// `--quiet`: only errors are printed.
    pub quiet: bool,
}

/// Where messages are made relative to.
struct Paths {
    cwd: PathBuf,
    main_dir: PathBuf,
}

/// The main program and the libraries it uses.
struct Loaded {
    program: Program,
    /// Keys of the libraries the main program uses, in search order.
    uses: Vec<String>,
    libraries: Vec<Library>,
}

impl Paths {
    fn of(job: &Job<'_>) -> Paths {
        let cwd = std::env::current_dir().unwrap_or_default();
        let main_dir = cwd
            .join(display_name(job))
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| cwd.clone());
        Paths { cwd, main_dir }
    }
}

/// OpenSCAD names stdin `<stdin>` and resolves it like a file in the
/// working directory.
fn display_name<'a>(job: &Job<'a>) -> &'a str {
    if job.input == "-" {
        "<stdin>"
    } else {
        job.input
    }
}

/// Parse the input with `-D` definitions, apply a parameter set and parse
/// the used libraries, printing OpenSCAD's messages to `con`. On failure
/// returns the exit code.
fn load<W: Write>(job: &Job<'_>, paths: &Paths, con: &mut Console<W>) -> Result<Loaded, u8> {
    let stdin = job.input == "-";
    let display = display_name(job);
    let path = paths.cwd.join(display);
    let mut text = Vec::new();
    let read = if stdin {
        std::io::stdin().read_to_end(&mut text).map(|_| ())
    } else {
        std::fs::read(job.input).map(|t| text = t)
    };
    if read.is_err() {
        con.print(
            None,
            format!("Can't open input file '{display}'!\n").as_bytes(),
        );
        return Err(EXIT_ERROR);
    }
    // cmdline(): the text, then an end-of-text marker, then each -D.
    let mut suffix = b"\n\x03\n".to_vec();
    for d in job.defines {
        suffix.extend_from_slice(d.as_bytes());
        suffix.extend_from_slice(b";\n");
    }
    text.extend_from_slice(&suffix);

    let libs = LibraryPath::from_env();
    let mut program = parse_program(path, text, &StdFs, &libs);
    for d in program.openscad_diags() {
        con.diagnostic(d, &program.sources, &paths.cwd);
    }
    if program.has_syntax_errors() {
        con.print(None, format!("Can't parse file '{display}'!\n").as_bytes());
        return Err(EXIT_ERROR);
    }

    if let (Some(file), Some(set_name)) = (job.parameter_file, job.parameter_set) {
        let mut warnings = Vec::new();
        let mut params = Parameters::from_ast(&program.ast, &mut warnings);
        for w in &warnings {
            con.diagnostic(w, &program.sources, &paths.cwd);
        }
        match read_parameter_sets(Path::new(file)) {
            Ok(sets) => {
                if let Some(set) = sets.iter().find(|s| s.name == set_name) {
                    params.import(set);
                    params.apply(&mut program.ast);
                }
            }
            Err(e) => con.diagnostic(&e, &program.sources, &paths.cwd),
        }
    }

    // handleDependencies(): parse the used libraries and report on them.
    let libraries = load_dependencies(&program, &suffix, &StdFs, &libs);
    for lib in &libraries {
        match (&lib.program, lib.open_error()) {
            (Some(p), _) => p
                .openscad_diags()
                .for_each(|d| con.diagnostic(d, &p.sources, &paths.cwd)),
            (None, Some(msg)) => con.print(Some(Severity::Warning), msg.as_bytes()),
            (None, None) => {}
        }
    }
    let uses = resolve_uses(&program, &StdFs, &libs);
    Ok(Loaded {
        program,
        uses,
        libraries,
    })
}

/// Write `data` to `-o` targets (`-` is stdout).
fn write_output(target: &str, data: &[u8]) -> Result<(), u8> {
    let r = if target == "-" {
        let mut out = std::io::stdout().lock();
        out.write_all(data).and_then(|_| out.flush())
    } else {
        std::fs::write(target, data)
    };
    r.map_err(|e| {
        eprintln!("ERROR: Can't write to '{target}': {e}");
        EXIT_ERROR
    })
}

/// `-o x.ast`: the parsed program printed back (`SourceFile::dump`).
pub fn export_ast(job: &Job<'_>) -> u8 {
    let paths = Paths::of(job);
    let mut con = Console::new(std::io::stderr(), paths.main_dir.clone(), job.quiet);
    let loaded = match load(job, &paths, &mut con) {
        Ok(l) => l,
        Err(code) => return code,
    };
    let text = lang::dump::dump(&loaded.program.ast);
    for target in job.outputs {
        // Like `.csg`, OpenSCAD writes a relative `.ast` into the document's
        // directory (`openscad.cc`), not the working directory.
        let target = if target != "-" {
            paths.main_dir.join(target).to_string_lossy().into_owned()
        } else {
            target.clone()
        };
        if let Err(code) = write_output(&target, &text) {
            return code;
        }
    }
    0
}

/// Evaluate a loaded program, printing messages to `con`.
fn evaluate<W: Write>(
    l: &Loaded,
    paths: &Paths,
    options: &Options,
    con: &mut Console<W>,
) -> eval::Evaluation {
    let libs: Vec<eval::Library<'_>> = l
        .libraries
        .iter()
        .map(|lib| eval::Library {
            path: &lib.path,
            program: lib.program.as_ref(),
            uses: &lib.uses,
        })
        .collect();
    // `main` runs this on a thread with `eval::DEFAULT_THREAD_STACK`.
    eval::evaluate(
        &l.program,
        &l.uses,
        &libs,
        paths.main_dir.clone(),
        options,
        con,
    )
}

/// A node-tree export.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TreeFormat {
    Csg,
    Term,
}

/// `-o x.csg` and `-o x.term`, one evaluation for all outputs, with
/// messages on stderr. As in OpenSCAD, an evaluation error still exports
/// the partial tree and exits 0; only a load failure is an error.
pub fn export_tree(job: &Job<'_>, options: &Options, formats: &[TreeFormat]) -> u8 {
    let paths = Paths::of(job);
    let mut con = Console::new(std::io::stderr(), paths.main_dir.clone(), job.quiet);
    let loaded = match load(job, &paths, &mut con) {
        Ok(l) => l,
        Err(code) => return code,
    };
    let ev = evaluate(&loaded, &paths, options, &mut con);
    // A root modifier (`!`) makes the tagged node the whole tree.
    let top = ev.root.find_root_tag().0.unwrap_or(&ev.root);
    for (target, format) in job.outputs.iter().zip(formats) {
        let text = match format {
            TreeFormat::Csg => eval::dump::csg(top, &paths.main_dir),
            // `openscad.cc` builds the CSG term with a `CSGTreeEvaluator`
            // that has no geometry evaluator, so every leaf is a null term
            // and the tree always reduces to nothing (`CSGTreeEvaluator.cc`,
            // `visit(AbstractPolyNode)`). The nightly prints this line for
            // any input, `cube();` included.
            TreeFormat::Term => "No top-level CSG object\n".to_string(),
        };
        // OpenSCAD changes into the document directory before writing a
        // `.csg` (`openscad.cc`, so `import()` paths print relative to it)
        // and opens the output there too: `openscad sub/a.scad -o a.csg`
        // writes `sub/a.csg`. `.term` is written from the original
        // directory.
        let target = if *format == TreeFormat::Csg && target != "-" {
            paths.main_dir.join(target).to_string_lossy().into_owned()
        } else {
            target.clone()
        };
        if let Err(code) = write_output(&target, text.as_bytes()) {
            return code;
        }
    }
    0
}

/// `-o x.echo`: every message of parsing and evaluation, as OpenSCAD's
/// `Echostream` captures them, and nothing on stderr.
pub fn export_echo(job: &Job<'_>, options: &Options) -> u8 {
    let paths = Paths::of(job);
    let mut con = Console::new(Vec::new(), paths.main_dir.clone(), job.quiet);
    let code = match load(job, &paths, &mut con) {
        Err(code) => code,
        Ok(l) => {
            evaluate(&l, &paths, options, &mut con);
            0
        }
    };
    let data = con.into_inner();
    for target in job.outputs {
        if let Err(c) = write_output(target, &data) {
            return c;
        }
    }
    code
}

/// A 3D mesh export format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MeshFormat {
    AsciiStl,
    BinaryStl,
    Off,
    Obj,
}

/// `-o x.stl|x.off|x.obj`: evaluate, build the geometry, and export it,
/// following the geometry branch of `openscad.cc`'s `do_export`
/// (`:476-541`): messages on stderr, a refusal with exit 1 when the result
/// is not 3D or is empty (`checkAndExport`), then the render summary.
pub fn export_mesh(job: &Job<'_>, options: &eval::Options, formats: &[MeshFormat], force: bool) -> u8 {
    let started = std::time::Instant::now();
    let paths = Paths::of(job);
    let mut con = Console::new(std::io::stderr(), paths.main_dir.clone(), job.quiet);
    let loaded = match load(job, &paths, &mut con) {
        Ok(l) => l,
        Err(code) => return code,
    };
    let ev = evaluate(&loaded, &paths, options, &mut con);
    let top = ev.root.find_root_tag().0.unwrap_or(&ev.root);
    let keys = eval::dump::Keys::new(&ev.root);
    let opts = geom::RenderOptions { force, ..Default::default() };
    let rendered = geom::Renderer::new().render(top, &keys, opts);
    let rendered = match rendered {
        Ok(r) => r,
        Err(u) => {
            let mut line = format!("neoscad: {}() is not implemented yet", u.what);
            if let Some(l) = &u.loc
                && let Some(sources) = unit_sources(&loaded, l.unit)
            {
                let rel = lang::diag::relative_path(sources.path(l.span.file), &paths.main_dir);
                line.push_str(&format!(" (in file {}, line {})", rel.display(), l.line));
            }
            eprintln!("{line}");
            return EXIT_NOT_IMPLEMENTED;
        }
    };
    for m in &rendered.messages {
        let mut diag = lang::diag::Diagnostic::new(lang::diag::DiagCode::Geometry, m.severity, m.text.clone());
        let mut sources = None;
        if let Some(l) = &m.loc {
            diag = diag.at(l.span, l.line);
            sources = unit_sources(&loaded, l.unit);
        }
        use eval::Output;
        con.message(&eval::Message { diag, text: m.text.as_bytes(), sources });
    }
    // `if (!root_geom) root_geom = std::make_shared<PolySet>(3);`
    let root = rendered.geometry;
    if force && root.as_ref().is_none_or(|g| g.dimension() == 3) {
        con.print(None, b"Converted to backend-specific geometry");
    }
    if root.as_ref().is_some_and(|g| g.dimension() != 3) {
        con.print(None, b"Current top level object is not a 3D object.");
        return EXIT_ERROR;
    }
    let Some(root) = root.filter(|g| !g.is_empty()) else {
        con.print(None, b"Current top level object is empty.");
        return EXIT_ERROR;
    };
    let ps = geom::export::as_polyset(&root, &opts.scheme).expect("3D geometry has a mesh");
    for (target, format) in job.outputs.iter().zip(formats) {
        let mut warnings = Vec::new();
        let data = match format {
            MeshFormat::AsciiStl => geom::export::stl(&ps, false, &mut warnings),
            MeshFormat::BinaryStl => geom::export::stl(&ps, true, &mut warnings),
            MeshFormat::Off => geom::export::off(&ps, &mut warnings),
            MeshFormat::Obj => geom::export::obj(&ps, &mut warnings),
        };
        for w in warnings {
            con.print(Some(Severity::Warning), format!("WARNING: {w}").as_bytes());
        }
        if let Err(code) = write_output(target, &data) {
            return code;
        }
    }
    // `RenderStatistic::printAll`: cache size, time, then the object.
    con.print(None, format!("Geometries in cache: {}", rendered.cache_entries).as_bytes());
    let ms = started.elapsed().as_millis();
    con.print(
        None,
        format!("Total rendering time: {}:{:02}:{:02}.{:03}", ms / 3_600_000, ms / 60_000 % 60, ms / 1000 % 60, ms % 1000).as_bytes(),
    );
    for l in geom::export::summary(&root) {
        con.print(None, l.as_bytes());
    }
    0
}

/// The source map of evaluation unit `unit`: 0 is the main program, `1 + i`
/// the i-th library, in the order `evaluate` passed them.
fn unit_sources(l: &Loaded, unit: u32) -> Option<&lang::source::SourceMap> {
    if unit == 0 {
        return Some(&l.program.sources);
    }
    l.libraries.get(unit as usize - 1).and_then(|lib| lib.program.as_ref()).map(|p| &p.sources)
}
