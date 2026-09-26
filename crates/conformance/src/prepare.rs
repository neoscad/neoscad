//! Create the test inputs OpenSCAD's CMake configure/build step generates.
//!
//! Some tier 0-2 inputs do not exist in a fresh checkout:
//! `misc/include-tests.scad`, `misc/use-tests.scad` and the `import_*-tests`
//! files are `configure_file` outputs (tests/CMakeLists.txt:164-173), and
//! `issue2342.scad` is written by a Python generator at build time
//! (`:183-187`). OpenSCAD's own `.gitignore` covers all of them, so writing
//! them into the reference checkout mirrors what an OpenSCAD build does and
//! leaves its `git status` clean.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::fs;
use std::path::Path;

use regex::Regex;

use crate::ctx::Ctx;
use crate::manifest::Manifest;

pub fn prepare(ctx: &Ctx, manifest: &Manifest) -> Result<(), String> {
    let r = ctx.ref_str();
    let vars: HashMap<&str, String> = [
        ("CMAKE_SOURCE_DIR", r.clone()),
        ("CMAKE_CURRENT_SOURCE_DIR", format!("{r}/tests")),
        ("CMAKE_BINARY_DIR", format!("{r}/build")),
        ("CMAKE_CURRENT_BINARY_DIR", format!("{r}/build/tests")),
    ]
    .into_iter()
    .collect();
    fs::create_dir_all(ctx.work_dir()).map_err(|e| e.to_string())?;

    for g in &manifest.generated_files {
        let out = ctx.ref_root.join(&g.output);
        let content = match (&g.template, g.generator.as_deref()) {
            (Some(tpl), _) => {
                let text = fs::read_to_string(ctx.ref_root.join(tpl))
                    .map_err(|e| format!("{tpl}: {e}"))?;
                if g.copy_only {
                    text
                } else {
                    configure(&text, &vars)
                }
            }
            (None, Some("gen_issue2342")) => gen_issue2342(),
            (None, other) => return Err(format!("{}: unknown generator {other:?}", g.output)),
        };
        write_if_changed(&out, &content)?;
    }
    Ok(())
}

/// `configure_file` substitution: `@VAR@` and `${VAR}` are replaced by the
/// variable's value, or by nothing if it is undefined, as CMake does.
fn configure(text: &str, vars: &HashMap<&str, String>) -> String {
    let re = Regex::new(r"@([A-Za-z0-9_]+)@|\$\{([A-Za-z0-9_]+)\}").expect("valid regex");
    re.replace_all(text, |c: &regex::Captures| {
        let name = c.get(1).or_else(|| c.get(2)).map_or("", |m| m.as_str());
        vars.get(name).cloned().unwrap_or_default()
    })
    .into_owned()
}

/// Port of `tests/data/python/gen_issue2342-template.py`: a 3 MB file of
/// translated cubes that stress-tests the parser.
fn gen_issue2342() -> String {
    let (xcount, ycount, zcount) = (100, 100, 10);
    let total = xcount * ycount * zcount;
    let mut s = String::with_capacity(3_300_000);
    for x in 1..xcount {
        for y in 1..ycount {
            for z in 1..zcount {
                let _ = writeln!(s, "translate([{x}, {y}, {z}])");
                s.push_str("  cube(0.5);\n");
            }
        }
    }
    let _ = writeln!(s, "echo(\"{total} elements processed\");");
    s
}

fn write_if_changed(path: &Path, content: &str) -> Result<(), String> {
    if fs::read_to_string(path).is_ok_and(|old| old == content) {
        return Ok(());
    }
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    fs::write(path, content).map_err(|e| format!("{}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn configure_substitutes_and_blanks_unknowns() {
        let vars: HashMap<&str, String> = [("A", "x".to_string())].into_iter().collect();
        assert_eq!(configure("@A@/${A}/@B@.", &vars), "x/x/.");
    }

    #[test]
    fn issue2342_matches_generator_shape() {
        let s = gen_issue2342();
        assert!(s.starts_with("translate([1, 1, 1])\n  cube(0.5);\n"));
        assert!(s.ends_with("echo(\"100000 elements processed\");\n"));
        assert_eq!(s.lines().count(), 99 * 99 * 9 * 2 + 1);
    }
}
