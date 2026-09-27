// Latency of the document loop, measured the way a person sees it: from an
// edit made in the page to (a) the editor's lint markers showing the new
// text's diagnostics and (b) the 3D view's model swapped in (the report
// turning `.rendered` for a request started after the edit; the next
// display refresh draws it). Opt-in, because it types for a while and only
// prints numbers:
//
//     TEST_RUNNER_NEOSCAD_PIPELINE_BENCH=1 xcodebuild ... test \
//         -only-testing:NeoSCADAppTests/PipelineLatencyTests
//
// The BOSL2 case needs `.reference/BOSL2` in the checkout.

import AppKit
import NeoSCADCore
import Testing

@testable import NeoSCAD

@MainActor
@Suite(.serialized, .enabled(if: ProcessInfo.processInfo.environment["NEOSCAD_PIPELINE_BENCH"] != nil))
struct PipelineLatencyTests {
    private static let repo = URL(fileURLWithPath: #filePath).deletingLastPathComponent()
        .deletingLastPathComponent().deletingLastPathComponent()

    @Test func csgExample() async throws {
        let file = Self.repo.appendingPathComponent(".reference/openscad/examples/Basics/CSG.scad")
        let text = try String(contentsOf: file, encoding: .utf8)
        try await measure("CSG.scad", text: text, file: file)
    }

    @Test func bosl2Part() async throws {
        let std = Self.repo.appendingPathComponent(".reference/BOSL2/std.scad")
        guard FileManager.default.fileExists(atPath: std.path) else {
            print("pipeline bench: skipped BOSL2 (no .reference/BOSL2)")
            return
        }
        let text = "include <\(std.path)>\ncuboid([20, 10, 5], rounding = 1);\n"
        try await measure("BOSL2 cuboid", text: text, file: nil)
    }

    /// Type a line that draws a warning, then delete it, `rounds` times;
    /// print p50 and p95 of each latency.
    private func measure(_ name: String, text: String, file: URL?, rounds: Int = 8) async throws {
        let doc = try await openDocument(text, file: file)
        let clock = ContinuousClock()
        // Let the first preview and the first diagnostics settle.
        try await waitUntil("the first preview") {
            if case .rendered = doc.model.report { return true }
            return false
        }
        _ = try await markers(doc, count: 0, clock: clock)
        var toMarkers: [Double] = []
        var toClear: [Double] = []
        var toView: [Double] = []
        let end = text.utf16.count
        let line = "cubx(1);\n"
        for i in 0..<(2 * rounds) {
            let adding = i % 2 == 0
            let requests = doc.requestCount
            let t0 = clock.now
            if adding {
                try await editInPage(doc, from: end, to: end, insert: line)
            } else {
                try await editInPage(doc, from: end, to: end + line.utf16.count, insert: "")
            }
            async let m = markers(doc, count: adding ? 1 : 0, clock: clock)
            let v = try await rendered(doc, after: requests, clock: clock)
            let mt = try await m
            // A deleted line takes its marker with it at once, so only
            // the added warning measures the diagnostics' path.
            if adding { toMarkers.append(ms(mt - t0)) } else { toClear.append(ms(mt - t0)) }
            toView.append(ms(v - t0))
            try await Task.sleep(for: .milliseconds(200))
        }
        print("pipeline bench: \(name): keystroke to a new marker \(stats(toMarkers)) ms")
        print("pipeline bench: \(name): keystroke to viewport \(stats(toView)) ms")
        doc.close()
    }

    /// When the page shows `count` warning or error markers.
    private func markers(
        _ doc: SCADDocument, count: Int, clock: ContinuousClock
    ) async throws -> ContinuousClock.Instant {
        let deadline = clock.now + .seconds(30)
        while clock.now < deadline {
            let n =
                try await doc.model.editor.call(
                    "return document.querySelectorAll('.cm-lintRange-warning, .cm-lintRange-error').length")
                as? Int ?? -1
            if n == count { return clock.now }
            try await Task.sleep(for: .milliseconds(2))
        }
        Issue.record("timed out waiting for \(count) markers")
        throw CancellationError()
    }

    /// When a request started after `requests` has reported its result.
    private func rendered(
        _ doc: SCADDocument, after requests: Int, clock: ContinuousClock
    ) async throws -> ContinuousClock.Instant {
        let deadline = clock.now + .seconds(30)
        while clock.now < deadline {
            if doc.requestCount > requests, case .rendered = doc.model.report { return clock.now }
            try await Task.sleep(for: .milliseconds(2))
        }
        Issue.record("timed out waiting for the view")
        throw CancellationError()
    }

    private func stats(_ xs: [Double]) -> String {
        let s = xs.sorted()
        let at = { (q: Double) in s[min(s.count - 1, Int(Double(s.count) * q))] }
        return String(format: "p50 %.0f, p95 %.0f, min %.0f", at(0.5), at(0.95), s.first ?? 0)
    }

    private func ms(_ d: Duration) -> Double {
        Double(d.components.seconds) * 1000 + Double(d.components.attoseconds) / 1e15
    }
}
