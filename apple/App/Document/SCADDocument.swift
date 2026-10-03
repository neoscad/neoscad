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
// Another program changing the file (an AI agent, another editor) is taken
// in as an undoable edit, or reported: Document/SCADDocument+Disk.swift.
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
        get { (try? storage.text()) ?? "" }
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
    /// Whether the inspector (customizer, check, measure) and the console
    /// list are shown, and which of the inspector's panels.
    var customizerShown = true
    var inspector: InspectorTab = .customizer
    var consoleCollapsed = false
    /// neoscad's `part()` extension for this document (`--enable part`):
    /// its runs, checks and measurements all accept `part("name")`.
    var partsEnabled = false
    /// Whether parameter sets can be read and written: the document has a
    /// file, next to which `name.json` lives.
    var parameterSetsAvailable = false
    /// What the bar over the editor says about the file on disk; nil while
    /// the file and the document agree (SCADDocument+Disk.swift).
    var diskNotice: DiskNotice?
    /// An agent's edit waiting for the user's Apply or Reject (only when
    /// they chose "Ask before applying"; SCADDocument+Agent.swift).
    var agentApproval: AgentApproval?
    /// How many marks the agent drew in the 3D view (0: none showing).
    var agentMarkCount = 0

    /// The check and measure panels.
    @ObservationIgnored let check = CheckModel()
    @ObservationIgnored let measure = MeasureModel()

    /// The 3D view's state (the Rust viewport and its view).
    @ObservationIgnored let viewport = ViewportController()
    /// The editor (CodeMirror in a web view) and its bridge.
    @ObservationIgnored let editor = EditorController()
    @ObservationIgnored var textReplaced: (() -> Void)?
    /// What the panels ask of their document (set by it).
    @ObservationIgnored var actions = DocumentActions()

    /// The document's copy of the text, edited in the editor's UTF-16
    /// offsets by the core (`EditorText`, `crates/client/src/text.rs`),
    /// which keeps its UTF-16 length with each edit and hands the edits
    /// back in the UTF-8 offsets `Core.edit` takes.
    @ObservationIgnored private let storage = EditorText(text: "")
    /// The copy itself, for the core's checks against the file on disk.
    var editorText: EditorText { storage }
    /// The text's length in UTF-16 units.
    var utf16Length: Int { Int((try? storage.utf16Length()) ?? 0) }
    /// The text's length in UTF-8 bytes (the core's `DocInfo.length`).
    var byteLength: UInt64 { get throws { try storage.byteLength() } }

    private func setStorage(_ text: String) {
        try? storage.replace(text: text)
    }

    /// Apply the editor's edits (UTF-16 offsets) and return them in the
    /// core's UTF-8 offsets. Throws if they do not fit this text, which is
    /// then out of step until `replaceWithEditorText`.
    func applyEditorEdits(_ edits: [Utf16Edit]) throws -> [TextEdit] {
        try storage.apply(edits: edits)
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
    /// The check and measure panels (Document/Inspect.swift).
    var setParts: (Bool) -> Void = { _ in }
    var runCheck: () -> Void = {}
    var selectFinding: (UInt32?) -> Void = { _ in }
    var runMeasure: () -> Void = {}
    var measureBetween: () -> Void = {}
    var updateSection: () -> Void = {}
    var clearPicks: () -> Void = {}
    /// The disk notice's buttons (SCADDocument+Disk.swift).
    var reloadFromDisk: () -> Void = {}
    var keepMine: () -> Void = {}
    /// Remove the marks an agent drew in the 3D view.
    var clearAgentMarks: () -> Void = {}
}

/// The inspector's panels, beside the 3D view.
enum InspectorTab: String, CaseIterable {
    case customizer = "Customizer"
    case check = "Check"
    case measure = "Measure"
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

    /// The document loop's state machine, from the core
    /// (`DocumentController`, `crates/client/src/document_loop.rs`): when
    /// to run, which run is current, whether the core's copy of the text
    /// is the document's, the customizer's edited values. This file keeps
    /// only the timer (`pendingPreview`) and the text.
    let loop = DocumentController(delayMs: nil)
    /// Mirrors the loop's state into the model (the customizer's values).
    private lazy var loopObserver = LoopObserver(self)

    /// The path the core knows this document by, once it has run: the
    /// file's, or for an untitled document one of its own (see
    /// `untitledPath`).
    var corePath: String? { (try? loop.state())?.path }
    /// Whether the core's copy of the text is the document's: edits are
    /// forwarded only then, and the next run sends the whole text
    /// otherwise.
    var coreInSync: Bool {
        get { (try? loop.state())?.inSync ?? false }
        set { _ = try? (newValue ? loop.textSent() : loop.textReplaced()) }
    }
    /// The run in flight (the app tests await it).
    var renderTask: Task<Void, Never>?
    /// How many runs were started (the app tests count them, so a key that
    /// reached two handlers would show).
    var requestCount: Int { Int((try? loop.state())?.requestCount ?? 0) }
    /// The host's timer: armed for the loop's next due run.
    var pendingPreview: Task<Void, Never>?
    /// What the last run built, so a scheme change or a file change on
    /// disk runs it again.
    var lastMode: RenderMode? { (try? loop.state())?.lastMode ?? nil }
    /// The customizer's parse of the text, in flight.
    var parameterTask: Task<Void, Never>?
    /// The check, measurement and export in flight (Document/Inspect.swift,
    /// Document/Export.swift).
    var checkTask: Task<Void, Never>?
    var measureTask: Task<Void, Never>?
    var exportTask: Task<Void, Never>?
    /// The files the last run read, watched for changes on disk, with the
    /// document's own file.
    let watcher = FileWatcher()
    /// The last run's files (`watcher` adds the document's own).
    var runFiles: [String] = []
    /// The document's file as last read or written, and the bytes of the
    /// save in progress (SCADDocument+Disk.swift).
    let diskFile = DocumentFile()
    var bytesBeingWritten: Data?
    /// A read of the changed file, waiting for the rest of a write.
    var diskReadAgain: Task<Void, Never>?
    /// The editor's language server: markers come from this document's
    /// runs (`hostDiagnostics`), which hand it their diagnostics.
    private(set) var languageServer: LanguageServer?
    /// Whether a new text (a file read, an example) runs at once; off
    /// while a heavy example is loaded, which waits for Preview or Render.
    var runsWhenTextIsReplaced = true
    /// Set by `close`: nothing more runs.
    private(set) var isClosed = false

    // MARK: AI agents (Document/SCADDocument+Agent.swift)

    /// The number agents know this document by while the app runs
    /// (`AgentLink.documentOpened`).
    let agentID = AgentService.newDocumentID()
    /// Counts every change of the text, the user's, the agent's and a
    /// reload's: the `version` an agent reads and must quote to edit. Kept
    /// here rather than taken from the editor, whose version restarts at
    /// each load, so a number never stands for two different texts while
    /// the document is open.
    private(set) var agentRevision: UInt64 = 1
    /// The window's toolbar and its delegate (NSToolbar holds it weakly).
    var toolbar: DocumentToolbar?
    /// Tells the agent link when this window becomes the focused one.
    var keyObserver: NSObjectProtocol?

    /// The text changed (any way): agents must read it again.
    func textRevised() {
        agentRevision += 1
    }

    /// Milliseconds on a monotonic clock: the "now" the loop schedules
    /// against (its delay is the core's `default_preview_delay_ms`).
    static func nowMs() -> UInt64 { DispatchTime.now().uptimeNanoseconds / 1_000_000 }

    override init() {
        super.init()
        hasUndoManager = false
        MainActor.assumeIsolated {
            try? loop.setObserver(observer: loopObserver)
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
            watcher.onDocumentChange = { [weak self] in self?.documentFileChanged() }
            connectPanels()
            // After `connectPanels`, which sets the actions afresh.
            model.actions.reloadFromDisk = { [weak self] in self?.reloadFromDisk() }
            model.actions.keepMine = { [weak self] in self?.keepMine() }
            model.actions.clearAgentMarks = { [weak self] in self?.clearAgentMarks() }
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
        let path =
            (try? CoreService.shared.get().core.untitledPath(
                dir: dir.path, base: shown, taken: Array(Self.untitledPaths)))
            ?? dir.appendingPathComponent("\(shown.isEmpty ? "Untitled" : shown).scad").path
        Self.untitledPaths.insert(path)
        untitledPathChosen = path
        return path
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
                // Moved or saved under a new name: watch the new file.
                model.diskNotice = nil
                watchFiles(runFiles)
                // Agents see the new name and path.
                AgentService.shared.documentOpened(self)
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
        attachAgentControls(to: window)
    }

    // MARK: Reading and writing

    override func data(ofType typeName: String) throws -> Data {
        MainActor.assumeIsolated {
            let data = Data(model.text.utf8)
            bytesBeingWritten = data
            return data
        }
    }

    /// UTF-8 only, as OpenSCAD reads files. Text that is not UTF-8 is
    /// refused rather than decoded lossily: saving it back would silently
    /// replace the bytes that did not decode.
    override func read(from data: Data, ofType typeName: String) throws {
        guard let text = String(data: data, encoding: .utf8) else {
            throw CocoaError(.fileReadInapplicableStringEncoding)
        }
        MainActor.assumeIsolated {
            model.text = text
            // What the file holds: the watcher's next event is then told
            // from another program's write.
            try? diskFile.loaded(bytes: data)
            model.diskNotice = nil
        }
    }

    // MARK: Edits

    /// The text was read (open, revert): show it and run it.
    private func textReplaced() {
        textRevised()
        model.editor.load(model.text)
        coreInSync = false
        if runsWhenTextIsReplaced { schedulePreview() }
    }

    /// One editor transaction: apply it to the document's copy, count it
    /// for NSDocument, and forward it to the core. False if it does not
    /// fit the copy, which the editor then replaces.
    private func editorChanged(_ edits: [Utf16Edit], kind: EditKind, length: Int) -> Bool {
        let coreEdits: [TextEdit]
        do {
            coreEdits = try model.applyEditorEdits(edits)
        } catch {
            return false
        }
        guard model.utf16Length == length else { return false }
        textRevised()
        updateChangeCount(kind.changeType)
        reloadLanded()
        if coreInSync, let path = corePath, case .success(let engine) = CoreService.shared {
            do {
                let info = try engine.edit(path, edits: coreEdits)
                // The byte count is a cheap check that both copies agree.
                if info.length != (try? model.byteLength) { coreInSync = false }
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
        textRevised()
        updateChangeCount(.changeDone)
        reloadLanded()
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
        showInspector(.customizer)
    }
    @objc func showCheck(_ sender: Any?) { showInspector(.check) }
    /// Design > Check and Measure: show the panel and run it.
    @objc func checkDocument(_ sender: Any?) {
        model.inspector = .check
        model.customizerShown = true
        runCheck()
    }
    @objc func measureDocument(_ sender: Any?) {
        model.inspector = .measure
        model.customizerShown = true
        runMeasure()
    }
    @objc func showMeasure(_ sender: Any?) { showInspector(.measure) }

    /// Show `tab`, or hide the inspector when it is already showing it.
    private func showInspector(_ tab: InspectorTab) {
        if model.customizerShown && model.inspector == tab {
            model.customizerShown = false
        } else {
            model.inspector = tab
            model.customizerShown = true
        }
    }
    @objc func toggleConsole(_ sender: Any?) {
        model.consoleCollapsed.toggle()
    }

    /// Check marks for the View menu's toggles and choices.
    override func validateMenuItem(_ item: NSMenuItem) -> Bool {
        switch item.action {
        case #selector(toggleCustomizer(_:)):
            item.state = model.customizerShown && model.inspector == .customizer ? .on : .off
            return true
        case #selector(showCheck(_:)):
            item.state = model.customizerShown && model.inspector == .check ? .on : .off
            return true
        case #selector(showMeasure(_:)):
            item.state = model.customizerShown && model.inspector == .measure ? .on : .off
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
        try? loop.close()
        pendingPreview?.cancel()
        pendingPreview = nil
        renderTask?.cancel()
        parameterTask?.cancel()
        checkTask?.cancel()
        measureTask?.cancel()
        exportTask?.cancel()
        watcher.stop()
        diskReadAgain?.cancel()
        detachAgentControls()
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

/// Carries the loop's state to the document's model on the main thread
/// (the loop calls it on the thread that changed it, which is the main
/// thread here: every loop call is made from it).
final class LoopObserver: DocumentObserver, @unchecked Sendable {
    private weak var document: SCADDocument?

    init(_ document: SCADDocument) { self.document = document }

    func stateChanged(state: DocumentState) {
        let apply = { @MainActor [weak document] in
            guard let model = document?.model else { return }
            let values = Dictionary(
                state.overrides.map { ($0.name, $0.value) }, uniquingKeysWith: { a, _ in a })
            if model.parameterValues != values { model.parameterValues = values }
            if model.selectedParameterSet != state.selectedSet {
                model.selectedParameterSet = state.selectedSet
            }
        }
        if Thread.isMainThread {
            MainActor.assumeIsolated { apply() }
        } else {
            DispatchQueue.main.async { MainActor.assumeIsolated { apply() } }
        }
    }
}
