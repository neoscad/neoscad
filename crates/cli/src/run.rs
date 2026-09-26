//! Running a program through the front end and evaluator, the way
//! `openscad.cc`'s `cmdline()` does, and the `.ast` and `.echo` exports.

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
        let main_dir = cwd.join(display_name(job)).parent().map(Path::to_path_buf).unwrap_or_else(|| cwd.clone());
        Paths { cwd, main_dir }
    }
}

/// OpenSCAD names stdin `<stdin>` and resolves it like a file in the
/// working directory.
fn display_name<'a>(job: &Job<'a>) -> &'a str {
    if job.input == "-" { "<stdin>" } else { job.input }
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
        con.print(None, format!("Can't open input file '{display}'!\n").as_bytes());
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
            (Some(p), _) => p.openscad_diags().for_each(|d| con.diagnostic(d, &p.sources, &paths.cwd)),
            (None, Some(msg)) => con.print(Some(Severity::Warning), msg.as_bytes()),
            (None, None) => {}
        }
    }
    let uses = resolve_uses(&program, &StdFs, &libs);
    Ok(Loaded { program, uses, libraries })
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
        if let Err(code) = write_output(target, &text) {
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
            let libs: Vec<eval::Library<'_>> = l
                .libraries
                .iter()
                .map(|lib| eval::Library { path: &lib.path, program: lib.program.as_ref(), uses: &lib.uses })
                .collect();
            // `main` runs this on a thread with `eval::DEFAULT_THREAD_STACK`.
            eval::evaluate(&l.program, &l.uses, &libs, paths.main_dir.clone(), options, &mut con);
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
