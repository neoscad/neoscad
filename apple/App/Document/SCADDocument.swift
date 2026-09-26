// A `.scad` file: UTF-8 text, edited in a window of its own.
//
// The document owns the text (`DocumentModel.text`) and saves it; the Rust
// core holds a copy under the document's path while rendering, so includes
// of this file from other open files see the unsaved text too.
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
    var text = "" {
        didSet {
            if text != oldValue { textDidChange?() }
        }
    }
    var report: RenderReport = .idle
    /// The 3D view's state (the Rust viewport and its view).
    @ObservationIgnored let viewport = ViewportController()
    @ObservationIgnored var textDidChange: (() -> Void)?
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
    private var corePath: String?
    private lazy var untitledPath =
        "/NeoSCAD-untitled/\(UUID().uuidString)/Untitled.scad"
    /// The render in flight (the app tests await it).
    private(set) var renderTask: Task<Void, Never>?
    /// The preview waiting for typing to pause.
    private var pendingPreview: Task<Void, Never>?
    /// What the last request built, so a scheme change can rebuild it.
    private var lastMode: RenderMode?

    /// How long typing must pause before the model is previewed again.
    static let previewDelay: Duration = .milliseconds(400)

    override init() {
        super.init()
        MainActor.assumeIsolated {
            model.textDidChange = { [weak self] in self?.schedulePreview() }
            model.viewport.onSchemeChange = { [weak self] in self?.rerender() }
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
        }
        corePath = path
        do {
            try engine.update(path, text: model.text)
        } catch let e as CoreError {
            model.report = .failed(e.message)
            return
        } catch {
            model.report = .failed("\(error)")
            return
        }
        renderTask?.cancel()
        lastMode = mode
        model.report = .running(mode)
        let model = self.model
        let viewport = model.viewport
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
            } catch CoreError.Cancelled {
                // A newer request took over; it reports instead.
            } catch let e as CoreError {
                model.report = .failed(e.message)
            } catch {
                model.report = .failed("\(error)")
            }
        }
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
        if let path = corePath, case .success(let engine) = CoreService.shared {
            _ = try? engine.close(path)
        }
        super.close()
    }
}
