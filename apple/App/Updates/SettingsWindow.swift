// The Settings window (NeoSCAD > Settings…, Command-comma), in two panes:
//
// - Updates: the choices the owner decided on (docs/audits/auto-update.md,
//   "Decisions"): automatic checks, on by default, and the opt-in
//   release-candidate channel;
// - Agents: whether AI agents may work on open documents, whether their
//   edits wait for Apply, and who is connected
//   (Agents/AgentService.swift);
// - Language: NeoSCAD's extensions to the OpenSCAD language, off by
//   default (Editor/LanguageSettings.swift).
//
// SwiftUI forms in an AppKit window with toolbar tabs, as macOS settings
// windows look, because the app is built on NSApplication rather than a
// SwiftUI App (whose `Settings` scene this would otherwise be).

import AppKit
import Combine
import Sparkle
import SwiftUI

@MainActor
final class SettingsWindowController: NSWindowController {
    static let shared = SettingsWindowController()

    enum Tab: Int {
        case updates, agents, language
    }

    private let tabs = NSTabViewController()

    private init() {
        tabs.tabStyle = .toolbar
        tabs.addTabViewItem(
            Self.pane(
                "Updates", "arrow.triangle.2.circlepath",
                SettingsView(model: UpdateSettingsModel())))
        tabs.addTabViewItem(Self.pane("Agents", "sparkles", AgentSettingsView(service: .shared)))
        tabs.addTabViewItem(
            Self.pane("Language", "curlybraces", LanguageSettingsView(model: LanguageSettingsModel())))
        let window = NSWindow(contentViewController: tabs)
        window.styleMask = [.titled, .closable]
        window.setFrameAutosaveName("NeoSCADSettings")
        super.init(window: window)
    }

    private static func pane<V: View>(_ title: String, _ symbol: String, _ view: V) -> NSTabViewItem {
        let host = NSHostingController(rootView: view)
        // The window takes each pane's own size as the tab changes.
        host.sizingOptions = [.preferredContentSize]
        let item = NSTabViewItem(viewController: host)
        item.label = title
        item.image = NSImage(systemSymbolName: symbol, accessibilityDescription: title)
        return item
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("not from a nib") }

    func show(tab: Tab? = nil) {
        if let tab { tabs.selectedTabViewItemIndex = tab.rawValue }
        if window?.isVisible != true { window?.center() }
        showWindow(nil)
        window?.makeKeyAndOrderFront(nil)
    }
}

/// What the form shows and changes. The automatic-check switch is
/// Sparkle's own setting (it keeps it in the app's defaults), observed so
/// the form stays right if Sparkle changes it.
@MainActor
final class UpdateSettingsModel: ObservableObject {
    let updater: SPUUpdater?
    @Published var automaticChecks: Bool {
        didSet {
            if let updater, updater.automaticallyChecksForUpdates != automaticChecks {
                updater.automaticallyChecksForUpdates = automaticChecks
            }
        }
    }
    @Published var releaseCandidates: Bool {
        didSet {
            UserDefaults.standard.set(releaseCandidates, forKey: UpdateSettings.releaseCandidatesKey)
        }
    }
    private var observation: NSKeyValueObservation?

    init(updater: SPUUpdater? = AppUpdater.shared?.updater) {
        self.updater = updater
        automaticChecks = updater?.automaticallyChecksForUpdates ?? false
        releaseCandidates = UpdateSettings.receiveReleaseCandidates()
        observation = updater?.observe(\.automaticallyChecksForUpdates) { [weak self] u, _ in
            MainActor.assumeIsolated {
                guard let self, self.automaticChecks != u.automaticallyChecksForUpdates else { return }
                self.automaticChecks = u.automaticallyChecksForUpdates
            }
        }
    }
}

struct SettingsView: View {
    @ObservedObject var model: UpdateSettingsModel

    var body: some View {
        Form {
            Section("Updates") {
                if model.updater != nil {
                    Toggle("Check for updates automatically", isOn: $model.automaticChecks)
                    Toggle("Receive release candidates", isOn: $model.releaseCandidates)
                    Text(
                        "About once a day NeoSCAD fetches a signed list of releases from neoscad.org. "
                            + "It sends no version, system details or identifier. "
                            + "Release candidates are previews of the next version."
                    )
                    .font(.footnote)
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
                } else {
                    Text("This build of NeoSCAD doesn't update itself.")
                        .foregroundStyle(.secondary)
                }
            }
        }
        .formStyle(.grouped)
        .frame(width: 460)
        .fixedSize(horizontal: false, vertical: true)
    }
}

/// Settings > Language's state, kept in `LanguageSettings` (the app's
/// defaults), which tells open documents when it changes.
@MainActor
final class LanguageSettingsModel: ObservableObject {
    @Published var sketches: Bool {
        didSet { LanguageSettings.sketches = sketches }
    }
    @Published var queries: Bool {
        didSet { LanguageSettings.queries = queries }
    }

    init() {
        sketches = LanguageSettings.sketches
        queries = LanguageSettings.queries
    }
}

/// Settings > Language: NeoSCAD's extensions to the OpenSCAD language.
struct LanguageSettingsView: View {
    @ObservedObject var model: LanguageSettingsModel

    var body: some View {
        Form {
            Section("NeoSCAD extensions") {
                Toggle("Constrained sketches (sketch)", isOn: $model.sketches)
                Text(
                    "Points, lines, arcs and circles tied by constraints and solved into a 2D shape, "
                        + "like the command line's --enable sketch. A NeoSCAD extension, not in OpenSCAD: "
                        + "off, sketch() is an unknown module as in OpenSCAD."
                )
                .font(.footnote)
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
                Toggle("Geometry queries (query)", isOn: $model.queries)
                Text(
                    "Bounding boxes, measurements, distances and named anchors of a module's children "
                        + "as values (child_bounds() and the like), like the command line's --enable query. "
                        + "A NeoSCAD extension, not in OpenSCAD: off, they are unknown functions as in OpenSCAD."
                )
                .font(.footnote)
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
            }
        }
        .formStyle(.grouped)
        .frame(width: 460)
        .fixedSize(horizontal: false, vertical: true)
    }
}

/// Settings > Agents.
struct AgentSettingsView: View {
    @Bindable var service: AgentService

    var body: some View {
        Form {
            Section {
                Toggle(
                    "Allow AI agents to work on open documents",
                    isOn: Binding(get: { service.allowed }, set: { service.setAllowed($0) }))
                Text(AgentHelp.consentText)
                    .font(.footnote)
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
                Toggle("Ask before applying edits", isOn: $service.askBeforeApplying)
                    .disabled(!service.allowed)
            }
            Section("Connected agents") {
                AgentStatusList(service: service)
            }
            Section {
                HStack {
                    Button("Set Up Agents…") { AgentSetupPresenter.show() }
                    Spacer()
                    Link("Using NeoSCAD with AI Agents", destination: AgentHelp.url)
                }
            }
        }
        .formStyle(.grouped)
        .frame(width: 460)
        .fixedSize(horizontal: false, vertical: true)
    }
}
