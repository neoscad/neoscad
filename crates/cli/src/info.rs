//! `--info` and `--help-export`: what the build is and which `-O` settings
//! exist, in the layout of OpenSCAD's `LibraryInfo::info()` and
//! `help_export()` (`openscad.cc`), with neoscad's own facts. `--info`
//! names the Rust libraries that stand in for OpenSCAD's C++ ones rather
//! than borrowing OpenSCAD's version numbers, and has no OpenGL section:
//! neoscad renders no images yet.

use crate::host::{self, FontSource, Host};

/// The `-O` settings, by section, as `help_export` lists them: name, type
/// and the values with the default in angle brackets. Every one of them is
/// implemented.
/// One `-O` setting: name, type, values.
type Setting = (&'static str, &'static str, &'static str);

const EXPORT_SETTINGS: &[(&str, &[Setting])] = &[
    (
        "export-pdf",
        &[
            ("paper-size", "enum", "[a6,a5,<a4>,a3,letter,legal,tabloid]"),
            ("orientation", "enum", "[<portrait>,landscape,auto]"),
            ("show-filename", "bool", "true/<false>"),
            ("show-scale", "bool", "<true>/false"),
            ("show-scale-message", "bool", "<true>/false"),
            ("show-grid", "bool", "true/<false>"),
            ("grid-size", "double", "1.000000 : <10.000000> : 100.000000"),
            ("add-meta-data", "bool", "<true>/false"),
            ("meta-data-title", "string", "\"\""),
            ("meta-data-author", "string", "\"\""),
            ("meta-data-subject", "string", "\"\""),
            ("meta-data-keywords", "string", "\"\""),
            ("fill", "bool", "true/<false>"),
            ("fill-color", "string", "\"black\""),
            ("stroke", "bool", "<true>/false"),
            ("stroke-color", "string", "\"black\""),
            (
                "stroke-width",
                "double",
                "0.000000 : <0.350000> : 999.000000",
            ),
        ],
    ),
    (
        "export-3mf",
        &[
            ("color-mode", "enum", "[<model>,none,selected-only]"),
            (
                "unit",
                "enum",
                "[micron,<millimeter>,centimeter,meter,inch,foot]",
            ),
            ("color", "string", "\"#f9d72c\""),
            ("material-type", "enum", "[color,<basematerial>]"),
            ("decimal-precision", "int", "1 : <6> : 16"),
            ("add-meta-data", "bool", "<true>/false"),
            ("meta-data-title", "string", "\"\""),
            ("meta-data-designer", "string", "\"\""),
            ("meta-data-description", "string", "\"\""),
            ("meta-data-copyright", "string", "\"\""),
            ("meta-data-license-terms", "string", "\"\""),
            ("meta-data-rating", "string", "\"\""),
        ],
    ),
    (
        "export-svg",
        &[
            ("fill", "bool", "true/<false>"),
            ("fill-color", "string", "\"white\""),
            ("stroke", "bool", "<true>/false"),
            ("stroke-color", "string", "\"black\""),
            (
                "stroke-width",
                "double",
                "0.000000 : <0.350000> : 999.000000",
            ),
        ],
    ),
];

/// `--help-export`, which OpenSCAD prints to stderr.
pub fn help_export() -> String {
    let mut s = format!(
        "neoscad version {}\n\nList of settings that can be given using the -O option using the\nformat '<section>/<key>=value', e.g.:\nneoscad -O export-pdf/paper-size=a6 -O export-pdf/show-grid=false\n\n",
        env!("CARGO_PKG_VERSION")
    );
    for (section, entries) in EXPORT_SETTINGS {
        s.push_str(&format!("Section '{section}':\n"));
        for (name, kind, values) in *entries {
            s.push_str(&format!("  - {name} ({kind}): {values}\n"));
        }
    }
    s
}

/// `sysctl -n` of one key, for the system line (macOS only).
fn sysctl(key: &str) -> Option<String> {
    let out = std::process::Command::new("sysctl")
        .args(["-n", key])
        .output()
        .ok()?;
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (out.status.success() && !s.is_empty()).then_some(s)
}

/// `PlatformUtils::sysinfo()`: OS and version, architecture, machine,
/// CPUs and memory, as far as they can be found out; and the user agent's
/// shorter form (`PlatformUtils::user_agent()`, without CPUs and memory).
fn sysinfo() -> (String, String) {
    let arch = match std::env::consts::ARCH {
        "aarch64" => "arm64",
        a => a,
    };
    let cpus = std::thread::available_parallelism().map_or(1, |n| n.get());
    let mut parts = Vec::new();
    if cfg!(target_os = "macos") {
        parts.push(format!(
            "macOS {}",
            sysctl("kern.osproductversion").unwrap_or_default()
        ));
        parts.push(arch.to_string());
        if let Some(m) = sysctl("hw.model") {
            parts.push(m);
        }
        let agent = parts.join(" ");
        parts.push(format!("{cpus} CPUs"));
        if let Some(bytes) = sysctl("hw.memsize").and_then(|m| m.parse::<f64>().ok()) {
            parts.push(format!("{:.2} GB RAM", bytes / f64::from(1u32 << 30)));
        }
        (parts.join(" "), agent)
    } else {
        let agent = format!("{} {arch}", std::env::consts::OS);
        (format!("{agent} {cpus} CPUs"), agent)
    }
}

/// `--info`, on stdout.
pub fn info() -> String {
    let version = env!("CARGO_PKG_VERSION");
    let (system, agent) = sysinfo();
    let mut s = format!("NeoSCAD Version: {version}\nSystem information: {system}\n");
    s.push_str(&format!("User Agent: NeoSCAD/{version} ({agent})\n"));
    s.push_str(&format!("Compiler: {}\n", env!("NEOSCAD_RUSTC_VERSION")));
    s.push_str(&format!(
        "Debug build: {}\n",
        if cfg!(debug_assertions) { "Yes" } else { "No" }
    ));
    for entry in env!("NEOSCAD_LIB_VERSIONS").split(';') {
        if let Some((name, v)) = entry.split_once('=') {
            s.push_str(&format!("{name} version: {v}\n"));
        }
    }
    // OpenSCAD lists the experimental features it has; neoscad implements
    // none of them (see `--enable`).
    s.push_str("Features: none\n");
    let exe = std::env::current_exe().ok();
    let app = exe
        .as_deref()
        .and_then(std::path::Path::parent)
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    s.push_str(&format!("Application Path: {app}\n"));
    s.push_str(&format!(
        "Resource Path: {}\n",
        host::resource_path().display()
    ));
    let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
    if let Some(h) = &home {
        let user = if cfg!(target_os = "macos") {
            h.join("Documents/OpenSCAD/libraries")
        } else {
            h.join(".local/share/OpenSCAD/libraries")
        };
        s.push_str(&format!("User Library Path: {}\n", user.display()));
    }
    let env = |k: &str| {
        std::env::var(k)
            .ok()
            .unwrap_or_else(|| "<not set>".to_string())
    };
    let host = Host::from_env();
    s.push_str(&format!("OPENSCADPATH: {}\n", env("OPENSCADPATH")));
    s.push_str("OpenSCAD library path:\n");
    for p in &host.libs.0 {
        s.push_str(&format!("  {}\n", p.display()));
    }
    s.push_str(&format!(
        "\n{}: {}\n",
        host::FONT_DIR_ENV,
        env(host::FONT_DIR_ENV)
    ));
    s.push_str(&format!(
        "OPENSCAD_FONT_PATH: {}\n",
        env("OPENSCAD_FONT_PATH")
    ));
    s.push_str("OpenSCAD font path:\n");
    for f in host.font_sources() {
        match f {
            FontSource::Bundled => {
                s.push_str("  <built-in Liberation fonts>\n");
            }
            FontSource::Dir(d) => s.push_str(&format!("  {}\n", d.display())),
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn help_export_lists_the_nightlys_settings() {
        let text = help_export();
        // The nightly's `--help-export`, from the section list on, verbatim.
        let nightly_tail = "Section 'export-3mf':\n  - color-mode (enum): [<model>,none,selected-only]\n  - unit (enum): [micron,<millimeter>,centimeter,meter,inch,foot]\n  - color (string): \"#f9d72c\"\n  - material-type (enum): [color,<basematerial>]\n  - decimal-precision (int): 1 : <6> : 16\n  - add-meta-data (bool): <true>/false\n  - meta-data-title (string): \"\"\n  - meta-data-designer (string): \"\"\n  - meta-data-description (string): \"\"\n  - meta-data-copyright (string): \"\"\n  - meta-data-license-terms (string): \"\"\n  - meta-data-rating (string): \"\"\nSection 'export-svg':\n  - fill (bool): true/<false>\n  - fill-color (string): \"white\"\n  - stroke (bool): <true>/false\n  - stroke-color (string): \"black\"\n  - stroke-width (double): 0.000000 : <0.350000> : 999.000000\n";
        assert!(text.ends_with(nightly_tail));
        assert!(text.contains(
            "\nSection 'export-pdf':\n  - paper-size (enum): [a6,a5,<a4>,a3,letter,legal,tabloid]\n"
        ));
    }

    #[test]
    fn info_names_the_linked_libraries() {
        let text = info();
        assert!(text.starts_with("NeoSCAD Version: "));
        assert!(text.contains("\nmanifold-rust version: "));
        assert!(text.contains("\nclipper2-rust version: "));
        assert!(text.contains("\nOpenSCAD library path:\n"));
    }
}
