// The check and measure panels' data paths in the app (Panels/
// CheckPanel.swift, Panels/MeasurePanel.swift, Document/Inspect.swift):
// a thin wall becomes an error finding that the view turns to; a cube's
// section has its exact area; two parts measure apart with the parts
// toggle on; and a click in the view picks a point on the surface.

import AppKit
import NeoSCADCore
import Testing

@testable import NeoSCAD

@MainActor
@Suite(.serialized) struct PanelTests {
    @Test func aThinWallIsAnErrorFindingTheViewTurnsTo() async throws {
        let doc = try await openDocument("cube(20);\ntranslate([30, 0, 0]) cube([10, 0.3, 10]);\n")
        doc.runCheck()
        await doc.checkTask?.value
        let report = try #require(doc.model.check.report)
        #expect(doc.model.check.error == nil)
        let f = try #require(report.findings.first { $0.code == "thin-wall" })
        #expect(f.severity == .error)
        #expect(f.value == 0.3)
        #expect(report.errors >= 1)
        // Selecting it moves the view's centre to its point.
        doc.selectFinding(f.id)
        #expect(doc.model.check.selected == f.id)
        let vpt = try #require(doc.model.viewport.viewport?.camera().vpt)
        for i in 0..<3 { #expect(abs(vpt[i] - f.point[i]) < 1e-9) }
        doc.close()
    }

    @Test func aCheckDoesNotCancelTheLivePreview() async throws {
        let doc = try await openDocument("cube(10);\n")
        await doc.renderTask?.value
        doc.run(.preview)
        doc.runCheck()
        await doc.renderTask?.value
        await doc.checkTask?.value
        guard case .rendered = doc.model.report else {
            Issue.record("the preview did not finish: \(doc.model.report)")
            return
        }
        #expect(doc.model.check.report?.failed == false)
        doc.close()
    }

    @Test func aCubesSectionHasItsExactArea() async throws {
        let doc = try await openDocument("cube(10);\n")
        doc.runMeasure()
        await doc.measureTask?.value
        let m = doc.model.measure
        let r = try #require(m.result)
        #expect(r.model?.volume == 1000)
        #expect(r.model?.centroid == [5, 5, 5])
        m.sectionShown = true
        m.axis = .z
        m.offset = 5
        doc.updateSection()
        try await waitUntil("the section arrives") { m.section != nil }
        let s = try #require(m.section)
        #expect(s.area == 100)
        #expect(s.perimeter == 40)
        #expect(s.plane == "z=5")
        #expect(m.offsetRange == 0...10)
        doc.close()
    }

    @Test func partsMeasureApartWithTheToggleOn() async throws {
        let text = "part(\"a\") cube(10);\npart(\"b\") translate([15, 0, 0]) cube(10);\n"
        let doc = try await openDocument(text)
        doc.setParts(true)
        doc.runMeasure()
        await doc.measureTask?.value
        let m = doc.model.measure
        #expect(m.result?.parts.map(\.name) == ["a", "b"])
        m.partA = "a"
        m.partB = "b"
        doc.measureBetween()
        #expect(m.between?.distance == 5)
        doc.runCheck()
        await doc.checkTask?.value
        #expect(doc.model.check.report?.parts == ["a", "b"])
        doc.close()
    }

    @Test func aClickInTheViewPicksASurfacePoint() async throws {
        let doc = try await openDocument("cube(10);\n")
        await doc.renderTask?.value
        doc.runMeasure()
        await doc.measureTask?.value
        let m = doc.model.measure
        m.picking = true
        let view = try #require(doc.model.viewport.view)
        try await waitUntil("the view has a size") { view.bounds.width > 0 }
        // The first model is fitted, so the view's middle looks at the
        // cube's centre and the ray meets its surface.
        let middle = CGPoint(x: view.bounds.midX, y: view.bounds.midY)
        #expect(doc.pick(at: middle))
        let p = try #require(m.picks.first)
        #expect(p.contains { abs($0) < 1e-6 || abs($0 - 10) < 1e-6 })
        #expect(doc.pick(at: CGPoint(x: middle.x + 5, y: middle.y)))
        #expect((m.pickDistance ?? 0) > 0)
        // Without picking a click is the view's own.
        m.picking = false
        #expect(!doc.pick(at: middle))
        doc.close()
    }
}
