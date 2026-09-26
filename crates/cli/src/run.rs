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
        con.print(
            None,
            format!("Can't open input file '{display}'!\n").as_bytes(),
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
    for frame in job.frames() {
        job.announce(frame, &mut con);
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
    let options = Options {
        fs: l.host.fs.clone(),
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
    let paths = Paths::of(job);
    let mut con = Console::new(std::io::stderr(), paths.main_dir.clone(), job.quiet);
    let loaded = match load(job, &paths, &mut con) {
        Ok(l) => l,
        Err(code) => return code,
    };
    for frame in job.frames() {
        job.announce(frame, &mut con);
        let ev = evaluate(&loaded, &paths, &at_time(options, frame), &mut con);
        if ev.hard_warning {
            return EXIT_ERROR;
        }
        if let Err(code) = write_trees(job, &paths, &loaded, &ev, formats, frame) {
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
    let mut con = Console::new(Vec::new(), paths.main_dir.clone(), job.quiet);
    let code = match load(job, &paths, &mut con) {
        Err(code) => code,
        // The echo file is written as messages arrive, so after a hard
        // warning it holds everything up to it. An animation writes every
        // frame into the one file: OpenSCAD opens the echo stream before
        // its frame loop and never renames it.
        Ok(l) => {
            let mut code = 0;
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
}

impl MeshFormat {
    /// The dimension `checkAndExport` requires (`fileformat::is3D/is2D`).
    fn dimension(self) -> u32 {
        match self {
            MeshFormat::Svg | MeshFormat::Dxf | MeshFormat::Pdf => 2,
            _ => 3,
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
    let paths = Paths::of(job);
    let mut con = Console::new(std::io::stderr(), paths.main_dir.clone(), job.quiet);
    let loaded = match load(job, &paths, &mut con) {
        Ok(l) => l,
        Err(code) => return code,
    };
    let renderer = geom::Renderer::new();
    for frame in job.frames() {
        job.announce(frame, &mut con);
        let code = render_frame(
            job,
            &paths,
            &loaded,
            &renderer,
            &at_time(options, frame),
            formats,
            force,
            frame,
            &mut con,
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
    renderer: &geom::Renderer,
    options: &eval::Options,
    formats: &[MeshFormat],
    force: bool,
    frame: Frame,
    con: &mut Console<W>,
) -> u8 {
    let ev = evaluate(loaded, paths, options, con);
    if ev.hard_warning {
        return EXIT_ERROR;
    }
    // `RenderStatistic` starts timing after instantiation, when geometry
    // evaluation begins.
    let started = std::time::Instant::now();
    let top = ev.root.find_root_tag().0.unwrap_or(&ev.root);
    let keys = eval::dump::Keys::new(&ev.root, &*loaded.host.fs);
    let used = std::iter::once(&loaded.program)
        .chain(
            loaded
                .libraries
                .iter()
                .filter_map(|lib| lib.program.as_ref()),
        )
        .flat_map(|p| p.ast.uses.iter());
    let opts = geom::RenderOptions {
        force,
        fs: loaded.host.fs.clone(),
        work_dir: paths.cwd.clone(),
        fonts: std::sync::Arc::new(loaded.host.fonts(used)),
        ..Default::default()
    };
    let rendered = renderer.render(top, &keys, opts.clone());
    let rendered = match rendered {
        Ok(r) => r,
        Err(u) => {
            let mut line = format!("neoscad: {}() is not implemented yet", u.what);
            if let Some(l) = &u.loc
                && let Some(sources) = unit_sources(loaded, l.unit)
            {
                let rel = lang::diag::relative_path(sources.path(l.span.file), &paths.main_dir);
                line.push_str(&format!(" (in file {}, line {})", rel.display(), l.line));
            }
            eprintln!("{line}");
            return EXIT_NOT_IMPLEMENTED;
        }
    };
    for m in &rendered.messages {
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
            return EXIT_ERROR;
        }
    }
    // `if (!root_geom) root_geom = std::make_shared<PolySet>(3);`
    let root = rendered.geometry;
    let dim = root.as_ref().map_or(3, geom::Geometry::dimension);
    if force && dim == 3 {
        con.print(None, b"Converted to backend-specific geometry");
    }
    let mut mesh = None;
    for (target, format) in job.outputs.iter().zip(formats) {
        let target = &frame_target(target, frame.number);
        // `checkAndExport`, per output: the dimension, then emptiness.
        let want = format.dimension();
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
        let mut warnings = Vec::new();
        let data = match (format, root) {
            (MeshFormat::Svg, geom::Geometry::Polygon2d(p)) => {
                geom::export::svg(p, &job.export_options.svg())
            }
            (MeshFormat::Dxf, geom::Geometry::Polygon2d(p)) => geom::export::dxf(p),
            (MeshFormat::Pdf, geom::Geometry::Polygon2d(p)) => {
                let (mut pdf_options, colors) = job.export_options.pdf();
                warnings.extend(colors.resolve(&mut pdf_options));
                let info = io::pdf::PdfInfo {
                    title: &file_title(job),
                    source_path: display_name(job),
                    creation_date: &iso8601_now(),
                };
                let (data, export_warnings) = geom::export::pdf(p, &pdf_options, &info);
                for w in export_warnings {
                    // `message_group::Export_Warning`: not a warning for
                    // `--hardwarnings`.
                    con.print(None, format!("EXPORT-WARNING: {w}").as_bytes());
                }
                data
            }
            _ => {
                let ps = mesh.get_or_insert_with(|| {
                    geom::export::as_polyset(root, &opts.scheme).expect("3D geometry has a mesh")
                });
                match format {
                    MeshFormat::AsciiStl => geom::export::stl(ps, false, &mut warnings),
                    MeshFormat::BinaryStl => geom::export::stl(ps, true, &mut warnings),
                    MeshFormat::Off => geom::export::off(ps, &mut warnings),
                    MeshFormat::Wrl => geom::export::wrl(ps, &mut warnings),
                    MeshFormat::Pov => io::pov::write(
                        ps.mesh(),
                        &io::pov::PovOptions {
                            title: &file_title(job),
                            default_color: opts.scheme.face_front,
                            // `ExportInfo::camera` is the command line's
                            // camera; the file's `$vp*` do not change it.
                            camera: Some(io::pov::PovCamera {
                                translation: options.camera.vpt,
                                rotation: options.camera.vpr,
                                distance: options.camera.vpd,
                                fov: options.camera.vpf,
                            }),
                        },
                    ),
                    MeshFormat::ThreeMf => {
                        // `export_3mf` with the `-O export-3mf/...` settings:
                        // the mesh is triangulated first, as
                        // `geom::export::threemf` does for the defaults.
                        let (o3, color_warning) =
                            job.export_options.threemf(opts.scheme.face_front);
                        warnings.extend(color_warning);
                        let tri = ps.tessellate(&mut warnings);
                        let (data, msgs) = io::threemf::write_with(
                            tri.mesh(),
                            &io::threemf::WriteOptions {
                                title: &file_title(job),
                                creation_date: &iso8601_now(),
                                default_color: opts.scheme.face_front,
                            },
                            &o3,
                        );
                        for m in msgs {
                            match m.severity {
                                Some(Severity::Warning) => warnings.push(m.text),
                                _ => con.print(Some(Severity::Error), m.text.as_bytes()),
                            }
                        }
                        data
                    }
                    _ => geom::export::obj(ps, &mut warnings),
                }
            }
        };
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
        cache_entries: rendered.cache_entries,
        elapsed_ms: started.elapsed().as_millis(),
        geometry: root.as_ref(),
        camera: &options.camera,
    };
    if !crate::summary::emit(job.summary, &facts, con) {
        return EXIT_ERROR;
    }
    0
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

/// `ExportInfo::title`: the input's file name.
fn file_title(job: &Job<'_>) -> String {
    Path::new(display_name(job))
        .file_name()
        .map(|f| f.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// `-o x.param` (`export_param.cc`): the program is evaluated first, as
/// for every export (`do_export`), then its customizer parameters are
/// written as JSON. Reading the parameters prints their range warnings
/// again, after any from `-p`/`-P`, as OpenSCAD does.
pub fn export_param(job: &Job<'_>, options: &Options) -> u8 {
    let paths = Paths::of(job);
    let mut con = Console::new(std::io::stderr(), paths.main_dir.clone(), job.quiet);
    let loaded = match load(job, &paths, &mut con) {
        Ok(l) => l,
        Err(code) => return code,
    };
    for frame in job.frames() {
        job.announce(frame, &mut con);
        let ev = evaluate(&loaded, &paths, &at_time(options, frame), &mut con);
        if ev.hard_warning {
            return EXIT_ERROR;
        }
        if let Err(code) = write_params(job, &paths, &loaded, frame, &mut con) {
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
fn iso8601_now() -> String {
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
