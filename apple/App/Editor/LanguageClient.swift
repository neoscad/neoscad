// The editor's language server: the core's (crates/lsp, through the
// generated `LanguageServer`), one per editor, reached from the page's
// @codemirror/lsp-client through the `lsp` message.
//
// Threads. Messages are handled in order on a serial queue of their own,
// never on the main thread: an answer takes a millisecond or two, but the
// first request on a BOSL2 model indexes the library. Answers come back
// on the main thread, in order.
//
// Diagnostics. A document window's server is made with `hostDiagnostics`:
// it never evaluates, and its markers come from the document's own runs
// (Document/SCADDocument.swift hands each run's publications here, and a
// version of the text the server receives after its run was supplied is
// published from `handle`). A library viewer's server has no diagnostics
// at all (`diagnostics` off). A server that evaluates by itself (neither)
// is still served: after a change it waits for the messages to pause
// (`debounce`), then evaluates on another queue.

import Foundation
import NeoSCADCore

@MainActor
final class LanguageClient {
    private let server: LanguageServer
    private let queue = DispatchQueue(label: "org.neoscad.lsp", qos: .userInitiated)
    private let diagnosticsQueue = DispatchQueue(label: "org.neoscad.lsp.diagnostics", qos: .utility)
    private var pendingDiagnostics: Task<Void, Never>?

    /// How long the messages must pause before diagnostics are computed.
    /// The page's client already batches typing (it syncs half a second
    /// after the last keystroke), so this only coalesces bursts.
    static let debounce: Duration = .milliseconds(150)

    /// Where the server's messages go (the page's client).
    var deliver: (String) -> Void = { _ in }
    /// Whether to compute diagnostics. A library file shown for reading
    /// is no model of its own: evaluating it would cost a BOSL2 file's
    /// evaluation per view and show warnings about code the user cannot
    /// change.
    var diagnostics = true
    /// How many diagnostics publications arrived (the app's tests wait on
    /// it).
    private(set) var publications = 0
    /// Set when the editor closes: nothing more is delivered or started.
    private var stopped = false

    init(server: LanguageServer) {
        self.server = server
    }

    /// Hand a message from the page to the server.
    func send(_ message: String) {
        let server = self.server
        queue.async { [weak self] in
            let out = (try? server.handle(message: message)) ?? []
            let pending = (try? server.diagnosticsPending()) ?? false
            DispatchQueue.main.async {
                MainActor.assumeIsolated {
                    guard let self, !self.stopped else { return }
                    self.publications += out.filter(Self.isPublication).count
                    for m in out { self.deliver(m) }
                    if pending && self.diagnostics { self.scheduleDiagnostics() }
                }
            }
        }
    }

    /// Publications from a document run (`DocumentResult.language`).
    func deliverPublications(_ messages: [String]) {
        guard !stopped, diagnostics else { return }
        publications += messages.count
        for m in messages { deliver(m) }
    }

    /// The editor closed: drop a pending evaluation and deliver nothing
    /// more.
    func stop() {
        stopped = true
        pendingDiagnostics?.cancel()
        pendingDiagnostics = nil
    }

    private static func isPublication(_ message: String) -> Bool {
        message.contains("\"textDocument/publishDiagnostics\"")
    }

    private func scheduleDiagnostics() {
        pendingDiagnostics?.cancel()
        pendingDiagnostics = Task { @MainActor [weak self] in
            try? await Task.sleep(for: Self.debounce)
            guard !Task.isCancelled else { return }
            self?.publishDiagnostics()
        }
    }

    private func publishDiagnostics() {
        let server = self.server
        diagnosticsQueue.async { [weak self] in
            let out = (try? server.publishDiagnostics()) ?? []
            DispatchQueue.main.async {
                MainActor.assumeIsolated {
                    guard let self, !self.stopped else { return }
                    self.publications += out.count
                    for m in out { self.deliver(m) }
                }
            }
        }
    }
}
