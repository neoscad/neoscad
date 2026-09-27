// The Swift face of the Rust core: an async wrapper over the generated
// `Core` (Generated/neoscad_ffi.swift, from crates/ffi).
//
// Every core call blocks: parsing and evaluation take milliseconds, a
// render can take minutes. Calling one on the main actor would freeze the
// UI, so the heavy operations here are `async` and run on a concurrent
// queue of the engine's own. Cancelling the Swift task cancels the Rust
// request (through `Core.cancel`), and the session itself cancels a
// request that an edit or a newer request on the same document has made
// stale; either way the call throws `CoreError.Cancelled`.
//
// Document calls (`open`, `update`, `edit`, `close`, `cancel`) and the
// limits are quick (they copy text and flip flags) and stay synchronous,
// so the order of edits is exactly the order of the calls.

import Foundation

public final class Engine: Sendable {
    /// The generated binding, for anything this wrapper does not cover.
    public let core: Core

    /// Concurrent, so a `cancel` or a render of another document never
    /// waits behind a long render. The session is built for concurrent
    /// requests (every operation takes `&self`).
    private let queue = DispatchQueue(
        label: "org.neoscad.core", qos: .userInitiated, attributes: .concurrent)

    /// A core with the agent resource limits (`defaultLimits()`), the
    /// bundled MCAD mounted under `resourceDirectory/libraries`.
    public init(resourceDirectory: String? = nil, testHooks: Bool = false) throws {
        core = try Core(config: CoreConfig(resourceDir: resourceDirectory, testHooks: testHooks))
    }

    // MARK: Documents

    /// Open `path` with unsaved `text` (every read of the path, includes
    /// included, sees it), or without text to read it from disk.
    @discardableResult
    public func open(_ path: String, text: String?) throws -> DocInfo {
        try core.open(path: path, text: text)
    }

    /// Replace a document's whole text.
    @discardableResult
    public func update(_ path: String, text: String) throws -> DocInfo {
        try core.update(path: path, text: text)
    }

    /// Apply edits; offsets are UTF-8 byte offsets (see `TextEdit`).
    @discardableResult
    public func edit(_ path: String, edits: [TextEdit]) throws -> DocInfo {
        try core.edit(path: path, edits: edits)
    }

    @discardableResult
    public func close(_ path: String) throws -> Bool {
        try core.close(path: path)
    }

    /// Stop every request running on `path`.
    @discardableResult
    public func cancel(_ path: String) throws -> UInt32 {
        try core.cancel(path: path)
    }

    // MARK: Limits

    public func setLimits(_ limits: ResourceLimits) throws {
        try core.setLimits(limits: limits)
    }

    public func limits() throws -> ResourceLimits {
        try core.limits()
    }

    // MARK: Operations

    public func evaluate(_ path: String) async throws -> Evaluation {
        try await run(path) { try $0.evaluate(path: path) }
    }

    public func render(_ path: String, mode: RenderMode = .render) async throws -> RenderResult {
        try await run(path) { try $0.render(path: path, mode: mode) }
    }

    /// Render (or preview) and show the result in `viewport`: the scene is
    /// built and uploaded to the GPU on the engine's queue, so the main
    /// thread only draws. The mesh never leaves Rust. A newer call for the
    /// same viewport wins even if this one finishes later.
    public func render(
        _ path: String, mode: RenderMode, into viewport: Viewport
    ) async throws -> RenderResult {
        try await run(path) { try $0.renderInto(path: path, mode: mode, viewport: viewport) }
    }

    /// One run of a document for its window (the document loop): evaluate
    /// and build it once, hand its diagnostics to `language` (the editor's
    /// server, made with `hostDiagnostics`), show the model in `viewport`,
    /// and return the console and the files it read. A newer run of the
    /// same document cancels this one.
    public func runDocument(
        _ path: String, request: DocumentRequest, viewport: Viewport?, language: LanguageServer?,
        early: (@Sendable ([String]) -> Void)? = nil
    ) async throws -> DocumentResult {
        let listener = early.map { Listener(language: $0) }
        return try await run(path) {
            try $0.runDocument(
                path: path, request: request, viewport: viewport, language: language,
                listener: listener)
        }
    }

    /// Hands a run's early language notifications (the evaluation's
    /// diagnostics, before the geometry stage) to `language`, on the
    /// engine's thread.
    private final class Listener: DocumentListener {
        let language: @Sendable ([String]) -> Void
        init(language: @escaping @Sendable ([String]) -> Void) { self.language = language }
        func language(messages: [String]) { language(messages) }
    }

    /// The customizer's groups of a document's current text. Parses the
    /// main file alone, on the engine's queue (a large file takes a few
    /// milliseconds); never cancelled by the document's runs.
    public func parameters(_ path: String) async throws -> [ParameterGroup] {
        let core = self.core
        return try await withCheckedThrowingContinuation { continuation in
            queue.async {
                continuation.resume(with: Result { try core.parameters(path: path) })
            }
        }
    }

    public func snapshot(
        _ path: String, options: SnapshotOptions = SnapshotOptions()
    ) async throws -> SnapshotResult {
        try await run(path) { try $0.snapshot(path: path, options: options) }
    }

    /// Render and write to `output` (absolute), as `format` or by the
    /// output's extension.
    public func export(
        _ path: String, to output: String, format: String? = nil
    ) async throws -> ExportResult {
        try await run(path) { try $0.export(path: path, output: output, format: format) }
    }

    /// `body` on the engine's queue; cancelling the calling task cancels
    /// the core's requests on `path`.
    private func run<T: Sendable>(
        _ path: String, _ body: @escaping @Sendable (Core) throws -> T
    ) async throws -> T {
        let core = self.core
        return try await withTaskCancellationHandler {
            try await withCheckedThrowingContinuation { continuation in
                queue.async {
                    continuation.resume(with: Result { try body(core) })
                }
            }
        } onCancel: {
            _ = try? core.cancel(path: path)
        }
    }
}

extension CoreError {
    /// A sentence for the user.
    public var message: String {
        switch self {
        case .Cancelled: "Cancelled by a newer request."
        case .InvalidArgument(let message): "Invalid argument: \(message)"
        case .Failed(let message): message
        case .Panicked(let message): "Internal error in the core: \(message)"
        }
    }
}
