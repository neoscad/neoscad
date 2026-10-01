//! `neoscad generate man` and `neoscad generate completions SHELL`: the
//! manual page, neoscad(1), and shell completion scripts, printed on
//! stdout for packages (the .deb, .rpm and AUR package install them) and
//! for anyone setting up a shell by hand.
//!
//! Both are generated from the clap definitions that parse the command
//! line and print `--help` ([`crate::Cli`] and each entry of
//! [`crate::SUBCOMMANDS`]), never from a second, hand-kept copy: a copy
//! drifts the first time a flag is added, and a manual page that documents
//! flags the binary no longer has is worse than none. The page's prose
//! (description, environment, exit status, examples) is written here; the
//! tests check that every example still parses.
//!
//! The page follows OpenSCAD's own `openscad.1` (`doc/openscad.1.in`):
//! NAME, SYNOPSIS, DESCRIPTION, OPTIONS and examples, with neoscad's
//! subcommands in a COMMANDS section of the same page rather than in pages
//! of their own, so that `man neoscad` is the whole reference. Output is
//! deterministic (no date in the title line), so a package rebuilt from the
//! same binary is byte-identical.

use std::ffi::OsString;
use std::io::Write;

use clap::{CommandFactory, Parser};
use clap_complete::Shell;
use roff::{Roff, bold, italic, roman};

const EXIT_ERROR: u8 = 1;

#[derive(Parser, Debug)]
#[command(
    name = "neoscad generate",
    about = "Print the manual page or a shell completion script",
    version,
    disable_help_subcommand = true
)]
pub(crate) struct Args {
    #[command(subcommand)]
    what: What,
}

#[derive(clap::Subcommand, Debug)]
enum What {
    /// The manual page, neoscad(1), in roff: gzip it into a man1
    /// directory, or read it with `man ./neoscad.1`.
    Man,
    /// A completion script for SHELL (bash, zsh, fish, elvish or
    /// powershell), e.g. `neoscad generate completions zsh > _neoscad`
    /// in a directory on zsh's fpath.
    Completions {
        #[arg(value_name = "SHELL")]
        shell: Shell,
    },
}

/// Run `neoscad generate` with the arguments after `generate`.
pub fn main(args: Vec<OsString>) -> u8 {
    let argv = std::iter::once(OsString::from("neoscad generate")).chain(args);
    let a = match Args::try_parse_from(argv) {
        Ok(a) => a,
        Err(e) => {
            let _ = e.print();
            return if e.use_stderr() { EXIT_ERROR } else { 0 };
        }
    };
    let out = match a.what {
        What::Man => man_page(),
        What::Completions { shell } => completions(shell),
    };
    let mut stdout = std::io::stdout().lock();
    match stdout.write_all(&out).and_then(|()| stdout.flush()) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("neoscad generate: {e}");
            EXIT_ERROR
        }
    }
}

/// The whole command line as one clap command: OpenSCAD's flags at the top
/// level and each of neoscad's subcommands under its own name, which is
/// the shape the completion generators need.
///
/// `neoscad` itself does not parse with this: `main` picks a subcommand by
/// its first word and hands the rest to that subcommand's own parser, so
/// that OpenSCAD's command line stays exactly OpenSCAD's.
/// `args_conflicts_with_subcommands` describes that: after a subcommand's
/// name, only its own flags apply.
pub(crate) fn command() -> clap::Command {
    let mut cmd = crate::Cli::command()
        .bin_name("neoscad")
        .version(env!("CARGO_PKG_VERSION"))
        .args_conflicts_with_subcommands(true)
        // `neoscad help` is not a command (it would be read as an input
        // file), so the scripts must not offer clap's implicit one.
        .disable_help_subcommand(true)
        .subcommand_value_name("COMMAND");
    for sub in crate::SUBCOMMANDS {
        // Each subcommand's parser is named "neoscad NAME" for its usage
        // and error messages; as a child of `neoscad` it is just NAME.
        cmd = cmd.subcommand((sub.command)().name(sub.name));
    }
    cmd
}

/// The completion script for `shell`.
pub(crate) fn completions(shell: Shell) -> Vec<u8> {
    let mut cmd = command();
    let mut out = Vec::new();
    clap_complete::generate(shell, &mut cmd, "neoscad", &mut out);
    out
}

/// The manual page, neoscad(1), as roff.
pub(crate) fn man_page() -> Vec<u8> {
    let version = env!("CARGO_PKG_VERSION");
    // roff's apostrophe definition, which every rendering starts with; the
    // page carries it once, at the top.
    let preamble = Roff::new().render();
    let piece = |r: &mut dyn FnMut(&mut Vec<u8>) -> std::io::Result<()>| -> String {
        let mut buf = Vec::new();
        r(&mut buf).expect("writing to a Vec cannot fail");
        let s = String::from_utf8(buf).expect("roff output is UTF-8");
        s.strip_prefix(&preamble).map(str::to_owned).unwrap_or(s)
    };
    let own = |r: &Roff| {
        r.render()
            .strip_prefix(&preamble)
            .unwrap_or_default()
            .to_owned()
    };

    let main = man_command(crate::Cli::command());
    let man = clap_mangen::Man::new(main);

    // The title line is written here: clap_mangen drops an empty date
    // argument instead of quoting it, which would shift the source
    // ("neoscad VERSION") into the date's place. The date stays empty so
    // that the page is the same bytes on every build of a version.
    let mut page = preamble.clone();
    page += &format!(".TH NEOSCAD 1 \"\" \"neoscad {version}\" \"NeoSCAD Manual\"\n");

    let mut r = Roff::new();
    r.control("SH", ["NAME"]);
    r.text([roman(
        "neoscad - OpenSCAD-compatible programmable solid CAD",
    )]);
    page += &own(&r);

    // SYNOPSIS: OpenSCAD's command line, then one line per subcommand.
    page += &piece(&mut |w| man.render_synopsis_section(w));
    for sub in crate::SUBCOMMANDS {
        let cmd = man_command((sub.command)());
        let sub_man = clap_mangen::Man::new(cmd);
        let mut r = Roff::new();
        r.control("br", []);
        page += &own(&r);
        page += &body(&piece(&mut |w| sub_man.render_synopsis_section(w)));
    }

    page += &own(&description());
    page += &piece(&mut |w| man.render_options_section(w));

    // COMMANDS: each subcommand's description and options, under its own
    // subsection, so the page is the whole reference.
    let mut r = Roff::new();
    r.control("SH", ["COMMANDS"]);
    r.text([
        roman("neoscad's own commands. Each takes the options listed under it, and "),
        bold("neoscad"),
        roman(" "),
        italic("COMMAND"),
        roman(" "),
        bold("--help"),
        roman(" prints them."),
    ]);
    page += &own(&r);
    for sub in crate::SUBCOMMANDS {
        let cmd = man_command((sub.command)());
        let name = cmd.get_name().to_string();
        let sub_man = clap_mangen::Man::new(cmd.clone());
        let mut r = Roff::new();
        r.control("SS", [name.as_str()]);
        page += &own(&r);
        page += &body(&piece(&mut |w| sub_man.render_synopsis_section(w)));
        let mut r = Roff::new();
        r.control("PP", []);
        page += &own(&r);
        page += &body(&piece(&mut |w| sub_man.render_description_section(w)));
        page += &body(&piece(&mut |w| sub_man.render_options_section(w)));
        // Nested commands (`neoscad generate man`), which clap_mangen would
        // list as pages of their own (neoscad-generate-man(1)).
        let nested: Vec<_> = cmd.get_subcommands().filter(|c| !c.is_hide_set()).collect();
        if !nested.is_empty() {
            let mut r = Roff::new();
            for c in nested {
                let mut head = vec![bold(format!("{name} {}", c.get_name()))];
                for a in c.get_positionals() {
                    let v = a
                        .get_value_names()
                        .map(|v| v.join(" "))
                        .unwrap_or_else(|| a.get_id().to_string());
                    head.push(roman(" "));
                    head.push(italic(v));
                }
                r.control("TP", []);
                r.text(head);
                let help = c
                    .get_long_about()
                    .or_else(|| c.get_about())
                    .map(|s| s.to_string())
                    .unwrap_or_default();
                r.text([roman(help)]);
            }
            page += &own(&r);
        }
    }

    page += &own(&environment());
    page += &own(&exit_status());
    page += &own(&examples());
    page += &own(&see_also());
    page.into_bytes()
}

/// `cmd` (whose name is already the words that run it, `neoscad check`)
/// usage-overridden for the page: the synopsis line is
/// clap's own short usage (`neoscad [OPTIONS] [INPUT]...`), as `--help`
/// prints it, rather than clap_mangen's default of spelling out every one
/// of OpenSCAD's forty-odd flags on one line.
fn man_command(cmd: clap::Command) -> clap::Command {
    let name = cmd.get_name().to_string();
    let mut cmd = cmd.bin_name(name);
    let usage = cmd.render_usage().to_string();
    let usage = usage.trim().trim_start_matches("Usage:").trim().to_string();
    cmd.override_usage(usage)
}

/// A rendered section without its `.SH` heading, for use as part of a
/// subsection.
fn body(section: &str) -> String {
    section
        .lines()
        .filter(|l| !l.starts_with(".SH "))
        .map(|l| format!("{l}\n"))
        .collect()
}

fn description() -> Roff {
    let mut r = Roff::new();
    r.control("SH", ["DESCRIPTION"]);
    r.text([
        bold("neoscad"),
        roman(
            " is a reimplementation of OpenSCAD, the programmable solid CAD modeller. It \
             reads OpenSCAD programs (.scad files) and exports the models they describe.",
        ),
    ]);
    r.control("PP", []);
    r.text([
        roman("Its command line is OpenSCAD's: an input file, "),
        bold("-o"),
        roman(
            " with the output's file name, whose extension selects the format, and \
             OpenSCAD's options, so scripts and makefiles written for ",
        ),
        bold("openscad"),
        roman(" work unchanged. Unlike "),
        bold("openscad"),
        roman(", "),
        bold("neoscad"),
        roman(" has no graphical mode to fall back to, so "),
        bold("-o"),
        roman(" is required."),
    ]);
    r.control("PP", []);
    r.text([roman(
        "It also has commands of its own (see COMMANDS): printability checks and \
         measurements, image contact sheets of a model, a formatter, model tests, \
         reference documentation, a JSON-RPC server that keeps caches warm between \
         exports, a Model Context Protocol server for AI agents and a language server \
         for editors.",
    )]);
    r
}

fn environment() -> Roff {
    let vars: [(&str, &str); 7] = [
        (
            "OPENSCADPATH",
            "Directories searched for include and use files, as in OpenSCAD \
             (separated by colons, or semicolons on Windows).",
        ),
        (
            "OPENSCAD_FONT_PATH",
            "Further directories of fonts, as in OpenSCAD.",
        ),
        (
            crate::host::FONT_DIR_ENV,
            "A directory of fonts that replaces the bundled Liberation fonts.",
        ),
        (
            crate::client::NO_SERVER_ENV,
            "Set (and not 0) to export in this process even when a neoscad serve is \
             running, as --no-server does.",
        ),
        (
            crate::transport::ADDRESS_ENV,
            "The address of the neoscad serve socket, instead of the per-user default.",
        ),
        (
            crate::update::NO_CHECK_ENV,
            "Set to turn off the once-a-day check for a newer release. Its notice \
             appears only when stdout and stderr are both terminals and CI is not set.",
        ),
        (
            crate::DIAGNOSTICS_ENV,
            "openscad: print diagnostics exactly as OpenSCAD does, even on a terminal; \
             rich: show the source line under each diagnostic even when stderr is not a \
             terminal.",
        ),
    ];
    let mut r = Roff::new();
    r.control("SH", ["ENVIRONMENT"]);
    for (name, help) in vars {
        r.control("TP", []);
        r.text([bold(name)]);
        r.text([roman(help)]);
    }
    r
}

fn exit_status() -> Roff {
    let codes = [
        (0, "Success."),
        (
            crate::EXIT_ERROR,
            "An error: in the program, in an export, or on the command line, as in \
             OpenSCAD.",
        ),
        (2, "No -o was given, or not exactly one input file."),
        (
            crate::EXIT_NOT_IMPLEMENTED,
            "The output format exists in OpenSCAD but not yet in neoscad.",
        ),
    ];
    let mut r = Roff::new();
    r.control("SH", ["EXIT STATUS"]);
    for (code, help) in codes {
        r.control("TP", []);
        r.text([bold(code.to_string())]);
        r.text([roman(help)]);
    }
    r
}

/// The EXAMPLES section's command lines, each with what it does. A test
/// parses every one with the parser `neoscad` would use for it.
pub(crate) const EXAMPLES: &[(&str, &str)] = &[
    (
        "Render example001.scad to an STL file:",
        "neoscad -o example001.stl example001.scad",
    ),
    (
        "Draw a PNG with a camera rotated 25 degrees in x and 35 in z, distance 500, \
         with orthographic projection:",
        "neoscad -o o.png o.scad --camera=0,0,0,25,0,35,500 --projection=ortho",
    ),
    (
        "Set the variable mode before the program runs, and export a DXF:",
        "neoscad -o example017.dxf -D 'mode=\"parts\"' example017.scad",
    ),
    (
        "Check a part for FDM printing on a 220 by 220 by 250 mm bed, as JSON:",
        "neoscad check --bed 220x220x250 --format json part.scad",
    ),
    (
        "Install zsh completions for the current user (with ~/.zfunc on zsh's fpath):",
        "neoscad generate completions zsh > ~/.zfunc/_neoscad",
    ),
];

fn examples() -> Roff {
    let mut r = Roff::new();
    r.control("SH", ["EXAMPLES"]);
    for (i, (what, line)) in EXAMPLES.iter().enumerate() {
        // A paragraph break straight after the heading is redundant
        // (mandoc warns about it).
        if i > 0 {
            r.control("PP", []);
        }
        r.text([roman(*what)]);
        r.control("PP", []);
        r.control("RS", []);
        r.text([bold(*line)]);
        r.control("RE", []);
    }
    r
}

fn see_also() -> Roff {
    let mut r = Roff::new();
    r.control("SH", ["SEE ALSO"]);
    r.text([
        bold("openscad"),
        roman("(1). The documentation at https://neoscad.org and in the source, "),
        roman("https://github.com/neoscad/neoscad."),
    ]);
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    fn man() -> String {
        String::from_utf8(man_page()).unwrap()
    }

    /// Every long and short flag of `cmd`, as the page and the scripts
    /// spell them.
    fn flags(cmd: &clap::Command) -> Vec<String> {
        let mut out = Vec::new();
        for a in cmd.get_arguments().filter(|a| !a.is_hide_set()) {
            if let Some(l) = a.get_long() {
                out.push(format!("--{l}"));
            }
            if let Some(s) = a.get_short() {
                out.push(format!("-{s}"));
            }
        }
        out
    }

    #[test]
    fn combined_command_is_consistent() {
        command().debug_assert();
    }

    /// The generated files are what packages install, so a rebuild of the
    /// same version must produce the same bytes.
    #[test]
    fn generation_is_deterministic() {
        assert_eq!(man_page(), man_page());
        for shell in [Shell::Bash, Shell::Zsh, Shell::Fish] {
            assert_eq!(completions(shell), completions(shell));
        }
        assert!(
            man().contains(&format!(
                "\n.TH NEOSCAD 1 \"\" \"neoscad {}\" \"NeoSCAD Manual\"\n",
                env!("CARGO_PKG_VERSION")
            )),
            "no date in the title line, and the source in its own place"
        );
    }

    /// Drift check: every subcommand and every flag, top level and
    /// per subcommand, appears in the page (roff escapes '-' as '\-').
    #[test]
    fn man_page_covers_every_command_and_flag() {
        let page = man();
        assert!(page.starts_with(&Roff::new().render()));
        assert!(page.contains(".TH NEOSCAD 1"));
        assert!(page.contains(&format!("neoscad {}", env!("CARGO_PKG_VERSION"))));
        for section in ["NAME", "SYNOPSIS", "DESCRIPTION", "OPTIONS", "COMMANDS"] {
            assert!(page.contains(&format!(".SH {section}\n")), "{section}");
        }
        let esc = |s: &str| s.replace('-', "\\-");
        for f in flags(&crate::Cli::command()) {
            assert!(page.contains(&esc(&f)), "top-level {f}");
        }
        for sub in crate::SUBCOMMANDS {
            let sub_section = format!(".SS \"neoscad {}\"\n", sub.name);
            let start = page
                .find(&sub_section)
                .unwrap_or_else(|| panic!("no section for {}", sub.name));
            let rest = &page[start + sub_section.len()..];
            let end = rest
                .find(".SS ")
                .or(rest.find(".SH "))
                .unwrap_or(rest.len());
            let section = &rest[..end];
            for f in flags(&(sub.command)()) {
                assert!(section.contains(&esc(&f)), "{} {f}", sub.name);
            }
        }
        assert!(page.contains("neoscad generate man"));
        assert!(page.contains("neoscad generate completions"));
        // One title line and one apostrophe definition: the pieces were
        // joined without repeating them.
        assert_eq!(page.matches(".TH ").count(), 1);
        assert_eq!(page.matches(".ie \\n(.g .ds Aq").count(), 1);
    }

    /// Drift check for the scripts: each shell's script names every
    /// subcommand, and the top-level and per-subcommand long flags.
    #[test]
    fn completions_cover_every_command_and_flag() {
        for shell in [Shell::Bash, Shell::Zsh, Shell::Fish] {
            let script = String::from_utf8(completions(shell)).unwrap();
            assert!(!script.contains("subcmd__help"), "{shell}: no help command");
            for sub in crate::SUBCOMMANDS {
                assert!(script.contains(sub.name), "{shell}: {}", sub.name);
                for f in flags(&(sub.command)())
                    .iter()
                    .filter(|f| f.starts_with("--"))
                {
                    // fish writes `-l name` rather than `--name`.
                    let fish = format!("-l {}", &f[2..]);
                    assert!(
                        script.contains(f.as_str()) || script.contains(&fish),
                        "{shell}: {} {f}",
                        sub.name
                    );
                }
            }
            for f in flags(&crate::Cli::command())
                .iter()
                .filter(|f| f.starts_with("--"))
            {
                let fish = format!("-l {}", &f[2..]);
                assert!(
                    script.contains(f.as_str()) || script.contains(&fish),
                    "{shell}: {f}"
                );
            }
        }
        let bash = String::from_utf8(completions(Shell::Bash)).unwrap();
        assert!(bash.contains("complete -F _neoscad"));
        let zsh = String::from_utf8(completions(Shell::Zsh)).unwrap();
        assert!(zsh.starts_with("#compdef neoscad"));
    }

    /// The page's examples run as written: each parses with the parser
    /// `neoscad` would hand it to.
    #[test]
    fn examples_parse() {
        for (_, line) in EXAMPLES {
            let line = line.split(" > ").next().unwrap();
            let words: Vec<String> = line
                .split_whitespace()
                .map(|w| w.replace(['"', '\''], ""))
                .collect();
            assert_eq!(words[0], "neoscad");
            let sub = crate::SUBCOMMANDS.iter().find(|s| s.name == words[1]);
            let r = match sub {
                Some(s) => (s.command)().try_get_matches_from(&words[1..]).map(|_| ()),
                None => crate::Cli::try_parse_from(&words).map(|_| ()),
            };
            assert!(r.is_ok(), "{line}: {r:?}");
        }
    }
}
