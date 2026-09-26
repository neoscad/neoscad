//! Running a program through the front end, the way `openscad.cc`'s
//! `cmdline()` does, and the `.ast` export.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use lang::customizer::{Parameters, read_parameter_sets};
use lang::deps::{Library, load_dependencies};
use lang::diag::Diagnostic;
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
}

/// Where messages are made relative to.
struct Paths {
    cwd: PathBuf,
    main_dir: PathBuf,
}

fn print_diag(d: &Diagnostic, program: &Program, paths: &Paths) {
    eprintln!("{}", d.render_openscad(&program.sources, &paths.cwd, &paths.main_dir));
}

/// The main program and the libraries it uses.
struct Loaded {
    program: Program,
    #[allow(dead_code)] // consumed by the evaluator (next phase)
    libraries: Vec<Library>,
}

/// Parse the input with `-D` definitions, apply a parameter set and parse
/// the used libraries, printing OpenSCAD's messages. On failure returns the
/// exit code.
fn load(job: &Job<'_>) -> Result<(Loaded, Paths), u8> {
    let cwd = std::env::current_dir().unwrap_or_default();
    let stdin = job.input == "-";
    // OpenSCAD names stdin `<stdin>` and resolves it like a file in the
    // working directory.
    let display = if stdin { "<stdin>" } else { job.input };
    let mut text = Vec::new();
    let read = if stdin {
        std::io::stdin().read_to_end(&mut text).map(|_| ())
    } else {
        std::fs::read(job.input).map(|t| text = t)
    };
    if read.is_err() {
        eprintln!("Can't open input file '{display}'!\n");
        return Err(EXIT_ERROR);
    }
    // cmdline(): the text, then an end-of-text marker, then each -D.
    let mut suffix = b"\n\x03\n".to_vec();
    for d in job.defines {
        suffix.extend_from_slice(d.as_bytes());
        suffix.extend_from_slice(b";\n");
    }
    text.extend_from_slice(&suffix);
    let path = cwd.join(display);
    let paths = Paths { main_dir: path.parent().map(Path::to_path_buf).unwrap_or_else(|| cwd.clone()), cwd: cwd.clone() };

    let libs = LibraryPath::from_env();
    let mut program = parse_program(path, text, &StdFs, &libs);
    for d in program.openscad_diags() {
        print_diag(d, &program, &paths);
    }
    if program.has_syntax_errors() {
        eprintln!("Can't parse file '{display}'!\n");
        return Err(EXIT_ERROR);
    }

    if let (Some(file), Some(set_name)) = (job.parameter_file, job.parameter_set) {
        let mut warnings = Vec::new();
        let mut params = Parameters::from_ast(&program.ast, &mut warnings);
        for w in &warnings {
            print_diag(w, &program, &paths);
        }
        match read_parameter_sets(Path::new(file)) {
            Ok(sets) => {
                if let Some(set) = sets.iter().find(|s| s.name == set_name) {
                    params.import(set);
                    params.apply(&mut program.ast);
                }
            }
            Err(e) => print_diag(&e, &program, &paths),
        }
    }

    // handleDependencies(): parse the used libraries and report on them.
    let libraries = load_dependencies(&program, &suffix, &StdFs, &libs);
    for lib in &libraries {
        match (&lib.program, lib.open_error()) {
            (Some(p), _) => p.openscad_diags().for_each(|d| print_diag(d, p, &paths)),
            (None, Some(msg)) => eprintln!("{msg}"),
            (None, None) => {}
        }
    }
    Ok((Loaded { program, libraries }, paths))
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
    let (loaded, _paths) = match load(job) {
        Ok(p) => p,
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
