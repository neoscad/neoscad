//! The customizer panel's logic without GTK: what each control shows, what
//! an edit makes of it, and the parameter sets on disk.
//!
//! As in the macOS app (`apple/App/Panels/CustomizerView.swift`), editing
//! a value never touches the text: the window's `client::DocumentLoop`
//! keeps the edited values and the document runs again with them as
//! `-D`-style assignments after the text. A value equal to the text's own
//! is no edit (`client::edit_parameter` says `None`), so a later change of
//! that value in the text shows through. Reset drops every edit.
//!
//! Parameter sets are OpenSCAD's JSON file beside the model
//! (`client::parameter_set_path`): choosing one applies it as `-p file -P
//! name` does, and Save writes the current values as a set OpenSCAD reads.
//! An untitled document has nowhere to keep one, so its sets are off.

use std::path::{Path, PathBuf};

use client::{
    Client, Parameter, ParameterControl, ParameterEdit, ParameterGroup, ParameterOption,
    ParameterOverride, ParameterValue,
};

/// The value a control shows: the edited one, else the text's.
pub fn value_of(p: &Parameter, overrides: &[ParameterOverride]) -> ParameterValue {
    overrides
        .iter()
        .find(|o| o.name == p.name)
        .map_or_else(|| p.default_value.clone(), |o| o.value.clone())
}

/// Whether `name` has an edited value.
pub fn is_edited(name: &str, overrides: &[ParameterOverride]) -> bool {
    overrides.iter().any(|o| o.name == name)
}

/// The parameter `name` of `groups` (the "Global" group's parameters are
/// in every group, so the first match is as good as any).
pub fn find<'g>(groups: &'g [ParameterGroup], name: &str) -> Option<&'g Parameter> {
    groups
        .iter()
        .flat_map(|g| &g.parameters)
        .find(|p| p.name == name)
}

/// What a control's `edit` of `name` asks the loop to set: `Some(None)`
/// drops the edit (back to the text's value), `Some(Some(v))` sets `v`;
/// `None` when the text no longer has that parameter (a control from a
/// panel built for an older text).
pub fn apply_edit(
    groups: &[ParameterGroup],
    overrides: &[ParameterOverride],
    name: &str,
    edit: ParameterEdit,
) -> Option<Option<ParameterValue>> {
    let p = find(groups, name)?;
    let current = value_of(p, overrides);
    Some(client::edit_parameter(p, &current, edit))
}

/// A number as the customizer's fields write it (`%g`, as OpenSCAD's).
pub fn number_text(x: f64) -> String {
    client::format_number(x)
}

/// A typed number: finite, surrounding spaces ignored. A half-typed `1.`
/// or `-` is `None`, so the field keeps its value rather than running the
/// model with a guess.
pub fn parse_number(s: &str) -> Option<f64> {
    s.trim().parse::<f64>().ok().filter(|x| x.is_finite())
}

/// How many decimals a field with `step` shows: enough for the step (0.1
/// shows one, 0.05 two), none for whole steps and none without a step but
/// for a value that needs them (`value`'s own, up to six).
pub fn digits_for(step: Option<f64>, value: f64) -> u32 {
    let of = |x: f64| -> u32 {
        let s = number_text(x.abs());
        match s.split_once('.') {
            Some((_, frac)) if !s.contains('e') => frac.len().min(6) as u32,
            _ => 0,
        }
    };
    match step.filter(|s| *s > 0.0 && s.is_finite()) {
        Some(s) => of(s),
        None => of(value),
    }
}

/// A slider or spin button's adjustment: lower and upper bounds and the
/// step. A spin box without bounds still needs some; ±1e9 is beyond any
/// model's millimetres. A missing step is 1 for whole numbers and the
/// value's last decimal otherwise, as OpenSCAD's spin boxes step.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Range {
    pub lower: f64,
    pub upper: f64,
    pub step: f64,
    pub digits: u32,
}

/// The adjustment of a number control (a slider or a spin box; `None` for
/// any other control).
pub fn range(control: &ParameterControl, value: f64) -> Option<Range> {
    const WIDE: f64 = 1e9;
    let (lo, hi, step) = match control {
        ParameterControl::Slider { min, max, step } => (*min, max.max(*min), *step),
        ParameterControl::SpinBox { min, max, step }
        | ParameterControl::Vector { min, max, step } => {
            (min.unwrap_or(-WIDE), max.unwrap_or(WIDE), *step)
        }
        _ => return None,
    };
    let digits = digits_for(step, value);
    let step = step
        .filter(|s| *s > 0.0 && s.is_finite())
        .unwrap_or_else(|| 10f64.powi(-(digits as i32)));
    Some(Range {
        lower: lo.min(value),
        upper: hi.max(value),
        step,
        digits,
    })
}

/// The dropdown entry showing `value`, if any is that value.
pub fn option_index(options: &[ParameterOption], value: &ParameterValue) -> Option<u32> {
    options
        .iter()
        .position(|o| &o.value == value)
        .and_then(|i| u32::try_from(i).ok())
}

/// The parameter sets file of the document saved at `file`.
pub fn sets_file(file: &Path) -> PathBuf {
    PathBuf::from(client::parameter_set_path(&file.to_string_lossy()))
}

/// The set names in the JSON file beside `file`, in the file's order; none
/// for an untitled document or without a file. A file the core cannot read
/// is an error to show, not an empty list (an empty list would let Save
/// replace it).
pub fn set_names(client: &Client, file: Option<&Path>) -> Result<Vec<String>, String> {
    let Some(file) = file else {
        return Ok(Vec::new());
    };
    client
        .parameter_sets(&sets_file(file).to_string_lossy())
        .map_err(|e| e.to_string())
}

/// The values the set `name` gives every parameter (checked and clamped as
/// OpenSCAD's `-P` does), for `DocumentLoop::parameter_set_applied`.
pub fn apply_set(
    client: &Client,
    core_path: &str,
    file: &Path,
    name: &str,
) -> Result<Vec<ParameterOverride>, String> {
    client
        .apply_parameter_set(core_path, &sets_file(file).to_string_lossy(), name)
        .map_err(|e| e.to_string())
}

/// Save `values` (the edited ones; the rest stay the text's) as the set
/// `name` in the file beside `file`, keeping its other sets. The file is
/// written whole through a temporary file, so a failed save leaves the
/// old one.
pub fn save_set(
    client: &Client,
    core_path: &str,
    file: &Path,
    name: &str,
    values: &[ParameterOverride],
) -> Result<PathBuf, String> {
    let json = sets_file(file);
    let text = client
        .parameter_set_file(
            core_path,
            &json.to_string_lossy(),
            name,
            values,
            json.is_file(),
        )
        .map_err(|e| e.to_string())?;
    crate::run::write_atomic(&json, text.as_bytes())
        .map_err(|e| format!("Could not write {}: {e}", json.display()))?;
    Ok(json)
}

/// The name Save offers: the set shown, else the next "Set N".
pub fn suggested_set_name(selected: Option<&str>, existing: &[String]) -> String {
    if let Some(s) = selected {
        return s.to_string();
    }
    (existing.len() + 1..)
        .map(|n| format!("Set {n}"))
        .find(|n| !existing.contains(n))
        .unwrap_or_else(|| "Set".into())
}

/// The log line for an edit, which linux/smoke.sh waits for.
pub fn describe_edit(name: &str, value: Option<&ParameterValue>) -> String {
    match value {
        None => format!("customizer: {name} back to the text's value"),
        Some(v) => format!(
            "customizer: {name} = {}",
            client::literal(v).unwrap_or_else(|| "?".into())
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn client() -> Client {
        let mut cfg = crate::host::config();
        cfg.gpu = None;
        cfg.limits = session::Limits::AGENT;
        Client::new(cfg)
    }

    const MODEL: &str = "/* [Size] */\n\
        // Whether the box is hollow\n\
        hollow = false;\n\
        width = 20; // [10:5:50]\n\
        height = 3;\n\
        label = \"box\"; // 8\n\
        shape = \"cube\"; // [cube, sphere]\n\
        offset = [1, 2, 3];\n\
        module m() {}\n\
        cube(width);\n";

    fn groups(c: &Client, path: &str) -> Vec<ParameterGroup> {
        c.open(path, Some(MODEL.into())).unwrap();
        c.parameters(path).unwrap()
    }

    #[test]
    fn each_parameter_gets_openscads_control() {
        let c = client();
        let g = groups(&c, "/nonexistent-neoscad-linux-app/customizer.scad");
        assert_eq!(g.len(), 1);
        assert_eq!(g[0].name, "Size");
        let control = |n: &str| find(&g, n).unwrap().control.clone();
        assert_eq!(control("hollow"), ParameterControl::Checkbox);
        assert!(matches!(
            control("width"),
            ParameterControl::Slider {
                min: 10.0,
                max: 50.0,
                step: Some(5.0)
            }
        ));
        assert!(matches!(
            control("height"),
            ParameterControl::SpinBox { .. }
        ));
        assert!(matches!(
            control("label"),
            ParameterControl::Text {
                max_length: Some(8)
            }
        ));
        assert!(matches!(
            control("shape"),
            ParameterControl::Dropdown { .. }
        ));
        assert!(matches!(control("offset"), ParameterControl::Vector { .. }));
        assert_eq!(
            find(&g, "hollow").unwrap().description,
            "Whether the box is hollow"
        );
    }

    #[test]
    fn edits_snap_clamp_and_drop_back_to_the_text() {
        let c = client();
        let g = groups(&c, "/nonexistent-neoscad-linux-app/edits.scad");
        let none: Vec<ParameterOverride> = Vec::new();
        // A slider lands on its grid from the minimum.
        assert_eq!(
            apply_edit(&g, &none, "width", ParameterEdit::Slide { value: 23.0 }),
            Some(Some(ParameterValue::Number { value: 25.0 }))
        );
        // The text's own value is no edit.
        assert_eq!(
            apply_edit(&g, &none, "width", ParameterEdit::Slide { value: 21.0 }),
            Some(None)
        );
        // A spin box steps by one without a step.
        assert_eq!(
            apply_edit(&g, &none, "height", ParameterEdit::Step { up: true }),
            Some(Some(ParameterValue::Number { value: 4.0 }))
        );
        // Text is cut to the control's length.
        assert_eq!(
            apply_edit(
                &g,
                &none,
                "label",
                ParameterEdit::Set {
                    value: ParameterValue::Text {
                        value: "a long label".into()
                    }
                }
            ),
            Some(Some(ParameterValue::Text {
                value: "a long l".into()
            }))
        );
        // One element of a vector, from the edited value.
        let edited = vec![ParameterOverride {
            name: "offset".into(),
            value: ParameterValue::Vector {
                value: vec![1.0, 5.0, 3.0],
            },
        }];
        assert_eq!(
            apply_edit(
                &g,
                &edited,
                "offset",
                ParameterEdit::Item {
                    index: 2,
                    value: 9.0
                }
            ),
            Some(Some(ParameterValue::Vector {
                value: vec![1.0, 5.0, 9.0]
            }))
        );
        assert_eq!(
            value_of(find(&g, "offset").unwrap(), &edited),
            ParameterValue::Vector {
                value: vec![1.0, 5.0, 3.0]
            }
        );
        assert!(is_edited("offset", &edited) && !is_edited("width", &edited));
        // A parameter the text no longer has.
        assert_eq!(
            apply_edit(&g, &none, "gone", ParameterEdit::Step { up: true }),
            None
        );
    }

    #[test]
    fn fields_ranges_and_dropdowns() {
        assert_eq!(parse_number(" 2.5 "), Some(2.5));
        assert_eq!(parse_number("1."), Some(1.0));
        assert_eq!(parse_number("-"), None);
        assert_eq!(parse_number("inf"), None);
        assert_eq!(number_text(0.1 + 0.2), "0.3");
        assert_eq!(digits_for(Some(0.05), 1.0), 2);
        assert_eq!(digits_for(Some(5.0), 1.5), 0);
        assert_eq!(digits_for(None, 2.25), 2);
        let r = range(
            &ParameterControl::Slider {
                min: 10.0,
                max: 50.0,
                step: Some(5.0),
            },
            20.0,
        )
        .unwrap();
        assert_eq!(
            r,
            Range {
                lower: 10.0,
                upper: 50.0,
                step: 5.0,
                digits: 0
            }
        );
        // An unbounded spin box, and a value outside its bounds widens
        // them rather than being clamped by the widget.
        let r = range(
            &ParameterControl::SpinBox {
                min: Some(0.0),
                max: None,
                step: None,
            },
            -2.5,
        )
        .unwrap();
        assert_eq!((r.lower, r.upper, r.step, r.digits), (-2.5, 1e9, 0.1, 1));
        assert_eq!(range(&ParameterControl::Checkbox, 0.0), None);
        let options = vec![
            ParameterOption {
                label: "Cube".into(),
                value: ParameterValue::Text {
                    value: "cube".into(),
                },
            },
            ParameterOption {
                label: "Sphere".into(),
                value: ParameterValue::Text {
                    value: "sphere".into(),
                },
            },
        ];
        assert_eq!(
            option_index(
                &options,
                &ParameterValue::Text {
                    value: "sphere".into()
                }
            ),
            Some(1)
        );
        assert_eq!(
            option_index(&options, &ParameterValue::Number { value: 1.0 }),
            None
        );
    }

    #[test]
    fn sets_are_saved_beside_the_model_and_applied() {
        let c = client();
        let dir =
            std::env::temp_dir().join(format!("neoscad-linux-app-sets-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("box.scad");
        std::fs::write(&file, MODEL).unwrap();
        let path = file.to_string_lossy().into_owned();
        c.open(&path, Some(MODEL.into())).unwrap();
        assert_eq!(set_names(&c, Some(&file)).unwrap(), Vec::<String>::new());
        assert_eq!(set_names(&c, None).unwrap(), Vec::<String>::new());
        assert_eq!(suggested_set_name(None, &[]), "Set 1");

        let values = vec![ParameterOverride {
            name: "width".into(),
            value: ParameterValue::Number { value: 40.0 },
        }];
        let json = save_set(&c, &path, &file, "big", &values).unwrap();
        assert_eq!(json, dir.join("box.json"));
        save_set(&c, &path, &file, "default", &[]).unwrap();
        assert_eq!(set_names(&c, Some(&file)).unwrap(), ["big", "default"]);
        assert_eq!(
            suggested_set_name(None, &["Set 1".into(), "x".into()]),
            "Set 3"
        );
        assert_eq!(suggested_set_name(Some("big"), &[]), "big");

        let applied = apply_set(&c, &path, &file, "big").unwrap();
        let width = applied.iter().find(|o| o.name == "width").unwrap();
        assert_eq!(width.value, ParameterValue::Number { value: 40.0 });
        assert!(apply_set(&c, &path, &file, "missing").is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn edits_are_logged_as_openscad_literals() {
        assert_eq!(
            describe_edit("hollow", Some(&ParameterValue::Bool { value: true })),
            "customizer: hollow = true"
        );
        assert_eq!(
            describe_edit("w", None),
            "customizer: w back to the text's value"
        );
    }
}
