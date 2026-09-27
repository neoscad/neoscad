// A `.scad` file: UTF-8 text, edited in a window of its own.
//
// Three copies of the text stay in step. The editor's (CodeMirror, in a
// web view) is the one being edited; each of its changes is applied to the
// document's copy (`DocumentModel.text`), which is what NSDocument saves,
// and forwarded to the Rust core's (`Core.edit`, in UTF-8 byte offsets),
// which is what renders. Editor/EditorController.swift explains the choice.
// The core's copy lives under the document's path, so includes of this
// file from other open files see the unsaved text too.
//
// Change tracking: the editor's undo history is the only one (the
// document has no undo manager; Editor/EditorWebView.swift says why).
// Each change counts as done, undone or redone, as the editor reports it,
// so undoing back to the saved text clears the edited dot, and autosave
// saves whenever NSDocument sees unsaved changes.
//
// Updating the 3D view (a stand-in for 8f's document loop): Design >
// Preview (F5) and Render (F6) render into the viewport, and every text
// change schedules a preview after a short pause. Each request supersedes
// the one before: the session cancels a document's older requests, and the
// viewport ignores a model from an older request that finishes late.

import AppKit
import NeoSCADCore
import SwiftUI

/// The state a document window shows. Main-actor only: AppKit reads and
/// writes documents on the main thread here (concurrent reading is off).
@MainActor
@Observable
final class DocumentModel {
    /// The document's text. Setting it replaces the text everywhere (the
    /// file was read); the editor's changes arrive through
    /// `applyEditorEdits` instead. Not observed: no view shows it (the
    /// editor has its own copy), and observing it would cost a SwiftUI
    /// update per keystroke.
    var text: String {
        get { storage }
        set {
            setStorage(newValue)
            textReplaced?()
        }
    }
    var report: RenderReport = .idle
    /// The 3D view's state (the Rust viewport and its view).
    @ObservationIgnored let viewport = ViewportController()
    /// The editor (CodeMirror in a web view) and its bridge.
    @ObservationIgnored let editor = EditorController()
    @ObservationIgnored var textReplaced: (() -> Void)?
    /// The lint markers last sent to the editor (the app tests read them).
    @ObservationIgnored var diagnostics: [EditorDiagnostic] = []

    @ObservationIgnored private var storage = ""
    /// The text's length in UTF-16 units, kept with each edit: counting a
    /// long text's UTF-16 units anew for each keystroke is a scan of it.
    @ObservationIgnored private(set) var utf16Length = 0

    private func setStorage(_ text: String) {
        storage = text
        // Native UTF-8, so byte offsets into it are arithmetic, not scans
        // (a string bridged from the web view is UTF-16 inside).
        storage.makeContiguousUTF8()
        utf16Length = storage.utf16.count
    }

    /// Apply the editor's edits (UTF-16 offsets) and return them in the
    /// core's UTF-8 offsets. Throws if they do not fit this text, which is
    /// then out of step until `replaceWithEditorText`.
    func applyEditorEdits(_ edits: [UTF16Edit]) throws -> [TextEdit] {
        let out = try TextOffsets.apply(edits, to: &storage)
        for e in edits { utf16Length += e.insert.utf16.count - (e.to - e.from) }
        return out
    }

    /// Take the editor's whole text, after the copies disagreed.
    func replaceWithEditorText(_ text: String) {
        setStorage(text)
    }
}

/// What the console area shows about the last render.
enum RenderReport {
    case idle
    case running(RenderMode)
    case rendered(RenderResult, RenderMode)
    case failed(String)
}

@objc(SCADDocument)
final class SCADDocument: NSDocument {
    let model = DocumentModel()

    /// The path the core knows this document by, once it has rendered:
    /// the file's, or for an untitled document a path of its own that
    /// exists nowhere on disk.
    private(set) var corePath: String?
    /// The same path as the core normalised it, which its diagnostics name.
    private var coreDocumentPath: String?
    /// Whether the core's copy of the text is the document's: edits are
    /// forwarded only then, and the next request sends the whole text
    /// otherwise.
    private(set) var coreInSync = false
    private lazy var untitledPath =
        "/NeoSCAD-untitled/\(UUID().uuidString)/Untitled.scad"
    /// The render in flight (the app tests await it).
    private(set) var renderTask: Task<Void, Never>?
    /// How many renders and previews were started (the app tests count
    /// them, so a key that reached two handlers would show).
    private(set) var requestCount = 0
    /// The preview waiting for typing to pause.
    private var pendingPreview: Task<Void, Never>?
    /// What the last request built, so a scheme change can rebuild it.
    private var lastMode: RenderMode?

    /// How long typing must pause before the model is previewed again.
    static let previewDelay: Duration = .milliseconds(400)

    override init() {
        super.init()
        hasUndoManager = false
        MainActor.assumeIsolated {
            model.textReplaced = { [weak self] in self?.textReplaced() }
            model.viewport.onSchemeChange = { [weak self] in self?.rerender() }
            let editor = model.editor
            editor.text = { [weak self] in self?.model.text ?? "" }
            editor.applyChanges = { [weak self] edits, kind, length in
                self?.editorChanged(edits, kind: kind, length: length) ?? false
            }
            editor.replaceText = { [weak self] text in self?.editorReplacedText(text) }
            editor.perform = { [weak self] command in
                switch command {
                case "preview": self?.previewDocument(nil)
                case "render": self?.renderDocument(nil)
                default: break
                }
            }
        }
    }

    override class var autosavesInPlace: Bool { true }

    override func makeWindowControllers() {
        let window = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 1200, height: 760),
            styleMask: [.titled, .closable, .miniaturizable, .resizable],
            backing: .buffered, defer: false)
        window.contentViewController = NSHostingController(
            rootView: DocumentView(model: model))
        window.setContentSize(NSSize(width: 1200, height: 760))
        let controller = NSWindowController(window: window)
        controller.shouldCascadeWindows = true
        addWindowController(controller)
        window.center()
        // Typing goes to the editor from the start.
        window.initialFirstResponder = model.editor.webView
    }

    // MARK: Reading and writing

    override func data(ofType typeName: String) throws -> Data {
        MainActor.assumeIsolated { Data(model.text.utf8) }
    }

    /// UTF-8 only, as OpenSCAD reads files. Text that is not UTF-8 is
    /// refused rather than decoded lossily: saving it back would silently
    /// replace the bytes that did not decode.
    override func read(from data: Data, ofType typeName: String) throws {
        guard let text = String(data: data, encoding: .utf8) else {
            throw CocoaError(.fileReadInapplicableStringEncoding)
        }
        MainActor.assumeIsolated { model.text = text }
    }

    // MARK: Edits

    /// The text was read (open, revert): show it and render it.
    private func textReplaced() {
        model.editor.load(model.text)
        coreInSync = false
        schedulePreview()
    }

    /// One editor transaction: apply it to the document's copy, count it
    /// for NSDocument, and forward it to the core. False if it does not
    /// fit the copy, which the editor then replaces.
    private func editorChanged(_ edits: [UTF16Edit], kind: EditKind, length: Int) -> Bool {
        let coreEdits: [TextEdit]
        do {
            coreEdits = try model.applyEditorEdits(edits)
        } catch {
            return false
        }
        guard model.utf16Length == length else { return false }
        updateChangeCount(kind.changeType)
        if coreInSync, let path = corePath, case .success(let engine) = CoreService.shared {
            do {
                let info = try engine.edit(path, edits: coreEdits)
                // The byte count is a cheap check that both copies agree.
                if info.length != UInt64(model.text.utf8.count) { coreInSync = false }
            } catch {
                coreInSync = false
            }
        }
        schedulePreview()
        return true
    }

    /// The copies disagreed, so the editor's text replaced the document's.
    /// The editor had changes this copy missed (that is how they come to
    /// disagree), so the document counts one: marking it edited when it is
    /// not costs an autosave of the same text, while missing a change
    /// would leave it unsaved.
    private func editorReplacedText(_ text: String) {
        model.replaceWithEditorText(text)
        updateChangeCount(.changeDone)
        coreInSync = false
        schedulePreview()
    }

    // MARK: Render and preview

    /// Design > Render (F6): the full geometry, its statistics in the
    /// console and the mesh in the 3D view.
    @objc func renderDocument(_ sender: Any?) {
        run(.render)
    }

    /// Design > Preview (F5): OpenSCAD's fast preview (the CSG products,
    /// with `#` and `%` objects) in the 3D view.
    @objc func previewDocument(_ sender: Any?) {
        run(.preview)
    }

    /// A preview once typing pauses; each keystroke restarts the wait.
    private func schedulePreview() {
        pendingPreview?.cancel()
        pendingPreview = Task { @MainActor [weak self] in
            try? await Task.sleep(for: Self.previewDelay)
            guard !Task.isCancelled else { return }
            self?.run(.preview)
        }
    }

    /// The last request again (after the colour scheme changed).
    private func rerender() {
        if let mode = lastMode { run(mode) }
    }

    /// Render the current text in the core, off the main actor, into the
    /// 3D view; show the statistics and diagnostics in the console area.
    /// A newer request supersedes an unfinished one.
    private func run(_ mode: RenderMode) {
        pendingPreview?.cancel()
        pendingPreview = nil
        let engine: Engine
        switch CoreService.shared {
        case .success(let e): engine = e
        case .failure(let e):
            model.report = .failed("The core did not start: \(e.message)")
            return
        }
        let path = fileURL?.path ?? untitledPath
        if let old = corePath, old != path {
            // Saved under a new name: the old buffer would shadow the file
            // that is still on disk at the old path.
            _ = try? engine.close(old)
            coreInSync = false
        }
        corePath = path
        if !coreInSync {
            do {
                coreDocumentPath = try engine.update(path, text: model.text).path
                coreInSync = true
            } catch let e as CoreError {
                model.report = .failed(e.message)
                return
            } catch {
                model.report = .failed("\(error)")
                return
            }
        }
        renderTask?.cancel()
        requestCount += 1
        lastMode = mode
        model.report = .running(mode)
        let model = self.model
        let viewport = model.viewport
        // The text and the editor version the core renders, for placing
        // its diagnostics in the editor (which maps them through any
        // typing done meanwhile).
        let text = model.text
        let version = model.editor.version
        let documentPath = coreDocumentPath ?? path
        renderTask = Task { @MainActor in
            do {
                let result: RenderResult
                if let v = viewport.viewport {
                    result = try await engine.render(path, mode: mode, into: v)
                    viewport.view?.requestFrame()
                } else {
                    result = try await engine.render(path, mode: mode)
                }
                model.report = .rendered(result, mode)
                Self.publish(result.diagnostics, of: documentPath, text: text, version: version, to: model)
            } catch CoreError.Cancelled {
                // A newer request took over; it reports instead.
            } catch let e as CoreError {
                model.report = .failed(e.message)
                Self.publish([], of: documentPath, text: text, version: version, to: model)
            } catch {
                model.report = .failed("\(error)")
                Self.publish([], of: documentPath, text: text, version: version, to: model)
            }
        }
    }

    /// Show a request's diagnostics as the editor's lint markers.
    private static func publish(
        _ diagnostics: [Diagnostic], of path: String, text: String, version: Int?,
        to model: DocumentModel
    ) {
        let list =
            diagnostics.isEmpty
            ? [] : EditorDiagnostic.from(diagnostics, path: path, lines: SourceLines(text))
        model.diagnostics = list
        // Without a version the editor is being reloaded, and the text these
        // ranges are in may not be its text.
        if let version { model.editor.setDiagnostics(list, version: version) }
    }

    // MARK: View menu

    @objc func showTop(_ sender: Any?) { preset(.top) }
    @objc func showBottom(_ sender: Any?) { preset(.bottom) }
    @objc func showLeft(_ sender: Any?) { preset(.left) }
    @objc func showRight(_ sender: Any?) { preset(.right) }
    @objc func showFront(_ sender: Any?) { preset(.front) }
    @objc func showBack(_ sender: Any?) { preset(.back) }
    @objc func showDiagonal(_ sender: Any?) { preset(.diagonal) }

    private func preset(_ p: ViewPreset) {
        model.viewport.perform { try $0.setView(preset: p) }
    }

    @objc func centerView(_ sender: Any?) {
        model.viewport.perform { try $0.center() }
    }

    @objc func viewAll(_ sender: Any?) {
        model.viewport.perform { try $0.viewAll() }
    }

    @objc func resetView(_ sender: Any?) {
        model.viewport.perform { try $0.resetView() }
    }

    @objc func zoomIn(_ sender: Any?) {
        model.viewport.perform { try $0.zoom(notches: 1) }
    }

    @objc func zoomOut(_ sender: Any?) {
        model.viewport.perform { try $0.zoom(notches: -1) }
    }

    @objc func toggleAxes(_ sender: Any?) { model.viewport.update { $0.axes.toggle() } }
    @objc func toggleScaleMarkers(_ sender: Any?) { model.viewport.update { $0.scales.toggle() } }
    @objc func toggleGrid(_ sender: Any?) { model.viewport.update { $0.grid.toggle() } }
    @objc func toggleEdges(_ sender: Any?) { model.viewport.update { $0.edges.toggle() } }
    @objc func toggleCrosshairs(_ sender: Any?) {
        model.viewport.update { $0.crosshairs.toggle() }
    }
    @objc func usePerspective(_ sender: Any?) {
        model.viewport.update { $0.orthographic = false }
    }
    @objc func useOrthographic(_ sender: Any?) {
        model.viewport.update { $0.orthographic = true }
    }
    @objc func useOpenSCADLighting(_ sender: Any?) {
        model.viewport.update { $0.lighting = .openScad }
    }
    @objc func useHeadlight(_ sender: Any?) {
        model.viewport.update { $0.lighting = .headlight }
    }

    /// Check marks for the View menu's toggles and choices.
    override func validateMenuItem(_ item: NSMenuItem) -> Bool {
        guard let s = model.viewport.settings else {
            return super.validateMenuItem(item)
        }
        let on: Bool? =
            switch item.action {
            case #selector(toggleAxes(_:)): s.axes
            case #selector(toggleScaleMarkers(_:)): s.scales
            case #selector(toggleGrid(_:)): s.grid
            case #selector(toggleEdges(_:)): s.edges
            case #selector(toggleCrosshairs(_:)): s.crosshairs
            case #selector(usePerspective(_:)): !s.orthographic
            case #selector(useOrthographic(_:)): s.orthographic
            case #selector(useOpenSCADLighting(_:)): s.lighting == .openScad
            case #selector(useHeadlight(_:)): s.lighting == .headlight
            default: nil
            }
        if let on {
            item.state = on ? .on : .off
            return true
        }
        return super.validateMenuItem(item)
    }

    override func close() {
        pendingPreview?.cancel()
        renderTask?.cancel()
        model.viewport.detach()
        model.editor.detach()
        if let path = corePath, case .success(let engine) = CoreService.shared {
            _ = try? engine.close(path)
        }
        super.close()
    }
}
