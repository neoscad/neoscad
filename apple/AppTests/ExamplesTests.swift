// File > Examples: the menu lists the core's examples, and one opens as a
// new untitled document with the example's text (heavy ones without
// running).

import AppKit
import NeoSCADCore
import Testing

@testable import NeoSCAD

@MainActor
@Suite struct ExamplesTests {
    @Test func theMenuListsTheCoresExamples() throws {
        let menu = ExampleMenu.shared.menu()
        let ids = menu.items.compactMap { $0.representedObject as? String }
        #expect(ids == (try examples()).map(\.id))
        #expect(ids.contains("csg"))
        #expect(menu.items.allSatisfy { $0.action == #selector(ExampleMenu.open(_:)) })
    }

    @Test func anExampleOpensAsAnUntitledDocument() throws {
        let doc = try #require(try ExampleMenu.open(id: "csg"))
        defer { doc.close() }
        #expect(doc.fileURL == nil)
        #expect(doc.model.text == (try exampleSource(id: "csg")))
        #expect(doc.pendingPreview != nil, "a light example runs")
    }

    @Test func aHeavyExampleWaitsForPreviewOrRender() throws {
        let heavy = try #require((try examples()).first { !$0.autorun })
        let doc = try #require(try ExampleMenu.open(id: heavy.id))
        defer { doc.close() }
        #expect(doc.model.text == (try exampleSource(id: heavy.id)))
        #expect(doc.pendingPreview == nil)
        #expect(doc.requestCount == 0)
    }
}
