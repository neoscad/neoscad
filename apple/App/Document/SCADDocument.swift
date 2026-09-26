// A `.scad` file: UTF-8 text, edited in a window of its own.
//
// The document owns the text (`DocumentModel.text`) and saves it; the Rust
// core holds a copy under the document's path while rendering, so includes
// of this file from other open files see the unsaved text too.

import AppKit
import NeoSCADCore
import SwiftUI

/// The state a document window shows. Main-actor only: AppKit reads and
/// writes documents on the main thread here (concurrent reading is off).
@MainActor
@Observable
final class DocumentModel {
    var text = ""
    var report: RenderReport = .idle
}

/// What the console area shows about the last render.
enum RenderReport {
    case idle
    case running
    case rendered(RenderResult)
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

    override class var autosavesInPlace: Bool { true }

    override func makeWindowControllers() {
        let window = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 900, height: 700),
            styleMask: [.titled, .closable, .miniaturizable, .resizable],
            backing: .buffered, defer: false)
        window.contentViewController = NSHostingController(
            rootView: DocumentView(model: model))
        window.setContentSize(NSSize(width: 900, height: 700))
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

    // MARK: Render

    /// Design > Render (F6): render the current text in the core, off the
    /// main actor, and show the statistics and diagnostics in the console
    /// area. A newer render supersedes an unfinished one.
    @objc func renderDocument(_ sender: Any?) {
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
        model.report = .running
        let model = self.model
        renderTask = Task { @MainActor in
            do {
                let result = try await engine.render(path)
                model.report = .rendered(result)
            } catch CoreError.Cancelled {
                // A newer render took over; it reports instead.
            } catch let e as CoreError {
                model.report = .failed(e.message)
            } catch {
                model.report = .failed("\(error)")
            }
        }
    }

    override func close() {
        renderTask?.cancel()
        if let path = corePath, case .success(let engine) = CoreService.shared {
            _ = try? engine.close(path)
        }
        super.close()
    }
}
