//! Running a program through the front end and evaluator, the way
//! `openscad.cc`'s `cmdline()` does, and the `.ast`, `.echo`, `.csg`,
//! `.term`, `.param`, mesh (`.stl`, `.off`, `.obj`, `.3mf`, `.wrl`, `.pov`)
//! and 2D (`.svg`, `.dxf`, `.pdf`) exports, each once or once per
//! `--animate` frame.
//!
//! `--hardwarnings` stops at the first warning with exit 1, wherever it is
//! printed: by the parser (which OpenSCAD turns into the parse error "stop
//! on first warning"), the customizer, a missing library, the evaluator
//! (see `eval::Options::hardwarnings`), the geometry or an export. OpenSCAD
//! raises the warning as an exception that the command line maps to exit 1
//! without printing anything more (`openscad.cc:1186`).

use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use eval::{Console, Options};
use lang::customizer::{Parameters, read_parameter_sets};
use lang::deps::{Library, load_dependencies, resolve_uses};
use lang::diag::Severity;
use lang::{Program, parse_program};

use crate::host::Host;

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
    /// `--hardwarnings`: stop at the first warning, with exit 1.
    pub hardwarnings: bool,
    /// `-O` settings.
    pub export_options: &'a crate::export_options::ExportOptions,
    /// `--summary` and `--summary-file`.
    pub summary: &'a crate::summary::Request,
    /// `--animate` (with `--animate_sharding`): the frames to export.
    pub animate: Option<Animate>,
    /// `--colorscheme`, which colours exported Manifold meshes as well as
    /// images.
    pub scheme: &'a render::ColorScheme,
    /// PNG settings, when an output is a PNG.
    pub png: Option<&'a crate::png::Settings>,
    /// `--format json`: record every message for the report instead of
    /// printing it (`crate::report`).
    pub json: bool,
    /// Follow located diagnostics on stderr with the source line and a
    /// caret (a terminal is reading; see `main::rich_diagnostics`).
    pub rich: bool,
}

/// `AnimateArgs`: `frames` frames in all, of which this run exports
/// `start..limit` (the whole range without sharding).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Animate {
    pub frames: u32,
    pub start: u32,
    pub limit: u32,
}

/// One export pass: the frame number when animating, and `$t`.
#[derive(Debug, Clone, Copy)]
struct Frame {
    number: Option<u32>,
    time: f64,
}

impl Job<'_> {
    /// The passes to make: one at `$t = 0`, or one per animation frame at
    /// `$t = frame / frames` (`cmdline()`, `openscad.cc:654-686`).
    fn frames(&self) -> Vec<Frame> {
        match self.animate {
            None => vec![Frame {
                number: None,
                time: 0.0,
            }],
            Some(a) => (a.start..a.limit)
                .map(|f| Frame {
                    number: Some(f),
                    time: f64::from(f) * (1.0 / f64::from(a.frames)),
                })
                .collect(),
        }
    }

    /// Before each animation frame OpenSCAD logs the input it is exporting.
    fn announce<W: Write>(&self, frame: Frame, con: &mut Console<W>) {
        if frame.number.is_some() {
            con.print(
                None,
                format!("Exporting {}...", display_name(self)).as_bytes(),
            );
        }
    }
}

/// The output name of an animation frame: the frame number, five digits,
/// before the extension (`out/x.stl` becomes `out/x00003.stl`).
pub fn frame_target(target: &str, frame: Option<u32>) -> String {
    let Some(n) = frame else {
        return target.to_string();
    };
    let p = Path::new(target);
    let stem = p.with_extension("");
    let mut s = format!("{}{n:05}", stem.to_string_lossy());
    if let Some(ext) = p.extension() {
        s.push('.');
        s.push_str(&ext.to_string_lossy());
    }
    s
}

/// The geometry oracle of an evaluation with `--enable query`
/// (`child_bounds()`, `child_measure()`): it renders through `renderer`
/// with the export's settings, so a render that follows finds the queried
/// subtrees in the cache. `None` without the extension, so no other run
/// pays for the fonts it loads.
///
/// Its renders record their messages for replay (epoch 0: the command
/// line's sources never change between its renders), and a render after a
/// query must replay them (`opts.replay`), or the child's warnings, which
/// a query render does not print, would never be printed: the command
/// line's own rule is that a cache hit is silent.
fn query_oracle(
    job: &Job<'_>,
    loaded: &Loaded,
    paths: &Paths,
    options: &Options,
    renderer: &std::sync::Arc<geom::Renderer>,
) -> Option<std::sync::Arc<session::oracle::Oracle>> {
    options.extensions.has(eval::Extension::Query).then(|| {
        let mut opts = render_options(job, loaded, paths, false);
        opts.replay = Some(0);
        std::sync::Arc::new(session::oracle::Oracle::new(renderer.clone(), opts))
    })
}

/// `options` with `oracle` as its geometry oracle.
fn with_oracle(
    options: &Options,
    oracle: Option<&std::sync::Arc<session::oracle::Oracle>>,
) -> Options {
    Options {
        geometry: oracle.map(|o| o.clone() as std::sync::Arc<dyn eval::GeometryOracle>),
        ..options.clone()
    }
}

/// `options` at the frame's `$t`.
fn at_time(options: &Options, frame: Frame) -> Options {
    Options {
        time: frame.time,
        ..options.clone()
    }
}

/// Where messages are made relative to.
struct Paths {
    cwd: PathBuf,
    main_dir: PathBuf,
}

/// The main program and the libraries it uses.
struct Loaded {
    host: Host,
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
        con.print_error_line(
            lang::diag::DiagCode::InputNotFound,
            format!("Can't open input file '{display}'!\n").as_bytes(),
            false,
        );
        return Err(EXIT_ERROR);
    }
    if !stdin {
        // The input is a dependency as named on the command line.
        crate::deps::add(job.input);
    }
    // cmdline(): the text, then an end-of-text marker, then each -D.
    let mut suffix = b"\n\x03\n".to_vec();
    for d in job.defines {
        suffix.extend_from_slice(d.as_bytes());
        suffix.extend_from_slice(b";\n");
    }
    text.extend_from_slice(&suffix);

    let host = Host::from_env();
    let (fs, libs) = (&*host.fs, &host.libs);
    let mut program = parse_program(path, text, fs, libs);
    crate::deps::add_sources(&program.sources);
    let stopped = parser_diagnostics(&program, job.hardwarnings, paths, con);
    if stopped || program.has_syntax_errors() {
        con.print(None, format!("Can't parse file '{display}'!\n").as_bytes());
        return Err(EXIT_ERROR);
    }

    if let (Some(file), Some(set_name)) = (job.parameter_file, job.parameter_set) {
        let mut warnings = Vec::new();
        let mut params = Parameters::from_ast(&program.ast, &mut warnings);
        for w in &warnings {
            con.diagnostic(w, &program.sources, &paths.cwd);
            if job.hardwarnings {
                return Err(EXIT_ERROR);
            }
        }
        match read_parameter_sets(fs, Path::new(file)) {
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
    let mut libraries = load_dependencies(&program, &suffix, fs, libs);
    for lib in &mut libraries {
        crate::deps::add(&lib.path);
        if let Some(p) = &lib.program {
            crate::deps::add_sources(&p.sources);
        }
        match (&lib.program, lib.open_error()) {
            (Some(p), _) => {
                if parser_diagnostics(p, job.hardwarnings, paths, con) {
                    // The library's parse failed on the warning, so it
                    // defines nothing; evaluation goes on without it.
                    lib.program = None;
                }
            }
            (None, Some(msg)) => {
                con.print(Some(Severity::Warning), msg.as_bytes());
                if job.hardwarnings {
                    return Err(EXIT_ERROR);
                }
            }
            (None, None) => {}
        }
    }
    let uses = resolve_uses(&program, fs, libs);
    if job.json {
        crate::report::set_names(session::Names::of(
            std::iter::once(&program).chain(libraries.iter().filter_map(|l| l.program.as_ref())),
        ));
    }
    Ok(Loaded {
        host,
        program,
        uses,
        libraries,
    })
}

/// Print a parsed file's diagnostics. With `--hardwarnings` the first
/// warning ends the parse: the parser catches the warning's exception and
/// reports it as a syntax error at the scanner's position, which is the
/// warning's line (`parser.y`, `yyerror("stop on first warning")`).
/// Returns whether that happened.
fn parser_diagnostics<W: Write>(
    program: &Program,
    hardwarnings: bool,
    paths: &Paths,
    con: &mut Console<W>,
) -> bool {
    for d in program.openscad_diags() {
        con.diagnostic(d, &program.sources, &paths.cwd);
        if hardwarnings
            && d.severity == Severity::Warning
            && let Some(span) = d.span
        {
            let e = lang::diag::Diagnostic::new(
                lang::diag::DiagCode::SyntaxError,
                Severity::Error,
                "Parser error: stop on first warning",
            )
            .at(span, d.line);
            con.diagnostic(&e, &program.sources, &paths.cwd);
            return true;
        }
    }
    false
}

/// What a run's console resolves message paths through: the disk, as
/// OpenSCAD's `std::filesystem::relative` does. (The run's [`Host`] file
/// system is the disk too, with the bundled libraries mounted in memory,
/// which resolve to their lexical paths either way; the console is made
/// before the host.)
fn disk() -> std::sync::Arc<dyn lang::loader::FileSystem + Send + Sync> {
    std::sync::Arc::new(lang::loader::StdFs)
}

/// The console of a run's stderr: OpenSCAD's lines, or with `--format
/// json` nothing printed and everything recorded for the report.
fn stderr_console(job: &Job<'_>, paths: &Paths) -> Console<Box<dyn Write>> {
    let out: Box<dyn Write> = if job.json {
        Box::new(std::io::sink())
    } else {
        Box::new(std::io::stderr())
    };
    Console::new(out, paths.main_dir.clone(), disk(), job.quiet)
        .record(job.json)
        .rich(job.rich && !job.json)
}

/// Run `f` with the run's stderr console, then hand its records to the
/// report.
fn with_console(job: &Job<'_>, f: impl FnOnce(&Paths, &mut Console<Box<dyn Write>>) -> u8) -> u8 {
    let paths = Paths::of(job);
    let mut con = stderr_console(job, &paths);
    let code = f(&paths, &mut con);
    crate::report::add(con.take_records());
    code
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
    with_console(job, |paths, con| export_ast_with(job, paths, con))
}

fn export_ast_with<W: Write>(job: &Job<'_>, paths: &Paths, con: &mut Console<W>) -> u8 {
    let loaded = match load(job, paths, con) {
        Ok(l) => l,
        Err(code) => return code,
    };
    let text = lang::dump::dump(&loaded.program.ast);
    for frame in job.frames() {
        job.announce(frame, con);
        for target in job.outputs {
            // Like `.csg`, OpenSCAD writes a relative `.ast` into the
            // document's directory (`openscad.cc`), not the working directory.
            let target = frame_target(target, frame.number);
            let target = if target != "-" {
                paths.main_dir.join(target).to_string_lossy().into_owned()
            } else {
                target
            };
            if let Err(code) = write_output(&target, &text) {
                return code;
            }
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
    // `textmetrics()` measures with the fonts `text()` renders with: the
    // bundled ones and the program's `use`d font files.
    let fonts = options.features.has(eval::Feature::TextMetrics).then(|| {
        let used = std::iter::once(&l.program)
            .chain(l.libraries.iter().filter_map(|lib| lib.program.as_ref()))
            .flat_map(|p| p.ast.uses.iter());
        std::sync::Arc::new(l.host.fonts(used))
    });
    let options = Options {
        fs: l.host.fs.clone(),
        fonts: fonts.or_else(|| options.fonts.clone()),
        ..options.clone()
    };
    // `main` runs this on a thread with `eval::DEFAULT_THREAD_STACK`.
    let ev = eval::evaluate(
        &l.program,
        &l.uses,
        &libs,
        paths.main_dir.clone(),
        &options,
        con,
    );
    // OpenSCAD records imported files (and runs `-m`) as it instantiates
    // them, so every export format sees them, not just geometry.
    crate::deps::add_node_files(&ev.root, &*l.host.fs);
    ev
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
    with_console(job, |paths, con| {
        export_tree_with(job, options, formats, paths, con)
    })
}

fn export_tree_with<W: Write>(
    job: &Job<'_>,
    options: &Options,
    formats: &[TreeFormat],
    paths: &Paths,
    con: &mut Console<W>,
) -> u8 {
    let loaded = match load(job, paths, con) {
        Ok(l) => l,
        Err(code) => return code,
    };
    // Queries render even when only the tree is written: the tree holds
    // their answers. Frames share the cache, as a mesh export's do.
    let renderer = std::sync::Arc::new(geom::Renderer::new());
    let oracle = query_oracle(job, &loaded, paths, options, &renderer);
    let options = &with_oracle(options, oracle.as_ref());
    for frame in job.frames() {
        job.announce(frame, con);
        let ev = evaluate(&loaded, paths, &at_time(options, frame), con);
        if ev.hard_warning {
            return EXIT_ERROR;
        }
        if let Err(code) = write_trees(job, paths, &loaded, &ev, formats, frame) {
            return code;
        }
    }
    0
}

/// The `.csg`/`.term` outputs of one evaluation.
fn write_trees(
    job: &Job<'_>,
    paths: &Paths,
    loaded: &Loaded,
    ev: &eval::Evaluation,
    formats: &[TreeFormat],
    frame: Frame,
) -> Result<(), u8> {
    // A root modifier (`!`) makes the tagged node the whole tree.
    let top = ev.root.find_root_tag().0.unwrap_or(&ev.root);
    for (target, format) in job.outputs.iter().zip(formats) {
        let target = &frame_target(target, frame.number);
        let text = match format {
            TreeFormat::Csg => eval::dump::csg(top, &paths.main_dir, &*loaded.host.fs),
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
        write_output(&target, text.as_bytes())?;
    }
    Ok(())
}

/// `-o x.echo`: every message of parsing and evaluation, as OpenSCAD's
/// `Echostream` captures them, and nothing on stderr.
pub fn export_echo(job: &Job<'_>, options: &Options) -> u8 {
    let paths = Paths::of(job);
    let mut con =
        Console::new(Vec::new(), paths.main_dir.clone(), disk(), job.quiet).record(job.json);
    let code = match load(job, &paths, &mut con) {
        Err(code) => code,
        // The echo file is written as messages arrive, so after a hard
        // warning it holds everything up to it. An animation writes every
        // frame into the one file: OpenSCAD opens the echo stream before
        // its frame loop and never renames it.
        Ok(l) => {
            let mut code = 0;
            let renderer = std::sync::Arc::new(geom::Renderer::new());
            let oracle = query_oracle(job, &l, &paths, options, &renderer);
            let options = &with_oracle(options, oracle.as_ref());
            for frame in job.frames() {
                job.announce(frame, &mut con);
                let ev = evaluate(&l, &paths, &at_time(options, frame), &mut con);
                if ev.hard_warning {
                    code = EXIT_ERROR;
                    break;
                }
            }
            code
        }
    };
    crate::report::add(con.take_records());
    let data = con.into_inner();
    for target in job.outputs {
        if let Err(c) = write_output(target, &data) {
            return c;
        }
    }
    code
}

/// A geometry export format: 3D meshes and 2D outlines.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MeshFormat {
    AsciiStl,
    BinaryStl,
    Off,
    Obj,
    ThreeMf,
    Wrl,
    Svg,
    Dxf,
    Pdf,
    Pov,
    /// An image, which takes 2D and 3D results alike (and draws an empty
    /// one as the background alone).
    Png,
    /// STEP with exact surfaces (`--enable exact`; `geom::exact`), from a
    /// second render of the tree rather than from the mesh.
    Step,
}

impl MeshFormat {
    /// The shared encoder's format (not for [`MeshFormat::Png`]).
    fn session(self) -> session::export::Format {
        use session::export::Format;
        match self {
            MeshFormat::AsciiStl => Format::AsciiStl,
            MeshFormat::BinaryStl => Format::BinaryStl,
            MeshFormat::Off => Format::Off,
            MeshFormat::Obj => Format::Obj,
            MeshFormat::ThreeMf => Format::ThreeMf,
            MeshFormat::Wrl => Format::Wrl,
            MeshFormat::Pov => Format::Pov,
            MeshFormat::Svg => Format::Svg,
            MeshFormat::Dxf => Format::Dxf,
            MeshFormat::Pdf => Format::Pdf,
            MeshFormat::Png => unreachable!("images are drawn, not encoded"),
            MeshFormat::Step => unreachable!("STEP is reconstructed, not encoded"),
        }
    }

    /// The dimension `checkAndExport` requires (`fileformat::is3D/is2D`);
    /// none for an image.
    fn dimension(self) -> Option<u32> {
        match self {
            MeshFormat::Svg | MeshFormat::Dxf | MeshFormat::Pdf => Some(2),
            MeshFormat::Png => None,
            _ => Some(3),
        }
    }
}

/// `-o x.stl|x.off|x.obj|x.3mf|x.wrl|x.pov|x.svg|x.dxf|x.pdf`: evaluate,
/// build the geometry, and export it, following the geometry branch of
/// `openscad.cc`'s `do_export` (`:476-541`): messages on stderr, a refusal
/// with exit 1 when the result has the wrong dimension or is empty
/// (`checkAndExport`), then the render summary. Animation frames share
/// one geometry cache, as they share OpenSCAD's.
pub fn export_mesh(
    job: &Job<'_>,
    options: &eval::Options,
    formats: &[MeshFormat],
    force: bool,
) -> u8 {
    with_console(job, |paths, con| {
        export_mesh_with(job, options, formats, force, paths, con)
    })
}

fn export_mesh_with<W: Write>(
    job: &Job<'_>,
    options: &eval::Options,
    formats: &[MeshFormat],
    force: bool,
    paths: &Paths,
    con: &mut Console<W>,
) -> u8 {
    let loaded = match load(job, paths, con) {
        Ok(l) => l,
        Err(code) => return code,
    };
    let renderer = std::sync::Arc::new(geom::Renderer::new());
    for frame in job.frames() {
        job.announce(frame, con);
        let code = render_frame(
            job,
            paths,
            &loaded,
            &renderer,
            &at_time(options, frame),
            formats,
            force,
            frame,
            con,
        );
        if code != 0 {
            return code;
        }
    }
    0
}

/// One evaluation, render and set of exports of [`export_mesh`].
#[allow(clippy::too_many_arguments)]
fn render_frame<W: Write>(
    job: &Job<'_>,
    paths: &Paths,
    loaded: &Loaded,
    renderer: &std::sync::Arc<geom::Renderer>,
    options: &eval::Options,
    formats: &[MeshFormat],
    force: bool,
    frame: Frame,
    con: &mut Console<W>,
) -> u8 {
    // One oracle per frame, so `asked` says whether this frame's
    // evaluation rendered anything.
    let oracle = query_oracle(job, loaded, paths, options, renderer);
    let ev = evaluate(loaded, paths, &with_oracle(options, oracle.as_ref()), con);
    // `--limit`: the evaluator printed the limit it passed.
    let exceeded = || options.guard.as_ref().and_then(|g| g.exceeded());
    if ev.hard_warning || exceeded().is_some() {
        return EXIT_ERROR;
    }
    // `RenderStatistic` starts timing after instantiation, when geometry
    // evaluation begins.
    let started = std::time::Instant::now();
    let top = ev.root.find_root_tag().0.unwrap_or(&ev.root);
    let keys = eval::dump::Keys::new(&ev.root, &*loaded.host.fs);
    let mut opts = render_options(job, loaded, paths, force);
    opts.interrupt = options.interrupt.clone();
    opts.guard = options.guard.clone();
    if oracle.as_ref().is_some_and(|o| o.asked()) {
        // A query rendered into the cache: see `query_oracle`.
        opts.replay = Some(0);
    }
    let mut render_ms = 0.0;
    let unsupported = |u: geom::Unsupported, con: &mut Console<W>| {
        // A limit the geometry stage passed (only `--limit` runs have
        // any), reported at the node that passed it.
        if u.is_interrupted()
            && let Some(e) = exceeded()
        {
            let mut d = lang::diag::Diagnostic::new(
                lang::diag::DiagCode::ResourceLimit,
                lang::diag::Severity::Error,
                e.message(),
            )
            .with_hint(e.hint());
            let mut sources = &loaded.program.sources;
            if let Some(at) = e.at
                && let Some(s) = unit_sources(loaded, at.unit)
            {
                d = d
                    .at(at.span, at.line)
                    .with_base(lang::diag::PathBase::MainFileDir);
                sources = s;
            }
            con.diagnostic(&d, sources, &paths.cwd);
            return EXIT_ERROR;
        }
        let mut line = format!("neoscad: {}() is not implemented yet", u.what);
        if let Some(l) = &u.loc
            && let Some(sources) = unit_sources(loaded, l.unit)
        {
            let rel = lang::diag::relative_display(
                sources.path(l.span.file),
                &paths.main_dir,
                &lang::loader::StdFs,
            );
            line.push_str(&format!(" (in file {rel}, line {})", l.line));
        }
        // Past `--quiet`, as the `eprintln!` it replaces was; recorded for
        // `--format json`.
        con.print_unfiltered(line.as_bytes());
        EXIT_NOT_IMPLEMENTED
    };
    // A PNG preview needs only the leaves' geometry and the CSG products
    // (`prepare_preview`); the full render is for the other outputs and
    // for a PNG with `--render`.
    let previewer = job
        .png
        .and_then(|s| s.previewer)
        .filter(|_| formats.contains(&MeshFormat::Png));
    let needs_geometry = previewer.is_none() || formats.iter().any(|f| *f != MeshFormat::Png);
    let tree = match job.png.filter(|_| previewer.is_some()) {
        Some(settings) => {
            match geom::csg::CsgTree::build(top, renderer, &keys, opts.clone(), settings.csg_limit)
            {
                Ok(t) => {
                    if print_messages(job, loaded, paths, &t.messages, con) {
                        return EXIT_ERROR;
                    }
                    Some(t)
                }
                Err(u) => return unsupported(u, con),
            }
        }
        None => None,
    };
    let rendered = if needs_geometry {
        let rendered = renderer.render(top, &keys, opts.clone());
        // The normal render's time, which `-o x.step`'s report compares
        // its own stages with (the exact-geometry audit's gate 5).
        render_ms = started.elapsed().as_secs_f64() * 1000.0;
        match rendered {
            Ok(r) => {
                if print_messages(job, loaded, paths, &r.messages, con) {
                    return EXIT_ERROR;
                }
                Some(r)
            }
            Err(u) => return unsupported(u, con),
        }
    } else {
        None
    };
    // NeoSCAD's own findings on the input meshes, for `--format json`
    // only: they are never printed, so a run that only prints (every
    // conformance run) skips the work.
    if con.recording() && (rendered.is_some() || tree.is_some()) {
        let import_mesh = |n: &eval::Node| match renderer.cached_leaf(n, &keys) {
            Some(geom::Geometry::PolySet(ps)) => Some(ps),
            _ => None,
        };
        session::orient::report(
            con,
            top,
            &import_mesh,
            &mut |_, f| std::sync::Arc::new(f()),
            &|u| unit_program(loaded, u),
            &paths.cwd,
        );
    }
    // Fillet calls (`--enable fillet`) select their edges on the rendered
    // children and say what they selected, as every other host does.
    if (rendered.is_some() || tree.is_some()) && session::fillets::any(top) {
        session::fillets::report(
            con,
            top,
            renderer,
            &keys,
            &opts,
            &|u| unit_program(loaded, u),
            &paths.cwd,
        );
    }
    let cache_entries = rendered.as_ref().map_or(0, |r| r.cache_entries);
    // A preview (no `rendered`) reports an empty cache, as its entry count
    // already does; the budget is the renderer's either way.
    let cache = renderer.stats();
    let cache_bytes = if rendered.is_some() { cache.bytes } else { 0 };
    // `if (!root_geom) root_geom = std::make_shared<PolySet>(3);`
    let root = rendered.and_then(|r| r.geometry);
    if job.json
        && let Some(g) = root.as_ref().filter(|g| !g.is_empty())
    {
        crate::report::set_geometry(session::stats::geometry(g, &opts.scheme));
    }
    let dim = root.as_ref().map_or(3, geom::Geometry::dimension);
    if force && dim == 3 {
        con.print(None, b"Converted to backend-specific geometry");
    }
    // The summary's camera: the view after `$vp*` (`Camera::updateView`),
    // or, after a PNG, the camera the image was drawn with (`export_png`
    // fits `--viewall` into the same object).
    let mut summary_camera = ev.camera;
    let mut mesh = None;
    let mut settings = None;
    for (target, format) in job.outputs.iter().zip(formats) {
        let target = &frame_target(target, frame.number);
        if *format == MeshFormat::Png {
            let Some(settings) = job.png else {
                unreachable!("PNG settings are made for PNG outputs");
            };
            let drawn = match &tree {
                Some(t) => crate::png::preview_png(settings, t, &ev.camera),
                None => crate::png::render_png(
                    settings,
                    root.as_ref().filter(|g| !g.is_empty()),
                    &ev.camera,
                ),
            };
            let data = match drawn {
                Ok((d, cam)) => {
                    summary_camera = crate::png::summary_camera(&cam, &ev.camera);
                    d
                }
                Err(e) => {
                    eprintln!("neoscad: cannot export PNG: {e}");
                    return EXIT_ERROR;
                }
            };
            if let Err(code) = write_output(target, &data) {
                return code;
            }
            continue;
        }
        // `checkAndExport`, per output: the dimension, then emptiness.
        let Some(want) = format.dimension() else {
            unreachable!("only images have no dimension");
        };
        if dim != want {
            con.print(
                None,
                format!("Current top level object is not a {want}D object.").as_bytes(),
            );
            return EXIT_ERROR;
        }
        let Some(root) = root.as_ref().filter(|g| !g.is_empty()) else {
            con.print(None, b"Current top level object is empty.");
            return EXIT_ERROR;
        };
        if *format == MeshFormat::Step {
            let code = export_step(
                job, paths, loaded, renderer, top, &keys, &opts, root, target, render_ms, con,
            );
            if code != 0 {
                return code;
            }
            continue;
        }
        let settings = settings.get_or_insert_with(|| export_settings(job, options, &opts));
        let enc = session::export::encode(format.session(), root, settings, &mut mesh);
        for (severity, line) in &enc.immediate {
            con.print(*severity, line.as_bytes());
        }
        let (data, warnings) = (enc.data, enc.warnings);
        for w in warnings {
            con.print(Some(Severity::Warning), format!("WARNING: {w}").as_bytes());
            if job.hardwarnings {
                return EXIT_ERROR;
            }
        }
        if let Err(code) = write_output(target, &data) {
            return code;
        }
    }
    // `RenderStatistic::printAll`: cache size, time, the object, then the
    // parts `--summary` asks for; or all of it as JSON to `--summary-file`.
    let facts = crate::summary::Facts {
        cache_entries,
        cache_bytes,
        cache_budget: cache.budget,
        elapsed_ms: started.elapsed().as_millis(),
        geometry: root.as_ref(),
        camera: &summary_camera,
    };
    if !crate::summary::emit(job.summary, &facts, con) {
        return EXIT_ERROR;
    }
    // A fillet or chamfer call that failed left its child sharp: the files
    // are written (so the model can be looked at), but the run fails
    // (`docs/fillets.md`, section 18, decision 2), so a script or an agent
    // cannot ship the sharp part believing it rounded.
    if con.failed_fillets() > 0 {
        return EXIT_ERROR;
    }
    0
}

/// `-o x.step` with `--enable exact`: the exact export of the tree the
/// normal render just built (`geom::exact`), its substitutions printed as
/// `INFO` and `WARNING` lines at their source locations, and no file at
/// all when the export fails its checks.
#[allow(clippy::too_many_arguments)]
fn export_step<W: Write>(
    job: &Job<'_>,
    paths: &Paths,
    loaded: &Loaded,
    renderer: &geom::Renderer,
    top: &eval::Node,
    keys: &eval::dump::Keys,
    opts: &geom::RenderOptions,
    normal: &geom::Geometry,
    target: &str,
    render_ms: f64,
    con: &mut Console<W>,
) -> u8 {
    let t0 = std::time::Instant::now();
    let clock = move || t0.elapsed().as_secs_f64() * 1000.0;
    let file_name = std::path::Path::new(target).file_name().map_or_else(
        || "part.step".to_string(),
        |f| f.to_string_lossy().into_owned(),
    );
    let product = std::path::Path::new(display_name(job))
        .file_stem()
        .map_or_else(|| "part".to_string(), |f| f.to_string_lossy().into_owned());
    let x = geom::exact::ExactOptions {
        // Fixed names and date: the same model gives the same file. The
        // originating system has no version in it for the same reason.
        step: geom::exact::meshbrep::StepOptions {
            product_name: product,
            file_name,
            originating_system: "NeoSCAD".into(),
            ..Default::default()
        },
        clock: Some(&clock),
    };
    let result = geom::exact::export_step(renderer, top, keys, opts, normal, &x);
    let (subs, stats) = match &result {
        Ok(e) => (&e.substitutions, &e.stats),
        Err(f) => (&f.substitutions, &f.stats),
    };
    // The session's report, so `--format json` and every host's (serve,
    // MCP, the apps, the web page) agree on the counts and their words.
    let locate = |l: &geom::MsgLoc| {
        let sources = unit_sources(loaded, l.unit)?;
        let rel = lang::diag::relative_display(
            sources.path(l.span.file),
            &paths.main_dir,
            &lang::loader::StdFs,
        );
        Some((rel, l.line))
    };
    let mut report = session::exact::ExactReport::new(
        stats,
        subs,
        result.as_ref().err().map(|f| f.message.as_str()),
        &locate,
    );
    report.normal_render_ms = Some(render_ms);
    crate::report::set_exact(report.json());
    if print_messages(
        job,
        loaded,
        paths,
        &geom::exact::substitution_messages(subs),
        con,
    ) {
        return EXIT_ERROR;
    }
    match result {
        Ok(e) => match write_output(target, e.step.as_bytes()) {
            Ok(()) => 0,
            Err(code) => code,
        },
        Err(f) => {
            con.print(
                Some(Severity::Error),
                format!(
                    "ERROR: STEP export failed: {}. No file was written.",
                    f.message
                )
                .as_bytes(),
            );
            EXIT_ERROR
        }
    }
}

/// How the shared encoder writes this run's files: the `-O` settings, the
/// scheme's colours and what the files record about their origin.
pub fn export_settings(
    job: &Job<'_>,
    options: &eval::Options,
    opts: &geom::RenderOptions,
) -> session::export::Settings {
    encode_settings(
        job.export_options,
        opts.scheme,
        display_name(job),
        &options.camera,
        options.features,
    )
}

/// [`export_settings`] from its parts: `input` as named on the command
/// line, the command line's camera, and the `--enable` features (for
/// `predictible-output`).
pub fn encode_settings(
    export_options: &crate::export_options::ExportOptions,
    scheme: geom::color::Scheme,
    input: &str,
    camera: &eval::Camera,
    features: eval::Features,
) -> session::export::Settings {
    let (mut pdf, colors) = export_options.pdf();
    let pdf_warnings = colors.resolve(&mut pdf);
    let (threemf, threemf_warning) = export_options.threemf(scheme.face_front);
    session::export::Settings {
        scheme,
        svg: export_options.svg(),
        pdf,
        pdf_warnings,
        threemf,
        threemf_warning,
        // `ExportInfo::title`: the input's file name.
        title: Path::new(input)
            .file_name()
            .map(|f| f.to_string_lossy().into_owned())
            .unwrap_or_default(),
        source_path: input.to_string(),
        creation_date: iso8601_now(),
        // `ExportInfo::camera` is the command line's camera; the file's
        // `$vp*` do not change it.
        pov_camera: Some(io::pov::PovCamera {
            translation: camera.vpt,
            rotation: camera.vpr,
            distance: camera.vpd,
            fov: camera.vpf,
        }),
        predictible_output: features.has(eval::Feature::PredictibleOutput),
    }
}

/// What the geometry evaluator needs besides the tree: files, fonts (those
/// the program `use`s as well as the bundled ones) and the scheme's face
/// colours.
fn render_options(
    job: &Job<'_>,
    loaded: &Loaded,
    paths: &Paths,
    force: bool,
) -> geom::RenderOptions {
    let used = std::iter::once(&loaded.program)
        .chain(
            loaded
                .libraries
                .iter()
                .filter_map(|lib| lib.program.as_ref()),
        )
        .flat_map(|p| p.ast.uses.iter());
    geom::RenderOptions {
        force,
        fs: loaded.host.fs.clone(),
        work_dir: paths.cwd.clone(),
        fonts: std::sync::Arc::new(loaded.host.fonts(used)),
        scheme: job.scheme.geometry_scheme(),
        // Cache hits are silent, as in OpenSCAD (animation frames share
        // the cache).
        interrupt: None,
        guard: None,
        replay: None,
    }
}

/// Print geometry messages as OpenSCAD's log does; `true` when
/// `--hardwarnings` stops the run at one.
fn print_messages<W: Write>(
    job: &Job<'_>,
    loaded: &Loaded,
    paths: &Paths,
    messages: &[geom::Msg],
    con: &mut Console<W>,
) -> bool {
    for m in messages {
        let Some(severity) = m.severity else {
            // A plain `LOG(...)` line.
            con.print(None, m.text.as_bytes());
            continue;
        };
        let mut diag =
            lang::diag::Diagnostic::new(lang::diag::DiagCode::Geometry, severity, m.text.clone());
        let mut sources = &loaded.program.sources;
        if let Some(l) = &m.loc
            && let Some(s) = unit_sources(loaded, l.unit)
        {
            // Each message's file prints relative to its own base (see
            // `geom::MsgLoc::base`): `in file ../../x.scad` from the
            // working directory for a reader's error, as the nightly
            // prints it.
            diag = diag.at(l.span, l.line).with_base(l.base);
            sources = s;
        }
        con.diagnostic(&diag, sources, &paths.cwd);
        // OpenSCAD's geometry evaluation stops here; neoscad has already
        // built the rest, but prints nothing more.
        if job.hardwarnings && severity == Severity::Warning && !printed_in_handler(&m.text) {
            return true;
        }
    }
    false
}

/// Whether OpenSCAD prints this geometry warning from inside a `catch`
/// block. `PRINT` only raises `--hardwarnings` when no exception is being
/// handled (`if (!std::current_exception())`, `printutils.cc:125`), so
/// these warnings never stop a run: without this, `import()` of a missing
/// 3MF file would exit 1 where the nightly renders on. The list is every
/// `LOG(message_group::Warning, ...)` in a handler that neoscad reproduces:
/// `import_3mf_v2.cc:385`, `SurfaceNode.cc:211`, `DxfData.cc:144,362,364`
/// and `manifold-applyops-minkowski.cc:246`.
fn printed_in_handler(text: &str) -> bool {
    const PREFIXES: [&str; 6] = [
        "Could not read file '",
        "Illegal value in '",
        "Illegal ID '",
        "Illegal value '",
        "Not enough input values for ",
        "[manifold] Minkowski hard-crashed",
    ];
    PREFIXES.iter().any(|p| text.starts_with(p))
}

/// `-o x.param` (`export_param.cc`): the program is evaluated first, as
/// for every export (`do_export`), then its customizer parameters are
/// written as JSON. Reading the parameters prints their range warnings
/// again, after any from `-p`/`-P`, as OpenSCAD does.
pub fn export_param(job: &Job<'_>, options: &Options) -> u8 {
    with_console(job, |paths, con| {
        export_param_with(job, options, paths, con)
    })
}

fn export_param_with<W: Write>(
    job: &Job<'_>,
    options: &Options,
    paths: &Paths,
    con: &mut Console<W>,
) -> u8 {
    let loaded = match load(job, paths, con) {
        Ok(l) => l,
        Err(code) => return code,
    };
    let renderer = std::sync::Arc::new(geom::Renderer::new());
    let oracle = query_oracle(job, &loaded, paths, options, &renderer);
    let options = &with_oracle(options, oracle.as_ref());
    for frame in job.frames() {
        job.announce(frame, con);
        let ev = evaluate(&loaded, paths, &at_time(options, frame), con);
        if ev.hard_warning {
            return EXIT_ERROR;
        }
        if let Err(code) = write_params(job, paths, &loaded, frame, con) {
            return code;
        }
    }
    0
}

/// The `.param` outputs after one evaluation.
fn write_params<W: Write>(
    job: &Job<'_>,
    paths: &Paths,
    loaded: &Loaded,
    frame: Frame,
    con: &mut Console<W>,
) -> Result<(), u8> {
    let mut warnings = Vec::new();
    let params = Parameters::from_ast(&loaded.program.ast, &mut warnings);
    for w in &warnings {
        con.diagnostic(w, &loaded.program.sources, &paths.cwd);
        if job.hardwarnings {
            return Err(EXIT_ERROR);
        }
    }
    // `path.stem()` of the absolute input path; stdin has none.
    let title = if job.input == "-" {
        "Unnamed".to_string()
    } else {
        Path::new(job.input)
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "Unnamed".into())
    };
    let json = crate::param_json::export(&params, &title);
    for target in job.outputs {
        write_output(&frame_target(target, frame.number), json.as_bytes())?;
    }
    Ok(())
}

/// `get_current_iso8601_date_time_utc` (`export.cc`): `YYYY-MM-DDTHH:MM:SSZ`.
pub fn iso8601_now() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs()) as i64;
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    // Howard Hinnant's `civil_from_days`.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        rem / 60 % 60,
        rem % 60
    )
}

/// The program of evaluation unit `unit` (see [`unit_sources`]).
fn unit_program(l: &Loaded, unit: u32) -> Option<&Program> {
    if unit == 0 {
        return Some(&l.program);
    }
    l.libraries
        .get(unit as usize - 1)
        .and_then(|lib| lib.program.as_ref())
}

/// The source map of evaluation unit `unit`: 0 is the main program, `1 + i`
/// the i-th library, in the order `evaluate` passed them.
fn unit_sources(l: &Loaded, unit: u32) -> Option<&lang::source::SourceMap> {
    if unit == 0 {
        return Some(&l.program.sources);
    }
    l.libraries
        .get(unit as usize - 1)
        .and_then(|lib| lib.program.as_ref())
        .map(|p| &p.sources)
}
