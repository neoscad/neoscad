use super::*;

fn apply(old: &str, edits: &[ReloadEdit]) -> String {
    apply_reload_edits(old, edits)
}

fn sorted_and_apart(edits: &[ReloadEdit]) -> bool {
    edits
        .windows(2)
        .all(|w| (w[0].end_line, w[0].end_character) < (w[1].start_line, w[1].start_character))
}

#[test]
fn reload_edits_turn_the_old_text_into_the_new() {
    let cases = [
        ("", "cube(1);\n"),
        ("cube(1);\n", ""),
        ("a\nb\nc\n", "a\nB\nc\n"),
        ("a\nb\nc\n", "a\nc\n"),
        ("a\nc\n", "a\nb\nc\n"),
        ("a\nb\nc\nd\ne\n", "x\nb\nc\nd\ny\n"),
        ("no newline", "no newline\n"),
        ("r = 5;\ncube(r);\n", "r = 6;\ncube(r);\n"),
        ("é漢😀\nx\n", "é漢😃\nx\n"),
        ("😀😀\n", "😀\n"),
        ("a\r\nb\r\n", "a\r\nc\r\n"),
        ("same\nsame\nsame\n", "same\nsame\n"),
    ];
    for (old, new) in cases {
        let edits = reload_edits(old, new);
        assert_eq!(apply(old, &edits), new, "{old:?} -> {new:?}: {edits:?}");
        assert!(sorted_and_apart(&edits), "{edits:?}");
    }
    assert!(reload_edits("same", "same").is_empty());
}

#[test]
fn reload_edits_mark_only_what_changed() {
    // One value changed: one edit of one character, in UTF-16 columns.
    let e = reload_edits("r = 5;\ncube(r);\n", "r = 6;\ncube(r);\n");
    assert_eq!(
        e,
        [ReloadEdit {
            start_line: 0,
            start_character: 4,
            end_line: 0,
            end_character: 5,
            insert: "6".into(),
        }]
    );
    // Columns count UTF-16 units: "é漢😀" is 1 + 1 + 2 units.
    let e = reload_edits("é漢😀 = 1;\n", "é漢😀 = 2;\n");
    assert_eq!((e[0].start_line, e[0].start_character), (0, 7));
    // Two separate changes: two edits, the lines between untouched.
    let e = reload_edits("a = 1;\nb = 2;\nc = 3;\n", "a = 9;\nb = 2;\nc = 8;\n");
    assert_eq!(e.len(), 2);
    assert_eq!((e[1].start_line, e[1].start_character), (2, 4));
}

#[test]
fn a_wholesale_rewrite_still_reloads_exactly() {
    // More differences than the diff explores: one edit, still exact.
    let old: String = (0..3000).map(|i| format!("a{i}\n")).collect();
    let new: String = (0..3000).map(|i| format!("b{i}\n")).collect();
    let edits = reload_edits(&old, &new);
    assert_eq!(edits.len(), 1);
    assert_eq!(apply(&old, &edits), new);
}

#[test]
fn reload_edits_agree_with_brute_force_on_small_texts() {
    // Every pair of short line lists over a tiny alphabet.
    let words = ["a\n", "b\n", "c"];
    let mut texts = vec![String::new()];
    for _ in 0..4 {
        let more: Vec<String> = texts
            .iter()
            .flat_map(|t| words.iter().map(move |w| format!("{t}{w}")))
            .collect();
        texts.extend(more);
    }
    texts.sort();
    texts.dedup();
    for old in &texts {
        for new in &texts {
            let edits = reload_edits(old, new);
            assert_eq!(&apply(old, &edits), new, "{old:?} -> {new:?}: {edits:?}");
            assert!(sorted_and_apart(&edits), "{old:?} -> {new:?}: {edits:?}");
        }
    }
}

const SAVED: &str = "cube(1);\n";
const THEIRS: &str = "cube(2);\n";

fn loaded() -> DiskTracker {
    let mut t = DiskTracker::new();
    t.loaded(SAVED.as_bytes());
    t
}

#[test]
fn our_own_save_seen_by_the_watcher_does_nothing() {
    let mut t = loaded();
    assert_eq!(
        t.check(Some(SAVED.as_bytes()), SAVED, false),
        DiskAction::None
    );
    let mine = "cube(3);\n";
    t.saved(mine.as_bytes());
    assert_eq!(
        t.check(Some(mine.as_bytes()), mine, false),
        DiskAction::None
    );
    assert_eq!(t.save_check(Some(mine.as_bytes())), SaveCheck::Write);
}

#[test]
fn a_clean_document_reloads_as_edits_and_is_clean_once_they_land() {
    let mut t = loaded();
    let a = t.check(Some(THEIRS.as_bytes()), SAVED, false);
    let DiskAction::Reload { edits } = a else {
        panic!("{a:?}")
    };
    assert_eq!(apply(SAVED, &edits), THEIRS);
    // The editor reports the change: the document matches the file.
    assert!(t.editor_changed(THEIRS.as_bytes()));
    // Later keystrokes are not the reload.
    assert!(!t.editor_changed(b"cube(22);\n"));
    // The file is now known as theirs: the watcher's next event about it
    // does nothing, and Save writes without asking.
    assert_eq!(
        t.check(Some(THEIRS.as_bytes()), THEIRS, false),
        DiskAction::None
    );
    assert_eq!(t.save_check(Some(THEIRS.as_bytes())), SaveCheck::Write);
}

#[test]
fn a_second_write_waits_for_the_first_reload_to_land() {
    let mut t = loaded();
    assert!(matches!(
        t.check(Some(THEIRS.as_bytes()), SAVED, false),
        DiskAction::Reload { .. }
    ));
    // The agent writes again before the editor reported the first reload:
    // edits against the host's copy (still SAVED) would be wrong.
    let again = "cube(9);\n";
    assert!(matches!(
        t.check(Some(again.as_bytes()), SAVED, false),
        DiskAction::ReadAgain { .. }
    ));
    // The first lands; the second is then worked out against it.
    assert!(t.editor_changed(THEIRS.as_bytes()));
    let a = t.check(Some(again.as_bytes()), THEIRS, false);
    let DiskAction::Reload { edits } = a else {
        panic!("{a:?}")
    };
    assert_eq!(apply(THEIRS, &edits), again);
    // An editor that never answers stalls it only a few reads.
    let mut t = loaded();
    t.check(Some(THEIRS.as_bytes()), SAVED, false);
    let mut reads = 0;
    while let DiskAction::ReadAgain { .. } = t.check(Some(again.as_bytes()), SAVED, false) {
        reads += 1;
        assert!(reads < 10);
    }
}

#[test]
fn a_keystroke_that_raced_the_reload_leaves_the_document_edited() {
    let mut t = loaded();
    assert!(matches!(
        t.check(Some(THEIRS.as_bytes()), SAVED, false),
        DiskAction::Reload { .. }
    ));
    assert!(!t.editor_changed(b"cube(2);\nx"));
}

#[test]
fn a_dirty_document_reports_a_conflict_once_and_save_asks() {
    let mut t = loaded();
    let mine = "cube(1); sphere(1);\n";
    assert_eq!(
        t.check(Some(THEIRS.as_bytes()), mine, true),
        DiskAction::Conflict { reloadable: true }
    );
    assert!(t.is_reporting());
    // The same write seen again (a burst of events): reported once.
    assert_eq!(
        t.check(Some(THEIRS.as_bytes()), mine, true),
        DiskAction::None
    );
    t.keep_mine();
    assert_eq!(
        t.check(Some(THEIRS.as_bytes()), mine, true),
        DiskAction::None
    );
    // Keep mine does not make Save overwrite silently.
    assert_eq!(t.save_check(Some(THEIRS.as_bytes())), SaveCheck::Changed);
    // Another write is a new change: reported again.
    let again = "cube(4);\n";
    assert_eq!(
        t.check(Some(again.as_bytes()), mine, true),
        DiskAction::Conflict { reloadable: true }
    );
    // Overwriting: the save is now the file.
    t.saved(mine.as_bytes());
    assert!(!t.is_reporting());
    assert_eq!(
        t.check(Some(mine.as_bytes()), mine, false),
        DiskAction::None
    );
}

#[test]
fn reload_from_the_notice_takes_their_text() {
    let mut t = loaded();
    let mine = "sphere(1);\n";
    assert!(matches!(
        t.check(Some(THEIRS.as_bytes()), mine, true),
        DiskAction::Conflict { .. }
    ));
    let edits = t.reload(Some(THEIRS.as_bytes()), mine).unwrap();
    assert_eq!(apply(mine, &edits), THEIRS);
    assert!(!t.is_reporting());
    assert!(t.editor_changed(THEIRS.as_bytes()));
    assert_eq!(t.save_check(Some(THEIRS.as_bytes())), SaveCheck::Write);
    // Gone, or not text: nothing to reload.
    assert_eq!(t.reload(None, mine), None);
    assert_eq!(t.reload(Some(&[0xff, 0xfe]), mine), None);
}

#[test]
fn the_conflict_resolves_when_the_file_goes_back() {
    let mut t = loaded();
    let mine = "sphere(1);\n";
    assert!(matches!(
        t.check(Some(THEIRS.as_bytes()), mine, true),
        DiskAction::Conflict { .. }
    ));
    assert_eq!(
        t.check(Some(SAVED.as_bytes()), mine, true),
        DiskAction::Resolved
    );
    assert!(!t.is_reporting());
}

#[test]
fn the_file_now_holding_the_documents_text_marks_it_saved() {
    let mut t = loaded();
    let mine = "sphere(1);\n";
    assert_eq!(
        t.check(Some(mine.as_bytes()), mine, true),
        DiskAction::Saved
    );
    assert_eq!(t.known(), Some(DiskStamp::of(mine.as_bytes())));
    assert_eq!(t.save_check(Some(mine.as_bytes())), SaveCheck::Write);
}

#[test]
fn text_that_is_not_utf8_is_a_conflict_without_reload() {
    let mut t = loaded();
    let bad: &[u8] = b"cube(1); // \xff\n";
    // Not UTF-8 also does not look complete: read again, then taken.
    assert!(matches!(
        t.check(Some(bad), SAVED, false),
        DiskAction::ReadAgain { .. }
    ));
    assert_eq!(
        t.check(Some(bad), SAVED, false),
        DiskAction::Conflict { reloadable: false }
    );
}

#[test]
fn a_half_written_file_is_read_again_until_it_parses_or_settles() {
    let mut t = loaded();
    // Caught mid-write: read again.
    let part = "module m() { cube(";
    assert_eq!(
        t.check(Some(part.as_bytes()), SAVED, false),
        DiskAction::ReadAgain {
            ms: DISK_READ_AGAIN_MS
        }
    );
    // The write finished: it parses, and reloads.
    let whole = "module m() { cube(2); }\nm();\n";
    let a = t.check(Some(whole.as_bytes()), SAVED, false);
    let DiskAction::Reload { edits } = a else {
        panic!("{a:?}")
    };
    assert_eq!(apply(SAVED, &edits), whole);

    // A real syntax error that stays: taken on the second read.
    let mut t = loaded();
    let broken = "cube(;\n";
    assert!(matches!(
        t.check(Some(broken.as_bytes()), SAVED, false),
        DiskAction::ReadAgain { .. }
    ));
    assert!(matches!(
        t.check(Some(broken.as_bytes()), SAVED, false),
        DiskAction::Reload { .. }
    ));

    // A file that keeps changing and never parses: taken after a bound.
    let mut t = loaded();
    let mut reads = 0;
    let a = loop {
        reads += 1;
        let text = format!("cube({reads}");
        match t.check(Some(text.as_bytes()), SAVED, false) {
            DiskAction::ReadAgain { .. } => assert!(reads < 10),
            a => break a,
        }
    };
    assert!(matches!(a, DiskAction::Reload { .. }), "{a:?}");
    assert_eq!(reads, MAX_READS + 1);
}

#[test]
fn a_deleted_file_is_reported_once_and_saving_writes_it_again() {
    let mut t = loaded();
    // Missing may be a delete before a write: read again first.
    assert!(matches!(
        t.check(None, SAVED, false),
        DiskAction::ReadAgain { .. }
    ));
    assert_eq!(t.check(None, SAVED, false), DiskAction::Missing);
    assert_eq!(t.check(None, SAVED, false), DiskAction::None);
    assert_eq!(t.save_check(None), SaveCheck::Write);
    // Back as it was (an editor's delete-and-write): the notice goes.
    assert_eq!(
        t.check(Some(SAVED.as_bytes()), SAVED, false),
        DiskAction::Resolved
    );
    // Deleted and written again with other text: reloaded.
    assert!(matches!(
        t.check(None, SAVED, false),
        DiskAction::ReadAgain { .. }
    ));
    assert!(matches!(
        t.check(Some(THEIRS.as_bytes()), SAVED, false),
        DiskAction::Reload { .. }
    ));
}

#[test]
fn an_untitled_document_has_nothing_to_compare() {
    let mut t = DiskTracker::new();
    assert_eq!(t.check(None, "x", true), DiskAction::None);
    assert_eq!(t.save_check(Some(b"anything")), SaveCheck::Write);
    let mut t = loaded();
    t.forget();
    assert_eq!(t.known(), None);
}

#[test]
fn agent_edit_json_is_what_the_editor_takes() {
    let e = reload_edits("a = 1;\n", "a = \"é\";\n");
    assert_eq!(
        agent_edit_json(&e),
        r#"[{"from":[0,4],"insert":"\"é\"","to":[0,5]}]"#
    );
    assert_eq!(agent_edit_json(&[]), "[]");
}

#[test]
fn stamps_differ_with_content_and_length() {
    assert_eq!(DiskStamp::of(b"abc"), DiskStamp::of(b"abc"));
    assert_ne!(DiskStamp::of(b"abc"), DiskStamp::of(b"abd"));
    assert_ne!(DiskStamp::of(b""), DiskStamp::of(b"\0"));
}
