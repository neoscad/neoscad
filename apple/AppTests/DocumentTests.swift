// The app's own paths, hosted in the app: a document's text round trip
// and the Design > Render command, from the document's action to the
// report the console area shows. UI scripting (clicking the menu) needs
// Accessibility permission, so the tests call the action the menu item
// sends instead.

import AppKit
import NeoSCADCore
import Testing

@testable import NeoSCAD

private let scad = "org.openscad.scad"

@MainActor
@Suite struct DocumentTests {
    @Test func textRoundTripsAsUTF8() throws {
        let doc = SCADDocument()
        let text = "cube(1); // größe\n"
        try doc.read(from: Data(text.utf8), ofType: scad)
        #expect(doc.model.text == text)
        #expect(try doc.data(ofType: scad) == Data(text.utf8))
    }

    @Test func textThatIsNotUTF8IsRefused() {
        let doc = SCADDocument()
        #expect(throws: CocoaError.self) {
            try doc.read(from: Data([0x63, 0xFF, 0xFE]), ofType: scad)
        }
    }

    @Test func renderShowsTheStatistics() async throws {
        let doc = SCADDocument()
        doc.model.text = "cube(10);"
        doc.renderDocument(nil)
        await doc.renderTask?.value
        guard case .rendered(let r, _) = doc.model.report else {
            Issue.record("expected a render, got \(doc.model.report)")
            return
        }
        #expect(r.geometry?.volume == 1000)
        #expect(ConsoleView.describe(r).contains("volume 1000"))
        doc.close()
    }

    @Test func renderShowsASyntaxError() async throws {
        let doc = SCADDocument()
        doc.model.text = "cube(10);\ncube(;\n"
        doc.renderDocument(nil)
        await doc.renderTask?.value
        guard case .rendered(let r, _) = doc.model.report else {
            Issue.record("expected a render, got \(doc.model.report)")
            return
        }
        #expect(r.exitCode != 0)
        #expect(r.diagnostics.first?.code == "syntax-error")
        #expect(r.diagnostics.first?.span?.startLine == 2)
        doc.close()
    }
}
