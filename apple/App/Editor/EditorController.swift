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
//                                    server (8e)
//   {type: "log", level, message}    a script error, for the app's log
//
// Swift -> JS through `callAsyncJavaScript` on `window.NeoSCADEditor`:
// `load(text)`, `text()`, `setDiagnostics(version, list)`,
// `setFontSize(px)`, `undo()`, `redo()`, `selectAll()`, `openSearch()`,
// `focus()`, `lspReceive(message)`, and `edit`, `select`, `state` and
// `benchmarkTyping` for tests.
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

/// A lint marker for the editor: a range in the UTF-16 offsets of the text
/// at some editor version.
struct EditorDiagnostic: Equatable {
    var from: Int
    var to: Int
    /// CodeMirror's: "error", "warning", "info" or "hint".
    var severity: String
    var message: String
    /// The diagnostic's code, e.g. `unknown-module`.
    var source: String?
    /// Fixes: a name and the replacement.
    var actions: [(name: String, edit: UTF16Edit)] = []

    static func == (a: Self, b: Self) -> Bool {
        a.from == b.from && a.to == b.to && a.severity == b.severity
            && a.message == b.message && a.source == b.source
            && a.actions.map(\.name) == b.actions.map(\.name)
            && a.actions.map(\.edit) == b.actions.map(\.edit)
    }

    var json: [String: Any] {
        var d: [String: Any] = [
            "from": from, "to": to, "severity": severity, "message": message,
        ]
        if let source { d["source"] = source }
        if !actions.isEmpty {
            d["actions"] = actions.map {
                ["name": $0.name, "from": $0.edit.from, "to": $0.edit.to, "insert": $0.edit.insert]
            }
        }
        return d
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
    /// Where language server messages go (8e). Without one, requests are
    /// answered with JSON-RPC's "method not found", so a client never
    /// waits for an answer that is not coming.
    var languageServer: ((String) -> Void)?

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
                let state = try await call("return NeoSCADEditor.load(text)", ["text": text])
                updateHistory(state)
            } catch {
                Self.log.error("load failed: \(error)")
            }
        }
    }

    /// Replace the lint markers. `version` is the editor version whose text
    /// the ranges are in; the editor maps them through the edits since.
    func setDiagnostics(_ diagnostics: [EditorDiagnostic], version: Int) {
        guard isReady else { return }
        Task {
            _ = try? await call(
                "return NeoSCADEditor.setDiagnostics(version, list)",
                ["version": version, "list": diagnostics.map(\.json)])
        }
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

    /// Send a language server message to the editor's LSP client.
    func receiveFromLanguageServer(_ message: String) {
        guard isReady else { return }
        Task { _ = try? await call("return NeoSCADEditor.lspReceive(message)", ["message": message]) }
    }

    private func applyFontSize() {
        guard isReady else { return }
        let size = EditorSettings.fontSize
        Task { _ = try? await call("return NeoSCADEditor.setFontSize(size)", ["size": size]) }
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
                let state = try await call("return NeoSCADEditor.load(text)", ["text": text])
                updateHistory(state)
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
        if let languageServer {
            languageServer(message)
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
