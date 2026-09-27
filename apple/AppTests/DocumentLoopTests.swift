// The document loop in the app (Document/DocumentLoop.swift): one run per
// pause feeding the markers and the console; the console's jump to a
// span; the customizer's values, reset and parameter sets; a re-run when
// an included file changes on disk; closing a window stopping its work;
// untitled documents' include directory; and autosave and revert.

import AppKit
import NeoSCADCore
import Testing

@testable import NeoSCAD

private let scad = "org.openscad.scad"

/// A directory of its own, removed at the end.
private func tempDirectory(_ name: String) throws -> URL {
    let dir = FileManager.default.temporaryDirectory
        .appendingPathComponent("neoscad-\(name)-\(UUID().uuidString)")
    try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
    return dir.resolvingSymlinksInPath()
}

/// Wait until a run started after `count` runs has reported.
@MainActor
private func waitForRun(_ doc: SCADDocument, after count: Int, _ what: String = "a run") async throws {
    try await waitUntil(what) {
        guard doc.requestCount > count, case .rendered = doc.model.report else { return false }
        return true
    }
}

@MainActor
@Suite(.serialized) struct DocumentLoopTests {
    @Test func onePauseRunsOnceAndFeedsMarkersAndConsole() async throws {
        let doc = try await openDocument("cube(1);\n")
        try await waitForRun(doc, after: 0)
        let runs = doc.requestCount
        let pubs = doc.model.editor.languageClient?.publications ?? 0
        let end = doc.model.utf16Length
        try await editInPage(doc, from: end, to: end, insert: "cubx(2);\necho(\"hi\");\n")
        #expect(try await lintMarkers(doc, count: 1) == ["cubx(2);"])
        try await waitForRun(doc, after: runs)
        // One run for the pause, and the markers are its.
        #expect(doc.requestCount == runs + 1)
        #expect((doc.model.editor.languageClient?.publications ?? 0) > pubs)
        #expect(doc.model.console.contains { $0.kind == .echo && $0.text == "ECHO: \"hi\"" })
        let warning = try #require(doc.model.console.first { $0.kind == .warning })
        #expect(warning.location?.startLine == 1)
        doc.close()
    }

    @Test func aGeometryWarningReachesTheMarkers() async throws {
        let doc = try await openDocument("union() { cube(1); square(1); }\n")
        doc.renderDocument(nil)
        await doc.renderTask?.value
        let shown = try await lintMarkers(doc, count: 1)
        #expect(shown == ["square(1);"])
        doc.close()
    }

    @Test func clickingAConsoleLineSelectsItsSpan() async throws {
        // "é" is two bytes and one UTF-16 unit: the span must be the
        // editor's.
        let text = "cube(1);\nx = \"é\"; cub(2);\n"
        let doc = try await openDocument(text)
        try await waitUntil("the warning is in the console") {
            doc.model.console.contains { $0.location != nil }
        }
        let line = try #require(doc.model.console.first { $0.location != nil })
        // What the row's click does.
        doc.model.actions.jump(try #require(line.location))
        let start = (text as NSString).range(of: "cub(2)").location
        var state: [String: Any] = [:]
        try await waitUntil("the span is selected") {
            (state["selection"] as? [Int])?.first == start
        } while: {
            state = try await editorState(doc)
        }
        let sel = try #require(state["selection"] as? [Int])
        #expect(sel[1] > sel[0])
        doc.close()
    }

    @Test func theCustomizerRerunsWithoutTouchingTheText() async throws {
        let text = "/* [Size] */\nsize = 10; // [1:20]\nshape = \"round\"; // [round, square]\necho(size, shape);\ncube(size);\n"
        let doc = try await openDocument(text)
        try await waitUntil("the parameters appear") { !doc.model.parameterGroups.isEmpty }
        let group = try #require(doc.model.parameterGroups.first)
        #expect(group.name == "Size")
        #expect(group.parameters.map(\.name) == ["size", "shape"])
        guard case .slider(let min, let max, _) = group.parameters[0].control else {
            Issue.record("expected a slider, got \(group.parameters[0].control)")
            return
        }
        #expect(min == 1 && max == 20)
        var runs = doc.requestCount
        doc.model.actions.setParameter("size", .number(value: 3))
        doc.model.actions.setParameter("shape", .text(value: "square"))
        try await waitUntil("the edited values run") {
            doc.model.console.contains { $0.text == "ECHO: 3, \"square\"" }
        }
        #expect(doc.requestCount > runs)
        #expect(doc.model.text == text)
        #expect(!doc.isDocumentEdited)
        runs = doc.requestCount
        doc.model.actions.resetParameters()
        try await waitUntil("the text's values run again") {
            doc.model.console.contains { $0.text == "ECHO: 10, \"round\"" }
        }
        #expect(doc.model.parameterValues.isEmpty)
        doc.close()
    }

    @Test func parameterSetsAreSavedAndAppliedBesideTheModel() async throws {
        let dir = try tempDirectory("sets")
        defer { try? FileManager.default.removeItem(at: dir) }
        let file = dir.appendingPathComponent("box.scad")
        let text = "width = 10; // [1:100]\necho(width);\ncube(width);\n"
        try text.write(to: file, atomically: true, encoding: .utf8)
        let doc = try await openDocument(text, file: file)
        try await waitUntil("the parameters appear") { !doc.model.parameterGroups.isEmpty }
        #expect(doc.model.parameterSetsAvailable)
        doc.model.actions.setParameter("width", .number(value: 42))
        doc.saveParameterSet(named: "wide")
        let json = dir.appendingPathComponent("box.json")
        let written = try String(contentsOf: json, encoding: .utf8)
        #expect(written.contains("\"wide\""))
        #expect(written.contains("\"width\": \"42\""))
        #expect(doc.model.parameterSets == ["wide"])
        doc.model.actions.resetParameters()
        try await waitUntil("back to the text's value") {
            doc.model.console.contains { $0.text == "ECHO: 10" }
        }
        doc.model.actions.applyParameterSet("wide")
        #expect(doc.model.selectedParameterSet == "wide")
        try await waitUntil("the set's value runs") {
            doc.model.console.contains { $0.text == "ECHO: 42" }
        }
        doc.close()
    }

    @Test func aChangedIncludeOnDiskRerunsTheDocument() async throws {
        let dir = try tempDirectory("watch")
        defer { try? FileManager.default.removeItem(at: dir) }
        let parts = dir.appendingPathComponent("parts.scad")
        try "echo(\"v1\");\n".write(to: parts, atomically: true, encoding: .utf8)
        let main = dir.appendingPathComponent("main.scad")
        let text = "include <parts.scad>\ncube(1);\n"
        try text.write(to: main, atomically: true, encoding: .utf8)
        let doc = try await openDocument(text, file: main)
        try await waitUntil("the first run") {
            doc.model.console.contains { $0.text == "ECHO: \"v1\"" }
        }
        try await waitUntil("the include is watched") {
            doc.watcher.files.contains(parts.path)
        }
        // Saved by another editor: written aside and renamed over it.
        try "echo(\"v2\");\n".write(to: parts, atomically: true, encoding: .utf8)
        try await waitUntil("the document runs again") {
            doc.model.console.contains { $0.text == "ECHO: \"v2\"" }
        }
        doc.close()
    }

    @Test func closingADocumentStopsItsWork() async throws {
        // About ten seconds of evaluation.
        let text = "n = 9000;\necho(len([for (i = [0:n]) for (j = [0:n]) if (i * j < 0) 1]));\n"
        let doc = try await openDocument(text)
        let engine = try CoreService.shared.get()
        try await waitUntil("the run is under way") {
            guard let p = doc.corePath else { return false }
            return ((try? engine.core.running(path: p)) ?? 0) > 0
        }
        let path = try #require(doc.corePath)
        let clock = ContinuousClock()
        let t0 = clock.now
        doc.close()
        try await waitUntil("the core's requests stop", timeout: .seconds(3)) {
            ((try? engine.core.running(path: path)) ?? 0) == 0
        }
        #expect(clock.now - t0 < .seconds(3))
        // Nothing starts again: the watcher and the pending run are gone.
        let runs = doc.requestCount
        doc.previewDocument(nil)
        #expect(doc.requestCount == runs)
        #expect(doc.watcher.files.isEmpty)
    }

    @Test func anUntitledDocumentLivesInTheCurrentDirectory() async throws {
        let doc = try await openDocument("cube(1);\n")
        try await waitForRun(doc, after: 0)
        let path = try #require(doc.corePath)
        let dir =
            NSDocumentController.shared.currentDirectory
            ?? FileManager.default.urls(for: .documentDirectory, in: .userDomainMask).first!.path
        #expect((path as NSString).deletingLastPathComponent == dir)
        #expect((path as NSString).lastPathComponent.hasPrefix("Untitled"))
        #expect(!FileManager.default.fileExists(atPath: path))
        // A second untitled document gets a path of its own.
        let other = try await openDocument("sphere(1);\n")
        try await waitForRun(other, after: 0)
        #expect(other.corePath != path)
        other.close()
        doc.close()
    }

    @Test func autosaveInPlaceSavesAndARevertLoadsTheText() async throws {
        #expect(SCADDocument.autosavesInPlace)
        let dir = try tempDirectory("versions")
        defer { try? FileManager.default.removeItem(at: dir) }
        let file = dir.appendingPathComponent("v.scad")
        try "cube(1);\n".write(to: file, atomically: true, encoding: .utf8)
        let doc = try await openDocument("cube(1);\n", file: file)
        doc.fileType = scad
        doc.fileModificationDate = try FileManager.default
            .attributesOfItem(atPath: file.path)[.modificationDate] as? Date
        try await editInPage(doc, from: 5, to: 6, insert: "2")
        #expect(doc.isDocumentEdited)
        try await doc.save(to: file, ofType: scad, for: .saveOperation)
        #expect(try String(contentsOf: file, encoding: .utf8) == "cube(2);\n")
        #expect(!doc.isDocumentEdited)
        // Reverting to other contents (what Revert To and the versions
        // browser do with a stored version) loads that text into the
        // editor. The version store itself keeps no versions of files in
        // the temporary directory (NSFileVersion lists none there), so
        // browsing versions is on the manual checklist instead
        // (docs/followups.md, "macOS app").
        let old = dir.appendingPathComponent("old.scad")
        try "cube(1);\n".write(to: old, atomically: true, encoding: .utf8)
        try doc.revert(toContentsOf: old, ofType: scad)
        #expect(doc.model.text == "cube(1);\n")
        try await waitUntil("the editor shows the reverted text") {
            doc.model.editor.version != nil
        }
        let r = try #require(try await doc.model.editor.call("return NeoSCADEditor.text()") as? [String: Any])
        #expect(r["text"] as? String == "cube(1);\n")
        doc.close()
    }
}
