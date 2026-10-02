// The Settings window (NeoSCAD > Settings…, Command-comma). It holds the
// update choices the owner decided on (docs/audits/auto-update.md,
// "Decisions"): automatic checks, on by default, and the opt-in
// release-candidate channel. A SwiftUI form in an AppKit window, as the
// documents are, because the app is built on NSApplication rather than a
// SwiftUI App (whose `Settings` scene this would otherwise be).

import AppKit
import Combine
import Sparkle
import SwiftUI

@MainActor
final class SettingsWindowController: NSWindowController {
    static let shared = SettingsWindowController()

    private init() {
        let host = NSHostingController(rootView: SettingsView(model: UpdateSettingsModel()))
        let window = NSWindow(contentViewController: host)
        window.title = "Settings"
        window.styleMask = [.titled, .closable]
        window.setFrameAutosaveName("NeoSCADSettings")
        super.init(window: window)
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("not from a nib") }

    func show() {
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
        .frame(width: 420)
        .fixedSize(horizontal: false, vertical: true)
    }
}
