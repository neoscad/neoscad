// AI agents working on the open documents (docs/agent-bridge.md, "Desktop
// apps"; docs/audits/agent-connection-desktop.md, Option C): the app's one
// agent link, the user's consent, and the live status the toolbar control,
// the setup sheet and Settings show.
//
// Off until the user turns it on (the owner's decision 2). Until then there
// is no link object, no socket, no thread and no observer: the documents
// are only numbered and remembered, which costs a dictionary entry each.
// Turning agents on makes the link (`AgentLink`), registers the open
// documents, says the user agreed (`setAllowed`) and listens (`start`).
// Turning them off tells every agent why and releases the link, which
// closes its socket at once. The choice is kept in the app's defaults, so
// the link starts again at the next launch and agents reconnect by
// themselves.
//
// Threads. The link calls its observer on its own threads; the status hops
// to the main thread with `DispatchQueue.main.async`, never waiting, since
// the main thread may be inside a link call at that moment
// (crates/ffi/src/agent.rs, "Threads"). Statuses can arrive out of order,
// so only a higher `sequence` than the last one replaces it. Nothing polls.

import AppKit
import NeoSCADCore
import os

@MainActor
@Observable
final class AgentService {
    /// The app's service. Under XCTest it keeps its choices in a defaults
    /// suite of its own and starts nothing by itself, so the hosted tests
    /// neither read nor change the developer's own consent, and never
    /// open a socket a real `neoscad mcp` would find.
    static let shared: AgentService = {
        if NSClassFromString("XCTestCase") != nil {
            let suite = "org.neoscad.NeoSCAD.agent-tests"
            UserDefaults().removePersistentDomain(forName: suite)
            return AgentService(defaults: UserDefaults(suiteName: suite) ?? .standard)
        }
        return AgentService(defaults: .standard)
    }()

    /// The defaults' keys. `allowedKey` is absent until the user first
    /// chooses, which is how "never turned on" (the toolbar invites) is
    /// told from "turned off" (the toolbar control hides; the Help menu
    /// and Settings still reach the sheet).
    static let allowedKey = "AgentsAllowed"
    static let askFirstKey = "AgentsAskBeforeApplying"

    private static let log = Logger(subsystem: "org.neoscad.NeoSCAD", category: "agents")

    /// The documents' numbers, given in the order they were made and never
    /// reused while the app runs (the link's `documentOpened` id).
    private static var lastDocumentID: UInt64 = 0
    static func newDocumentID() -> UInt64 {
        lastDocumentID += 1
        return lastDocumentID
    }

    @ObservationIgnored let defaults: UserDefaults
    /// Makes the link for `host`: the user's own place normally, a test's
    /// directory in the tests (`AgentLink.withDir`).
    @ObservationIgnored var makeLink: (AgentHost) -> AgentLink = { host in
        AgentLink(host: host, appVersion: AgentService.appVersion)
    }

    /// Whether the user allows agents (the persisted consent).
    private(set) var allowed: Bool
    /// The user turned agents off after choosing: the toolbar control hides.
    private(set) var turnedOff: Bool
    /// Make each agent edit wait for the user's Apply (off by default, as
    /// on the web page).
    var askBeforeApplying: Bool {
        didSet { defaults.set(askBeforeApplying, forKey: Self.askFirstKey) }
    }
    /// The link's last status; nil while agents are off.
    private(set) var status: AgentStatus?
    /// What agents did lately, newest first, for the popover.
    private(set) var recent: [AgentActivity] = []

    @ObservationIgnored private(set) var link: AgentLink?
    @ObservationIgnored private var observer: StatusObserver?
    /// Which link the statuses arriving now belong to: a late status of a
    /// released link must not bring its clients back.
    @ObservationIgnored private var generation: UInt64 = 0
    @ObservationIgnored private var lastSequence: UInt64 = 0
    @ObservationIgnored private var documents: [UInt64: WeakDocument] = [:]
    @ObservationIgnored private var focused: UInt64?

    init(defaults: UserDefaults) {
        self.defaults = defaults
        let chosen = defaults.object(forKey: Self.allowedKey) as? Bool
        allowed = chosen ?? false
        turnedOff = chosen == false
        askBeforeApplying = defaults.bool(forKey: Self.askFirstKey)
    }

    static var appVersion: String {
        Bundle.main.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String ?? "0"
    }

    // MARK: Consent

    /// At launch: listen again if the user allowed agents before.
    func startIfAllowed() {
        if allowed { startLink() }
    }

    /// The user's switch. On: listen. Off: every agent is told and
    /// disconnected, the socket is gone, and edits waiting for approval
    /// are rejected.
    func setAllowed(_ on: Bool) {
        defaults.set(on, forKey: Self.allowedKey)
        allowed = on
        turnedOff = !on
        if on {
            startLink()
        } else {
            stopLink()
        }
    }

    private func startLink() {
        let link: AgentLink
        if let existing = self.link {
            link = existing
        } else {
            generation += 1
            lastSequence = 0
            link = makeLink(DocumentsAgentHost(service: self))
            let generation = self.generation
            let observer = StatusObserver { [weak self] status, sequence in
                DispatchQueue.main.async {
                    MainActor.assumeIsolated {
                        self?.received(status, sequence: sequence, generation: generation)
                    }
                }
            }
            self.observer = observer
            self.link = link
            try? link.setObserver(observer: observer)
            // The documents already open, in the order they were opened,
            // then the one in front.
            for id in documents.keys.sorted() {
                if let doc = documents[id]?.document { register(doc, with: link) }
            }
            if let focused { try? link.documentFocused(id: focused) }
        }
        do {
            try link.setAllowed(allowed: true)
            let address = try link.start()
            Self.log.info("listening for agents at \(address, privacy: .public)")
        } catch {
            Self.log.error("could not listen for agents: \(error.localizedDescription, privacy: .public)")
            // The status carries the error for the sheet.
            status = try? link.status()
        }
    }

    private func stopLink() {
        if let link {
            // Tells each agent why, and closes the socket.
            try? link.setAllowed(allowed: false)
            try? link.setObserver(observer: nil)
        }
        link = nil
        observer = nil
        generation += 1
        status = nil
        recent = []
        for doc in documents.values.compactMap(\.document) {
            doc.model.agentApproval?.resolve(false)
        }
    }

    /// End one agent's connection.
    func disconnect(_ client: AgentClient) {
        try? link?.disconnect(client: client.id)
    }

    // MARK: Status

    /// The control's text (`agentStatusLine`): "Connect your AI agent",
    /// "Claude Code connected", "Claude Code is editing".
    var statusLine: String {
        agentStatusLine(status: status ?? Self.idleStatus)
    }

    private static let idleStatus = AgentStatus(
        allowed: false, listening: false, address: nil, clients: [], error: nil)

    var clients: [AgentClient] { status?.clients ?? [] }
    var isConnected: Bool { !clients.isEmpty }
    var isWorking: Bool { clients.contains { $0.activity != nil } }

    /// The toolbar's short label: the agent's name, or how many.
    var shortLabel: String {
        switch clients.count {
        case 0: statusLine
        case 1: clients[0].name ?? "Agent"
        default: "\(clients.count) agents"
        }
    }

    private func received(_ new: AgentStatus, sequence: UInt64, generation: UInt64) {
        guard generation == self.generation, link != nil, sequence > lastSequence else { return }
        lastSequence = sequence
        let before = Dictionary(
            (status?.clients ?? []).map { ($0.id, $0.activity) }, uniquingKeysWith: { a, _ in a })
        for c in new.clients {
            guard let activity = c.activity, before[c.id] != .some(activity) else { continue }
            recent.insert(
                AgentActivity(text: "\(c.name ?? "An agent") \(activity)", at: Date()), at: 0)
        }
        if recent.count > 5 { recent.removeLast(recent.count - 5) }
        if status != new { status = new }
    }

    // MARK: Documents

    /// A document window opened, or its file's name or place changed.
    func documentOpened(_ doc: SCADDocument) {
        documents[doc.agentID] = WeakDocument(doc)
        if let link { register(doc, with: link) }
    }

    func documentFocused(_ doc: SCADDocument) {
        focused = doc.agentID
        try? link?.documentFocused(id: doc.agentID)
    }

    func documentClosed(_ doc: SCADDocument) {
        documents[doc.agentID] = nil
        if focused == doc.agentID { focused = nil }
        try? link?.documentClosed(id: doc.agentID)
    }

    /// The open document with this number.
    func document(_ id: UInt64) -> SCADDocument? {
        documents[id]?.document
    }

    private func register(_ doc: SCADDocument, with link: AgentLink) {
        let file = doc.fileURL?.lastPathComponent ?? "\(doc.displayName ?? "Untitled").scad"
        try? link.documentOpened(id: doc.agentID, file: file, path: doc.fileURL?.path)
    }
}

/// One line of the popover's recent activity.
struct AgentActivity: Equatable, Identifiable {
    let id = UUID()
    let text: String
    let at: Date
}

private struct WeakDocument {
    weak var document: SCADDocument?
    init(_ document: SCADDocument) { self.document = document }
}

/// The link's observer: hands each status on (to the main thread).
final class StatusObserver: AgentObserver {
    private let deliver: @Sendable (AgentStatus, UInt64) -> Void

    init(_ deliver: @escaping @Sendable (AgentStatus, UInt64) -> Void) {
        self.deliver = deliver
    }

    func statusChanged(status: AgentStatus, sequence: UInt64) {
        deliver(status, sequence)
    }
}
