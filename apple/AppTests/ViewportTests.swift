// The 3D view end to end: a MetalView's own CAMetalLayer handed to the
// Rust core as a pointer, a preview rendered into it, and a frame read
// back from the layer's drawable.
//
// Headless: the view is not put in a window. A window that is not on
// screen reports itself occluded, and wgpu then (rightly) declines to
// hand out a drawable, so a test that needs a frame regardless of the
// screen's state uses a layer with no window, which wgpu draws into
// unconditionally. The layer, the pointer and the draw path are the
// app's; only the display link (which needs a window) is not exercised.

import AppKit
import NeoSCADCore
import Testing

@testable import NeoSCAD

/// Pixels that differ from the background of `rgba`'s first pixel.
private func foreground(_ image: ViewportImage) -> Int {
    let px = [UInt8](image.rgba)
    let bg = Array(px[0..<3])
    var n = 0
    for i in stride(from: 0, to: px.count, by: 4)
    where (0..<3).contains(where: { abs(Int(px[i + $0]) - Int(bg[$0])) > 2 }) {
        n += 1
    }
    return n
}

@MainActor
@Suite struct ViewportTests {
    @Test func aPreviewDrawsIntoTheViewsLayer() async throws {
        let doc = SCADDocument()
        let controller = doc.model.viewport
        let viewport = try #require(controller.viewport, "\(controller.error ?? "")")
        let view = MetalView(controller: controller)
        view.setFrameSize(NSSize(width: 64, height: 48))
        view.attachLayer(scale: 2, readable: true)
        #expect(view.isAttached)
        #expect(try viewport.samples() == 4)
        // Axes and grid off, so everything not background is the model.
        controller.update {
            $0.axes = false
            $0.grid = false
        }

        let empty = try viewport.readPixels()
        #expect(empty.width == 128 && empty.height == 96)
        #expect(foreground(empty) == 0)

        doc.model.text = "cube(10);"
        doc.previewDocument(nil)
        await doc.renderTask?.value
        guard case .rendered(let r, .preview) = doc.model.report else {
            Issue.record("expected a preview, got \(doc.model.report)")
            return
        }
        #expect(r.exitCode == 0)
        let image = try viewport.readPixels()
        #expect(foreground(image) > 128 * 96 / 10, "the cube fills the view")

        // A camera move asks for exactly one frame.
        try viewport.orbit(dx: 30, dy: 0)
        #expect(try viewport.needsDraw())
        #expect(try viewport.draw().drawn)
        #expect(try !viewport.needsDraw())

        view.detachLayer()
        #expect(!view.isAttached)
        #expect(throws: CoreError.self) { try viewport.readPixels() }
        doc.close()
    }

    @Test func menuTogglesChangeTheSettings() throws {
        let doc = SCADDocument()
        let before = try #require(doc.model.viewport.settings)
        doc.toggleGrid(nil)
        doc.useOrthographic(nil)
        doc.useHeadlight(nil)
        let after = try #require(doc.model.viewport.settings)
        #expect(after.grid == !before.grid)
        #expect(after.orthographic)
        #expect(after.lighting == .headlight)
        let item = NSMenuItem(
            title: "", action: #selector(SCADDocument.toggleGrid(_:)), keyEquivalent: "")
        #expect(doc.validateMenuItem(item))
        #expect(item.state == (after.grid ? .on : .off))
        doc.close()
    }
}
