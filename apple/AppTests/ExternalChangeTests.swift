// Another program changing the open document's file
// (Document/SCADDocument+Disk.swift), hosted in the app with real windows,
// files and FSEvents: a clean document takes the change as one undoable,
// highlighted edit; a document with unsaved changes shows the bar and
// NSDocument refuses to save over the newer file; Reload is undoable; the
// app's own save is not mistaken for another program's; a deleted file is
// reported.

import AppKit
import NeoSCADCore
import Testing

@testable import NeoSCAD

/// A file `m.scad` holding `text` in a folder of its own, open in a window
/// as File > Open opens it, with the editor loaded.
@MainActor
private func openFile(_ text: String) async throws -> (SCADDocument, URL) {
    let dir = FileManager.default.temporaryDirectory.appendingPathComponent(
        "neoscad-disk-\(UUID().uuidString)")
    try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
    let url = dir.appendingPathComponent("m.scad")
    try Data(text.utf8).write(to: url)
    let (opened, _) = try await NSDocumentController.shared.openDocument(
        withContentsOf: url, display: true)
    let doc = try #require(opened as? SCADDocument)
    let editor = doc.model.editor
    try await waitUntil("the editor is ready") { editor.isReady && editor.version != nil }
    return (doc, url)
}

@MainActor
private func finish(_ doc: SCADDocument, _ url: URL) {
    doc.close()
    try? FileManager.default.removeItem(at: url.deletingLastPathComponent())
}

/// Another program's save: a temporary file renamed over the original.
private func theirSave(_ text: String, to url: URL) throws {
    try Data(text.utf8).write(to: url, options: .atomic)
}

@MainActor
private func agentMarks(_ doc: SCADDocument) async throws -> Int {
    try await doc.model.editor.call("return NeoSCADEditor.agentMarks()") as? Int ?? -1
}

@MainActor
@Suite(.serialized) struct ExternalChangeTests {
    @Test func aCleanDocumentTakesTheChangeAsOneUndoableHighlightedEdit() async throws {
        let (doc, url) = try await openFile("r = 5;\ncube(r);\n")
        defer { finish(doc, url) }
        try theirSave("r = 6;\ncube(r);\n", to: url)
        try await waitUntil("the change shows") { doc.model.text == "r = 6;\ncube(r);\n" }
        #expect(!doc.isDocumentEdited)
        #expect(doc.model.diskNotice == nil)
        let editor = doc.model.editor
        // An edit, not a load: the history has it, and it is marked.
        #expect(editor.undoDepth == 1)
        #expect(try await agentMarks(doc) == 1)
        // Undo brings the old text back, which the file no longer holds.
        editor.undo()
        try await waitUntil("the undo arrives") { doc.model.text == "r = 5;\ncube(r);\n" }
        #expect(doc.isDocumentEdited)
        // Saving that is not a conflict for NSDocument: the change taken in
        // brought its idea of the file's date up to date.
        var saved: Error?? = .none
        doc.save(to: url, ofType: doc.fileType ?? "org.openscad.scad", for: .saveOperation) {
            saved = .some($0)
        }
        try await waitUntil("the save answers") { saved != nil }
        #expect((saved ?? nil) == nil)
        #expect(try String(contentsOf: url, encoding: .utf8) == "r = 5;\ncube(r);\n")
    }

    @Test func nsDocumentsPresenterCallbackTakesTheSamePath() async throws {
        let (doc, url) = try await openFile("cube(1);\n")
        defer { finish(doc, url) }
        doc.watcher.stop()  // only the presenter callback is left
        try Data("cube(2);\n".utf8).write(to: url)
        doc.presentedItemDidChange()
        try await waitUntil("the change shows") { doc.model.text == "cube(2);\n" }
        #expect(doc.model.editor.undoDepth == 1)
        #expect(!doc.isDocumentEdited)
    }

    @Test func aDirtyDocumentShowsTheBarAndReloadIsUndoable() async throws {
        let (doc, url) = try await openFile("cube(1);\n")
        defer { finish(doc, url) }
        try await editInPage(doc, from: 0, to: 0, insert: "// mine\n")
        #expect(doc.isDocumentEdited)
        try theirSave("cube(3);\n", to: url)
        try await waitUntil("the bar shows") { doc.model.diskNotice != nil }
        #expect(doc.model.diskNotice == .changed(reloadable: true))
        #expect(doc.model.text == "// mine\ncube(1);\n")

        // Reload: theirs, as one step; the document is clean.
        doc.model.actions.reloadFromDisk()
        try await waitUntil("the reload lands") { doc.model.text == "cube(3);\n" }
        #expect(!doc.isDocumentEdited)
        #expect(doc.model.diskNotice == nil)
        // Undo brings the user's text back.
        doc.model.editor.undo()
        try await waitUntil("the undo arrives") { doc.model.text == "// mine\ncube(1);\n" }
        #expect(doc.isDocumentEdited)
    }

    @Test func keepMineLeavesTheFileAndSaveDoesNotOverwriteUnasked() async throws {
        let (doc, url) = try await openFile("cube(1);\n")
        defer { finish(doc, url) }
        try await editInPage(doc, from: 0, to: 0, insert: "// mine\n")
        try theirSave("cube(4);\n", to: url)
        try await waitUntil("the bar shows") { doc.model.diskNotice != nil }
        doc.model.actions.keepMine()
        #expect(doc.model.diskNotice == nil)
        #expect(doc.model.text == "// mine\ncube(1);\n")
        // Autosave in place refuses (NSDocument's own check)...
        var autosaved: Error?? = .none
        doc.autosave(withImplicitCancellability: false) { autosaved = .some($0) }
        try await waitUntil("the autosave answers") { autosaved != nil }
        #expect((autosaved ?? nil) != nil)
        #expect(try String(contentsOf: url, encoding: .utf8) == "cube(4);\n")
        // ...and Save asks (its sheet), leaving the file as it is.
        doc.save(nil)
        let window = try #require(doc.windowForSheet)
        try await waitUntil("the sheet shows") { window.attachedSheet != nil }
        #expect(try String(contentsOf: url, encoding: .utf8) == "cube(4);\n")
        if let sheet = window.attachedSheet { window.endSheet(sheet) }
    }

    @Test func theAppsOwnSaveIsNotAnotherProgramsChange() async throws {
        let (doc, url) = try await openFile("cube(1);\n")
        defer { finish(doc, url) }
        try await editInPage(doc, from: 0, to: 0, insert: "// mine\n")
        var saved: Error?? = .none
        doc.save(to: url, ofType: doc.fileType ?? "org.openscad.scad", for: .saveOperation) {
            saved = .some($0)
        }
        try await waitUntil("the save answers") { saved != nil }
        #expect((saved ?? nil) == nil)
        #expect(!doc.isDocumentEdited)
        // FSEvents reports the write; nothing comes of it.
        try await Task.sleep(for: .seconds(1))
        #expect(doc.model.diskNotice == nil)
        #expect(doc.model.editor.undoDepth == 1)
        #expect(doc.model.text == "// mine\ncube(1);\n")
        // And a change after it is taken in as usual.
        try theirSave("cube(5);\n", to: url)
        try await waitUntil("the change shows") { doc.model.text == "cube(5);\n" }
    }

    @Test func aDeletedFileIsReported() async throws {
        let (doc, url) = try await openFile("cube(1);\n")
        defer { finish(doc, url) }
        try FileManager.default.removeItem(at: url)
        try await waitUntil("the bar shows") { doc.model.diskNotice != nil }
        #expect(doc.model.diskNotice == .missing)
        #expect(doc.model.text == "cube(1);\n")
    }
}
