// The Shortcuts actions' work (Intents/SCADIntents.swift): each intent's
// perform, on files on disk, through the app's core under the agent
// limits.

import AppIntents
import Foundation
import NeoSCADCore
import Testing

@testable import NeoSCAD

private func model(_ text: String, name: String = "model.scad") throws -> URL {
    let dir = FileManager.default.temporaryDirectory
        .appendingPathComponent("neoscad-intents-\(UUID().uuidString)")
    try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
    let url = dir.appendingPathComponent(name)
    try Data(text.utf8).write(to: url)
    return url
}

@Suite(.serialized) struct IntentTests {
    @Test func theCoreRunsUnderTheAgentLimits() throws {
        let engine = try IntentRunner.engine()
        #expect(try engine.limits() == defaultLimits())
    }

    @Test func renderWritesTheFormatAsked() async throws {
        let url = try model("cube(10);\n")
        var intent = RenderSCADIntent()
        intent.file = IntentFile(fileURL: url, filename: url.lastPathComponent)
        intent.format = .binstl
        let result = try await intent.perform()
        let out = try #require(result.value?.fileURL)
        #expect(out.lastPathComponent == "model.stl")
        #expect(try Data(contentsOf: out).count == 84 + 50 * 12)
        // 3MF by the runner directly, beside the intent.
        let tmf = try await IntentRunner.render(path: url.path, format: .threemf)
        #expect(try Data(contentsOf: tmf).starts(with: [0x50, 0x4b]))
    }

    @Test func renderFailsWithTheReason() async throws {
        let url = try model("square(10);\n")
        await #expect {
            try await IntentRunner.render(path: url.path, format: .stl)
        } throws: { e in
            (e as? IntentFailure)?.message.contains("not a 3D object") == true
        }
    }

    @Test func snapshotIsAPNG() async throws {
        let url = try model("cube(10);\n")
        var intent = SnapshotSCADIntent()
        intent.file = IntentFile(fileURL: url, filename: url.lastPathComponent)
        intent.size = 256
        let result = try await intent.perform()
        let out = try #require(result.value?.fileURL)
        #expect(try Data(contentsOf: out).starts(with: [0x89, 0x50, 0x4e, 0x47]))
    }

    @Test func checkReturnsTheReport() async throws {
        let url = try model("cube([10, 0.3, 10]);\n")
        var intent = CheckSCADIntent()
        intent.file = IntentFile(fileURL: url, filename: url.lastPathComponent)
        intent.nozzle = 0.4
        let result = try await intent.perform()
        let report = try #require(result.value)
        #expect(report.contains("thin-wall"))
        #expect(report.hasPrefix("check "))
        let (_, errors) = try await IntentRunner.check(path: url.path, nozzle: nil, bed: nil)
        #expect(errors >= 1)
    }

    @Test func aFileGivenAsDataIsWrittenFirst() throws {
        let path = try IntentRunner.modelPath(
            url: nil, data: Data("cube(1);".utf8), filename: "given.scad")
        #expect(path.hasSuffix("/given.scad"))
        #expect(try String(contentsOfFile: path, encoding: .utf8) == "cube(1);")
    }
}
