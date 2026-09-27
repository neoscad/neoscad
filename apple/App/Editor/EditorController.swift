// The editor of a document window: CodeMirror 6 in a WKWebView
// (apple/Editor/web), and the bridge between it and the document.
//
// Who owns the text. CodeMirror does, while editing: it holds the
// selection and the undo history. Every change it makes comes here at
// once, in order, as a CodeMirror change set, and the document applies it
// to a copy of its own (`DocumentModel.text`). The copy is needed anyway:
// turning CodeMirror's UTF-16 offsets into the core's UTF-8 ones takes the
// text as it was before the change. And it is what NSDocument saves.
// NSDocument asks for the data synchronously on the main thread (save,
// autosave, Versions), and a web view answers only asynchronously on that
// same thread, so pulling the text at save time would mean spinning a
// nested run loop inside AppKit's save machinery. With the copy, a save is
// a plain read. The one path the other way is a load: when the document
// is read (open, revert), the app replaces CodeMirror's text.
//
// The protocol. JS -> Swift through `WKScriptMessageHandlerWithReply`
// (`window.webkit.messageHandlers.editor.postMessage`), one object each:
//
//   {type: "ready"}                  the page is up: the app loads the text
//   {type: "changes", base, version, edits, kind, length, undoDepth, redoDepth}
//                                    one transaction: `edits` are
//                                    [from, to, insert] in UTF-16 offsets,
//                                    each to the text the previous left;
//                                    `base` is the version they apply to,
//                                    `kind` "edit", "undo" or "redo"
//   {type: "command", name}          a key the app's menu owns ("preview",
//                                    "render")
//   {type: "lsp", message}           a JSON-RPC message for the language
//                                    server (LanguageClient.swift)
//   {type: "open", uri, line, character}
//                                    go to a definition in another file
//                                    (0-based line, UTF-16 column)
//   {type: "log", level, message}    a script error, for the app's log
//
// Swift -> JS through `callAsyncJavaScript` on `window.NeoSCADEditor`:
// `load(text, uri, readOnly)`, `text()`, `setURI(uri)`,
// `reveal(line, character)`, `setFontSize(px)`, `undo()`, `redo()`,
// `selectAll()`, `openSearch()`, `focus()`, `lspReceive(message)`, and
// for tests `edit`, `select`, `state`, `benchmarkTyping`, and `complete`,
// `hover`, `signature`, `definition`, `format` and `lspReady`, which
// trigger the language features as a person would.
//
// Diagnostics. The lint markers come from the language server only
// (`textDocument/publishDiagnostics`, handled in the page): it evaluates
// the exact text version the editor sent it, so the markers always fit
// the text, and their fixes come with them. Renders report to the console.
//
// Versions keep the copies in step. The editor numbers its texts; a change
// is applied here only if its `base` is the version this copy holds. After
// a load, or when a change cannot be applied (a message lost, an offset
// that does not fit), `version` is nil and changes are dropped until the
// editor's answer arrives: the loaded version, or its whole text.

import AppKit
import NeoSCADCore
import WebKit
import os

/// What a change did, for NSDocument's change count: an undo takes a
/// change back, so undoing to the saved text clears the edited dot.
enum EditKind: String {
    case edit, undo, redo

    var changeType: NSDocument.ChangeType {
        switch self {
        case .edit: .changeDone
        case .undo: .changeUndone
        case .redo: .changeRedone
        }
    }
}

@MainActor
final class EditorController: NSObject {
    private static let log = Logger(subsystem: "org.neoscad.NeoSCAD", category: "editor")

    // MARK: The document's side

    /// The text to show when the page is ready (the document's copy).
    var text: () -> String = { "" }
    /// Apply one transaction's edits to the document's copy, which then has
    /// `length` UTF-16 units. False if they do not fit it: the copies
    /// disagree and the editor's whole text replaces the document's.
    var applyChanges: (_ edits: [UTF16Edit], _ kind: EditKind, _ length: Int) -> Bool = {
        _, _, _ in false
    }
    /// Replace the document's copy with the editor's text (after a
    /// disagreement). Not an edit: the document's change count stays.
    var replaceText: (String) -> Void = { _ in }
    /// A menu command the editor forwarded (F5, F6).
    var perform: (String) -> Void = { _ in }
    /// The document's `file://` URI, which the language server knows it
    /// by; asked at each load. Nil: no language features.
    var documentURI: () -> String? = { nil }
    /// A library file shown for reading: the editor refuses edits.
    var readOnly = false
    /// Show a location in another file (a definition): its URI, 0-based
    /// line and UTF-16 column.
    var openLocation: (_ uri: String, _ line: Int, _ character: Int) -> Void = { _, _, _ in }
    /// The language server. Without one, requests are answered with
    /// JSON-RPC's "method not found", so a client never waits for an
    /// answer that is not coming.
    private(set) var languageClient: LanguageClient?

    // MARK: State

    /// Whether the page has loaded and taken the text.
    private(set) var isReady = false
    /// The editor version the document's copy matches; nil while a load
    /// or a resynchronisation is on its way.
    private(set) var version: Int?
    private(set) var undoDepth = 0
    private(set) var redoDepth = 0
    /// Called whenever the page becomes ready (tests await it).
    var onReady: (() -> Void)?
    /// How many transactions were applied, and how many forced a resync.
    private(set) var changeCount = 0
    private(set) var resyncCount = 0

    private var fontObserver: NSObjectProtocol?
    /// A location to show once the page is ready.
    private var pendingReveal: (Int, Int)?

    /// The web view, made on first use: a document that is never shown
    /// (a test, a render from a script) starts no web content process.
    private(set) lazy var webView: EditorWebView = makeWebView()

    private func makeWebView() -> EditorWebView {
        let config = WKWebViewConfiguration()
        config.setURLSchemeHandler(EditorSchemeHandler(), forURLScheme: EditorSchemeHandler.scheme)
        config.userContentController.addScriptMessageHandler(
            WeakMessageHandler(self), contentWorld: .page, name: "editor")
        // Nothing the page does needs these, and each is a way out of it.
        config.preferences.javaScriptCanOpenWindowsAutomatically = false
        config.defaultWebpagePreferences.allowsContentJavaScript = true
        let view = EditorWebView(frame: .zero, configuration: config)
        view.controller = self
        view.navigationDelegate = self
        #if DEBUG
            view.isInspectable = true
        #endif
        view.setAccessibilityIdentifier("editor")
        view.load(URLRequest(url: EditorSchemeHandler.pageURL))
        fontObserver = NotificationCenter.default.addObserver(
            forName: EditorSettings.fontSizeDidChange, object: nil, queue: .main
        ) { [weak self] _ in
            MainActor.assumeIsolated { self?.applyFontSize() }
        }
        return view
    }

    // MARK: Swift -> JS

    /// Call `NeoSCADEditor.<function>` with `arguments` (named, so no text
    /// is ever spliced into a script).
    @discardableResult
    func call(_ body: String, _ arguments: [String: Any] = [:]) async throws -> Any? {
        try await webView.callAsyncJavaScript(
            body, arguments: arguments, in: nil, contentWorld: .page)
    }

    /// Show `text` (the document was read). Clears the editor's undo
    /// history, as reverting a document does.
    func load(_ text: String) {
        guard isReady else { return }  // `ready` loads the current text
        version = nil
        Task {
            do {
                let state = try await call(
                    "return NeoSCADEditor.load(text, uri, readOnly)", loadArguments(text))
                updateHistory(state)
            } catch {
                Self.log.error("load failed: \(error)")
            }
        }
    }

    /// Talk to `server` from now on: the page's client (connected when
    /// the page loads) reaches it through here.
    func connect(_ server: LanguageServer) {
        let client = LanguageClient(server: server)
        client.diagnostics = !readOnly
        client.deliver = { [weak self] message in self?.receiveFromLanguageServer(message) }
        languageClient = client
    }

    /// The document now has this URI (saved under a new name): the
    /// language server sees the old file close and the new one open.
    func documentURIChanged() {
        guard isReady else { return }
        let uri = documentURI()
        Task { _ = try? await call("return NeoSCADEditor.setURI(uri)", ["uri": uri.map { $0 as Any } ?? NSNull()]) }
    }

    /// Put the cursor at a 0-based line and UTF-16 column, in view.
    func reveal(line: Int, character: Int) {
        guard isReady else {
            pendingReveal = (line, character)
            return
        }
        Task {
            _ = try? await call(
                "return NeoSCADEditor.reveal(line, character)",
                ["line": line, "character": character])
        }
    }

    /// Select a span given in editor positions (0-based lines, UTF-16
    /// columns), in view and focused: a console line's jump.
    func reveal(line: Int, character: Int, endLine: Int, endCharacter: Int) {
        guard isReady else {
            pendingReveal = (line, character)
            return
        }
        Task {
            _ = try? await call(
                "return NeoSCADEditor.revealRange(line, character, endLine, endCharacter)",
                ["line": line, "character": character, "endLine": endLine, "endCharacter": endCharacter])
        }
    }

    /// Have the page's language client send its pending changes now: a run
    /// of the current text has started, and its markers are published for
    /// the version that carries that text.
    func syncLanguage() {
        guard isReady else { return }
        Task { _ = try? await call("return NeoSCADEditor.lspSync()") }
    }

    func undo() { command("undo") }
    func redo() { command("redo") }
    func selectAll() { command("selectAll") }
    func openSearch() { command("openSearch") }
    func focus() { command("focus") }

    private func command(_ name: String) {
        guard isReady else { return }
        Task { _ = try? await call("return NeoSCADEditor.\(name)()") }
    }

    /// Send a language server message to the editor's LSP client. Not
    /// gated on `isReady`: the page's client starts before the page says
    /// it is ready, and its `initialize` must be answered.
    func receiveFromLanguageServer(_ message: String) {
        Task { _ = try? await call("return NeoSCADEditor.lspReceive(message)", ["message": message]) }
    }

    private func applyFontSize() {
        guard isReady else { return }
        let size = EditorSettings.fontSize
        Task { _ = try? await call("return NeoSCADEditor.setFontSize(size)", ["size": size]) }
    }

    private func loadArguments(_ text: String) -> [String: Any] {
        // A missing URI crosses as `null` (an Optional would not cross).
        ["text": text, "uri": documentURI().map { $0 as Any } ?? NSNull(), "readOnly": readOnly]
    }

    private func updateHistory(_ state: Any?) {
        guard let s = state as? [String: Any] else { return }
        if let v = s["version"] as? Int { version = v }
        undoDepth = s["undoDepth"] as? Int ?? undoDepth
        redoDepth = s["redoDepth"] as? Int ?? redoDepth
    }

    /// Make the document's copy the editor's text again.
    private func resync() {
        version = nil
        resyncCount += 1
        Task {
            guard let r = try? await call("return NeoSCADEditor.text()") as? [String: Any],
                let v = r["version"] as? Int, let text = r["text"] as? String
            else { return }
            replaceText(text)
            version = v
        }
    }

    // MARK: JS -> Swift

    fileprivate func receive(_ body: Any) -> Any? {
        guard let m = body as? [String: Any], let type = m["type"] as? String else { return nil }
        switch type {
        case "ready":
            ready()
        case "changes":
            changes(m)
        case "command":
            if let name = m["name"] as? String { perform(name) }
        case "lsp":
            if let message = m["message"] as? String { languageServerMessage(message) }
        case "open":
            if let uri = m["uri"] as? String {
                openLocation(uri, m["line"] as? Int ?? 0, m["character"] as? Int ?? 0)
            }
        case "log":
            Self.log.error("editor: \(m["message"] as? String ?? "", privacy: .public)")
        default:
            Self.log.error("editor: unknown message \(type, privacy: .public)")
        }
        return nil
    }

    private func ready() {
        isReady = true
        version = nil
        let text = self.text()
        Task {
            do {
                _ = try await call(
                    "return NeoSCADEditor.setFontSize(size)", ["size": EditorSettings.fontSize])
                let state = try await call(
                    "return NeoSCADEditor.load(text, uri, readOnly)", loadArguments(text))
                updateHistory(state)
                if let (line, character) = pendingReveal {
                    pendingReveal = nil
                    reveal(line: line, character: character)
                }
                onReady?()
            } catch {
                Self.log.error("the editor did not load: \(error)")
            }
        }
    }

    private func changes(_ m: [String: Any]) {
        undoDepth = m["undoDepth"] as? Int ?? undoDepth
        redoDepth = m["redoDepth"] as? Int ?? redoDepth
        guard let base = m["base"] as? Int, let next = m["version"] as? Int else { return }
        // A load or a resync is on its way and will replace these.
        guard let version else { return }
        guard base == version,
            let raw = m["edits"] as? [[Any]],
            let length = m["length"] as? Int
        else {
            resync()
            return
        }
        var edits: [UTF16Edit] = []
        for e in raw {
            guard e.count == 3, let from = e[0] as? Int, let to = e[1] as? Int,
                let insert = e[2] as? String
            else {
                resync()
                return
            }
            edits.append(UTF16Edit(from: from, to: to, insert: insert))
        }
        let kind = EditKind(rawValue: m["kind"] as? String ?? "") ?? .edit
        if applyChanges(edits, kind, length) {
            self.version = next
            changeCount += 1
        } else {
            resync()
        }
    }

    private func languageServerMessage(_ message: String) {
        if let languageClient {
            languageClient.send(message)
            return
        }
        // A request (it has an id) gets an error; a notification nothing.
        guard let data = message.data(using: .utf8),
            let json = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
            let id = json["id"], json["method"] != nil
        else { return }
        let reply: [String: Any] = [
            "jsonrpc": "2.0", "id": id,
            "error": ["code": -32601, "message": "No language server yet"],
        ]
        if let out = try? JSONSerialization.data(withJSONObject: reply),
            let text = String(data: out, encoding: .utf8)
        {
            receiveFromLanguageServer(text)
        }
    }

    /// Stop observing and let the web view go (the window closed).
    func detach() {
        if let fontObserver { NotificationCenter.default.removeObserver(fontObserver) }
        fontObserver = nil
        languageClient?.stop()
    }
}

extension EditorController: WKNavigationDelegate {
    /// The page never navigates: a link or a script that tries would leave
    /// the editor, so only the editor's own page loads.
    func webView(
        _ webView: WKWebView, decidePolicyFor action: WKNavigationAction
    ) async -> WKNavigationActionPolicy {
        action.request.url?.scheme == EditorSchemeHandler.scheme ? .allow : .cancel
    }

    /// The web content process died (a crash, or the system reclaimed it).
    /// The document's copy of the text is complete, so reload the page:
    /// `ready` shows the text again. Only the undo history is lost.
    func webViewWebContentProcessDidTerminate(_ webView: WKWebView) {
        Self.log.error("the editor's web process ended; reloading")
        isReady = false
        version = nil
        webView.load(URLRequest(url: EditorSchemeHandler.pageURL))
    }
}

/// The message handler the web view holds: WKUserContentController retains
/// its handlers, and the controller owns the web view, so a direct
/// reference would be a cycle that keeps every closed document's editor.
private final class WeakMessageHandler: NSObject, WKScriptMessageHandlerWithReply {
    weak var controller: EditorController?

    init(_ controller: EditorController) {
        self.controller = controller
    }

    func userContentController(
        _ userContentController: WKUserContentController, didReceive message: WKScriptMessage,
        replyHandler: @escaping @MainActor @Sendable (Any?, String?) -> Void
    ) {
        MainActor.assumeIsolated {
            replyHandler(controller?.receive(message.body), nil)
        }
    }
}
