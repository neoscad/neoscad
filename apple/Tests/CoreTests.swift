// The bridge end to end: Swift -> generated bindings -> crates/ffi ->
// session. crates/ffi's own Rust tests cover the details; these prove the
// generated layer and the async wrapper carry them intact.

import Foundation
import Testing

@testable import NeoSCADCore

/// A path with no file behind it: the text lives in the core's buffer.
private let doc = "/NeoSCAD-swift-test/model.scad"

private func engine(_ text: String) throws -> Engine {
    let e = try Engine(testHooks: true)
    try e.open(doc, text: text)
    return e
}

@Suite struct CoreTests {
    @Test func rendersACubeWithVolume1000() async throws {
        let r = try await engine("cube(10);").render(doc)
        #expect(r.exitCode == 0, "\(r.console)")
        let g = try #require(r.geometry)
        #expect(g.dimensions == 3)
        #expect(abs((g.volume ?? 0) - 1000) < 1e-9)
        #expect(g.bboxMax == [10, 10, 10])
        #expect(g.manifold == true)
    }

    @Test func aSyntaxErrorHasALineAndColumn() async throws {
        let r = try await engine("cube(10);\ncube(;\n").evaluate(doc)
        #expect(r.exitCode != 0)
        let d = try #require(r.diagnostics.first)
        #expect(d.code == "syntax-error")
        #expect(d.severity == .error)
        #expect(d.line == 2)
        let span = try #require(d.span)
        #expect(span.startLine == 2)
        #expect(span.startColumn == 6)
    }

    @Test func aRustPanicIsAnErrorNotACrash() async throws {
        let e = try engine("cube(1);")
        #expect {
            try e.core.debugPanic()
        } throws: { error in
            guard case CoreError.Panicked(let message) = error else { return false }
            return message.contains("debug_panic")
        }
        // The core is still usable after the panic.
        let r = try await e.render(doc)
        #expect(r.exitCode == 0)
    }

    @Test func aSnapshotIsPNG() async throws {
        let s = try await engine("cube(10);").snapshot(
            doc, options: SnapshotOptions(width: 256, height: 256))
        #expect(s.exitCode == 0)
        let png = try #require(s.png)
        #expect(png.starts(with: [0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A]))
        #expect(s.summaryJson.hasPrefix("{"))
    }

    @Test func setLimitsMakesARunawaySphereFail() async throws {
        let e = try engine("sphere(r=1, $fn=48);")
        #expect(try await e.render(doc).exitCode == 0)
        var limits = try e.limits()
        #expect(limits == (try defaultLimits()))
        limits.fragments = 32
        try e.setLimits(limits)
        // A model not yet rendered: a cached result is reused as it is.
        try e.update(doc, text: "sphere(r=1, $fn=1e6);")
        let r = try await e.render(doc)
        #expect(r.exitCode != 0)
        #expect(r.diagnostics.first?.code == "resource-limit", "\(r.console)")
    }

    @Test func anExportWritesTheFile() async throws {
        let dir = FileManager.default.temporaryDirectory
            .appendingPathComponent("neoscad-swift-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: dir) }
        let out = dir.appendingPathComponent("cube.stl").path
        let r = try await engine("cube(10);").export(doc, to: out)
        #expect(r.exitCode == 0, "\(r.console)")
        #expect(r.format == "stl")
        let data = try Data(contentsOf: URL(fileURLWithPath: out))
        #expect(UInt64(data.count) == r.bytes)
    }

    @Test func cancellingTheTaskCancelsTheRequest() async throws {
        // A model that runs for a long time without tripping a limit: a
        // billion loop iterations (three nested ranges, each well under
        // OpenSCAD's range size limit) that add nothing to the list.
        let e = try engine(
            "r = [0:999]; x = [for (i = r) for (j = r) for (k = r) if (false) i]; echo(len(x));")
        let task = Task { try await e.evaluate(doc) }
        try await Task.sleep(for: .milliseconds(300))
        task.cancel()
        await #expect(throws: CoreError.Cancelled) { try await task.value }
    }
}
