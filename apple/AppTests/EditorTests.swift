// The editor end to end, hosted in the app: a document window with
// CodeMirror loaded from the app's own scheme, edits made in the page and
// carried over the bridge into the document's copy and the core, undo
// against NSDocument's edited state, the language server's diagnostics as
// lint markers, keys
// sent through NSApp as the keyboard would, and input method calls.
//
// These tests put windows on screen: focus, keys and layout need one.

import AppKit
import NeoSCADCore
import Testing
import WebKit

@testable import NeoSCAD

private let scad = "org.openscad.scad"

/// Wait until `condition` holds, letting the main actor run meanwhile.
@MainActor
func waitUntil(
    _ what: String, timeout: Duration = .seconds(20), _ condition: () -> Bool
) async throws {
    let clock = ContinuousClock()
    let deadline = clock.now + timeout
    while !condition() {
        if clock.now > deadline {
            Issue.record("timed out waiting until \(what)")
            throw CancellationError()
        }
        try await Task.sleep(for: .milliseconds(5))
    }
}

/// A document showing `text` in its window, the editor loaded.
@MainActor
func openDocument(_ text: String, file: URL? = nil) async throws -> SCADDocument {
    let doc = SCADDocument()
    try doc.read(from: Data(text.utf8), ofType: scad)
    doc.fileURL = file
    doc.makeWindowControllers()
    doc.showWindows()
    let editor = doc.model.editor
    try await waitUntil("the editor is ready") { editor.isReady && editor.version != nil }
    return doc
}

/// The text under each warning or error marker, once there are `count`.
@MainActor
func lintMarkers(_ doc: SCADDocument, count: Int) async throws -> [String] {
    var shown: [String] = []
    let clock = ContinuousClock()
    let deadline = clock.now + .seconds(20)
    while clock.now < deadline {
        shown =
            try await doc.model.editor.call(
                """
                let out = [];
                document.querySelectorAll('.cm-lintRange-warning, .cm-lintRange-error')
                    .forEach(e => out.push(e.textContent));
                return out;
                """) as? [String] ?? []
        if shown.count == count { return shown }
        try await Task.sleep(for: .milliseconds(20))
    }
    Issue.record("timed out waiting for \(count) lint markers (have \(shown))")
    return shown
}

@MainActor
func editorState(_ doc: SCADDocument) async throws -> [String: Any] {
    try #require(try await doc.model.editor.call("return NeoSCADEditor.state()") as? [String: Any])
}

/// Edit in the page as typing would: the change comes back over the
/// bridge like any keystroke's.
@MainActor
func editInPage(_ doc: SCADDocument, from: Int, to: Int, insert: String) async throws {
    let editor = doc.model.editor
    let before = editor.changeCount
    try await editor.call(
        "return NeoSCADEditor.edit(from, to, insert)", ["from": from, "to": to, "insert": insert])
    try await waitUntil("the edit arrives") { editor.changeCount > before }
}

/// A key press through NSApp, as the keyboard delivers it: menu key
/// equivalents first, then the key window's first responder.
@MainActor
func press(
    _ characters: String, keyCode: UInt16, modifiers: NSEvent.ModifierFlags = [], in window: NSWindow
) {
    for type in [NSEvent.EventType.keyDown, .keyUp] {
        let event = NSEvent.keyEvent(
            with: type, location: .zero, modifierFlags: modifiers,
            timestamp: ProcessInfo.processInfo.systemUptime, windowNumber: window.windowNumber,
            context: nil, characters: characters, charactersIgnoringModifiers: characters,
            isARepeat: false, keyCode: keyCode)!
        NSApp.sendEvent(event)
    }
}

@MainActor
func focusEditor(_ doc: SCADDocument) async throws -> NSWindow {
    let window = try #require(doc.windowControllers.first?.window)
    // Only an active app has a key window, and a plain activate() is a
    // request that macOS may decline for an app started in the background
    // (as the test host is).
    NSApp.activate(ignoringOtherApps: true)
    window.makeKeyAndOrderFront(nil)
    window.makeFirstResponder(doc.model.editor.webView)
    doc.model.editor.focus()
    try await waitUntil("the window is key (bring the app to the front)", timeout: .seconds(90)) {
        window.isKeyWindow
    }
    // The page's focus follows asynchronously.
    try await Task.sleep(for: .milliseconds(100))
    return window
}

@MainActor
@Suite(.serialized) struct EditorTests {
    @Test func theEditorShowsTheDocumentsText() async throws {
        let text = "// 漢字 😀\ncube(10);\n"
        let doc = try await openDocument(text)
        let r = try #require(
            try await doc.model.editor.call("return NeoSCADEditor.text()") as? [String: Any])
        #expect(r["text"] as? String == text)
        let state = try await editorState(doc)
        #expect(state["lines"] as? Int == 3)
        doc.close()
    }

    @Test func anEditInThePageReachesTheDocumentAndTheCore() async throws {
        let doc = try await openDocument("echo(\"a\");\n")
        // Render once, so the core holds the text and edits are forwarded
        // to it one by one (Core.edit) rather than sent whole.
        doc.previewDocument(nil)
        await doc.renderTask?.value
        #expect(doc.coreInSync)
        #expect(!doc.isDocumentEdited)

        // Replace `a` (UTF-16 offset 6) with CJK and an emoji: the UTF-16
        // and UTF-8 offsets differ from here on.
        try await editInPage(doc, from: 6, to: 7, insert: "漢😀")
        try await editInPage(doc, from: 9, to: 9, insert: "é")
        #expect(doc.model.text == "echo(\"漢😀é\");\n")
        #expect(doc.isDocumentEdited)
        #expect(doc.coreInSync, "the edits were applied by Core.edit, not a full update")

        // The core evaluates the new text.
        let engine = try CoreService.shared.get()
        let path = try #require(doc.corePath)
        let result = try await engine.evaluate(path)
        #expect(result.echo == ["ECHO: \"漢😀é\""])
        #expect(try doc.data(ofType: scad) == Data("echo(\"漢😀é\");\n".utf8))
        doc.close()
    }

    @Test func undoingToTheSavedTextClearsTheEditedState() async throws {
        let doc = try await openDocument("cube(1);\n")
        let editor = doc.model.editor
        try await editInPage(doc, from: 5, to: 6, insert: "2")
        #expect(doc.isDocumentEdited)
        #expect(editor.undoDepth == 1)

        var before = editor.changeCount
        editor.undo()
        try await waitUntil("the undo arrives") { editor.changeCount > before }
        #expect(doc.model.text == "cube(1);\n")
        #expect(!doc.isDocumentEdited)
        #expect(editor.redoDepth == 1)

        before = editor.changeCount
        editor.redo()
        try await waitUntil("the redo arrives") { editor.changeCount > before }
        #expect(doc.model.text == "cube(2);\n")
        #expect(doc.isDocumentEdited)
        doc.close()
    }

    @Test func revertingLoadsTheTextAndClearsTheHistory() async throws {
        let doc = try await openDocument("cube(1);\n")
        try await editInPage(doc, from: 0, to: 0, insert: "// x\n")
        let editor = doc.model.editor
        try doc.read(from: Data("sphere(2);\n".utf8), ofType: scad)
        try await waitUntil("the load arrives") { editor.version != nil && editor.undoDepth == 0 }
        let r = try #require(try await editor.call("return NeoSCADEditor.text()") as? [String: Any])
        #expect(r["text"] as? String == "sphere(2);\n")
        // Editing after the load applies to the loaded text.
        try await editInPage(doc, from: 0, to: 6, insert: "cube")
        #expect(doc.model.text == "cube(2);\n")
        doc.close()
    }

    @Test func diagnosticsBecomeLintMarkersAtTheirUTF16Offsets() async throws {
        // "é" is one UTF-16 unit and two bytes, so the core's byte column
        // of `cub` is one more than its UTF-16 column: a marker placed by
        // bytes would start one character late. The markers come from the
        // document's own run, through the language server.
        let text = "cube(1);\nx = \"é\"; cub(2);\n"
        let doc = try await openDocument(text)
        let shown = try await lintMarkers(doc, count: 1)
        #expect(shown == ["cub(2);"])
        let state = try await editorState(doc)
        #expect(state["fixes"] as? [String] == ["Change 'cub' to 'cube'"])
        doc.close()
    }

    @Test func diagnosticsFollowTypingDoneMeanwhile() async throws {
        let doc = try await openDocument("cub(1);\n")
        #expect(try await lintMarkers(doc, count: 1) == ["cub(1);"])
        // Typing above the marker moves it with its text, and the next
        // publication (of the new version) puts it in the same place.
        let before = doc.model.editor.languageClient?.publications ?? 0
        try await editInPage(doc, from: 0, to: 0, insert: "// 😀\n")
        #expect(try await lintMarkers(doc, count: 1) == ["cub(1);"])
        try await waitUntil("the new version's diagnostics arrive") {
            (doc.model.editor.languageClient?.publications ?? 0) > before
        }
        #expect(try await lintMarkers(doc, count: 1) == ["cub(1);"])
        doc.close()
    }

    @Test func theFontSizeFollowsTheSetting() async throws {
        let doc = try await openDocument("cube();\n")
        let saved = EditorSettings.fontSize
        defer { EditorSettings.fontSize = saved }
        EditorSettings.fontSize = 17
        try await Task.sleep(for: .milliseconds(200))
        var state = try await editorState(doc)
        #expect(state["fontSize"] as? Double == 17)
        let px = try await doc.model.editor.call(
            "return getComputedStyle(document.querySelector('.cm-scroller')).fontSize")
        #expect(px as? String == "17px")
        EditorSettings.fontSize = saved
        try await Task.sleep(for: .milliseconds(200))
        state = try await editorState(doc)
        #expect(state["fontSize"] as? Double == saved)
        doc.close()
    }

    @Test func theThemeFollowsTheAppearance() async throws {
        let doc = try await openDocument("cube();\n")
        let web = doc.model.editor.webView
        let background = {
            try await doc.model.editor.call(
                "return getComputedStyle(document.querySelector('.cm-editor')).backgroundColor")
                as? String
        }
        web.appearance = NSAppearance(named: .darkAqua)
        try await Task.sleep(for: .milliseconds(300))
        #expect(try await editorState(doc)["dark"] as? Bool == true)
        // Tomorrow Night's paper, #1d1f21.
        #expect(try await background() == "rgb(29, 31, 33)")
        web.appearance = NSAppearance(named: .aqua)
        try await Task.sleep(for: .milliseconds(300))
        #expect(try await editorState(doc)["dark"] as? Bool == false)
        #expect(try await background() == "rgb(255, 255, 255)")
        doc.close()
    }

    @Test func thePageLoadsNothingButTheBundle() async throws {
        let doc = try await openDocument("cube();\n")
        let editor = doc.model.editor
        // Inline script is refused by the page's policy; fetch has nowhere
        // to go.
        let result = try await editor.call(
            """
            let inline = false;
            const s = document.createElement('script');
            s.textContent = 'window.__inline = true';
            document.body.appendChild(s);
            inline = window.__inline === true;
            let fetched = 'no';
            try { await fetch('https://example.com/'); fetched = 'yes'; } catch (e) {}
            return [inline, fetched, document.location.href];
            """)
        let r = try #require(result as? [Any])
        #expect(r[0] as? Bool == false)
        #expect(r[1] as? String == "no")
        #expect(r[2] as? String == EditorSchemeHandler.pageURL.absoluteString)
        doc.close()
    }

    // MARK: Keys, layer by layer

    /// A key as the page sees it: a DOM keydown on CodeMirror's content,
    /// which is where WebKit delivers a key the page is offered. This is
    /// CodeMirror's keymap and the editor's forwarding of the app's keys,
    /// without the window server (the next suite has that).
    private func keyInPage(
        _ doc: SCADDocument, key: String, code: String, meta: Bool = false, shift: Bool = false
    ) async throws -> Bool {
        let handled = try await doc.model.editor.call(
            """
            const e = new KeyboardEvent('keydown', {key, code, metaKey: meta, shiftKey: shift,
              bubbles: true, cancelable: true});
            document.querySelector('.cm-content').dispatchEvent(e);
            return e.defaultPrevented;
            """, ["key": key, "code": code, "meta": meta, "shift": shift])
        return handled as? Bool ?? false
    }

    @Test func f5AndF6InTheEditorPreviewAndRender() async throws {
        let doc = try await openDocument("cube(3);\n")
        doc.renderTask?.cancel()
        doc.model.report = .idle
        #expect(try await keyInPage(doc, key: "F5", code: "F5"))
        try await waitUntil("a preview starts") {
            if case .idle = doc.model.report { return false }
            return true
        }
        await doc.renderTask?.value
        guard case .rendered(_, .preview) = doc.model.report else {
            Issue.record("expected a preview, got \(doc.model.report)")
            return
        }
        #expect(try await keyInPage(doc, key: "F6", code: "F6"))
        try await waitUntil("a render starts") {
            if case .running(.render) = doc.model.report { return true }
            if case .rendered(_, .render) = doc.model.report { return true }
            return false
        }
        await doc.renderTask?.value
        doc.close()
    }

    @Test func theEditorKeepsCommandZAndDeclinesCommandS() async throws {
        let doc = try await openDocument("cube(1);\n")
        try await editInPage(doc, from: 5, to: 6, insert: "2")
        let editor = doc.model.editor
        let before = editor.changeCount
        // ⌘Z is CodeMirror's: taken (so WebKit does not pass it on to the
        // menu) and undone in the editor's history.
        #expect(try await keyInPage(doc, key: "z", code: "KeyZ", meta: true))
        try await waitUntil("the undo arrives") { editor.changeCount > before }
        #expect(doc.model.text == "cube(1);\n")
        #expect(!doc.isDocumentEdited)
        // ⇧⌘Z redoes.
        #expect(try await keyInPage(doc, key: "z", code: "KeyZ", meta: true, shift: true))
        try await waitUntil("the redo arrives") { doc.model.text == "cube(2);\n" }
        // ⌘S is declined, so WebKit hands it to the menu (Save).
        #expect(try await !keyInPage(doc, key: "s", code: "KeyS", meta: true))
        doc.close()
    }

    @Test func editMenuActionsReachTheEditor() async throws {
        let doc = try await openDocument("cube(1);\n")
        let editor = doc.model.editor
        let web = editor.webView
        let undo = NSMenuItem(title: "Undo", action: #selector(EditorWebView.undo(_:)), keyEquivalent: "")
        #expect(!web.validateUserInterfaceItem(undo), "nothing to undo yet")
        try await editInPage(doc, from: 0, to: 4, insert: "sphere")
        #expect(web.validateUserInterfaceItem(undo))
        let before = editor.changeCount
        web.undo(nil)
        try await waitUntil("the undo arrives") { editor.changeCount > before }
        #expect(doc.model.text == "cube(1);\n")
        web.selectAll(nil)
        try await Task.sleep(for: .milliseconds(100))
        let state = try await editorState(doc)
        #expect(state["selection"] as? [Int] == [0, 9])
        web.performFindPanelAction(nil)
        try await Task.sleep(for: .milliseconds(100))
        let panel = try await editor.call("return document.querySelector('.cm-search') !== null")
        #expect(panel as? Bool == true)
        doc.close()
    }

    // MARK: Input methods

    /// What a Japanese input method does: marked (composing) text, replaced
    /// by the committed text. The calls are NSTextInputClient's, which
    /// the system's input methods make on the focused view; the input
    /// method itself is not driven (that would switch the user's input
    /// source).
    @Test func composedTextIsCommittedOnce() async throws {
        let doc = try await openDocument("")
        let window = try #require(doc.windowControllers.first?.window)
        window.makeFirstResponder(doc.model.editor.webView)
        doc.model.editor.focus()
        try await Task.sleep(for: .milliseconds(100))
        let client = try #require(doc.model.editor.webView as NSView as? any NSTextInputClient)
        client.setMarkedText(
            "にほん", selectedRange: NSRange(location: 3, length: 0),
            replacementRange: NSRange(location: NSNotFound, length: 0))
        try await Task.sleep(for: .milliseconds(100))
        client.insertText("日本", replacementRange: NSRange(location: NSNotFound, length: 0))
        try await waitUntil("the committed text arrives") { doc.model.text == "日本" }
        try await Task.sleep(for: .milliseconds(200))
        #expect(doc.model.text == "日本")
        let r = try #require(
            try await doc.model.editor.call("return NeoSCADEditor.text()") as? [String: Any])
        #expect(r["text"] as? String == "日本")
        doc.close()
    }
}

/// Keys as the keyboard delivers them: NSEvents through NSApp, so menu
/// key equivalents, WebKit's offer to the page and its hand-back of
/// declined keys all take part. Only an active app has a key window, and
/// macOS does not let a test host started in the background take focus
/// from the app in front, so these run only when asked for, with the host
/// brought to the front:
///
///     TEST_RUNNER_NEOSCAD_EDITOR_KEYS=1 xcodebuild ... test \
///         -only-testing:NeoSCADAppTests/EditorKeyTests &
///     # once the tests start, bring the test host forward:
///     open -a apple/build/DerivedData/Build/Products/Debug/NeoSCAD.app
///
/// (or click its window). Each test waits up to 90 s for its window to
/// become key.
@MainActor
@Suite(
    .serialized,
    .enabled(if: ProcessInfo.processInfo.environment["NEOSCAD_EDITOR_KEYS"] != nil))
struct EditorKeyTests {
    @Test func typedKeysReachTheEditor() async throws {
        let doc = try await openDocument("")
        let window = try await focusEditor(doc)
        press("c", keyCode: 8, in: window)
        try await waitUntil("the key arrives") { doc.model.text == "c" }
        doc.close()
    }

    @Test func optionShiftFFormatsThroughNSApp() async throws {
        let doc = try await openDocument("cube( 1 );\n")
        let window = try await focusEditor(doc)
        let ready = try await doc.model.editor.call("return await NeoSCADEditor.lspReady()")
        #expect(ready as? Bool == true)
        // ⌥⇧F types "Ï" on a US layout; the editor must format, not type.
        press("Ï", keyCode: 3, modifiers: [.option, .shift], in: window)
        try await waitUntil("the document is formatted") { doc.model.text == "cube(1);\n" }
        doc.close()
    }

    @Test func commandZUndoesInTheEditor() async throws {
        let doc = try await openDocument("")
        let window = try await focusEditor(doc)
        press("c", keyCode: 8, in: window)
        try await waitUntil("the key arrives") { doc.model.text == "c" }
        press("z", keyCode: 6, modifiers: .command, in: window)
        try await waitUntil("the undo arrives") { doc.model.text.isEmpty }
        #expect(doc.model.editor.redoDepth == 1)
        #expect(!doc.isDocumentEdited)
        doc.close()
    }

    @Test func f5AndF6PreviewAndRender() async throws {
        let doc = try await openDocument("cube(3);\n")
        let window = try await focusEditor(doc)
        doc.renderTask?.cancel()
        doc.model.report = .idle
        let before = doc.requestCount
        press(String(Character(UnicodeScalar(NSF5FunctionKey)!)), keyCode: 96, in: window)
        try await waitUntil("a preview starts") {
            if case .idle = doc.model.report { return false }
            return true
        }
        await doc.renderTask?.value
        // Once: the menu and the editor's own forwarding never both act.
        try await Task.sleep(for: .milliseconds(300))
        #expect(doc.requestCount == before + 1)
        press(String(Character(UnicodeScalar(NSF6FunctionKey)!)), keyCode: 97, in: window)
        try await waitUntil("a render starts") {
            if case .running(.render) = doc.model.report { return true }
            if case .rendered(_, .render) = doc.model.report { return true }
            return false
        }
        await doc.renderTask?.value
        doc.close()
    }

    @Test func commandSSaves() async throws {
        let dir = FileManager.default.temporaryDirectory
            .appendingPathComponent("NeoSCAD-editor-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: dir) }
        let file = dir.appendingPathComponent("save.scad")
        try Data("cube(1);\n".utf8).write(to: file)
        let doc = try NSDocumentController.shared.makeDocument(withContentsOf: file, ofType: scad)
            as! SCADDocument
        doc.makeWindowControllers()
        doc.showWindows()
        try await waitUntil("the editor is ready") {
            doc.model.editor.isReady && doc.model.editor.version != nil
        }
        let window = try await focusEditor(doc)
        try await doc.model.editor.call("return NeoSCADEditor.select(4)")
        press("x", keyCode: 7, in: window)
        try await waitUntil("the key arrives") { doc.model.text == "cubex(1);\n" }
        #expect(doc.isDocumentEdited)
        press("s", keyCode: 1, modifiers: .command, in: window)
        try await waitUntil("the save") { !doc.isDocumentEdited }
        #expect(try String(contentsOf: file, encoding: .utf8) == "cubex(1);\n")
        doc.close()
    }
}

/// Typing in a large file, measured (8d): `NEOSCAD_EDITOR_BENCH=path` names
/// the file (for example several BOSL2 files concatenated to 1 MB).
///
///     TEST_RUNNER_NEOSCAD_EDITOR_BENCH=/path/big.scad xcodebuild ... test \
///         -only-testing:NeoSCADAppTests/EditorBenchmark
///
/// Prints, in milliseconds: the time to show the file; CodeMirror's own
/// work per keystroke (dispatch and layout, in the page); a keystroke's
/// round trip through the bridge (from the app's call into the page to the
/// change arriving back, applied to the document's copy and the core's);
/// and the app's share of that (offsets, the copy, `Core.edit`).
@MainActor
@Suite(.enabled(if: ProcessInfo.processInfo.environment["NEOSCAD_EDITOR_BENCH"] != nil))
struct EditorBenchmark {
    private func stats(_ xs: [Double]) -> String {
        let s = xs.sorted()
        let at = { (q: Double) in s[min(s.count - 1, Int(q * Double(s.count)))] }
        return String(format: "p50 %.3f, p95 %.3f, max %.3f", at(0.5), at(0.95), s.last!)
    }

    @Test func typingInALargeFile() async throws {
        let path = try #require(ProcessInfo.processInfo.environment["NEOSCAD_EDITOR_BENCH"])
        let text = try String(contentsOfFile: path, encoding: .utf8)
        let clock = ContinuousClock()
        let t0 = clock.now
        let doc = try await openDocument(text)
        let shown = clock.now - t0
        let editor = doc.model.editor
        print("editor bench: \(text.utf8.count) bytes, \(text.split(separator: "\n", omittingEmptySubsequences: false).count) lines, shown in \(shown)")
        // The core holds the text, so edits go through Core.edit.
        doc.previewDocument(nil)
        await doc.renderTask?.value
        doc.renderTask?.cancel()

        // Type in the middle of the file.
        let middle = doc.model.utf16Length / 2
        try await editor.call("return NeoSCADEditor.select(pos)", ["pos": middle])

        let page = try #require(
            try await editor.call("return NeoSCADEditor.benchmarkTyping(200)") as? [String: Double])
        print(String(format: "editor bench: CodeMirror per keystroke: p50 %.3f, p95 %.3f, max %.3f ms",
            page["p50"]!, page["p95"]!, page["max"]!))

        var roundTrips: [Double] = []
        for i in 0..<200 {
            let before = editor.changeCount
            let t = clock.now
            try await editor.call(
                "return NeoSCADEditor.edit(pos, pos, 'y')", ["pos": middle + 200 + i])
            try await waitUntil("the edit arrives") { editor.changeCount > before }
            let d = clock.now - t
            roundTrips.append(Double(d.components.attoseconds) / 1e15 + Double(d.components.seconds) * 1000)
        }
        print("editor bench: bridge round trip per keystroke: \(stats(roundTrips)) ms")

        // The app's share: the same edits applied to a copy of the model's
        // text and forwarded to the core.
        let engine = try CoreService.shared.get()
        let corePath = try #require(doc.corePath)
        var appShare: [Double] = []
        var offsetsOnly: [Double] = []
        let model = DocumentModel()
        model.text = doc.model.text
        try engine.open("/NeoSCAD-bench/copy.scad", text: model.text)
        for i in 0..<200 {
            let t = clock.now
            let edits = try model.applyEditorEdits([UTF16Edit(from: middle + i, to: middle + i, insert: "z")])
            let t1 = clock.now
            try engine.edit("/NeoSCAD-bench/copy.scad", edits: edits)
            let d = clock.now - t
            let o = t1 - t
            appShare.append(Double(d.components.attoseconds) / 1e15 + Double(d.components.seconds) * 1000)
            offsetsOnly.append(Double(o.components.attoseconds) / 1e15 + Double(o.components.seconds) * 1000)
        }
        try engine.close("/NeoSCAD-bench/copy.scad")
        print("editor bench: app per keystroke (offsets + copy + Core.edit): \(stats(appShare)) ms")
        print("editor bench:   of which offsets + copy: \(stats(offsetsOnly)) ms")
        _ = corePath
        #expect(editor.resyncCount == 0)
        doc.close()
    }
}
