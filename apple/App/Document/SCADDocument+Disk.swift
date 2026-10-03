// The document's own file, changed by another program: an AI agent
// through `neoscad mcp`, or another editor.
//
// What NSDocument does by itself (checked by a hosted test with the app in
// the background, 2026-10-02): it did not take in a plain write or a
// rename-over within 8 seconds, so the window kept showing the old text.
// For a document with unsaved changes it refuses to save over the newer
// file, both on autosave ("could not be autosaved. The file has been
// changed by another application") and on Save (a sheet: Save Anyway,
// Revert, Save As). Its Revert reads the file anew (`read(from:)`, then
// `editor.load`), which clears the editor's undo history.
//
// So this keeps NSDocument's save-time conflict sheet, which already
// guards against overwriting, and adds what it lacks, as the Linux and
// Windows apps do (the decisions are the core's `DocumentFile`,
// crates/client/src/disk.rs):
//
// - the document's file is watched (FileWatcher, with the run's files);
// - a clean document takes the change in place, as edits the editor's
//   `agentEdit` applies as one undoable, highlighted step, and is clean
//   again once they land;
// - a document with unsaved changes shows a bar over the editor: Reload
//   (the file's text, as one undoable step) or Keep Mine (Save then shows
//   NSDocument's sheet before overwriting);
// - a file removed from under the document is said so.
//
// NSDocument's own file-presenter notification is routed here too, so the
// reload never goes through Revert and the undo history survives.

import AppKit
import NeoSCADCore

/// What the bar over the editor says about the file on disk.
enum DiskNotice: Equatable {
    /// Changed under unsaved edits; Reload only when it is UTF-8 text.
    case changed(reloadable: Bool)
    /// Deleted, or moved where the document cannot follow.
    case missing
}

extension SCADDocument {
    /// Watch `files` (the last run's) and the document's own file.
    func watchFiles(_ files: [String]) {
        runFiles = files
        watcher.watch(files, document: fileURL?.path)
    }

    /// The file changed on disk (FileWatcher, or NSDocument's presenter
    /// callback): take the change in, or report it. Our own saves come
    /// back here too, and the core recognises them.
    func documentFileChanged() {
        guard !isClosed, let url = fileURL else { return }
        let action: DiskAction
        do {
            action = try diskFile.check(
                path: url.path, text: model.editorText, dirty: isDocumentEdited)
        } catch {
            return
        }
        switch action {
        case .none:
            break
        case .readAgain(let ms):
            diskReadAgain?.cancel()
            diskReadAgain = Task { [weak self] in
                try? await Task.sleep(for: .milliseconds(Int(ms)))
                guard !Task.isCancelled else { return }
                self?.documentFileChanged()
            }
        case .saved:
            updateChangeCount(.changeCleared)
            syncModificationDate()
            model.diskNotice = nil
        case .resolved:
            model.diskNotice = nil
        case .reload(let edits):
            applyReload(edits)
        case .conflict(let reloadable):
            model.diskNotice = .changed(reloadable: reloadable)
        case .missing:
            model.diskNotice = .missing
        }
    }

    /// The bar's Reload: the file's text replaces the document's, as one
    /// undoable step.
    func reloadFromDisk() {
        guard let url = fileURL,
            let edits = (try? diskFile.reload(path: url.path, text: model.editorText)) ?? nil
        else {
            NSSound.beep()
            return
        }
        applyReload(edits)
    }

    /// The bar's Keep Mine: the bar goes; Save then asks before
    /// overwriting (NSDocument's sheet).
    func keepMine() {
        try? diskFile.keepMine()
        model.diskNotice = nil
    }

    /// Apply a reload through the editor, which reports it back as a change
    /// (`editorChanged`, then `reloadLanded`); with no editor up (a
    /// document not shown, a page still loading), to the copy, which the
    /// editor then loads.
    func applyReload(_ edits: [ReloadEdit]) {
        model.diskNotice = nil
        let editor = model.editor
        if editor.isReady, editor.version != nil {
            editor.agentEdit(reloadEditsJson(edits: edits))
            return
        }
        // Setting the text loads it into the editor and schedules a run.
        model.text = applyReloadEdits(text: model.text, edits: edits)
        reloadLanded()
    }

    /// After the copy changed: a reload landing makes the document clean.
    func reloadLanded() {
        guard (try? diskFile.editorChanged(text: model.editorText)) == true else { return }
        updateChangeCount(.changeCleared)
        syncModificationDate()
        model.diskNotice = nil
    }

    /// The document now holds what the file holds: NSDocument, which
    /// compares this date with the file's before saving, must not take
    /// the change just taken in for one still to resolve.
    func syncModificationDate() {
        guard let url = fileURL,
            let date = try? url.resourceValues(forKeys: [.contentModificationDateKey])
                .contentModificationDate
        else { return }
        fileModificationDate = date
    }

    // MARK: NSDocument

    /// NSDocument's own reaction is to revert, which reads the file anew
    /// and clears the editor's undo history; the same check as the
    /// watcher's runs instead (on the main actor: this is called on the
    /// presenter's queue).
    override func presentedItemDidChange() {
        DispatchQueue.main.async { [weak self] in
            MainActor.assumeIsolated { self?.documentFileChanged() }
        }
    }

    /// A write of the document's file (Save, Save As, autosave in place)
    /// records what it wrote, so the watcher's report of it is known for
    /// the document's own. NSDocument's check against a newer file comes
    /// first, inside `super`.
    override func save(
        to url: URL, ofType typeName: String, for saveOperation: NSDocument.SaveOperationType,
        completionHandler: @escaping (Error?) -> Void
    ) {
        let records =
            saveOperation == .saveOperation || saveOperation == .saveAsOperation
            || saveOperation == .autosaveInPlaceOperation
        // `data(ofType:)` kept the bytes it handed NSDocument for this
        // write: what the file holds once it succeeds.
        super.save(to: url, ofType: typeName, for: saveOperation) { [weak self] error in
            let record = { @MainActor in
                guard let self else { return }
                if error == nil, records, let data = self.bytesBeingWritten {
                    try? self.diskFile.saved(bytes: data)
                    self.model.diskNotice = nil
                }
                self.bytesBeingWritten = nil
            }
            if Thread.isMainThread {
                MainActor.assumeIsolated { record() }
            } else {
                DispatchQueue.main.async { MainActor.assumeIsolated { record() } }
            }
            completionHandler(error)
        }
    }
}
