//! The presentation tables, the customizer's edit rules and the UTF-16
//! edits (`present.rs`, `text.rs`): what every host shows and sends, so a
//! change here changes the macOS, web, Linux and Windows apps at once.

use super::*;

const DOC: &str = "/doc/main.scad";

fn client() -> Client {
    use lang::loader::{FileSystem, LibraryPath};
    let files = std::sync::Arc::new(lang::vfs::MemFs::new());
    files.insert("/work/Untitled.scad", b"cube(1);".to_vec());
    let fs: std::sync::Arc<dyn FileSystem + Send + Sync> = files;
    Client::new(session::Config::new(fs, LibraryPath(Vec::new())))
}

fn result(exit_code: u8, geometry: Option<GeometryStats>) -> RenderResult {
    RenderResult {
        exit_code,
        diagnostics: vec![],
        echo: vec![],
        console: String::new(),
        geometry,
        cache_entries: 0,
        timings: Timings {
            parse_ms: 1.25,
            evaluate_ms: 2.0,
            geometry_ms: 3.0,
            total_ms: 6.25,
        },
    }
}

fn cube_stats() -> GeometryStats {
    GeometryStats {
        dimensions: 3,
        bbox_min: vec![0.0; 3],
        bbox_max: vec![10.0; 3],
        area: 600.0,
        volume: Some(1000.0),
        triangles: Some(12),
        vertices: Some(8),
        manifold: Some(true),
        components: Some(1),
        contours: None,
    }
}

#[test]
fn a_render_is_summed_up_in_one_line() {
    assert_eq!(
        describe_render(&result(0, Some(cube_stats())), RenderMode::Render),
        "Rendered in 6.2 ms: 3D, bbox 10 × 10 × 10, volume 1000, area 600, 12 triangles, \
         1 component, manifold"
    );
    assert_eq!(
        describe_render(&result(0, None), RenderMode::Render),
        "Rendered in 6.2 ms: empty result."
    );
    assert_eq!(
        describe_render(&result(1, None), RenderMode::Render),
        "Render failed (6.2 ms)."
    );
    assert_eq!(
        describe_render(&result(0, None), RenderMode::Preview),
        "Previewed in 6.2 ms."
    );
    assert_eq!(
        describe_render(&result(1, None), RenderMode::Preview),
        "Preview failed (6.2 ms)."
    );
    let mut two = cube_stats();
    two.components = Some(2);
    two.manifold = Some(false);
    assert!(
        describe_render(&result(0, Some(two)), RenderMode::Force)
            .ends_with("2 components, not manifold")
    );
    assert_eq!(
        describe_timings(&result(0, None).timings),
        "Parse 1.2 ms, evaluate 2.0 ms, geometry 3.0 ms; total 6.2 ms"
    );
}

#[test]
fn every_console_kind_has_exactly_one_filter() {
    let groups = console_groups();
    let ids: Vec<&str> = groups.iter().map(|g| g.id.as_str()).collect();
    assert_eq!(ids, ["errors", "warnings", "echo", "other"]);
    let all: Vec<ConsoleKind> = groups.iter().flat_map(|g| g.kinds.clone()).collect();
    assert_eq!(all.len(), 6);
    assert_eq!(console_group(ConsoleKind::Trace), ConsoleGroup::Errors);
    assert_eq!(
        console_group(ConsoleKind::Deprecated),
        ConsoleGroup::Warnings
    );
    assert_eq!(console_group(ConsoleKind::Info), ConsoleGroup::Other);
}

#[test]
fn export_formats_agree_with_the_session() {
    let f = export_formats();
    assert_eq!(f.len(), 10);
    assert_eq!(f[0].id, "binstl");
    assert_eq!(f[0].extension, "stl");
    for x in f.iter().filter(|x| x.kind == ExportKind::Geometry) {
        assert!(
            export_format(Some(&x.id), "x").is_ok(),
            "{} is a core format",
            x.id
        );
    }
    assert_eq!(export_format_info("svg").unwrap().dimension, Some(2));
    assert_eq!(export_format_info("snapshot").unwrap().dimension, None);
    assert_eq!(suggest_export_format("binstl", Some(2)), "svg");
    assert_eq!(suggest_export_format("svg", Some(3)), "binstl");
    assert_eq!(suggest_export_format("3mf", Some(3)), "3mf");
    assert_eq!(suggest_export_format("view-image", Some(2)), "view-image");
    assert_eq!(suggest_export_format("obj", None), "obj");
    assert_eq!(suggest_export_format("gone", None), "binstl");
}

#[test]
fn a_failed_export_says_why() {
    let mut r = ExportResult {
        exit_code: 0,
        written: true,
        fillet_errors: vec![],
        format: "stl".into(),
        bytes: 3,
        geometry: None,
        diagnostics: vec![],
        console: String::new(),
        timings: result(0, None).timings,
        step: None,
    };
    assert_eq!(export_failure_reason(&r), None);
    r.exit_code = 1;
    assert_eq!(
        export_failure_reason(&r).unwrap(),
        "The export failed (exit code 1)."
    );
    r.console = "WARNING: w\nCurrent top level object is not a 3D object.\n".into();
    assert_eq!(
        export_failure_reason(&r).unwrap(),
        "Current top level object is not a 3D object."
    );
    r.console = "WARNING: w\nnote\nERROR: a\nERROR: b\n".into();
    assert_eq!(export_failure_reason(&r).unwrap(), "ERROR: a\nERROR: b");
    // Written with a failed fillet: a failure that says the file is
    // there and its edges are sharp, never "was not exported".
    r.fillet_errors = vec!["fillet_edges: r = 3 is too large".into()];
    assert_eq!(
        export_failure_reason(&r).unwrap(),
        "The file was written, but 1 fillet_edges() call failed and its edges are sharp: \
         fillet_edges: r = 3 is too large"
    );
    r.fillet_errors.push("chamfer_edges(): bad d".into());
    assert_eq!(
        export_failure_reason(&r).unwrap(),
        "The file was written, but 2 fillet_edges()/chamfer_edges() errors left their calls' \
         edges sharp:\n- fillet_edges: r = 3 is too large\n- chamfer_edges(): bad d"
    );
    // Not written (an unwritable folder as well): the console's reason.
    r.written = false;
    assert_eq!(export_failure_reason(&r).unwrap(), "ERROR: a\nERROR: b");
}

#[test]
fn printer_presets_and_settings() {
    let p = printer_presets();
    assert_eq!(p.len(), 6);
    assert!(p.iter().all(|p| p.nozzle == 0.4 && p.bed.len() == 3));
    let d = PrinterSettings::default();
    assert_eq!((d.nozzle, d.min_wall, d.max_overhang), (0.4, 0.8, 45.0));
    assert!(!d.use_bed);
    assert_eq!(d.check_options().bed, None);
    let s = d.clone().apply_preset("prusa-mk4");
    assert_eq!(s.preset, "prusa-mk4");
    assert_eq!(s.min_wall, 0.8);
    assert!(s.use_bed);
    assert_eq!(s.check_options().bed, Some(vec![250.0, 210.0, 220.0]));
    assert_eq!(d.clone().apply_preset("nope"), d);
    let bad = PrinterSettings {
        preset: "custom".into(),
        nozzle: -1.0,
        min_wall: f64::NAN,
        max_overhang: 120.0,
        use_bed: true,
        bed: vec![1.0, 2.0],
    }
    .validated();
    assert_eq!(
        (bad.nozzle, bad.min_wall, bad.max_overhang, bad.bed.len()),
        (0.4, 0.8, 45.0, 3)
    );
    assert!(bad.use_bed);
}

#[test]
fn the_check_summary_line() {
    let mut r = CheckReport {
        exit_code: 0,
        failed: false,
        errors: 1,
        warnings: 2,
        info: 0,
        findings: vec![],
        truncated: vec![TruncatedFindings {
            code: "thin-wall".into(),
            count: 3,
        }],
        min_wall: Some(0.35),
        parts: vec![],
        text: String::new(),
        summary_json: String::new(),
        diagnostics: vec![],
        console: String::new(),
    };
    assert_eq!(
        check_summary(&r),
        "1 error, 2 warnings, 0 info · thinnest wall 0.35 mm · 3 more not listed"
    );
    r.failed = true;
    assert_eq!(check_summary(&r), "The model did not render.");
    r.console = "WARNING: x\nERROR: Parser error in file main.scad, line 1: syntax error\n".into();
    assert_eq!(
        check_summary(&r),
        "The model did not render: ERROR: Parser error in file main.scad, line 1: syntax error"
    );
}

fn number(control: ParameterControl, default: f64) -> Parameter {
    Parameter {
        name: "x".into(),
        description: String::new(),
        control,
        default_value: ParameterValue::Number { value: default },
    }
}

fn n(value: f64) -> Option<ParameterValue> {
    Some(ParameterValue::Number { value })
}

#[test]
fn customizer_edits_snap_clamp_and_drop_defaults() {
    let slider = number(
        ParameterControl::Slider {
            min: 0.0,
            max: 1.0,
            step: Some(0.1),
        },
        0.5,
    );
    let cur = ParameterValue::Number { value: 0.5 };
    assert_eq!(
        edit_parameter(&slider, &cur, ParameterEdit::Slide { value: 0.2999 }),
        n(0.3)
    );
    assert_eq!(
        edit_parameter(&slider, &cur, ParameterEdit::Slide { value: 0.52 }),
        None
    );
    // A slider's field takes what is typed, as OpenSCAD's does.
    assert_eq!(
        edit_parameter(&slider, &cur, ParameterEdit::Type { value: 7.0 }),
        n(7.0)
    );
    assert_eq!(snap_to_step(13.0, Some(5.0), 1.0), 11.0);
    assert_eq!(snap_to_step(0.123, None, 0.0), 0.123);

    let spin = number(
        ParameterControl::SpinBox {
            min: Some(0.0),
            max: Some(10.0),
            step: None,
        },
        1.0,
    );
    let cur = ParameterValue::Number { value: 10.0 };
    assert_eq!(
        edit_parameter(&spin, &cur, ParameterEdit::Step { up: true }),
        n(10.0)
    );
    assert_eq!(
        edit_parameter(&spin, &cur, ParameterEdit::Step { up: false }),
        n(9.0)
    );
    assert_eq!(
        edit_parameter(&spin, &cur, ParameterEdit::Type { value: -4.0 }),
        n(0.0)
    );

    let vector = Parameter {
        name: "v".into(),
        description: String::new(),
        control: ParameterControl::Vector {
            min: None,
            max: Some(5.0),
            step: None,
        },
        default_value: ParameterValue::Vector {
            value: vec![1.0, 2.0],
        },
    };
    let cur = vector.default_value.clone();
    assert_eq!(
        edit_parameter(
            &vector,
            &cur,
            ParameterEdit::Item {
                index: 1,
                value: 9.0
            }
        ),
        Some(ParameterValue::Vector {
            value: vec![1.0, 5.0]
        })
    );

    let text = Parameter {
        name: "t".into(),
        description: String::new(),
        control: ParameterControl::Text {
            max_length: Some(4),
        },
        default_value: ParameterValue::Text { value: "a".into() },
    };
    // Cut on a character boundary: "é" is two bytes.
    assert_eq!(
        edit_parameter(
            &text,
            &text.default_value,
            ParameterEdit::Set {
                value: ParameterValue::Text {
                    value: "abcé".into()
                }
            }
        ),
        Some(ParameterValue::Text {
            value: "abc".into()
        })
    );
    assert_eq!(format_number(0.1 + 0.2), "0.3");
    assert_eq!(format_number(1e-5), "1e-05");
    assert_eq!(format_number(123456789.0), "1.23457e+08");
}

#[test]
fn paths_a_document_window_uses() {
    assert_eq!(parameter_set_path("/a/b/model.scad"), "/a/b/model.json");
    let c = client();
    assert_eq!(
        c.untitled_path("/work", "Untitled", &[]).unwrap(),
        "/work/Untitled 2.scad"
    );
    assert_eq!(
        c.untitled_path("/work", "", &["/work/Untitled 2.scad".into()])
            .unwrap(),
        "/work/Untitled 3.scad"
    );
    assert_eq!(
        c.untitled_path("/work", "Part", &[]).unwrap(),
        "/work/Part.scad"
    );
    let names = color_scheme_names();
    assert!(names.contains(&"Cornfield".to_string()));
    assert!(names.contains(&"Tomorrow Night".to_string()));
}

// The six tests of the Swift `TextOffsets` this replaced, ported.

fn apply(text: &str, edits: &[(u64, u64, &str)]) -> (String, Result<Vec<TextEdit>, CoreError>) {
    let mut t = EditorText::new(text.into());
    let edits: Vec<Utf16Edit> = edits
        .iter()
        .map(|(from, to, insert)| Utf16Edit {
            from: *from,
            to: *to,
            insert: (*insert).into(),
        })
        .collect();
    let r = t.apply(&edits);
    assert_eq!(
        t.utf16_length(),
        t.text().encode_utf16().count() as u64,
        "the kept UTF-16 length"
    );
    (t.text(), r)
}

fn te(start: u64, end: u64, text: &str) -> TextEdit {
    TextEdit {
        start,
        end,
        text: text.into(),
    }
}

#[test]
fn ascii_offsets_are_unchanged() {
    let (text, r) = apply("cube(1);", &[(5, 6, "10")]);
    assert_eq!(text, "cube(10);");
    assert_eq!(r.unwrap(), [te(5, 6, "10")]);
}

#[test]
fn emoji_and_cjk_shift_the_byte_offsets() {
    let src = "// 😀 漢字\ncube(1);";
    let cube = src.encode_utf16().collect::<Vec<_>>();
    let at = (0..cube.len())
        .find(|i| String::from_utf16_lossy(&cube[*i..]).starts_with("cube"))
        .unwrap() as u64;
    assert_eq!(at, 9);
    let (text, r) = apply(src, &[(at, at + 4, "sphere")]);
    assert_eq!(text, "// 😀 漢字\nsphere(1);");
    assert_eq!(r.unwrap(), [te(15, 19, "sphere")]);
}

#[test]
fn edits_apply_in_order_each_to_the_previous_result() {
    // What the editor sends for a multi-cursor edit: last change first,
    // each in the offsets of the text before the transaction.
    let (text, r) = apply("é = 1;\n😀 = 2;\n", &[(12, 13, "二"), (4, 5, "10")]);
    assert_eq!(text, "é = 10;\n😀 = 二;\n");
    assert_eq!(r.unwrap(), [te(15, 16, "二"), te(5, 6, "10")]);
}

#[test]
fn inserting_an_emoji_between_characters() {
    let (text, r) = apply("a😀b", &[(3, 3, "🎉")]);
    assert_eq!(text, "a😀🎉b");
    assert_eq!(r.unwrap(), [te(5, 5, "🎉")]);
}

#[test]
fn an_offset_inside_a_surrogate_pair_is_refused() {
    let (text, r) = apply("a😀b", &[(2, 2, "x")]);
    assert!(
        matches!(r, Err(CoreError::InvalidArgument { message }) if message.contains("inside a character"))
    );
    assert_eq!(text, "a😀b");
    let (_, r) = apply("a😀b", &[(0, 9, "")]);
    assert!(
        matches!(r, Err(CoreError::InvalidArgument { message }) if message.contains("past the end"))
    );
    let (_, r) = apply("abc", &[(2, 1, "")]);
    assert!(r.is_err(), "an edit ending before it starts");
}

/// The converted edits, applied by the session, give the same text the
/// editor has: the property every host relies on.
#[test]
fn the_session_applies_utf16_edits() {
    let c = client();
    let before = "echo(\"😀\", \"漢字\", \"é\");\n";
    c.open(DOC, Some(before.into())).unwrap();
    let cjk =
        before.encode_utf16().count() as u64 - "漢字\", \"é\");\n".encode_utf16().count() as u64;
    assert_eq!(cjk, 12);
    let edits = [Utf16Edit {
        from: cjk,
        to: cjk + 2,
        insert: "かな🎉".into(),
    }];
    let want = "echo(\"😀\", \"かな🎉\", \"é\");\n";
    let info = c
        .edit_utf16(DOC, &edits, Some(want.encode_utf16().count() as u64))
        .unwrap();
    assert_eq!(info.length, Some(want.len() as u64));
    let r = c.evaluate(DOC).unwrap();
    assert_eq!(r.echo, ["ECHO: \"😀\", \"かな🎉\", \"é\""]);
    // A length that disagrees leaves the buffer alone.
    let x = Utf16Edit {
        from: 0,
        to: 0,
        insert: "x".into(),
    };
    assert!(c.edit_utf16(DOC, &[x], Some(1)).is_err());
    assert_eq!(c.read_file(DOC).unwrap(), want);
}

fn finding(id: u32, severity: FindingSeverity) -> CheckFinding {
    CheckFinding {
        id,
        severity,
        code: "thin-wall".into(),
        message: String::new(),
        part: None,
        point: vec![1.0, 2.0, 3.0],
        bbox_min: Some(vec![0.0; 3]),
        bbox_max: Some(vec![1.0; 3]),
        fix: String::new(),
        value: None,
        limit: None,
    }
}

#[test]
fn the_overlay_draws_the_panels_state() {
    let mut s = OverlayState {
        findings: vec![
            finding(1, FindingSeverity::Error),
            finding(2, FindingSeverity::Warning),
            finding(3, FindingSeverity::Info),
        ],
        ..OverlayState::default()
    };
    let o = view_overlay(&s);
    assert_eq!(o.markers.len(), 2, "info findings are not marked");
    assert!(o.lines.is_empty(), "no box without a selection");
    assert_eq!(
        o.markers[0].color,
        [190.0 / 255.0, 20.0 / 255.0, 20.0 / 255.0, 0.55]
    );
    s.selected = Some(2);
    let o = view_overlay(&s);
    assert_eq!(
        o.lines.len(),
        6,
        "the selected box: two rings, four uprights"
    );
    assert_eq!(o.markers[1].color[3], 1.0);
    assert_eq!(o.lines[0].points.len(), 12);
    s.picks = vec![vec![0.0; 3], vec![3.0, 4.0, 0.0]];
    let o = view_overlay(&s);
    assert_eq!(o.markers.last().unwrap().label, "B");
    assert_eq!(o.lines.last().unwrap().color, PICK_COLOR);
    assert_eq!(pick_distance(&s.picks), Some(5.0));
    assert_eq!(pick_distance(&s.picks[..1]), None);
}

#[test]
fn the_section_slider_spans_the_target() {
    let solid = |lo: f64, hi: f64| SolidStats {
        volume: 1.0,
        area: 1.0,
        bbox_min: vec![lo; 3],
        bbox_max: vec![hi; 3],
        centroid: vec![0.0; 3],
        triangles: 12,
    };
    let parts = vec![
        PartStats {
            name: "lid".into(),
            instances: 1,
            context: None,
            solid: Some(solid(2.0, 3.0)),
        },
        PartStats {
            name: "flat".into(),
            instances: 1,
            context: None,
            solid: None,
        },
    ];
    let model = solid(-5.0, 5.0);
    assert_eq!(
        section_range(Some(&model), &parts, SectionAxis::Z, None),
        [-5.0, 5.0]
    );
    assert_eq!(
        section_range(Some(&model), &parts, SectionAxis::X, Some("lid")),
        [2.0, 3.0]
    );
    assert_eq!(
        section_range(Some(&model), &parts, SectionAxis::Y, Some("flat")),
        [-5.0, 5.0],
        "a part that is not a solid: the model's box"
    );
    assert_eq!(
        section_range(Some(&solid(1.0, 1.0)), &[], SectionAxis::Z, None),
        [1.0, 2.0]
    );
    assert_eq!(section_range(None, &[], SectionAxis::Z, None), [0.0, 1.0]);
}

// --- The document loop (document_loop.rs) ------------------------------------

fn num(v: f64) -> ParameterValue {
    ParameterValue::Number { value: v }
}

#[test]
fn one_pause_runs_once() {
    let mut l = DocumentLoop::new(DEFAULT_PREVIEW_DELAY_MS);
    assert_eq!(DEFAULT_PREVIEW_DELAY_MS, 150);
    l.schedule(1000);
    l.schedule(1100); // another keystroke restarts the wait
    assert_eq!(l.next_due_ms(), Some(1250));
    assert_eq!(l.due(1249), None, "not yet");
    assert_eq!(l.due(1250), Some(RenderMode::Preview));
    assert_eq!(l.due(2000), None, "taken once");
    assert_eq!(l.next_due_ms(), None);
}

#[test]
fn runs_supersede_and_keep_the_buffer_in_step() {
    let mut l = DocumentLoop::new(300);
    l.schedule(0);
    let a = l.begin_run(RenderMode::Preview, "/d/a.scad").unwrap();
    assert!(a.send_text && a.close.is_none());
    assert_eq!(l.next_due_ms(), None, "the run covers what waited");
    l.text_sent();
    let b = l.begin_run(RenderMode::Render, "/d/a.scad").unwrap();
    assert!(!b.send_text, "in sync: edits went through");
    assert!(!l.is_current(a.generation) && l.is_current(b.generation));
    assert_eq!(l.request_count(), 2);
    // Saved under a new name: the old buffer is closed, the text sent.
    let c = l.begin_run(RenderMode::Preview, "/d/b.scad").unwrap();
    assert_eq!(c.close.as_deref(), Some("/d/a.scad"));
    assert!(c.send_text);
    l.text_sent();
    l.text_replaced();
    assert!(!l.in_sync());
    l.close();
    assert!(l.begin_run(RenderMode::Preview, "/d/b.scad").is_none());
    assert!(!l.is_current(c.generation));
}

#[test]
fn a_changed_file_reruns_the_last_mode() {
    let mut l = DocumentLoop::new(150);
    assert_eq!(l.files_changed(10), None);
    assert_eq!(l.next_due_ms(), Some(160), "a preview after the pause");
    l.begin_run(RenderMode::Render, "/d/a.scad");
    assert_eq!(
        l.files_changed(20),
        Some(RenderMode::Render),
        "a render at once"
    );
}

#[test]
fn customizer_values_are_kept_sorted_and_pruned() {
    let mut l = DocumentLoop::new(150);
    assert!(l.set_parameter("width", Some(num(2.0)), 0));
    assert!(l.set_parameter("depth", Some(num(3.0)), 0));
    assert!(
        !l.set_parameter("depth", Some(num(3.0)), 0),
        "the same value"
    );
    let names: Vec<String> = l.overrides().into_iter().map(|o| o.name).collect();
    assert_eq!(names, ["depth", "width"]);
    let plan = l.begin_run(RenderMode::Preview, "/d/a.scad").unwrap();
    assert_eq!(plan.request.overrides.len(), 2);
    let groups = vec![ParameterGroup {
        name: "Parameters".into(),
        parameters: vec![Parameter {
            name: "width".into(),
            description: String::new(),
            control: ParameterControl::SpinBox {
                min: None,
                max: None,
                step: None,
            },
            default_value: num(1.0),
        }],
    }];
    assert!(l.parameters_read(&groups), "depth is gone");
    assert_eq!(l.overrides().len(), 1);
    l.parameter_set_applied(
        "big",
        vec![ParameterOverride {
            name: "width".into(),
            value: num(1.0),
        }],
        &groups,
        0,
    );
    assert!(l.overrides().is_empty(), "equal to the text's value");
    assert_eq!(l.state().selected_set.as_deref(), Some("big"));
    assert!(l.set_parameter("width", Some(num(5.0)), 0));
    assert_eq!(l.state().selected_set, None, "an edit leaves the set");
    assert!(l.reset_parameters(0));
    assert!(!l.reset_parameters(0));
    assert!(l.set_parts(true) && !l.set_parts(true));
    assert!(
        l.begin_run(RenderMode::Preview, "/d/a.scad")
            .unwrap()
            .request
            .parts
    );
    // The window's `enable` names (the app's sketch setting) reach every
    // run from then on.
    let sketch = vec!["sketch".to_string()];
    assert!(l.set_enable(&sketch) && !l.set_enable(&sketch));
    assert_eq!(
        l.begin_run(RenderMode::Preview, "/d/a.scad")
            .unwrap()
            .request
            .enable,
        sketch
    );
}

// --- The file-manager preview (preview.rs) -----------------------------------

#[test]
fn the_preview_page_golden() {
    let r = PreviewOutcome {
        png: Some(vec![0x89, b'P', b'N', b'G']),
        source: "echo(\"<b>&\");".into(),
        notes: vec!["Skipped: a<b>.scad".into()],
        ..PreviewOutcome::default()
    };
    let want = "<!DOCTYPE html>
<html><head><meta charset=\"utf-8\"><title>x&quot;.scad</title>
<style>
:root { color-scheme: light dark; }
body { margin: 0; font: 13px -apple-system, sans-serif; }
.model img { max-width: 100%; height: auto; display: block; margin: 0 auto; }
.notes { margin: 8px 12px; padding: 6px 10px 6px 28px; border-radius: 6px;
         background: rgba(255, 196, 0, 0.18); }
pre { margin: 0; padding: 12px; font: 12px ui-monospace, Menlo, monospace;
      white-space: pre-wrap; overflow-wrap: anywhere; tab-size: 4; }
</style></head>
<body><div class=\"model\"><img src=\"cid:model.png\" alt=\"x&quot;.scad\"></div>\
<ul class=\"notes\"><li>Skipped: a&lt;b&gt;.scad</li></ul>\
<pre>echo(&quot;&lt;b&gt;&amp;&quot;);</pre></body></html>";
    assert_eq!(preview_html(&r, "x\".scad"), want);
    let bare = preview_html(&PreviewOutcome::default(), "x.scad");
    assert!(!bare.contains("<img") && !bare.contains("<ul"));
    let long = PreviewOutcome {
        source: "é".repeat(PREVIEW_MAX_SOURCE_CHARS + 5),
        ..PreviewOutcome::default()
    };
    let page = preview_html(&long, "x.scad");
    assert!(page.contains("cut short"));
    assert_eq!(page.matches('é').count(), PREVIEW_MAX_SOURCE_CHARS);
}

fn diag(code: &str, message: &str, line: Option<u32>) -> Diagnostic {
    Diagnostic {
        code: code.into(),
        severity: Severity::Error,
        message: message.into(),
        text: String::new(),
        file: None,
        line,
        span: None,
        hints: vec![],
        trace: vec![],
    }
}

#[test]
fn preview_notes_say_what_went_wrong() {
    let d = [diag(
        "include-not-found",
        "Can't find include file 'parts.scad'.",
        None,
    )];
    let console = "WARNING: Can't open import file '/m/x.stl', import() at line 2\n\
                   WARNING: Can't open import file '/m/x.stl', import() at line 3\n";
    let r = preview_outcome("s".into(), Some(vec![1]), 0, false, &d, console, "/m");
    assert_eq!(r.unreadable, ["parts.scad", "x.stl"]);
    assert!(r.notes[0].starts_with("Skipped files Quick Look could not read: parts.scad, x.stl."));
    let r = preview_outcome(
        "s".into(),
        None,
        1,
        false,
        &[diag("syntax", "syntax error", Some(3))],
        "",
        "/m",
    );
    assert_eq!(r.notes, ["Error on line 3: syntax error"]);
    let r = preview_outcome(
        "s".into(),
        None,
        1,
        false,
        &[diag("resource-limit", "time limit", None)],
        "",
        "/m",
    );
    assert!(r.notes[0].starts_with("The model is too large for Quick Look: time limit."));
    let r = preview_outcome("s".into(), None, 0, true, &[], "", "/m");
    assert_eq!(r.notes, ["The model is empty."]);
    let t = preview_timed_out("s".into(), 6);
    assert!(t.timed_out && t.notes[0].contains("(6 s)"));
    let l = preview_limits(ResourceLimits::from(session::Limits::AGENT));
    assert_eq!(
        (l.time_seconds, l.memory_bytes),
        (Some(5.0), Some(512 << 20))
    );
}
