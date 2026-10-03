// The bundled `neoscad` CLI and the stable link agent clients run it
// through (App/Agents/CommandLineTool.swift). Every link here is made in a
// temporary directory: the developer's own link
// (~/Library/Application Support/NeoSCAD/bin) is never touched.

import Foundation
import Testing

@testable import NeoSCAD

struct CommandLineToolTests {
    private func scratch() throws -> URL {
        let dir = FileManager.default.temporaryDirectory
            .appendingPathComponent("NeoSCADToolLink-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        return dir
    }

    private func run(_ tool: URL, _ args: [String], input: String? = nil) throws -> String {
        let p = Process()
        p.executableURL = tool
        p.arguments = args
        let out = Pipe()
        p.standardOutput = out
        p.standardError = FileHandle.nullDevice
        let inPipe = Pipe()
        p.standardInput = inPipe
        try p.run()
        if let input { inPipe.fileHandleForWriting.write(Data(input.utf8)) }
        try inPipe.fileHandleForWriting.close()
        let data = out.fileHandleForReading.readDataToEndOfFile()
        p.waitUntilExit()
        return String(decoding: data, as: UTF8.self)
    }

    /// The app carries the CLI in Contents/Helpers, signed, and it runs.
    @Test func bundleCarriesARunnableCLI() throws {
        let tool = try #require(CommandLineTool.bundledTool())
        #expect(tool.path.hasSuffix("NeoSCAD.app/Contents/Helpers/neoscad"))
        let version = try run(tool, ["--version"])
        #expect(version.hasPrefix("neoscad "), "\(version)")
    }

    @Test func linkIsMadeThenKeptThenMoved() throws {
        let dir = try scratch()
        defer { try? FileManager.default.removeItem(at: dir) }
        let tool = try #require(CommandLineTool.bundledTool())
        let bin = dir.appendingPathComponent("NeoSCAD/bin", isDirectory: true)
        let link = bin.appendingPathComponent("neoscad")

        #expect(try CommandLineTool.refreshLink(to: tool, in: bin) == .created)
        #expect(try FileManager.default.destinationOfSymbolicLink(atPath: link.path) == tool.path)
        #expect(try CommandLineTool.refreshLink(to: tool, in: bin) == .current)

        // The app moved: the link follows it.
        let moved = dir.appendingPathComponent("Moved.app/Contents/Helpers/neoscad")
        #expect(try CommandLineTool.refreshLink(to: moved, in: bin) == .updated(previous: tool.path))
        #expect(try FileManager.default.destinationOfSymbolicLink(atPath: link.path) == moved.path)
        // No temporary link left beside it.
        #expect(try FileManager.default.contentsOfDirectory(atPath: bin.path) == ["neoscad"])

        // Through the link, the CLI runs and serves MCP.
        _ = try CommandLineTool.refreshLink(to: tool, in: bin)
        #expect(try run(link, ["--version"]).hasPrefix("neoscad "))
        let initialize = """
            {"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"app-test","version":"0"}}}
            {"jsonrpc":"2.0","method":"notifications/initialized"}
            {"jsonrpc":"2.0","id":2,"method":"tools/list"}

            """
        let reply = try run(link, ["mcp"], input: initialize)
        #expect(reply.contains("\"serverInfo\""), "\(reply)")
        #expect(reply.contains("\"name\":\"render\""), "\(reply)")
    }

    /// A file of the user's own at the link's path is never replaced.
    @Test func aRegularFileIsLeftAlone() throws {
        let dir = try scratch()
        defer { try? FileManager.default.removeItem(at: dir) }
        let mine = dir.appendingPathComponent("neoscad")
        try Data("#!/bin/sh\n".utf8).write(to: mine)
        let outcome = try CommandLineTool.refreshLink(to: URL(fileURLWithPath: "/x/neoscad"), in: dir)
        guard case .skipped = outcome else {
            Issue.record("expected skipped, got \(outcome)")
            return
        }
        #expect(try String(contentsOf: mine, encoding: .utf8) == "#!/bin/sh\n")
    }

    @Test func translocatedAndReadOnlyCopiesAreNotLinked() throws {
        let translocated = URL(fileURLWithPath:
            "/var/folders/xy/T/AppTranslocation/1234/d/NeoSCAD.app")
        #expect(CommandLineTool.reasonNotToLink(appAt: translocated) != nil)
        // The test host lives in DerivedData, on a writable volume.
        #expect(CommandLineTool.reasonNotToLink(appAt: Bundle.main.bundleURL) == nil)
    }

    @Test func linkDirectoryCanBeMovedByADefault() throws {
        let suite = "org.neoscad.NeoSCADAppTests.toolLink"
        let defaults = try #require(UserDefaults(suiteName: suite))
        defer { defaults.removePersistentDomain(forName: suite) }
        #expect(CommandLineTool.linkDirectory(defaults).path.hasSuffix(
            "Library/Application Support/NeoSCAD/bin"))
        defaults.set("/tmp/elsewhere", forKey: CommandLineTool.directoryDefaultsKey)
        #expect(CommandLineTool.linkPath(defaults).path == "/tmp/elsewhere/neoscad")
    }
}
