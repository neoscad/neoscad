// What the check and measure panels do to their document
// (Panels/CheckPanel.swift, Panels/MeasurePanel.swift): run `check` and
// `measure` on the document's current text with its customizer values,
// and keep the 3D view's annotations (findings, the section's outline,
// picked points) in step with the panels.
//
// Both requests run detached from the document loop (crates/ffi/src/
// inspect.rs): a check does not cancel the live preview, and typing does
// not cancel a check. A newer check or measurement cancels the older one
// of its own kind.

import AppKit
import NeoSCADCore

extension SCADDocument {
    /// The core's path for this document with its text sent: the
    /// document loop's first step without starting a run, for a panel
    /// request made before any run (or after the copies fell out of step).
    func panelPath() throws -> (Engine, String) {
        let engine: Engine
        switch CoreService.shared {
        case .success(let e): engine = e
        case .failure(let e): throw e
        }
        let path = fileURL?.path ?? untitledPath
        if corePath != path || !coreInSync {
            if let old = corePath, old != path { _ = try? engine.close(old) }
            corePath = path
            try engine.update(path, text: model.text)
            coreInSync = true
        }
        return (engine, path)
    }

    var runOptions: RunOptions {
        RunOptions(overrides: overrides, parts: model.partsEnabled)
    }

    /// The window's parts toggle: the view runs again with it, and the
    /// panels' last results (made without it) are stale.
    func setParts(_ on: Bool) {
        guard model.partsEnabled != on else { return }
        model.partsEnabled = on
        if let mode = lastMode { run(mode) } else { schedulePreview() }
    }

    // MARK: Check

    func runCheck() {
        guard !isClosed else { return }
        let check = model.check
        let engine: Engine, path: String
        do { (engine, path) = try panelPath() } catch {
            check.error = describe(error)
            return
        }
        checkTask?.cancel()
        check.running = true
        let options = check.settings.options
        let run = runOptions
        checkTask = Task { @MainActor [weak self] in
            do {
                let r = try await engine.check(path, options: options, run: run)
                guard !Task.isCancelled else { return }
                check.report = r
                check.error = nil
                if let s = check.selected, !r.findings.contains(where: { $0.id == s }) {
                    check.selected = nil
                }
            } catch CoreError.Cancelled {
                return
            } catch let e as CoreError {
                check.error = e.message
            } catch {
                check.error = "\(error)"
            }
            check.running = false
            self?.updateAnnotations()
        }
    }

    /// Show a finding in the view: its marker picked out with its box, and
    /// the view turned to its point.
    func selectFinding(_ id: UInt32?) {
        model.check.selected = id
        if let id, let f = model.check.report?.findings.first(where: { $0.id == id }),
            f.point.count == 3
        {
            model.viewport.perform { try $0.lookAt(point: f.point) }
        }
        updateAnnotations()
    }

    // MARK: Measure

    func runMeasure() {
        guard !isClosed else { return }
        let m = model.measure
        let engine: Engine, path: String
        do { (engine, path) = try panelPath() } catch {
            m.error = describe(error)
            return
        }
        measureTask?.cancel()
        m.running = true
        let run = runOptions
        measureTask = Task { @MainActor [weak self] in
            do {
                let r = try await engine.measure(path, run: run)
                guard !Task.isCancelled else { return }
                m.result = r
                m.error = nil
                m.picks = []
                let names = Set(r.parts.map(\.name))
                if let t = m.target, !names.contains(t) { m.target = nil }
                if let a = m.partA, !names.contains(a) { m.partA = nil }
                if let b = m.partB, !names.contains(b) { m.partB = nil }
                if !m.offsetRange.contains(m.offset) {
                    m.offset = (m.offsetRange.lowerBound + m.offsetRange.upperBound) / 2
                }
            } catch CoreError.Cancelled {
                return
            } catch let e as CoreError {
                m.error = e.message
            } catch {
                m.error = "\(error)"
            }
            m.running = false
            self?.measureBetween()
            self?.updateSection()
        }
    }

    func measureBetween() {
        let m = model.measure
        guard let meas = m.result?.measurement, let a = m.partA, let b = m.partB, a != b else {
            m.between = nil
            updateAnnotations()
            return
        }
        m.between = try? meas.between(a: a, b: b)
        updateAnnotations()
    }

    /// Cut again at the panel's plane. Off the main thread (a big model's
    /// slice takes a while), the latest request winning.
    func updateSection() {
        let m = model.measure
        m.sectionGeneration += 1
        guard m.sectionShown, let meas = m.result?.measurement else {
            m.section = nil
            updateAnnotations()
            return
        }
        let (generation, axis, offset, target) = (m.sectionGeneration, m.axis, m.offset, m.target)
        Task { @MainActor [weak self] in
            let r = await Task.detached {
                try? meas.section(axis: axis, offset: offset, part: target)
            }.value
            guard m.sectionGeneration == generation else { return }
            m.section = r
            self?.updateAnnotations()
        }
    }

    func clearPicks() {
        model.measure.picks = []
        updateAnnotations()
    }

    /// A click in the view at `point` (points from the top left): with
    /// picking on, the surface point under it. Whether the click was
    /// taken.
    func pick(at point: CGPoint) -> Bool {
        let m = model.measure
        guard m.picking, let meas = m.result?.measurement,
            let viewport = model.viewport.viewport,
            let ray = try? viewport.rayAt(x: point.x, y: point.y)
        else { return false }
        guard let hit = try? meas.pick(origin: ray.origin, direction: ray.direction) else {
            return true
        }
        if m.picks.count >= 2 { m.picks = [] }
        m.picks.append(hit)
        updateAnnotations()
        return true
    }

    // MARK: The view's annotations

    /// Draw what the panels show into the view: every located finding's
    /// numbered marker (the selected one with its box), the section's
    /// outline, the closest points between two parts and the picked
    /// points.
    func updateAnnotations() {
        var lines: [ViewLine] = []
        var markers: [ViewMarker] = []
        let check = model.check
        for f in check.report?.findings ?? [] where f.point.count == 3 && f.severity != .info {
            let selected = f.id == check.selected
            let color = Annotation.color(f.severity, selected: selected)
            markers.append(ViewMarker(point: f.point, label: "\(f.id)", color: color))
            if selected, let lo = f.bboxMin, let hi = f.bboxMax {
                lines += Annotation.box(lo, hi, color: color)
            }
        }
        let m = model.measure
        if let s = m.section {
            for c in s.outline {
                lines.append(ViewLine(points: c, closed: true, color: Annotation.section))
            }
        }
        if let b = m.between, let pa = b.pointA, let pb = b.pointB {
            lines.append(ViewLine(points: pa + pb, closed: false, color: Annotation.distance))
            markers.append(ViewMarker(point: pa, label: b.a, color: Annotation.distance))
            markers.append(ViewMarker(point: pb, label: b.b, color: Annotation.distance))
        }
        for (i, p) in m.picks.enumerated() {
            markers.append(ViewMarker(point: p, label: i == 0 ? "A" : "B", color: Annotation.pick))
        }
        if m.picks.count == 2 {
            lines.append(ViewLine(points: m.picks[0] + m.picks[1], closed: false, color: Annotation.pick))
        }
        model.viewport.perform { try $0.setAnnotations(lines: lines, markers: markers) }
    }
}

/// The annotations' colours: the snapshot's marker colours for findings
/// (`crates/session/src/snapshot.rs`, `marker_color`), and colours of
/// their own for the measure panel.
enum Annotation {
    static let section: [Float] = [0.0, 0.62, 0.85, 1]
    static let distance: [Float] = [0.58, 0.40, 0.74, 1]
    static let pick: [Float] = [0.10, 0.60, 0.20, 1]

    static func color(_ s: FindingSeverity, selected: Bool) -> [Float] {
        let c: [Float] =
            switch s {
            case .error: [190, 20, 20]
            case .warning: [200, 110, 0]
            case .info: [40, 90, 190]
            }
        // Unselected markers fade, so the chosen one stands out.
        return c.map { $0 / 255 } + [selected ? 1 : 0.55]
    }

    /// The twelve edges of a box.
    static func box(_ lo: [Double], _ hi: [Double], color: [Float]) -> [ViewLine] {
        guard lo.count == 3, hi.count == 3 else { return [] }
        func p(_ i: Int) -> [Double] {
            [i & 1 == 0 ? lo[0] : hi[0], i & 2 == 0 ? lo[1] : hi[1], i & 4 == 0 ? lo[2] : hi[2]]
        }
        let bottom = [0, 1, 3, 2].flatMap(p)
        let top = [4, 5, 7, 6].flatMap(p)
        var out = [
            ViewLine(points: bottom, closed: true, color: color),
            ViewLine(points: top, closed: true, color: color),
        ]
        for i in 0..<4 { out.append(ViewLine(points: p(i) + p(i + 4), closed: false, color: color)) }
        return out
    }
}

/// An error for the user: the core's own sentence, or the system's.
func describe(_ error: Error) -> String {
    (error as? CoreError)?.message ?? error.localizedDescription
}

/// The first error line of a failed request's console: why it failed,
/// for the panel to say (the document's own console keeps its run's).
func firstError(_ console: String) -> String? {
    console.split(separator: "\n").first { $0.hasPrefix("ERROR") || $0.contains("Parser error") }
        .map(String.init)
}
