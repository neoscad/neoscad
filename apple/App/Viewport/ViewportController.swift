// The document's 3D view state, on the Swift side: the Rust viewport (which
// holds the model on the GPU, the camera and the view settings), the view
// that shows it while the window is open, and the menu commands.
//
// The camera stays in Rust (crates/ffi/src/viewport.rs explains why): this
// class forwards pointer deltas and commands and asks the view for a frame.
// It never draws itself; the view's display link does, on the main thread.

import AppKit
import NeoSCADCore

@MainActor
final class ViewportController {
    /// `nil` when the GPU could not be opened (the reason is in `error`);
    /// the window then shows the reason instead of a view.
    let viewport: Viewport?
    let error: String?

    /// The view drawing this viewport, while it is in a window.
    weak var view: MetalView?

    /// The colour scheme pair: OpenSCAD's default for a light appearance,
    /// and a dark scheme of OpenSCAD's own for a dark one.
    static let lightScheme = "Cornfield"
    static let darkScheme = "Tomorrow Night"

    /// Called after the scheme changes with the appearance: the model's
    /// face colours come from the scheme it was built in, so the document
    /// renders it again.
    var onSchemeChange: (() -> Void)?

    /// A click in the view that did not drag, at a point in points from
    /// the view's top left; returns whether it was taken (then the view
    /// draws again). The measure panel picks surface points with it.
    var onClick: ((CGPoint) -> Bool)?

    init() {
        do {
            viewport = try Viewport(colorScheme: Self.lightScheme)
            error = nil
        } catch let e as CoreError {
            viewport = nil
            error = "No 3D view: \(e.message)"
        } catch {
            viewport = nil
            self.error = "No 3D view: \(error)"
        }
    }

    /// Run `body` on the viewport and schedule a frame. Errors from the
    /// core are dropped here: a camera move that fails leaves the old
    /// view, which is the most useful thing to show.
    func perform(_ body: (Viewport) throws -> Void) {
        guard let viewport else { return }
        try? body(viewport)
        view?.requestFrame()
    }

    var settings: ViewportSettings? {
        try? viewport?.settings()
    }

    func update(_ change: (inout ViewportSettings) -> Void) {
        guard var s = settings else { return }
        change(&s)
        perform { try $0.setSettings(s: s) }
    }

    /// Follow the system appearance: the light or dark scheme.
    func appearanceChanged(dark: Bool) {
        guard let viewport else { return }
        let name = dark ? Self.darkScheme : Self.lightScheme
        guard (try? viewport.colorScheme()) != name else { return }
        perform { try $0.setColorScheme(name: name) }
        onSchemeChange?()
    }

    /// Stop drawing into the view's layer (the window is closing).
    func detach() {
        view?.detachLayer()
        _ = try? viewport?.detach()
    }
}
