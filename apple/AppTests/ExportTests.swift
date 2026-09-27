// File > Export's data path (Document/Export.swift): it writes a valid
// binary STL and 3MF and an image of the view, and every failure becomes an
// alert's text rather than nothing (docs/audits/agent-surface.md, finding
// 5).

import AppKit
import NeoSCADCore
import Testing

@testable import NeoSCAD

private func tempDirectory() throws -> URL {
    let dir = FileManager.default.temporaryDirectory
        .appendingPathComponent("neoscad-export-\(UUID().uuidString)")
    try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
    return dir
}

@MainActor
@Suite(.serialized) struct ExportTests {
    @Test func writesBinarySTLAnd3MF() async throws {
        let dir = try tempDirectory()
        defer { try? FileManager.default.removeItem(at: dir) }
        let doc = try await openDocument("color(\"red\") cube(10);\n")
        let stl = dir.appendingPathComponent("cube.stl")
        var s = ExportSettings()
        s.format = .binaryStl
        let o = await doc.performExport(to: stl, settings: s)
        #expect(o == .written(stl, bytes: 684))
        let data = try Data(contentsOf: stl)
        #expect(data.count == 84 + 50 * 12)
        #expect(data[80..<84].withUnsafeBytes { $0.loadUnaligned(as: UInt32.self) } == 12)

        let tmf = dir.appendingPathComponent("cube.3mf")
        s.format = .threeMF
        s.threeMFColorMode = .selectedOnly
        s.threeMFColor = "#00ff00"
        guard case .written = await doc.performExport(to: tmf, settings: s) else {
            Issue.record("the 3MF was not written")
            return
        }
        let zip = try Data(contentsOf: tmf)
        #expect(zip.starts(with: [0x50, 0x4b, 0x03, 0x04]))
        doc.close()
    }

    @Test func anImageOfTheViewIsAPNG() async throws {
        let dir = try tempDirectory()
        defer { try? FileManager.default.removeItem(at: dir) }
        let doc = try await openDocument("cube(10);\n")
        await doc.renderTask?.value
        let png = dir.appendingPathComponent("view.png")
        var s = ExportSettings()
        s.format = .viewImage
        s.imageWidth = 320
        s.imageHeight = 200
        guard case .written = await doc.performExport(to: png, settings: s) else {
            Issue.record("the image was not written")
            return
        }
        let image = try #require(NSImage(contentsOf: png)?.representations.first)
        #expect(image.pixelsWide == 320 && image.pixelsHigh == 200)
        doc.close()
    }

    @Test func failuresBecomeAlerts() async throws {
        let dir = try tempDirectory()
        defer { try? FileManager.default.removeItem(at: dir) }
        let doc = try await openDocument("square(10);\n")
        // A 2D model to a 3D format.
        let stl = dir.appendingPathComponent("square.stl")
        var s = ExportSettings()
        s.format = .binaryStl
        guard case .failed(let a) = await doc.performExport(to: stl, settings: s) else {
            Issue.record("a 2D model exported as STL")
            return
        }
        #expect(a.title.contains("square.stl"))
        #expect(a.message.contains("not a 3D object"))
        #expect(!FileManager.default.fileExists(atPath: stl.path))
        // A folder that does not exist.
        s.format = .svg
        let svg = dir.appendingPathComponent("missing/square.svg")
        guard case .failed(let b) = await doc.performExport(to: svg, settings: s) else {
            Issue.record("an export to a missing folder succeeded")
            return
        }
        #expect(b.message.contains("Can't write"))
        // A syntax error.
        doc.model.text = "square(;\n"
        guard case .failed(let c) = await doc.performExport(to: dir.appendingPathComponent("x.svg"), settings: s)
        else {
            Issue.record("a model with a syntax error exported")
            return
        }
        #expect(c.message.contains("Parser error"))
        // Cancelling is not a failure to report.
        #expect(ExportOutcome.of(CoreError.Cancelled, to: svg) == .cancelled)
        doc.close()
    }

    @Test func aTwoDimensionalRenderStartsOnSVG() async throws {
        let doc = try await openDocument("square(10);\n")
        doc.renderDocument(nil)
        await doc.renderTask?.value
        #expect(doc.initialExportSettings().format.dimension != 3)
        doc.close()
    }
}
