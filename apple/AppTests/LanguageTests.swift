// The language features in the app: the page's LSP client, the bridge,
// and the core's language server (crates/lsp) together. Each feature is
// triggered in the page as a person would (the editor's test helpers:
// `complete`, `hover`, `signature`, `definition`, `format`) and checked
// by what the page then shows.

import AppKit
import NeoSCADCore
import Testing

@testable import NeoSCAD

/// Wait until the page's client has initialised with the server.
@MainActor
func languageReady(_ doc: SCADDocument) async throws {
    let ok = try await doc.model.editor.call("return await NeoSCADEditor.lspReady()")
    #expect(ok as? Bool == true)
}

@MainActor
@Suite(.serialized) struct LanguageTests {
    @Test func completionAppears() async throws {
        let text = "include <MCAD/units.scad>\ncub"
        let doc = try await openDocument(text)
        try await languageReady(doc)
        let labels = try await doc.model.editor.call(
            "return await NeoSCADEditor.complete(pos)", ["pos": text.utf16.count])
        let l = try #require(labels as? [String], "no completion shown")
        #expect(l.contains("cube"))
        doc.close()
    }

    @Test func completionOffersTheCallsParameters() async throws {
        let text = "cylinder(h = 2, r"
        let doc = try await openDocument(text)
        try await languageReady(doc)
        let labels = try await doc.model.editor.call(
            "return await NeoSCADEditor.complete(pos)", ["pos": text.utf16.count])
        let l = try #require(labels as? [String], "no completion shown")
        #expect(l.contains("r1="))
        doc.close()
    }

    @Test func hoverAppears() async throws {
        let text = "// é 😀\ncube(10);\n"
        let doc = try await openDocument(text)
        try await languageReady(doc)
        let at = (text as NSString).range(of: "cube").location + 1
        let shown = try await doc.model.editor.call(
            "return await NeoSCADEditor.hover(pos)", ["pos": at])
        let t = try #require(shown as? String, "no hover shown")
        #expect(t.contains("module cube(size=1, center=false)"))
        doc.close()
    }

    @Test func signatureHelpAppears() async throws {
        let text = "module box(size, center = false) {}\nbox(1, "
        let doc = try await openDocument(text)
        try await languageReady(doc)
        let shown = try await doc.model.editor.call(
            "return await NeoSCADEditor.signature(pos)", ["pos": text.utf16.count])
        let t = try #require(shown as? String, "no signature help shown")
        #expect(t.contains("box(size, center=false)"))
        doc.close()
    }

    @Test func goToDefinitionOpensALibraryFileReadOnly() async throws {
        // MCAD is bundled: it exists only in the core's memory.
        let text = "include <MCAD/units.scad>\nx = mm;\n"
        let doc = try await openDocument(text)
        try await languageReady(doc)
        let at = (text as NSString).range(of: "mm;").location
        _ = try await doc.model.editor.call("return NeoSCADEditor.definition(pos)", ["pos": at])
        try await waitUntil("a viewer of units.scad opens") {
            LibraryViewer.viewers.keys.contains { $0.hasSuffix("/MCAD/units.scad") }
        }
        let viewer = try #require(LibraryViewer.viewers.first { $0.key.hasSuffix("/MCAD/units.scad") }?.value)
        let window = try #require(viewer.window)
        #expect(window.isVisible)
        #expect(window.subtitle.contains("MCAD/units.scad"))
        // A tab of the document's window.
        let docWindow = try #require(doc.windowControllers.first?.window)
        #expect(window.tabbedWindows?.contains(docWindow) == true)
        try await waitUntil("the viewer's editor is ready") { viewer.editor.isReady }
        // Read-only, and the cursor at `mm = 1;` (line 9).
        var state: [String: Any] = [:]
        try await waitUntil("the cursor is at the definition") {
            (state["selection"] as? [Int])?.first ?? 0 > 0
        } while: {
            state = try await viewer.editor.call("return NeoSCADEditor.state()") as? [String: Any] ?? [:]
        }
        #expect(state["readOnly"] as? Bool == true)
        let r = try #require(
            try await viewer.editor.call("return NeoSCADEditor.text()") as? [String: Any])
        let body = try #require(r["text"] as? String)
        let head = try #require((state["selection"] as? [Int])?.first)
        let lineStart = (body as NSString).range(of: "mm = 1;").location
        #expect(head == lineStart)
        // Jumping again selects the same viewer.
        _ = try await doc.model.editor.call("return NeoSCADEditor.definition(pos)", ["pos": at])
        try await Task.sleep(for: .milliseconds(300))
        #expect(LibraryViewer.viewers.count == 1)
        window.close()
        #expect(LibraryViewer.viewers.isEmpty)
        doc.close()
    }

    @Test func goToDefinitionOpensTheUsersOwnFileAsADocument() async throws {
        let dir = FileManager.default.temporaryDirectory
            .appendingPathComponent("neoscad-lsp-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: dir) }
        let parts = dir.appendingPathComponent("parts.scad")
        try "// A peg.\nmodule peg() cylinder(h = 5, r = 1);\n".write(
            to: parts, atomically: true, encoding: .utf8)
        let main = dir.appendingPathComponent("main.scad")
        let text = "include <parts.scad>\npeg();\n"
        try text.write(to: main, atomically: true, encoding: .utf8)
        let doc = try await openDocument(text, file: main)
        try await languageReady(doc)
        let at = (text as NSString).range(of: "peg();").location
        _ = try await doc.model.editor.call("return NeoSCADEditor.definition(pos)", ["pos": at])
        try await waitUntil("parts.scad opens as a document") {
            NSDocumentController.shared.documents.contains {
                $0.fileURL?.resolvingSymlinksInPath() == parts.resolvingSymlinksInPath()
            }
        }
        let other = try #require(
            NSDocumentController.shared.documents.first {
                $0.fileURL?.resolvingSymlinksInPath() == parts.resolvingSymlinksInPath()
            } as? SCADDocument)
        try await waitUntil("its editor is ready") { other.model.editor.isReady }
        let state = try await editorState(other)
        #expect(state["readOnly"] as? Bool == false)
        other.close()
        doc.close()
    }

    @Test func optionShiftFFormats() async throws {
        let doc = try await openDocument("cube( 1 );\n")
        try await languageReady(doc)
        // ⌥⇧F on a US layout: the key "Ï", key code 70 (F).
        let taken = try await doc.model.editor.call(
            "return NeoSCADEditor.key({key: 'Ï', code: 'KeyF', keyCode: 70, altKey: true, shiftKey: true})")
        #expect(taken as? Bool == true)
        try await waitUntil("the document is formatted") { doc.model.text == "cube(1);\n" }
        doc.close()
    }

    @Test func formattingChangesTheDocument() async throws {
        let doc = try await openDocument("module m(){cube( 1 );}\n")
        try await languageReady(doc)
        let text = try await doc.model.editor.call("return await NeoSCADEditor.format()")
        #expect(text as? String == "module m() {\n    cube(1);\n}\n")
        try await waitUntil("the document has the formatted text") {
            doc.model.text == "module m() {\n    cube(1);\n}\n"
        }
        doc.close()
    }
}

/// Wait until `condition` holds, running `step` (which may await) before
/// each check.
@MainActor
func waitUntil(
    _ what: String, timeout: Duration = .seconds(20), _ condition: () -> Bool,
    while step: () async throws -> Void
) async throws {
    let clock = ContinuousClock()
    let deadline = clock.now + timeout
    while true {
        try await step()
        if condition() { return }
        if clock.now > deadline {
            Issue.record("timed out waiting until \(what)")
            throw CancellationError()
        }
        try await Task.sleep(for: .milliseconds(20))
    }
}

/// Completion and hover on a BOSL2 model through the whole path (the
/// page's client, the bridge, the server and back), warm. Needs BOSL2 at
/// `.reference/BOSL2` in the checkout; prints the numbers.
@MainActor
@Suite(.serialized) struct LanguageLatencyTests {
    @Test func bosl2CompletionAndHoverRoundTrips() async throws {
        let repo = URL(fileURLWithPath: #filePath).deletingLastPathComponent()
            .deletingLastPathComponent().deletingLastPathComponent()
        let std = repo.appendingPathComponent(".reference/BOSL2/std.scad")
        guard FileManager.default.fileExists(atPath: std.path) else {
            print("skipped: no .reference/BOSL2")
            return
        }
        let text = "include <\(std.path)>\ncuboid([20, 10, 5], rounding = 1);\nx = c"
        let doc = try await openDocument(text)
        try await languageReady(doc)
        let r = try #require(
            try await doc.model.editor.call(
                "return await NeoSCADEditor.benchmarkLanguage(end, cuboid, 100)",
                ["end": text.utf16.count, "cuboid": (text as NSString).range(of: "cuboid").location + 2])
                as? [String: Any])
        let c = try #require(r["completion"] as? [String: Double])
        let h = try #require(r["hover"] as? [String: Double])
        print(
            "BOSL2 through the bridge (warm, ms): completion p50 \(c["p50"]!) p95 \(c["p95"]!); hover p50 \(h["p50"]!) p95 \(h["p95"]!)"
        )
        #expect(c["p95"]! < 20 && h["p95"]! < 20)
        doc.close()
    }
}
