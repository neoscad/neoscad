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
//   config, keeping a backup beside it; then Claude must be reopened. Its
//   server gets ~/Documents/NeoSCAD as its `--root` (made if missing), the
//   folder its agent exports to: Claude Desktop starts servers in `/`,
//   which is never a root. An entry from before that (no `--root`) shows
//   as needing an update, and Update rewrites it, asking first.
// - Other clients: the JSON to copy.
//
// Every row also shows the command or JSON to do it by hand. `claude` and
// the config edit block (up to 60 s for `claude`), so they run off the
// main thread; the sheet stays live meanwhile.
//
// Layout. The clients are a segmented control (Claude Code first, and
// chosen until the user picks another; the choice is kept), and only the
// chosen client's row shows below it: five rows at once made the sheet a
// wall of buttons and JSON, most of it for clients the user does not have.
// Under the setup, "Using NeoSCAD with your agent" answers what a user
// asks right after connecting (keep the app open? does it save? where do
// I see it?), for the chosen client. Its text is the core's
// (`agentSetupUsage`, crates/client/src/agent_setup/usage.rs), shared
// with the Windows and Linux apps; the section folds away, open until the
// user closes it.
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
    /// Set up by an earlier version, in a way that no longer works fully
    /// (Claude Desktop without its folder): the button updates it.
    case outdated(String)
    case notInstalled(String)
    case failed(String)
}

extension AgentSetupState {
    var isOutdated: Bool {
        if case .outdated = self { return true }
        return false
    }
}

/// A question the sheet asks before acting.
enum AgentSetupQuestion: Equatable {
    /// Agents are off: allow them? (audit, "Consent, at first enable")
    case allowAgents
    /// Write Claude Desktop's config at `path`, giving its agent `folder`?
    /// `update` when it replaces an earlier setup's entry.
    case editClaudeDesktop(path: String, folder: String, update: Bool)
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

    /// The defaults' keys: the client last chosen, and whether the usage
    /// section is open. Kept in the service's defaults, which the tests
    /// replace with a suite of their own.
    static let selectedKey = "AgentSetupClient"
    static let usageOpenKey = "AgentUsageExpanded"

    /// The client whose setup shows: Claude Code until the user picks
    /// another, then the last one picked.
    var selected: AgentSetupClient {
        didSet { service.defaults.set(Self.name(selected), forKey: Self.selectedKey) }
    }
    /// "Using NeoSCAD with your agent" is open: at first, and until the
    /// user folds it.
    var usageOpen: Bool {
        didSet { service.defaults.set(usageOpen, forKey: Self.usageOpenKey) }
    }

    // What the machine is asked, replaceable in the tests.
    @ObservationIgnored var findClaude: @Sendable () -> String? = { agentSetupFindClaude() }
    @ObservationIgnored var addToClaudeCode: @Sendable (String, String, Bool) throws -> ClaudeCodeOutcome = {
        try agentSetupAddToClaudeCode(claude: $0, cli: $1, replace: $2)
    }
    @ObservationIgnored var addToClaudeDesktop: @Sendable (String) throws -> ClaudeDesktopOutcome = {
        try agentSetupAddToClaudeDesktop(cli: $0)
    }
    @ObservationIgnored var claudeDesktopStatus: (String) throws -> ClaudeDesktopStatus = {
        try agentSetupClaudeDesktopStatus(cli: $0)
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
        let rows = agentSetupRows(cli: cli)
        self.rows = rows
        cliPresent = FileManager.default.isExecutableFile(atPath: cli)
        let kept = (service.defaults.string(forKey: Self.selectedKey)).flatMap(Self.client(named:))
        // A kept client this host does not list (none today) falls back.
        selected =
            [kept, .claudeCode].compactMap { $0 }.first { c in rows.contains { $0.client == c } }
            ?? rows.first?.client ?? .claudeCode
        usageOpen = service.defaults.object(forKey: Self.usageOpenKey) as? Bool ?? true
    }

    func state(_ client: AgentSetupClient) -> AgentSetupState {
        states[client] ?? .idle
    }

    /// The chosen client's row.
    var selectedRow: AgentSetupRow? {
        rows.first { $0.client == selected }
    }

    /// "Using NeoSCAD with your agent" for the chosen client.
    var usage: AgentUsage {
        agentSetupUsage(host: agentSetupHost(), client: selected)
    }

    /// The segment's title: "Other" for "Other MCP clients".
    static func shortLabel(_ client: AgentSetupClient) -> String {
        agentSetupShortLabel(client: client)
    }

    /// The kept names, as the core spells the clients (`kebab-case`).
    static func name(_ c: AgentSetupClient) -> String {
        switch c {
        case .claudeCode: "claude-code"
        case .claudeDesktop: "claude-desktop"
        case .cursor: "cursor"
        case .vsCode: "vs-code"
        case .other: "other"
        }
    }

    static func client(named name: String) -> AgentSetupClient? {
        [.claudeCode, .claudeDesktop, .cursor, .vsCode, .other].first { Self.name($0) == name }
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
            case .mergeConfig:
                // One small file read, as the sheet opens.
                switch try? claudeDesktopStatus(cli) {
                case .notInstalled:
                    states[row.client] = .notInstalled("Claude Desktop isn't installed for this user.")
                case .upToDate:
                    states[row.client] = .alreadySetUp(
                        "Claude Desktop runs NeoSCAD. Its agent saves files in \(desktopFolderShown).")
                case .outdated:
                    states[row.client] = .outdated(
                        "Claude Desktop runs NeoSCAD from an earlier setup, so its agent can't save files. "
                            + "Update gives it \(desktopFolderShown).")
                case .notAdded, .other, .unreadable, nil:
                    break
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

    /// Claude Desktop's folder (`~/Documents/NeoSCAD`), as the sheet
    /// shows it.
    var desktopFolderShown: String {
        guard let folder = agentSetupClaudeDesktopFolder() else { return "~/Documents/NeoSCAD" }
        // The home the core used (`HOME`, which the tests point elsewhere),
        // not NSHomeDirectory's.
        if let home = ProcessInfo.processInfo.environment["HOME"], !home.isEmpty,
            folder.hasPrefix(home + "/")
        {
            return "~" + folder.dropFirst(home.count)
        }
        return folder
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
            let update = state(row.client).isOutdated
            guard await ask(.editClaudeDesktop(path: path, folder: desktopFolderShown, update: update))
            else { return }
            states[row.client] = .working
            let cli = self.cli
            let add = addToClaudeDesktop
            let outcome = await Task.detached { Result { try add(cli) } }.value
            switch outcome {
            case .success(.written(_, _, let replaced)):
                states[row.client] = .done(
                    "\(replaced ? "Updated" : "Added"). Quit and reopen Claude to finish. "
                        + "Its agent saves files in \(desktopFolderShown).")
            case .success(.unchanged):
                states[row.client] = .alreadySetUp(
                    "Claude Desktop runs NeoSCAD. Its agent saves files in \(desktopFolderShown).")
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
    /// The sheet's size, for the windows that hold it.
    static let size = NSSize(width: 580, height: 700)

    @Bindable var service: AgentService
    let model: AgentSetupModel
    var done: () -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            ScrollView {
                AgentSetupContent(service: service, model: model)
                    .padding(20)
            }
            Divider()
            HStack {
                Spacer()
                Button("Done", action: done)
                    .keyboardShortcut(.defaultAction)
            }
            .padding(.horizontal, 20)
            .padding(.vertical, 12)
        }
        .frame(width: Self.size.width, height: Self.size.height)
        .onAppear { model.look() }
    }
}

/// Everything the sheet scrolls: the consent and status, the client
/// picker and the chosen client's setup, then how to use it.
struct AgentSetupContent: View {
    @Bindable var service: AgentService
    @Bindable var model: AgentSetupModel

    var body: some View {
        VStack(alignment: .leading, spacing: 18) {
            header
            consent
            clients
            cliNote
            Divider()
            DisclosureGroup(isExpanded: $model.usageOpen) {
                AgentUsageView(usage: model.usage)
                    .padding(.top, 10)
            } label: {
                Text("Using NeoSCAD with your agent").font(.headline)
            }
            .accessibilityIdentifier("agent-usage")
        }
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
            Picker("Agent", selection: $model.selected) {
                ForEach(model.rows, id: \.client) { row in
                    Text(AgentSetupModel.shortLabel(row.client)).tag(row.client)
                }
            }
            .pickerStyle(.segmented)
            .labelsHidden()
            .accessibilityIdentifier("agent-client-picker")
            if let row = model.selectedRow {
                GroupBox {
                    AgentSetupRowView(row: row, model: model)
                        .padding(6)
                }
                // A view of its own per client, so "Show the JSON" and
                // "Copied" do not carry over to the next one.
                .id(row.client)
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

/// "Using NeoSCAD with your agent": a few headed items, a sentence or two
/// each, and the example requests under "Things to ask".
struct AgentUsageView: View {
    let usage: AgentUsage

    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            ForEach(usage.items, id: \.topic) { item in
                HStack(alignment: .firstTextBaseline, spacing: 10) {
                    Image(systemName: Self.symbol(item.topic))
                        .foregroundStyle(.tint)
                        .frame(width: 18)
                        .accessibilityHidden(true)
                    VStack(alignment: .leading, spacing: 3) {
                        Text(item.title).bold()
                        Text(item.body)
                            .foregroundStyle(.secondary)
                            .fixedSize(horizontal: false, vertical: true)
                        if item.topic == .whatToAsk {
                            examples
                        }
                    }
                }
            }
            Link("Learn more", destination: AgentHelp.url)
                .padding(.leading, 28)
        }
        .font(.callout)
        .textSelection(.enabled)
    }

    private var examples: some View {
        VStack(alignment: .leading, spacing: 2) {
            ForEach(usage.examples, id: \.self) { e in
                Text("“\(e)”").italic()
                    .fixedSize(horizontal: false, vertical: true)
            }
        }
        .padding(.top, 2)
    }

    static func symbol(_ topic: AgentUsageTopic) -> String {
        switch topic {
        case .keepOpen: "macwindow"
        case .whatToAsk: "text.bubble"
        case .edits: "arrow.uturn.backward"
        case .seeing: "eye"
        case .control: "hand.raised"
        case .export: "square.and.arrow.up"
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
                if let title = Self.actionTitle(row, state: state) {
                    Button(title) { Task { await model.add(row) } }
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

    /// The one-click button's title; none for a client with only text to
    /// copy. "Update" for a setup an earlier version made.
    static func actionTitle(_ row: AgentSetupRow, state: AgentSetupState = .idle) -> String? {
        switch row.action {
        case .openUrl: "Open \(row.label)"
        case .mergeConfig: state.isOutdated ? "Update" : "Add"
        case .runClaude: "Add"
        case .copyOnly: nil
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
        case .outdated:
            Label("Needs an update", systemImage: "arrow.triangle.2.circlepath").foregroundStyle(.orange)
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
        case .done(let m), .alreadySetUp(let m), .outdated(let m), .notInstalled(let m), .failed(let m): m
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
                contentRect: NSRect(origin: .zero, size: AgentSetupView.size),
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
            contentRect: NSRect(origin: .zero, size: AgentSetupView.size),
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
        case .editClaudeDesktop(let path, let folder, let update):
            alert.messageText = update ? "Update NeoSCAD in Claude Desktop?" : "Add NeoSCAD to Claude Desktop?"
            alert.informativeText =
                (update
                    ? "NeoSCAD will update the “neoscad” server in \(path) "
                    : "NeoSCAD will add a “neoscad” server to \(path) ")
                + "and make \(folder), where Claude’s agent can save files. "
                + "Everything else in the file stays, and a copy of it is kept beside it."
            alert.addButton(withTitle: update ? "Update" : "Add")
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
