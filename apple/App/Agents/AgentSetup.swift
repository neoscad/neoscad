// "Connect your AI agent": the sheet that turns agents on and sets up each
// agent client in one click (docs/mcp.md, "Setup from the apps";
// docs/audits/agent-connection-desktop.md, Option A and "UI spec").
//
// The rows are the core's (`agentSetupRows`), with the stable link to the
// bundled CLI (CommandLineTool.swift) as the server's command, so every
// surface shows the same commands and nothing here repeats them. Each row
// uses the client's own way in, so the client asks the user and owns its
// format:
//
// - Claude Code: find `claude` (its install places, then the login
//   shell's PATH) and run `claude mcp add --scope user …`. Already there:
//   say so and offer Replace. Not found: the command to copy.
// - Cursor, VS Code: open the client's install link; it asks the user.
// - Claude Desktop: after the user agrees, merge the server into its
//   config, keeping a backup beside it; then Claude must be reopened.
// - Other clients: the JSON to copy.
//
// Every row also shows the command or JSON to do it by hand. `claude` and
// the config edit block (up to 60 s for `claude`), so they run off the
// main thread; the sheet stays live meanwhile.
//
// Consent. The first switch is the user's consent for agents to work on
// open documents (off until turned on, the owner's decision 2). Adding a
// client while it is off asks once, with the audit's wording, since an
// agent set up but not allowed can only work on saved files.

import AppKit
import NeoSCADCore
import SwiftUI

/// What a row last did, or found.
enum AgentSetupState: Equatable {
    /// Not looked at yet, or nothing known (a link was not tried).
    case idle
    case working
    case done(String)
    case alreadySetUp(String)
    case notInstalled(String)
    case failed(String)
}

/// A question the sheet asks before acting.
enum AgentSetupQuestion: Equatable {
    /// Agents are off: allow them? (audit, "Consent, at first enable")
    case allowAgents
    /// Write Claude Desktop's config at this path?
    case editClaudeDesktop(path: String)
    /// Claude Code already has a `neoscad`: replace it?
    case replaceClaudeCode
}

@MainActor
@Observable
final class AgentSetupModel {
    /// The command clients run: the stable link's path.
    let cli: String
    let rows: [AgentSetupRow]
    private(set) var states: [AgentSetupClient: AgentSetupState] = [:]
    /// Whether the command-line tool the configs name exists.
    let cliPresent: Bool

    // What the machine is asked, replaceable in the tests.
    @ObservationIgnored var findClaude: @Sendable () -> String? = { agentSetupFindClaude() }
    @ObservationIgnored var addToClaudeCode: @Sendable (String, String, Bool) throws -> ClaudeCodeOutcome = {
        try agentSetupAddToClaudeCode(claude: $0, cli: $1, replace: $2)
    }
    @ObservationIgnored var addToClaudeDesktop: @Sendable (String) throws -> ClaudeDesktopOutcome = {
        try agentSetupAddToClaudeDesktop(cli: $0)
    }
    @ObservationIgnored var openURL: (URL) -> Bool = { NSWorkspace.shared.open($0) }
    @ObservationIgnored var hasHandler: (URL) -> Bool = {
        NSWorkspace.shared.urlForApplication(toOpen: $0) != nil
    }
    /// Ask the user; the sheet's window shows an alert.
    @ObservationIgnored var ask: (AgentSetupQuestion) async -> Bool = { _ in false }
    @ObservationIgnored let service: AgentService
    /// Asked once per sheet whether to allow agents.
    @ObservationIgnored private var askedToAllow = false

    init(
        service: AgentService = .shared,
        cli: String = CommandLineTool.linkPath().path
    ) {
        self.service = service
        self.cli = cli
        rows = agentSetupRows(cli: cli)
        cliPresent = FileManager.default.isExecutableFile(atPath: cli)
    }

    func state(_ client: AgentSetupClient) -> AgentSetupState {
        states[client] ?? .idle
    }

    /// What can be known without acting: whether each client is
    /// installed, and whether Claude Desktop already runs this command.
    func look() {
        for row in rows {
            switch row.action {
            case .openUrl(let url):
                if let u = URL(string: url), !hasHandler(u) {
                    states[row.client] = .notInstalled("\(row.label) isn't installed. Add the JSON below by hand.")
                }
            case .mergeConfig(let path):
                let file = URL(fileURLWithPath: path)
                if !FileManager.default.fileExists(atPath: file.deletingLastPathComponent().path) {
                    states[row.client] = .notInstalled("Claude Desktop isn't installed for this user.")
                } else if Self.config(at: file, runs: cli) {
                    states[row.client] = .alreadySetUp("Claude Desktop runs NeoSCAD.")
                }
            case .runClaude:
                let find = findClaude
                Task {
                    let found = await Task.detached { find() }.value
                    if found == nil, self.state(.claudeCode) == .idle {
                        self.states[.claudeCode] = .notInstalled(
                            "Claude Code wasn't found. Run this in a terminal:")
                    }
                }
            case .copyOnly:
                break
            }
        }
    }

    /// Whether Claude Desktop's config at `file` already has a `neoscad`
    /// server running `cli`.
    nonisolated static func config(at file: URL, runs cli: String) -> Bool {
        guard let data = try? Data(contentsOf: file),
            let json = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
            let servers = json["mcpServers"] as? [String: Any],
            let neoscad = servers["neoscad"] as? [String: Any]
        else { return false }
        return neoscad["command"] as? String == cli
    }

    /// The row's button.
    func add(_ row: AgentSetupRow) async {
        if !service.allowed, !askedToAllow, row.action != .copyOnly {
            askedToAllow = true
            if await ask(.allowAgents) { service.setAllowed(true) }
        }
        switch row.action {
        case .runClaude:
            await addToClaudeCode(row, replace: false)
        case .openUrl(let url):
            guard let u = URL(string: url), openURL(u) else {
                states[row.client] = .notInstalled("\(row.label) isn't installed. Add the JSON below by hand.")
                return
            }
            states[row.client] = .done("Opened in \(row.label). Confirm there to finish.")
        case .mergeConfig(let path):
            guard await ask(.editClaudeDesktop(path: path)) else { return }
            states[row.client] = .working
            let cli = self.cli
            let add = addToClaudeDesktop
            let outcome = await Task.detached { Result { try add(cli) } }.value
            switch outcome {
            case .success(.written):
                states[row.client] = .done("Added. Quit and reopen Claude to finish.")
            case .success(.unchanged):
                states[row.client] = .alreadySetUp("Claude Desktop runs NeoSCAD.")
            case .success(.notInstalled):
                states[row.client] = .notInstalled("Claude Desktop isn't installed for this user.")
            case .success(.refused(_, let reason)):
                states[row.client] = .failed("Its config was left as it was: \(reason). Add the JSON below by hand.")
            case .failure(let e):
                states[row.client] = .failed((e as? CoreError)?.message ?? e.localizedDescription)
            }
        case .copyOnly:
            copy(row)
        }
    }

    /// Claude Code: find `claude`, then `claude mcp add`.
    func addToClaudeCode(_ row: AgentSetupRow, replace: Bool) async {
        states[row.client] = .working
        let find = findClaude
        let add = addToClaudeCode
        let cli = self.cli
        guard let claude = await Task.detached(operation: { find() }).value else {
            states[row.client] = .notInstalled("Claude Code wasn't found. Run this in a terminal:")
            return
        }
        let outcome = await Task.detached { Result { try add(claude, cli, replace) } }.value
        switch outcome {
        case .success(.added):
            states[row.client] = .done("Added for all your projects. New Claude Code sessions can use it.")
        case .success(.alreadyExists):
            states[row.client] = .alreadySetUp("Claude Code already has a NeoSCAD server.")
        case .success(.failed(let output)):
            states[row.client] = .failed(Self.trimmed(output))
        case .failure(let e):
            states[row.client] = .failed((e as? CoreError)?.message ?? e.localizedDescription)
        }
    }

    /// "Replace" on Claude Code's "already set up".
    func replaceClaudeCode(_ row: AgentSetupRow) async {
        guard await ask(.replaceClaudeCode) else { return }
        await addToClaudeCode(row, replace: true)
    }

    func copy(_ row: AgentSetupRow) {
        NSPasteboard.general.clearContents()
        NSPasteboard.general.setString(row.copyText, forType: .string)
    }

    private static func trimmed(_ s: String) -> String {
        let t = s.trimmingCharacters(in: .whitespacesAndNewlines)
        return t.isEmpty ? "Claude Code did not add it." : String(t.prefix(400))
    }
}

// MARK: The sheet

struct AgentSetupView: View {
    @Bindable var service: AgentService
    let model: AgentSetupModel
    var done: () -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            ScrollView {
                VStack(alignment: .leading, spacing: 18) {
                    header
                    consent
                    clients
                    cliNote
                }
                .padding(20)
            }
            Divider()
            HStack {
                Link("Using NeoSCAD with AI Agents", destination: AgentHelp.url)
                Spacer()
                Button("Done", action: done)
                    .keyboardShortcut(.defaultAction)
            }
            .padding(.horizontal, 20)
            .padding(.vertical, 12)
        }
        .frame(width: 560, height: 640)
        .onAppear { model.look() }
    }

    private var header: some View {
        HStack(alignment: .top, spacing: 12) {
            Image(systemName: "sparkles")
                .font(.system(size: 28))
                .foregroundStyle(.tint)
            VStack(alignment: .leading, spacing: 4) {
                Text("Connect your AI agent").font(.title2).bold()
                Text(
                    "Let an AI agent like Claude Code read and edit the models open in NeoSCAD, "
                        + "look at the 3D view and point things out in it. You see every edit, and Undo takes it back."
                )
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
            }
        }
    }

    private var consent: some View {
        GroupBox {
            VStack(alignment: .leading, spacing: 10) {
                Toggle(
                    "Allow AI agents to work on open documents",
                    isOn: Binding(get: { service.allowed }, set: { service.setAllowed($0) })
                )
                .toggleStyle(.switch)
                .accessibilityIdentifier("agent-allow")
                Text(AgentHelp.consentText)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
                if service.allowed {
                    Divider()
                    AgentStatusList(service: service)
                }
            }
            .padding(6)
        }
    }

    private var clients: some View {
        VStack(alignment: .leading, spacing: 10) {
            Text("Add NeoSCAD to your agent").font(.headline)
            ForEach(model.rows, id: \.client) { row in
                AgentSetupRowView(row: row, model: model)
                if row.client != model.rows.last?.client { Divider() }
            }
        }
    }

    @ViewBuilder private var cliNote: some View {
        if !model.cliPresent {
            Label(
                "NeoSCAD's command-line tool isn't at \(model.cli) yet. Open NeoSCAD from the Applications folder once to put it there.",
                systemImage: "exclamationmark.triangle.fill"
            )
            .font(.caption)
            .foregroundStyle(.orange)
            .fixedSize(horizontal: false, vertical: true)
        }
    }
}

struct AgentSetupRowView: View {
    let row: AgentSetupRow
    let model: AgentSetupModel
    @State private var showText = false
    @State private var copied = false

    var body: some View {
        let state = model.state(row.client)
        VStack(alignment: .leading, spacing: 6) {
            HStack(alignment: .firstTextBaseline) {
                Text(row.label).bold()
                badge(state)
                Spacer()
                if state == .working {
                    ProgressView().controlSize(.small)
                }
                if case .alreadySetUp = state, row.action == .runClaude {
                    Button("Replace") { Task { await model.replaceClaudeCode(row) } }
                }
                if row.action != .copyOnly {
                    Button(addTitle) { Task { await model.add(row) } }
                        .disabled(state == .working)
                        .accessibilityIdentifier("agent-add-\(row.label)")
                }
                Button(copied ? "Copied" : "Copy") {
                    model.copy(row)
                    copied = true
                    Task {
                        try? await Task.sleep(for: .seconds(1.5))
                        copied = false
                    }
                }
            }
            Text(message(state))
                .font(.caption)
                .foregroundStyle(color(state))
                .fixedSize(horizontal: false, vertical: true)
                .textSelection(.enabled)
            if row.action == .copyOnly || showText || isFallback(state) {
                Text(row.copyText)
                    .font(.system(.caption, design: .monospaced))
                    .textSelection(.enabled)
                    .padding(8)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .background(.quaternary.opacity(0.5), in: RoundedRectangle(cornerRadius: 6))
            } else {
                Button(row.action == .runClaude ? "Show the command" : "Show the JSON") {
                    showText = true
                }
                .buttonStyle(.link)
                .font(.caption)
            }
        }
    }

    private var addTitle: String {
        switch row.action {
        case .openUrl: "Open \(row.label)"
        default: "Add"
        }
    }

    @ViewBuilder private func badge(_ s: AgentSetupState) -> some View {
        switch s {
        case .done:
            Label("Added", systemImage: "checkmark.circle.fill").foregroundStyle(.green)
                .labelStyle(.titleAndIcon).font(.caption)
        case .alreadySetUp:
            Label("Already set up", systemImage: "checkmark.circle").foregroundStyle(.green)
                .font(.caption)
        case .notInstalled:
            Text("Not installed").foregroundStyle(.secondary).font(.caption)
        case .failed:
            Label("Error", systemImage: "exclamationmark.triangle.fill").foregroundStyle(.red)
                .font(.caption)
        case .idle, .working:
            EmptyView()
        }
    }

    private func message(_ s: AgentSetupState) -> String {
        switch s {
        case .done(let m), .alreadySetUp(let m), .notInstalled(let m), .failed(let m): m
        case .working: "Working…"
        case .idle: row.note
        }
    }

    private func color(_ s: AgentSetupState) -> Color {
        if case .failed = s { return .red }
        return .secondary
    }

    /// The by-hand text shows itself when the click could not do it.
    private func isFallback(_ s: AgentSetupState) -> Bool {
        switch s {
        case .notInstalled, .failed: true
        default: false
        }
    }
}

/// The help page and the consent wording.
enum AgentHelp {
    static let url = URL(string: "https://neoscad.org/agents.html")!

    static let consentTitle = "Let AI agents work on your open models?"
    static let consentText =
        "Agents on this computer that use NeoSCAD (Claude Code, Cursor, VS Code and others) "
        + "will be able to read and edit the models open in NeoSCAD and see the 3D view. "
        + "What they read goes to the agent's AI service."
}

/// Opens the sheet on the front document window, or the same content as a
/// window of its own when no document window is in front.
@MainActor
enum AgentSetupPresenter {
    /// The window of its own, while it is open.
    private static var standalone: NSWindow?

    static func show(service: AgentService = .shared) {
        let model = AgentSetupModel(service: service)
        let front = NSApp.keyWindow ?? NSApp.mainWindow
        if let front, front.windowController?.document is SCADDocument, front.attachedSheet == nil {
            let sheet = NSWindow(
                contentRect: NSRect(x: 0, y: 0, width: 560, height: 640),
                styleMask: [.titled], backing: .buffered, defer: false)
            sheet.contentViewController = NSHostingController(
                rootView: AgentSetupView(service: service, model: model) { [weak front, weak sheet] in
                    if let sheet { front?.endSheet(sheet) }
                })
            model.ask = { [weak sheet] q in await ask(q, on: sheet) }
            front.beginSheet(sheet)
            return
        }
        if let standalone {
            standalone.makeKeyAndOrderFront(nil)
            return
        }
        let window = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 560, height: 640),
            styleMask: [.titled, .closable], backing: .buffered, defer: false)
        window.title = "Connect your AI agent"
        window.isReleasedWhenClosed = false
        window.contentViewController = NSHostingController(
            rootView: AgentSetupView(service: service, model: model) { [weak window] in
                window?.close()
                standalone = nil
            })
        model.ask = { [weak window] q in await ask(q, on: window) }
        window.center()
        standalone = window
        window.makeKeyAndOrderFront(nil)
    }

    static func ask(_ q: AgentSetupQuestion, on window: NSWindow?) async -> Bool {
        let alert = NSAlert()
        switch q {
        case .allowAgents:
            alert.messageText = AgentHelp.consentTitle
            alert.informativeText = AgentHelp.consentText
            alert.addButton(withTitle: "Allow")
            alert.addButton(withTitle: "Not Now")
        case .editClaudeDesktop(let path):
            alert.messageText = "Add NeoSCAD to Claude Desktop?"
            alert.informativeText =
                "NeoSCAD will add a “neoscad” server to \(path). Everything else in the file stays, and a copy of it is kept beside it."
            alert.addButton(withTitle: "Add")
            alert.addButton(withTitle: "Cancel")
        case .replaceClaudeCode:
            alert.messageText = "Replace Claude Code's NeoSCAD server?"
            alert.informativeText =
                "Claude Code already has a server named “neoscad”, perhaps another copy of NeoSCAD. Replace it with this app's?"
            alert.addButton(withTitle: "Replace")
            alert.addButton(withTitle: "Cancel")
        }
        guard let window else { return alert.runModal() == .alertFirstButtonReturn }
        return await alert.beginSheetModal(for: window) == .alertFirstButtonReturn
    }
}
