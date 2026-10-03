// What the app does for an agent's request (`AgentHost`, crates/ffi/src/
// agent.rs): each call names a document by its number and runs on one of
// the link's threads, never the main thread.
//
// Each call hops to the main actor, where the documents, the editor and
// the view live, and waits there for the answer; the link's thread is the
// one that waits, so the main thread never blocks on an agent. The waits
// are bounded a little under the command line's own (15 s for most, 90 s
// for a capture, 150 s for an edit, which may wait for the user's Apply:
// crates/cli/src/mcp/tools/browser.rs), so the agent hears why rather
// than a bare timeout.
//
// The capture's drawing and readback run here on the link's thread, not
// on the main thread: the view's own frames keep going while the GPU
// draws the copy (`Viewport.captureAsShown` copies under its lock and
// draws outside it).

import AppKit
import NeoSCADCore

final class DocumentsAgentHost: AgentHost, @unchecked Sendable {
    // Set once, read from any thread; the service itself is only touched
    // on the main actor.
    private weak var service: AgentService?

    init(service: AgentService) {
        self.service = service
    }

    func read(document: UInt64) throws -> AgentDocumentState {
        try onMain(seconds: 12, "read the document") { doc in await doc.agentRead() }
            .get(document, self)
    }

    func edit(document: UInt64, edit: AgentEditRequest) throws -> AgentEditOutcome {
        try onMain(seconds: 140, "apply the edit") { doc in try await doc.agentApply(edit) }
            .get(document, self)
    }

    func reveal(document: UInt64, from: EditorPosition, to: EditorPosition) throws {
        try onMain(seconds: 12, "show the code") { doc in doc.agentReveal(from: from, to: to) }
            .get(document, self)
    }

    func camera(document: UInt64, change: AgentCameraChange) throws -> AgentCamera {
        try onMain(seconds: 12, "move the camera") { doc in try doc.agentCamera(change) }
            .get(document, self)
    }

    func capture(document: UInt64, maxSide: UInt32) throws -> AgentCapture {
        let viewport = try onMain(seconds: 80, "finish the preview") { doc in
            try await doc.agentViewForCapture()
        }.get(document, self)
        do {
            return try viewport.captureAsShown(maxSide: maxSide)
        } catch let e as CoreError {
            throw AgentHostError.Refused(message: "NeoSCAD could not capture the view: \(e.message)")
        }
    }

    func annotate(document: UInt64, lines: [ViewLine], markers: [ViewMarker]) throws {
        try onMain(seconds: 12, "draw the marks") { doc in
            try doc.agentAnnotate(lines: lines, markers: markers)
        }.get(document, self)
    }

    // MARK: Hopping to the main actor

    /// A request's work on one document, run on the main actor.
    struct Work<T: Sendable> {
        let seconds: Int
        let what: String
        let body: @MainActor @Sendable (SCADDocument) async throws -> T

        /// Run it on the main actor for document `id` and wait here (on
        /// the link's thread) for the answer.
        func get(_ id: UInt64, _ host: DocumentsAgentHost) throws -> T {
            guard !Thread.isMainThread else {
                // Waiting here for the main actor would never end.
                throw AgentHostError.Refused(message: "NeoSCAD was asked on its main thread")
            }
            let box = ResultBox<T>()
            let done = DispatchSemaphore(value: 0)
            let body = self.body
            let task = Task { @MainActor [weak service = host.service] in
                defer { done.signal() }
                guard let doc = service?.document(id) else {
                    box.set(.failure(AgentHostError.Refused(message: "That document is no longer open in NeoSCAD.")))
                    return
                }
                do {
                    box.set(.success(try await body(doc)))
                } catch {
                    box.set(.failure(error))
                }
            }
            if done.wait(timeout: .now() + .seconds(seconds)) == .timedOut {
                task.cancel()
                throw AgentHostError.Refused(
                    message: "NeoSCAD did not \(what) within \(seconds) s (is it showing a dialog?)")
            }
            switch box.get() {
            case .success(let value):
                return value
            case .failure(let error as AgentHostError):
                throw error
            case .failure(let error as CoreError):
                throw AgentHostError.Refused(message: error.message)
            case .failure(let error):
                throw AgentHostError.Refused(message: error.localizedDescription)
            case nil:
                throw AgentHostError.Refused(message: "NeoSCAD did not \(what)")
            }
        }
    }

    private func onMain<T: Sendable>(
        seconds: Int, _ what: String,
        _ body: @escaping @MainActor @Sendable (SCADDocument) async throws -> T
    ) -> Work<T> {
        Work(seconds: seconds, what: what, body: body)
    }
}

/// A result handed from the main actor to the waiting thread.
private final class ResultBox<T>: @unchecked Sendable {
    private let lock = NSLock()
    private var result: Result<T, Error>?

    func set(_ r: Result<T, Error>) {
        lock.withLock { result = r }
    }

    func get() -> Result<T, Error>? {
        lock.withLock { result }
    }
}
