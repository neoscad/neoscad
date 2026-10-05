// AI agents on the open documents (App/Agents, Document/SCADDocument+
// Agent.swift), hosted in the app with real windows and editors:
//
// - consent: nothing listens until the user allows agents, the choice is
//   kept, and turning it off removes the socket;
// - the setup sheet's rows and actions, with a stand-in `claude` and a
//   Claude Desktop config in a temporary home, never the user's own;
// - the host's answers (read, edit and its undo, a stale version, the
//   approval bar, reveal, camera, capture, annotate) called from another
//   thread as the link calls them;
// - end to end: the bundled `neoscad mcp`, as an agent client would run
//   it, finds the app's link in a test directory and edits the document.
//
// Every socket is made in a temporary directory (`AgentLink.withDir`, and
// NEOSCAD_AGENT_DIR for the command line), so a developer's own agents
// never see the tests' app.

import AppKit
import Foundation
import NeoSCADCore
import SwiftUI
import Testing

@testable import NeoSCAD

/// A directory of its own, short enough for a socket path (104 bytes).
private func scratch(_ tag: String) throws -> URL {
    let dir = URL(fileURLWithPath: NSTemporaryDirectory(), isDirectory: true)
        .appendingPathComponent("nsa-\(tag)-\(UUID().uuidString.prefix(6))", isDirectory: true)
    try FileManager.default.createDirectory(
        at: dir, withIntermediateDirectories: true, attributes: [.posixPermissions: 0o700])
    return dir
}

private func sockets(in dir: URL) -> [String] {
    ((try? FileManager.default.contentsOfDirectory(atPath: dir.path)) ?? [])
        .filter { $0.hasSuffix(".sock") }
}

/// `text` as `m.scad` in a folder of its own, open in a window with its
/// editor loaded.
@MainActor
private func openDocument(_ text: String) async throws -> SCADDocument {
    let dir = try scratch("doc")
    let url = dir.appendingPathComponent("m.scad")
    try Data(text.utf8).write(to: url)
    let (opened, _) = try await NSDocumentController.shared.openDocument(
        withContentsOf: url, display: true)
    let doc = try #require(opened as? SCADDocument)
    let editor = doc.model.editor
    try await waitUntil("the editor is ready") { editor.isReady && editor.version != nil }
    return doc
}

@MainActor
private func finish(_ doc: SCADDocument) {
    let dir = doc.fileURL?.deletingLastPathComponent()
    doc.close()
    if let dir { try? FileManager.default.removeItem(at: dir) }
}

private func edit(
    _ version: UInt64, _ from: (UInt32, UInt32), _ to: (UInt32, UInt32), _ insert: String,
    client: String? = nil
) -> AgentEditRequest {
    AgentEditRequest(
        version: version,
        edits: [
            AgentTextEdit(
                from: EditorPosition(line: from.0, character: from.1),
                to: EditorPosition(line: to.0, character: to.1), insert: insert)
        ],
        summary: "line \(from.0 + 1)", client: client)
}

/// Call the host as the link does: from another thread, blocking it.
private func onLinkThread<T: Sendable>(_ body: @escaping @Sendable () throws -> T) async throws -> T {
    try await Task.detached { try body() }.value
}

@MainActor
@Suite(.serialized) struct AgentTests {
    // MARK: Consent

    @Test func nothingListensUntilAllowedAndTheChoiceIsKept() async throws {
        let dir = try scratch("consent")
        defer { try? FileManager.default.removeItem(at: dir) }
        let suite = "org.neoscad.NeoSCAD.agent-tests.consent"
        UserDefaults().removePersistentDomain(forName: suite)
        defer { UserDefaults().removePersistentDomain(forName: suite) }
        let defaults = try #require(UserDefaults(suiteName: suite))
        let make: (AgentHost) -> AgentLink = {
            AgentLink.withDir(host: $0, appVersion: "test", dir: dir.path)
        }

        let service = AgentService(defaults: defaults)
        service.makeLink = make
        // Never chosen: off, and the toolbar still invites.
        #expect(!service.allowed && !service.turnedOff)
        service.startIfAllowed()
        #expect(service.link == nil)
        #expect(sockets(in: dir).isEmpty)
        #expect(service.statusLine == "Connect your AI agent")

        service.setAllowed(true)
        #expect(service.link != nil)
        #expect(sockets(in: dir).count == 1)
        try await waitUntil("the status says it listens") { service.status?.listening == true }
        #expect(defaults.bool(forKey: AgentService.allowedKey))

        // Kept: the next launch listens again by itself.
        let relaunched = AgentService(defaults: defaults)
        relaunched.makeLink = make
        #expect(relaunched.allowed)
        relaunched.setAllowed(false)

        service.setAllowed(false)
        #expect(service.link == nil && service.status == nil)
        #expect(sockets(in: dir).isEmpty)
        let again = AgentService(defaults: defaults)
        // Turned off: stays off, and the toolbar control hides.
        #expect(!again.allowed && again.turnedOff)
        again.startIfAllowed()
        #expect(again.link == nil)
        #expect(sockets(in: dir).isEmpty)
    }

    // MARK: Setup

    @Test func rowsNameTheStableLink() throws {
        let service = AgentService(defaults: try #require(UserDefaults(suiteName: "agent-tests-rows")))
        let model = AgentSetupModel(service: service, cli: "/Apps/NeoSCAD/bin/neoscad")
        let clients = model.rows.map(\.client)
        #expect(clients == [.claudeCode, .claudeDesktop, .cursor, .vsCode, .other])
        for row in model.rows {
            #expect(row.copyText.contains("/Apps/NeoSCAD/bin/neoscad"), "\(row.label)")
        }
        #expect(model.rows[0].action == .runClaude)
        // The app's own link, by default.
        #expect(AgentSetupModel(service: service).cli == CommandLineTool.linkPath().path)
    }

    /// A stand-in `claude` that keeps its entry in a file and answers as
    /// Claude Code does (as crates/ffi/src/agent_setup.rs's test).
    @Test func claudeCodeIsAddedThenFoundThenReplaced() async throws {
        let dir = try scratch("claude")
        defer { try? FileManager.default.removeItem(at: dir) }
        let claude = dir.appendingPathComponent("claude")
        let state = dir.appendingPathComponent("state").path
        let log = dir.appendingPathComponent("log").path
        try """
        #!/bin/sh
        echo "$@" >> '\(log)'
        case "$2" in
        add) if [ -f '\(state)' ]; then echo "MCP server $5 already exists in user config"; exit 1; fi; touch '\(state)'; echo "Added stdio MCP server $5";;
        remove) rm -f '\(state)'; echo "Removed MCP server $5";;
        esac
        """.write(to: claude, atomically: true, encoding: .utf8)
        try FileManager.default.setAttributes([.posixPermissions: 0o755], ofItemAtPath: claude.path)

        let service = AgentService(defaults: try #require(UserDefaults(suiteName: "agent-tests-claude")))
        let model = AgentSetupModel(service: service, cli: "/Apps/NeoSCAD/bin/neoscad")
        let path = claude.path
        model.findClaude = { path }
        var asked: [AgentSetupQuestion] = []
        model.ask = { q in
            asked.append(q)
            return q == .replaceClaudeCode
        }
        let row = model.rows[0]

        await model.add(row)
        guard case .done = model.state(.claudeCode) else {
            Issue.record("\(model.state(.claudeCode))")
            return
        }
        // Agents were off, so the first Add asked once (Not Now here).
        #expect(asked == [.allowAgents])
        #expect(!service.allowed)

        await model.add(row)
        guard case .alreadySetUp = model.state(.claudeCode) else {
            Issue.record("\(model.state(.claudeCode))")
            return
        }
        await model.replaceClaudeCode(row)
        guard case .done = model.state(.claudeCode) else {
            Issue.record("\(model.state(.claudeCode))")
            return
        }
        #expect(asked == [.allowAgents, .replaceClaudeCode])
        let lines = try String(contentsOfFile: log, encoding: .utf8).split(separator: "\n")
        #expect(
            lines == [
                "mcp add --scope user neoscad -- /Apps/NeoSCAD/bin/neoscad mcp",
                "mcp add --scope user neoscad -- /Apps/NeoSCAD/bin/neoscad mcp",
                "mcp remove --scope user neoscad",
                "mcp add --scope user neoscad -- /Apps/NeoSCAD/bin/neoscad mcp",
            ])

        // Not found: say so, and the command shows to copy.
        model.findClaude = { nil }
        await model.add(row)
        guard case .notInstalled = model.state(.claudeCode) else {
            Issue.record("\(model.state(.claudeCode))")
            return
        }
    }

    /// Claude Desktop's config in a temporary home: not installed, then
    /// added with a backup and the other servers kept, then found.
    @Test func claudeDesktopIsAddedWithABackup() async throws {
        let home = try scratch("home")
        let realHome = ProcessInfo.processInfo.environment["HOME"]
        setenv("HOME", home.path, 1)
        defer {
            if let realHome { setenv("HOME", realHome, 1) }
            try? FileManager.default.removeItem(at: home)
        }
        let service = AgentService(defaults: try #require(UserDefaults(suiteName: "agent-tests-desktop")))
        let cli = "/Apps/NeoSCAD/bin/neoscad"
        var model = AgentSetupModel(service: service, cli: cli)
        model.ask = { $0 != .allowAgents }  // no real link from a test
        let row = try #require(model.rows.first { $0.client == .claudeDesktop })
        guard case .mergeConfig(let path) = row.action else {
            Issue.record("\(row.action)")
            return
        }
        #expect(path.hasPrefix(home.path))
        model.look()
        guard case .notInstalled = model.state(.claudeDesktop) else {
            Issue.record("\(model.state(.claudeDesktop))")
            return
        }

        let config = URL(fileURLWithPath: path)
        try FileManager.default.createDirectory(
            at: config.deletingLastPathComponent(), withIntermediateDirectories: true)
        let before = #"{"mcpServers": {"other": {"command": "x"}}, "theme": "dark"}"#
        try before.write(to: config, atomically: true, encoding: .utf8)
        model = AgentSetupModel(service: service, cli: cli)
        model.ask = { $0 != .allowAgents }  // no real link from a test
        await model.add(row)
        guard case .done = model.state(.claudeDesktop) else {
            Issue.record("\(model.state(.claudeDesktop))")
            return
        }
        let after = try String(contentsOf: config, encoding: .utf8)
        #expect(after.contains("\"other\"") && after.contains("\"theme\""))
        #expect(AgentSetupModel.config(at: config, runs: cli))
        let backups = try FileManager.default.contentsOfDirectory(
            atPath: config.deletingLastPathComponent().path
        ).filter { $0.contains("neoscad-backup") }
        #expect(backups.count == 1)
        let backup = config.deletingLastPathComponent().appendingPathComponent(backups[0])
        #expect(try String(contentsOf: backup, encoding: .utf8) == before)

        model = AgentSetupModel(service: service, cli: cli)
        model.look()
        guard case .alreadySetUp = model.state(.claudeDesktop) else {
            Issue.record("\(model.state(.claudeDesktop))")
            return
        }
    }

    @Test func cursorAndVSCodeOpenTheirInstallLinks() async throws {
        let service = AgentService(defaults: try #require(UserDefaults(suiteName: "agent-tests-links")))
        let model = AgentSetupModel(service: service, cli: "/Apps/NeoSCAD/bin/neoscad")
        model.ask = { _ in false }
        var opened: [URL] = []
        model.openURL = {
            opened.append($0)
            return true
        }
        for client in [AgentSetupClient.cursor, .vsCode] {
            let row = try #require(model.rows.first { $0.client == client })
            await model.add(row)
            guard case .done = model.state(client) else {
                Issue.record("\(client): \(model.state(client))")
                return
            }
        }
        #expect(opened.map(\.scheme) == ["cursor", "vscode"])
        // No app for the scheme: not installed, and the JSON to copy.
        model.openURL = { _ in false }
        await model.add(try #require(model.rows.first { $0.client == .cursor }))
        guard case .notInstalled = model.state(.cursor) else {
            Issue.record("\(model.state(.cursor))")
            return
        }
    }

    /// The client picker: Claude Code until the user picks another, the
    /// pick kept for the next sheet, and each client's panel with its own
    /// button.
    @Test func pickerStartsOnClaudeCodeAndKeepsTheChoice() throws {
        let suite = "agent-tests-picker"
        UserDefaults().removePersistentDomain(forName: suite)
        defer { UserDefaults().removePersistentDomain(forName: suite) }
        let service = AgentService(defaults: try #require(UserDefaults(suiteName: suite)))
        let cli = "/Apps/NeoSCAD/bin/neoscad"
        var model = AgentSetupModel(service: service, cli: cli)
        #expect(model.selected == .claudeCode)
        #expect(model.selectedRow?.action == .runClaude)
        #expect(model.usageOpen)
        #expect(
            model.rows.map { AgentSetupModel.shortLabel($0.client) }
                == ["Claude Code", "Claude Desktop", "Cursor", "VS Code", "Other"])

        model.selected = .cursor
        model.usageOpen = false
        model = AgentSetupModel(service: service, cli: cli)
        #expect(model.selected == .cursor)
        #expect(!model.usageOpen)

        // Each client's panel: its row, and the button it shows.
        var titles: [AgentSetupClient: String] = [:]
        for client in model.rows.map(\.client) {
            model.selected = client
            let row = try #require(model.selectedRow)
            #expect(row.client == client)
            titles[client] = AgentSetupRowView.actionTitle(row) ?? "(none)"
        }
        #expect(
            titles == [
                .claudeCode: "Add", .claudeDesktop: "Add", .cursor: "Open Cursor",
                .vsCode: "Open VS Code", .other: "(none)",
            ])
        if case .mergeConfig = model.rows[1].action {} else { Issue.record("\(model.rows[1].action)") }
        #expect(model.rows.last?.action == .copyOnly)
        // The last one picked (Other) is kept too.
        #expect(AgentSetupModel(service: service, cli: cli).selected == .other)

        // A name the app does not know falls back to Claude Code.
        service.defaults.set("emacs", forKey: AgentSetupModel.selectedKey)
        #expect(AgentSetupModel(service: service, cli: cli).selected == .claudeCode)
    }

    /// "Using NeoSCAD with your agent": the core's items, in the Mac's
    /// words, for the chosen client.
    @Test func usageSectionHasItsItems() throws {
        let suite = "agent-tests-usage"
        UserDefaults().removePersistentDomain(forName: suite)
        defer { UserDefaults().removePersistentDomain(forName: suite) }
        let service = AgentService(defaults: try #require(UserDefaults(suiteName: suite)))
        let model = AgentSetupModel(service: service, cli: "/Apps/NeoSCAD/bin/neoscad")
        let usage = model.usage
        #expect(
            usage.items.map(\.title) == [
                "Keep NeoSCAD open", "Things to ask", "Edits and saving", "Watching it work",
                "Staying in control", "Rendering and exporting",
            ])
        #expect(usage.items.map(\.topic) == [.keepOpen, .whatToAsk, .edits, .seeing, .control, .export])
        let edits = try #require(usage.items.first { $0.topic == .edits }).body
        #expect(edits.contains("⌘Z") && edits.contains("⌘S"))
        #expect(!usage.examples.isEmpty)
        #expect(usage.examples.contains { $0.contains("export") })

        // Claude Desktop cannot write files, so it is told to export from
        // the app, and is not offered an export request.
        model.selected = .claudeDesktop
        let desktop = model.usage
        #expect(try #require(desktop.items.last).body.contains("can’t write"))
        #expect(!desktop.examples.contains { $0.contains("export") })

        // The sheet lays out with the section open, in its width.
        let view = NSHostingView(rootView: AgentSetupContent(service: service, model: model))
        view.frame.size.width = AgentSetupView.size.width - 40
        view.layoutSubtreeIfNeeded()
        #expect(view.fittingSize.height > 600)
    }

    // MARK: The host

    @Test func hostReadsEditsAndUndoes() async throws {
        let doc = try await openDocument("r = 5;\ncube(r);\n")
        defer { finish(doc) }
        let host = DocumentsAgentHost(service: .shared)
        let id = doc.agentID

        let state = try await onLinkThread { try host.read(document: id) }
        #expect(state.text == "r = 5;\ncube(r);\n")
        #expect(state.version == doc.agentRevision)
        #expect(state.selection != nil)

        // `r = 5` -> `r = 7`: one undoable, highlighted step.
        let request = edit(state.version, (0, 4), (0, 5), "7")
        let outcome = try await onLinkThread { try host.edit(document: id, edit: request) }
        #expect(outcome == .applied(version: doc.agentRevision))
        #expect(doc.model.text == "r = 7;\ncube(r);\n")
        #expect(doc.isDocumentEdited)
        let editor = doc.model.editor
        #expect(editor.undoDepth == 1)
        #expect(try await editor.call("return NeoSCADEditor.agentMarks()") as? Int == 1)

        // The version it was read at is stale now.
        let late = try await onLinkThread { try host.edit(document: id, edit: request) }
        #expect(late == .stale(version: doc.agentRevision))
        #expect(doc.model.text == "r = 7;\ncube(r);\n")

        editor.undo()
        try await waitUntil("the undo arrives") { doc.model.text == "r = 5;\ncube(r);\n" }
        let reread = try await onLinkThread { try host.read(document: id) }
        #expect(reread.version > state.version)

        // The editor refuses an edit checked against a version it has
        // moved past (a keystroke whose change the app has not heard of
        // yet), rather than apply it in the wrong place.
        let behind = try #require(editor.version) - 1
        let refused =
            try await editor.call(
                "return NeoSCADEditor.agentEdit([{from: [0, 0], to: [0, 1], insert: 'q'}], 8000, v)",
                ["v": behind]) as? [String: Any]
        #expect(refused?["stale"] as? Bool == true)
        #expect(doc.model.text == "r = 5;\ncube(r);\n")
    }

    @Test func askBeforeApplyingWaitsForTheUser() async throws {
        let doc = try await openDocument("cube(1);\n")
        defer { finish(doc) }
        let service = AgentService.shared
        service.askBeforeApplying = true
        defer { service.askBeforeApplying = false }
        let host = DocumentsAgentHost(service: service)
        let id = doc.agentID

        for apply in [false, true] {
            let request = edit(doc.agentRevision, (0, 5), (0, 6), "2", client: "Test Agent")
            let pending = Task.detached { try host.edit(document: id, edit: request) }
            try await waitUntil("the bar asks") { doc.model.agentApproval != nil }
            #expect(doc.model.agentApproval?.client == "Test Agent")
            doc.model.agentApproval?.resolve(apply)
            let outcome = try await pending.value
            #expect(doc.model.agentApproval == nil)
            if apply {
                #expect(outcome == .applied(version: doc.agentRevision))
                #expect(doc.model.text == "cube(2);\n")
            } else {
                #expect(outcome == .declined)
                #expect(doc.model.text == "cube(1);\n")
            }
        }
    }

    @Test func hostShowsCapturesAndMarksTheView() async throws {
        let doc = try await openDocument("cube(10);\n")
        defer { finish(doc) }
        let host = DocumentsAgentHost(service: .shared)
        let id = doc.agentID

        try await onLinkThread {
            try host.reveal(
                document: id, from: EditorPosition(line: 0, character: 0),
                to: EditorPosition(line: 0, character: 4))
        }
        var selection: [Int] = []
        for _ in 0..<100 where selection != [0, 4] {
            let state = try await doc.model.editor.call("return NeoSCADEditor.state()") as? [String: Any]
            selection = state?["selection"] as? [Int] ?? []
            try await Task.sleep(for: .milliseconds(20))
        }
        #expect(selection == [0, 4])

        let camera = try await onLinkThread {
            try host.camera(
                document: id,
                change: AgentCameraChange(view: "top", fit: false, vpt: [1, 2, 3], vpr: nil, vpd: 80))
        }
        #expect(camera.vpt == [1, 2, 3] && camera.vpd == 80)

        let capture = try await onLinkThread { try host.capture(document: id, maxSide: 128) }
        #expect(capture.png.starts(with: [0x89, 0x50, 0x4E, 0x47]))
        #expect(max(capture.width, capture.height) <= 128)
        // The preview the open scheduled ran before the picture was taken.
        guard case .rendered = doc.model.report else {
            Issue.record("\(doc.model.report)")
            return
        }

        let marker = ViewMarker(point: [5, 5, 10], label: "here", color: [1, 0, 0, 1])
        try await onLinkThread { try host.annotate(document: id, lines: [], markers: [marker]) }
        #expect(doc.model.agentMarkCount == 1)
        doc.model.actions.clearAgentMarks()
        #expect(doc.model.agentMarkCount == 0)

        // A closed document is refused with a sentence the agent reads.
        doc.close()
        await #expect(throws: AgentHostError.self) {
            try await onLinkThread { try host.read(document: id) }
        }
    }

    // MARK: End to end

    /// The bundled `neoscad mcp`, run as Claude Code would, finds the app's
    /// link (in a test directory), reads the open document, edits it as one
    /// undoable step, and captures the view.
    @Test func neoscadMcpEditsTheOpenDocument() async throws {
        let rendezvous = try scratch("link")
        defer { try? FileManager.default.removeItem(at: rendezvous) }
        let service = AgentService.shared
        service.makeLink = { AgentLink.withDir(host: $0, appVersion: "test", dir: rendezvous.path) }
        service.setAllowed(true)
        defer { service.setAllowed(false) }
        #expect(sockets(in: rendezvous).count == 1)

        let doc = try await openDocument("cube(10);\n")
        defer { finish(doc) }
        service.documentFocused(doc)

        let tool = try #require(CommandLineTool.bundledTool())
        let mcp = try MCPClient(
            tool: tool, rendezvous: rendezvous, cwd: try #require(doc.fileURL).deletingLastPathComponent())
        defer { mcp.stop() }

        let initialized = try await mcp.call(
            "initialize",
            [
                "protocolVersion": "2025-11-25", "capabilities": [String: Any](),
                "clientInfo": ["name": "AppTest", "version": "1"],
            ])
        let instructions = ((initialized["result"] as? [String: Any])?["instructions"] as? String) ?? ""
        #expect(instructions.contains("NeoSCAD app open"), "\(instructions)")
        try await mcp.notify("notifications/initialized")
        try await waitUntil("the app sees the agent") {
            service.clients.contains { $0.name == "AppTest" }
        }
        #expect(service.statusLine == "AppTest connected")

        let read = try await mcp.tool("editor_read", [:])
        let text = MCPClient.text(read)
        #expect(text.contains("m.scad in NeoSCAD"), "\(text)")
        let version = try #require(
            text.firstMatch(of: /version (\d+)/).flatMap { Int($0.1) }, "\(text)")
        #expect(UInt64(version) == doc.agentRevision)

        let edited = try await mcp.tool(
            "editor_edit",
            ["version": version, "edits": [["old": "cube(10);", "new": "cube(20);"]]])
        #expect(MCPClient.text(edited).contains("applied 1 edit"), "\(edited)")
        #expect(doc.model.text == "cube(20);\n")
        let editor = doc.model.editor
        #expect(editor.undoDepth == 1)

        let captured = try await mcp.tool("view_capture", ["size": 256])
        let content = captured["content"] as? [[String: Any]] ?? []
        #expect(content.contains { $0["type"] as? String == "image" }, "\(captured)")

        editor.undo()
        try await waitUntil("the undo arrives") { doc.model.text == "cube(10);\n" }
    }
}

/// A minimal MCP client over the bundled CLI's stdio, one JSON message per
/// line. Its blocking reads run off the main actor, which must stay free
/// to answer the app's side of each request. A watchdog ends the process
/// after a minute, so a hang fails the test instead of stalling it.
private final class MCPClient: @unchecked Sendable {
    private let process = Process()
    private let input = Pipe()
    private let output = Pipe()
    private var buffer = Data()
    private var nextID = 1

    init(tool: URL, rendezvous: URL, cwd: URL) throws {
        process.executableURL = tool
        process.arguments = ["mcp"]
        var env = ProcessInfo.processInfo.environment
        env["NEOSCAD_AGENT_DIR"] = rendezvous.path
        env["OPENSCADPATH"] = nil
        process.environment = env
        process.currentDirectoryURL = cwd
        process.standardInput = input
        process.standardOutput = output
        process.standardError = FileHandle.nullDevice
        try process.run()
        let p = process
        DispatchQueue.global().asyncAfter(deadline: .now() + 60) {
            if p.isRunning { p.terminate() }
        }
    }

    func stop() {
        try? input.fileHandleForWriting.close()
        if process.isRunning { process.terminate() }
        process.waitUntilExit()
    }

    private func send(_ message: [String: Any]) throws {
        var data = try JSONSerialization.data(withJSONObject: message)
        data.append(0x0A)
        try input.fileHandleForWriting.write(contentsOf: data)
    }

    private func readMessage() -> [String: Any]? {
        while true {
            if let nl = buffer.firstIndex(of: 0x0A) {
                let line = buffer[buffer.startIndex..<nl]
                buffer = Data(buffer[(nl + 1)...])
                if let m = try? JSONSerialization.jsonObject(with: line) as? [String: Any] { return m }
                continue
            }
            let more = output.fileHandleForReading.availableData
            if more.isEmpty { return nil }
            buffer.append(more)
        }
    }

    private func callBlocking(_ method: String, _ params: [String: Any]) throws -> [String: Any] {
        let id = nextID
        nextID += 1
        try send(["jsonrpc": "2.0", "id": id, "method": method, "params": params])
        while let m = readMessage() {
            if m["id"] as? Int == id { return m }
        }
        throw CocoaError(.fileReadUnknown)
    }

    func call(_ method: String, _ params: [String: Any]) async throws -> [String: Any] {
        let p = JSONObject(params)
        return try await Task.detached { JSONObject(try self.callBlocking(method, p.value)) }.value.value
    }

    func notify(_ method: String) async throws {
        try send(["jsonrpc": "2.0", "method": method])
    }

    /// A tool's result.
    func tool(_ name: String, _ arguments: [String: Any]) async throws -> [String: Any] {
        let r = try await call("tools/call", ["name": name, "arguments": arguments])
        return r["result"] as? [String: Any] ?? r
    }

    static func text(_ result: [String: Any]) -> String {
        (result["content"] as? [[String: Any]] ?? []).compactMap { $0["text"] as? String }
            .joined(separator: "\n")
    }
}

/// A JSON object handed across a task boundary; neither side changes it.
private struct JSONObject: @unchecked Sendable {
    let value: [String: Any]
    init(_ value: [String: Any]) { self.value = value }
}
