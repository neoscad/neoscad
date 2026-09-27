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
// The document loop (Document/DocumentLoop.swift): each pause in typing,
// customizer edit or change on disk to a file the model read runs the
// document once, and that one run feeds the 3D view, the console, the
// customizer and the editor's markers.

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
    /// The last run's console lines.
    var console: [ConsoleLine] = []
    /// The customizer: the document's parameter groups, the values edited
    /// away from the text's (by name), and the parameter sets of the file
    /// beside the model.
    var parameterGroups: [ParameterGroup] = []
    var parameterValues: [String: ParameterValue] = [:]
    var parameterSets: [String] = []
    var selectedParameterSet: String?
    /// Whether the customizer and the console list are shown.
    var customizerShown = true
    var consoleCollapsed = false
    /// Whether parameter sets can be read and written: the document has a
    /// file, next to which `name.json` lives.
    var parameterSetsAvailable = false

    /// The 3D view's state (the Rust viewport and its view).
    @ObservationIgnored let viewport = ViewportController()
    /// The editor (CodeMirror in a web view) and its bridge.
    @ObservationIgnored let editor = EditorController()
    @ObservationIgnored var textReplaced: (() -> Void)?
    /// What the panels ask of their document (set by it).
    @ObservationIgnored var actions = DocumentActions()

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

    /// A parameter's value as the customizer shows it: the edited one, or
    /// the text's.
    func value(of p: Parameter) -> ParameterValue {
        parameterValues[p.name] ?? p.defaultValue
    }
}

/// What the console and the customizer ask of their document.
@MainActor
struct DocumentActions {
    /// Show a console line's span in the editor (or its file's window).
    var jump: (SourceRange) -> Void = { _ in }
    /// A customizer value was edited (`nil`: back to the text's).
    var setParameter: (String, ParameterValue?) -> Void = { _, _ in }
    /// Every value back to the text's.
    var resetParameters: () -> Void = {}
    var applyParameterSet: (String) -> Void = { _ in }
    var saveParameterSet: () -> Void = {}
}

/// What the console area shows about the last run.
enum RenderReport {
    case idle
    case running(RenderMode)
    case rendered(RenderResult, RenderMode)
    case failed(String)
}

@objc(SCADDocument)
final class SCADDocument: NSDocument {
    let model = DocumentModel()

    /// The path the core knows this document by, once it has run: the
    /// file's, or for an untitled document one of its own (see
    /// `untitledPath`).
    var corePath: String?
    /// Whether the core's copy of the text is the document's: edits are
    /// forwarded only then, and the next run sends the whole text
    /// otherwise.
    var coreInSync = false
    /// The run in flight (the app tests await it).
    var renderTask: Task<Void, Never>?
    /// How many runs were started (the app tests count them, so a key that
    /// reached two handlers would show).
    var requestCount = 0
    /// The run waiting for typing (or a customizer drag) to pause.
    var pendingPreview: Task<Void, Never>?
    /// What the last run built, so a scheme change or a file change on
    /// disk runs it again.
    var lastMode: RenderMode?
    /// The customizer's parse of the text, in flight.
    var parameterTask: Task<Void, Never>?
    /// The files the last run read, watched for changes on disk.
    let watcher = FileWatcher()
    /// The editor's language server: markers come from this document's
    /// runs (`hostDiagnostics`), which hand it their diagnostics.
    private(set) var languageServer: LanguageServer?
    /// Set by `close`: nothing more runs.
    private(set) var isClosed = false

    /// How long typing must pause before the document runs again. The
    /// run is the markers' source too, so this is also how soon they
    /// follow typing (the language server's own debounce was 150 ms).
    static let previewDelay: Duration = .milliseconds(150)

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
            editor.documentURI = { [weak self] in self?.languageURI }
            editor.openLocation = { [weak self] uri, line, character in
                LocationOpener.open(
                    uri: uri, line: line, character: character,
                    from: self?.windowControllers.first?.window)
            }
            if case .success(let engine) = CoreService.shared,
                let server = try? engine.core.languageServer(hostDiagnostics: true)
            {
                languageServer = server
                editor.connect(server)
            }
            watcher.onChange = { [weak self] in self?.filesChanged() }
            connectPanels()
        }
    }

    /// The URI the language server knows this document by: the core's
    /// path for it, as a file URI.
    var languageURI: String {
        URL(fileURLWithPath: fileURL?.path ?? untitledPath).absoluteString
    }

    /// Where an untitled document lives for its includes. OpenSCAD's GUI
    /// resolves an unsaved file's relative `include`s and `use`s against
    /// its working directory (`parser.y`: an empty file name is
    /// `fs::current_path()`), which for an app started from the Finder is
    /// `/`: nothing useful. The Mac's equivalent of "where the user is
    /// working" is the document controller's current directory (the
    /// last folder a file was opened from or saved to, else Documents),
    /// so an untitled document behaves as a file there: `include
    /// <parts.scad>` finds the parts next to the files just opened, and
    /// messages name it "Untitled.scad". The name is unique among open
    /// documents and never one of a file on disk (its buffer would hide
    /// that file from other documents' includes).
    var untitledPath: String {
        if let p = untitledPathChosen { return p }
        let dir =
            NSDocumentController.shared.currentDirectory.map { URL(fileURLWithPath: $0) }
            ?? FileManager.default.urls(for: .documentDirectory, in: .userDomainMask).first
            ?? URL(fileURLWithPath: NSHomeDirectory())
        let shown: String = displayName ?? ""
        let base = shown.isEmpty ? "Untitled" : shown
        var name = base
        var n = 1
        while true {
            let path = dir.appendingPathComponent("\(name).scad").path
            if !Self.untitledPaths.contains(path) && !FileManager.default.fileExists(atPath: path) {
                Self.untitledPaths.insert(path)
                untitledPathChosen = path
                return path
            }
            n += 1
            name = "\(base) \(n)"
        }
    }
    private var untitledPathChosen: String?

    /// The paths untitled documents use now.
    static var untitledPaths: Set<String> = []

    /// Saved under a new name (or moved): the language server follows,
    /// and so do the customizer's parameter sets.
    override var fileURL: URL? {
        didSet {
            guard oldValue != fileURL else { return }
            MainActor.assumeIsolated {
                model.editor.documentURIChanged()
                refreshParameterSets()
            }
        }
    }

    override class var autosavesInPlace: Bool { true }

    override func makeWindowControllers() {
        let window = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 1320, height: 760),
            styleMask: [.titled, .closable, .miniaturizable, .resizable],
            backing: .buffered, defer: false)
        window.contentViewController = NSHostingController(
            rootView: DocumentView(model: model))
        window.setContentSize(NSSize(width: 1320, height: 760))
        let controller = NSWindowController(window: window)
        controller.shouldCascadeWindows = true
        addWindowController(controller)
        window.center()
        // Typing goes to the editor from the start.
        window.initialFirstResponder = model.editor.webView
        refreshParameterSets()
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

    /// The text was read (open, revert): show it and run it.
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

    /// The last run again (after the colour scheme changed).
    private func rerender() {
        if let mode = lastMode { run(mode) }
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
    @objc func toggleCustomizer(_ sender: Any?) {
        model.customizerShown.toggle()
    }
    @objc func toggleConsole(_ sender: Any?) {
        model.consoleCollapsed.toggle()
    }

    /// Check marks for the View menu's toggles and choices.
    override func validateMenuItem(_ item: NSMenuItem) -> Bool {
        switch item.action {
        case #selector(toggleCustomizer(_:)):
            item.state = model.customizerShown ? .on : .off
            return true
        case #selector(toggleConsole(_:)):
            item.state = model.consoleCollapsed ? .off : .on
            return true
        default: break
        }
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

    /// Closing stops everything the document started: the waiting and
    /// running runs (the core's requests on its path are cancelled, not
    /// left to finish), the customizer's parse, the watcher, the editor's
    /// language client, and the core's buffer.
    override func close() {
        isClosed = true
        pendingPreview?.cancel()
        pendingPreview = nil
        renderTask?.cancel()
        parameterTask?.cancel()
        watcher.stop()
        model.viewport.detach()
        model.editor.detach()
        if let path = corePath, case .success(let engine) = CoreService.shared {
            _ = try? engine.cancel(path)
            _ = try? engine.close(path)
        }
        if let p = untitledPathChosen {
            Self.untitledPaths.remove(p)
        }
        super.close()
    }
}
