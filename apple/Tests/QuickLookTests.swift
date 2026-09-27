// The Quick Look extensions' entry point (Core/QuickLook.swift), outside
// the extensions: what a preview or thumbnail gets for a good model, a
// broken one, one whose includes cannot be read, and one that runs past
// the deadline. The sandbox itself is exercised by `qlmanage` against the
// built app, not here; a missing sibling file fails the same way.

import Foundation
import Testing

@testable import NeoSCADCore

private let pngSignature: [UInt8] = [0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A]

/// A fresh directory with `files` written into it.
private func directory(_ files: [String: String]) throws -> URL {
    let dir = FileManager.default.temporaryDirectory
        .appendingPathComponent("neoscad-ql-\(UUID().uuidString)")
    try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
    for (name, text) in files {
        try text.write(to: dir.appendingPathComponent(name), atomically: true, encoding: .utf8)
    }
    return dir
}

/// Width and height from a PNG's IHDR chunk.
private func pngSize(_ png: Data) -> (Int, Int) {
    let b = [UInt8](png)
    let be = { (i: Int) in b[i..<i + 4].reduce(0) { $0 << 8 | Int($1) } }
    return (be(16), be(20))
}

@Suite struct QuickLookTests {
    @Test func aModelGetsAPictureOfTheRequestedSize() async throws {
        let dir = try directory(["cube.scad": "color(\"teal\") cube(10);\n"])
        defer { try? FileManager.default.removeItem(at: dir) }
        let r = await QuickLookRender.render(
            fileAt: dir.appendingPathComponent("cube.scad"), width: 200, height: 120)
        let png = try #require(r.png, "\(r.notes)")
        #expect(png.starts(with: pngSignature))
        #expect(pngSize(png) == (200, 120))
        #expect(r.notes.isEmpty)
        #expect(r.source.contains("cube(10)"))
        #expect(!r.timedOut)
    }

    @Test func bundledMCADWorks() async throws {
        let dir = try directory([
            "gear.scad": "use <MCAD/regular_shapes.scad>\nhexagon_prism(5, 10);\n"
        ])
        defer { try? FileManager.default.removeItem(at: dir) }
        let r = await QuickLookRender.render(
            fileAt: dir.appendingPathComponent("gear.scad"), width: 64, height: 64)
        #expect(r.png != nil, "\(r.notes)")
        #expect(r.unreadable.isEmpty)
    }

    /// In the sandbox a sibling cannot be read; here it is simply missing,
    /// which the loader reports the same way. The rest still renders.
    @Test func unreadableIncludesAreNamedAndTheRestRenders() async throws {
        let dir = try directory([
            "main.scad": """
                include <parts.scad>
                use <lib/helpers.scad>
                cube(5);
                translate([10, 0, 0]) import("bracket.stl");
                """
        ])
        defer { try? FileManager.default.removeItem(at: dir) }
        let r = await QuickLookRender.render(
            fileAt: dir.appendingPathComponent("main.scad"), width: 64, height: 64)
        #expect(r.png != nil, "\(r.notes)")
        #expect(r.unreadable == ["parts.scad", "lib/helpers.scad", "bracket.stl"], "\(r.notes)")
        let note = try #require(r.notes.first)
        #expect(note.contains("parts.scad"))
        #expect(note.contains("NeoSCAD"))
    }

    @Test func aBrokenModelHasNoPictureAndSaysWhere() async throws {
        let dir = try directory(["bad.scad": "cube(10);\ncube(;\n"])
        defer { try? FileManager.default.removeItem(at: dir) }
        let r = await QuickLookRender.render(
            fileAt: dir.appendingPathComponent("bad.scad"), width: 64, height: 64)
        #expect(r.png == nil)
        #expect(r.notes.contains { $0.contains("line 2") }, "\(r.notes)")
        #expect(r.source == "cube(10);\ncube(;\n")
    }

    /// A model that runs away: the core's time limit or the watchdog stops
    /// it, and the answer comes back near the deadline, not after the
    /// model finishes.
    @Test func aRunawayModelStopsAtTheDeadline() async throws {
        // 2^40 calls, 40 deep: no depth or size limit trips, only time.
        let text = "function f(n) = n == 0 ? 0 : f(n - 1) + f(n - 1);\n"
            + "cube(f(40));\n"
        let start = Date()
        let r = await QuickLookRender.render(
            path: "/NeoSCAD-swift-test/runaway.scad", text: text, width: 64, height: 64,
            deadline: 1)
        let elapsed = Date().timeIntervalSince(start)
        #expect(r.png == nil)
        #expect(!r.notes.isEmpty)
        #expect(r.timedOut)
        #expect(elapsed < 3, "took \(elapsed) s")
    }

    @Test func thePageEscapesTheSourceAndRefersToTheImage() {
        let r = QuickLookResult(
            png: Data(pngSignature), source: "echo(\"<b>&\");", notes: ["Skipped: a<b>.scad"],
            timedOut: false, unreadable: [])
        let html = QuickLookPage.html(r, title: "x.scad")
        #expect(html.contains("cid:\(QuickLookPage.imageID)"))
        #expect(html.contains("echo(&quot;&lt;b&gt;&amp;&quot;);"))
        #expect(html.contains("<li>Skipped: a&lt;b&gt;.scad</li>"))
        let noPicture = QuickLookPage.html(
            QuickLookResult(png: nil, source: "", notes: [], timedOut: false, unreadable: []),
            title: "x.scad")
        #expect(!noPicture.contains("<img"))
    }

    @Test func theLimitsAreTight() {
        let l = QuickLookRender.limits
        #expect(l.timeSeconds == 5)
        #expect(l.memoryBytes == 512 << 20)
    }
}
