// The 3D view: an NSView backed by a CAMetalLayer that the Rust core draws
// into (docs/audits/macos-prep.md §3). The view owns the layer; Rust makes
// a wgpu surface from it (crates/ffi/src/layer.rs) and draws there.
//
// Lifetimes. The layer is attached to the Rust viewport when the view
// enters a window and detached when it leaves one or its window closes.
// wgpu retains the layer while attached, so the surface can never outlive
// the layer; detaching drops the surface and its retain, so a closed view
// is never kept alive or drawn into by the core.
//
// Threads. Everything here runs on the main thread (the class is
// @MainActor, and the display link is added to the main run loop): the
// layer is created, attached and resized there, and every frame is drawn
// there. That is what wgpu-hal's Metal surface requires (acquiring a
// drawable reads the window's occlusion state, an AppKit property of the
// main thread), and a frame is cheap: geometry was built and uploaded on
// the engine's queue before the frame was asked for.
//
// Pacing. The display link (NSView.displayLink, macOS 14) fires at the
// display's refresh rate, 120 Hz on a ProMotion panel. It is paused when
// nothing changes and resumed by `requestFrame()`: a camera move, a new
// model, a resize or a settings change. An idle view draws nothing.
//
// Input (OpenSCAD's mouse preset, src/core/MouseConfig.h, plus trackpad
// gestures): drag to orbit; right-drag, middle-drag, option-drag or a
// two-finger scroll to pan; the mouse wheel or a pinch to zoom; the rotate
// gesture turns the model about z; a two-finger double tap is View All.

import AppKit
import NeoSCADCore
import QuartzCore

@MainActor
final class MetalView: NSView {
    let controller: ViewportController
    private let metalLayer = CAMetalLayer()
    private var link: CADisplayLink?
    private(set) var isAttached = false
    /// Refreshes in a row with nothing to draw; the link pauses after a
    /// few, so an idle window costs nothing.
    private var idleTicks = 0
    private var closeObserver: NSObjectProtocol?
    private let stats = FrameStats()

    init(controller: ViewportController) {
        self.controller = controller
        super.init(frame: NSRect(x: 0, y: 0, width: 400, height: 300))
        wantsLayer = true
        // Redraw during a live resize rather than stretching the old frame.
        layerContentsRedrawPolicy = .duringViewResize
        setAccessibilityIdentifier("viewport")
        setAccessibilityRole(.image)
        setAccessibilityLabel("3D view")
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) {
        fatalError("MetalView is created in code")
    }

    override func makeBackingLayer() -> CALayer {
        metalLayer
    }

    override var isOpaque: Bool { true }
    override var acceptsFirstResponder: Bool { true }
    override func acceptsFirstMouse(for event: NSEvent?) -> Bool { true }

    // MARK: Lifetime

    override func viewWillMove(toWindow newWindow: NSWindow?) {
        super.viewWillMove(toWindow: newWindow)
        if newWindow == nil {
            detachLayer()
        }
    }

    override func viewDidMoveToWindow() {
        super.viewDidMoveToWindow()
        // Already set up for this window (a move between superviews).
        guard let window, link == nil else { return }
        attachLayer(scale: window.backingScaleFactor)
        closeObserver = NotificationCenter.default.addObserver(
            forName: NSWindow.willCloseNotification, object: window, queue: .main
        ) { [weak self] _ in
            MainActor.assumeIsolated { self?.detachLayer() }
        }
        let link = displayLink(target: self, selector: #selector(step(_:)))
        // Up to the panel's maximum: ProMotion's 120 Hz where there is one.
        link.preferredFrameRateRange = CAFrameRateRange(
            minimum: 60, maximum: 120, preferred: 120)
        // Common modes, so frames keep coming during a drag or a live
        // resize (the event tracking run loop mode).
        link.add(to: .main, forMode: .common)
        self.link = link
        appearanceDidChange()
        startBenchmarkIfAsked()
        requestFrame()
    }

    /// Hand the layer to the Rust viewport at `scale` pixels per point.
    /// `readable` keeps frames copyable for `readPixels` (tests only).
    func attachLayer(scale: CGFloat, readable: Bool = false) {
        guard !isAttached, let viewport = controller.viewport else { return }
        metalLayer.contentsScale = scale
        let size = bounds.size
        let layer = metalLayer
        do {
            try withExtendedLifetime(layer) {
                // The one raw pointer that crosses the bridge: unretained,
                // and alive for the call because `layer` is (Rust retains
                // it before returning; see crates/ffi/src/layer.rs).
                let address = UInt64(UInt(bitPattern: Unmanaged.passUnretained(layer).toOpaque()))
                try viewport.attachLayer(
                    layer: address, width: size.width, height: size.height,
                    scale: scale, readable: readable)
            }
            isAttached = true
            controller.view = self
        } catch {
            NSLog("NeoSCAD: cannot attach the 3D view: \(error)")
        }
    }

    /// Stop the link and release the layer from the core. Safe to call
    /// more than once.
    func detachLayer() {
        link?.invalidate()
        link = nil
        if let o = closeObserver {
            NotificationCenter.default.removeObserver(o)
            closeObserver = nil
        }
        if isAttached {
            _ = try? controller.viewport?.detach()
            isAttached = false
        }
    }

    // MARK: Size and appearance

    override func setFrameSize(_ newSize: NSSize) {
        super.setFrameSize(newSize)
        sizeDidChange()
    }

    override func viewDidChangeBackingProperties() {
        super.viewDidChangeBackingProperties()
        if let scale = window?.backingScaleFactor {
            metalLayer.contentsScale = scale
        }
        sizeDidChange()
    }

    private func sizeDidChange() {
        guard isAttached else { return }
        try? controller.viewport?.resize(
            width: bounds.width, height: bounds.height, scale: metalLayer.contentsScale)
        if inLiveResize {
            // Draw now, inside the resize, so the frame matches the new
            // size instead of being stretched until the next refresh.
            drawFrame()
        } else {
            requestFrame()
        }
    }

    override func viewDidChangeEffectiveAppearance() {
        super.viewDidChangeEffectiveAppearance()
        appearanceDidChange()
    }

    private func appearanceDidChange() {
        let dark = effectiveAppearance.bestMatch(from: [.aqua, .darkAqua]) == .darkAqua
        controller.appearanceChanged(dark: dark)
    }

    // MARK: Frames

    /// Something changed: draw at the next refresh.
    func requestFrame() {
        idleTicks = 0
        link?.isPaused = false
    }

    @objc private func step(_ link: CADisplayLink) {
        if stats.benchmarking {
            try? controller.viewport?.orbit(dx: 1, dy: 0)
        }
        let drew = drawFrame(timestamp: link.timestamp, period: link.duration)
        if drew {
            idleTicks = 0
        } else {
            idleTicks += 1
            if idleTicks > 3 {
                link.isPaused = true
            }
        }
    }

    /// Draw if a frame is due. Whether the link should keep running (a
    /// frame was drawn, or one is due but the layer could not take it).
    @discardableResult
    private func drawFrame(timestamp: CFTimeInterval? = nil, period: CFTimeInterval? = nil)
        -> Bool
    {
        guard isAttached, let viewport = controller.viewport else { return false }
        guard let report = try? viewport.draw() else { return false }
        if report.drawn {
            stats.record(cpuMs: report.cpuMs, timestamp: timestamp, period: period)
        }
        return report.drawn || report.deferred
    }

    // MARK: Input

    override func mouseDown(with event: NSEvent) {
        window?.makeFirstResponder(self)
    }

    override func mouseDragged(with event: NSEvent) {
        if event.modifierFlags.contains(.option) {
            controller.perform { try $0.pan(dx: event.deltaX, dy: event.deltaY) }
        } else {
            controller.perform { try $0.orbit(dx: event.deltaX, dy: event.deltaY) }
        }
    }

    override func rightMouseDragged(with event: NSEvent) {
        controller.perform { try $0.pan(dx: event.deltaX, dy: event.deltaY) }
    }

    override func otherMouseDragged(with event: NSEvent) {
        controller.perform { try $0.pan(dx: event.deltaX, dy: event.deltaY) }
    }

    override func scrollWheel(with event: NSEvent) {
        if event.hasPreciseScrollingDeltas {
            // A trackpad (or Magic Mouse) two-finger scroll: pan, the
            // model moving with the fingers under the user's scrolling
            // direction setting. With option held it zooms instead, for
            // a trackpad without pinching.
            if event.modifierFlags.contains(.option) {
                controller.perform { try $0.zoom(notches: event.scrollingDeltaY / 20) }
            } else {
                controller.perform {
                    try $0.pan(dx: event.scrollingDeltaX, dy: event.scrollingDeltaY)
                }
            }
        } else {
            // A mouse wheel: OpenSCAD's zoom, a tenth per notch, rolling
            // away from you to go closer.
            controller.perform { try $0.zoom(notches: event.scrollingDeltaY) }
        }
    }

    override func magnify(with event: NSEvent) {
        controller.perform { try $0.magnify(factor: 1 + event.magnification) }
    }

    override func rotate(with event: NSEvent) {
        controller.perform { try $0.turn(degrees: Double(event.rotation)) }
    }

    override func smartMagnify(with event: NSEvent) {
        controller.perform { try $0.viewAll() }
    }

    // MARK: Measurement

    /// `NEOSCAD_VIEWPORT_BENCH=seconds`: orbit one point a refresh for that
    /// long, then print frame statistics to standard error. How the frame
    /// numbers in the 8c report were taken; off otherwise.
    private func startBenchmarkIfAsked() {
        guard
            let s = ProcessInfo.processInfo.environment["NEOSCAD_VIEWPORT_BENCH"],
            let seconds = Double(s), seconds > 0
        else { return }
        let screenMax = window?.screen?.maximumFramesPerSecond ?? 0
        DispatchQueue.main.asyncAfter(deadline: .now() + 3) { [weak self] in
            guard let self else { return }
            self.stats.start()
            self.requestFrame()
            DispatchQueue.main.asyncAfter(deadline: .now() + seconds) { [weak self] in
                guard let self else { return }
                let line = self.stats.finish(
                    pixels: (
                        Int(self.bounds.width * self.metalLayer.contentsScale),
                        Int(self.bounds.height * self.metalLayer.contentsScale)
                    ),
                    screenMaxFPS: screenMax,
                    samples: Int((try? self.controller.viewport?.samples()) ?? 0))
                // The camera distance shows whether a model arrived: the
                // first one is fitted, moving the eye off OpenSCAD's
                // default 140.
                let vpd = (try? self.controller.viewport?.camera().vpd) ?? .nan
                let full = line + String(format: "; camera distance %.1f", vpd)
                FileHandle.standardError.write(Data((full + "\n").utf8))
            }
        }
    }
}

/// Frame timings for the benchmark mode.
@MainActor
private final class FrameStats {
    private(set) var benchmarking = false
    private var cpu: [Double] = []
    private var intervals: [Double] = []
    private var periods: [Double] = []
    private var last: CFTimeInterval?

    func start() {
        benchmarking = true
        cpu = []
        intervals = []
        periods = []
        last = nil
    }

    func record(cpuMs: Double, timestamp: CFTimeInterval?, period: CFTimeInterval?) {
        guard benchmarking else { return }
        cpu.append(cpuMs)
        if let t = timestamp {
            if let l = last { intervals.append((t - l) * 1000) }
            last = t
        }
        if let p = period { periods.append(p * 1000) }
    }

    func finish(pixels: (Int, Int), screenMaxFPS: Int, samples: Int) -> String {
        benchmarking = false
        func pct(_ xs: [Double], _ p: Double) -> Double {
            guard !xs.isEmpty else { return .nan }
            let s = xs.sorted()
            return s[min(s.count - 1, Int(Double(s.count) * p))]
        }
        let fps = intervals.isEmpty ? 0 : 1000 / pct(intervals, 0.5)
        return String(
            format:
                "viewport-bench: %d frames at %dx%d px, %dx MSAA; draw cpu min %.3f p10 %.3f p50 %.3f p95 %.3f max %.3f ms; "
                + "frame interval p50 %.2f p95 %.2f ms (%.0f fps); link period %.2f ms; screen max %d fps",
            cpu.count, pixels.0, pixels.1, samples, cpu.min() ?? .nan, pct(cpu, 0.1), pct(cpu, 0.5),
            pct(cpu, 0.95),
            cpu.max() ?? .nan, pct(intervals, 0.5), pct(intervals, 0.95), fps,
            pct(periods, 0.5), screenMaxFPS)
    }
}
