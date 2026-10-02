// The updater's configuration and settings (App/Updates). Sparkle itself
// is exercised end to end by scripts/apple/test-updates.sh, which builds
// two versions and installs one over the other from a local appcast.

import AppKit
import Foundation
import Testing

@testable import NeoSCAD

@MainActor
struct UpdaterTests {
    private let feed = "https://neoscad.org/updates/macos/appcast.xml"
    // 32 bytes of base64: the shape of a Sparkle EdDSA public key.
    private let key = Data(repeating: 7, count: 32).base64EncodedString()

    @Test func configuredOnlyWithAFeedAndAThirtyTwoByteKey() {
        #expect(AppUpdater.isConfigured(["SUFeedURL": feed, "SUPublicEDKey": key]))
        // What a build without NEOSCAD_SPARKLE_PUBLIC_KEY carries.
        #expect(!AppUpdater.isConfigured(["SUFeedURL": feed, "SUPublicEDKey": ""]))
        #expect(!AppUpdater.isConfigured(["SUFeedURL": feed]))
        #expect(!AppUpdater.isConfigured(["SUPublicEDKey": key]))
        #expect(!AppUpdater.isConfigured(["SUFeedURL": "", "SUPublicEDKey": key]))
        // Not base64, and base64 of the wrong length.
        #expect(!AppUpdater.isConfigured(["SUFeedURL": feed, "SUPublicEDKey": "not a key"]))
        let short = Data(repeating: 7, count: 31).base64EncodedString()
        #expect(!AppUpdater.isConfigured(["SUFeedURL": feed, "SUPublicEDKey": short]))
    }

    /// Development builds (this test host among them) have no key, so no
    /// updater, no "Check for Updates…" and no request; the signed and
    /// verified Info.plist keys are still there for a keyed build.
    @Test func developmentBuildHasNoUpdater() throws {
        let info = try #require(Bundle.main.infoDictionary)
        #expect(info["SUPublicEDKey"] as? String == "")
        #expect(info["SUFeedURL"] as? String == feed)
        #expect(info["SURequireSignedFeed"] as? Bool == true)
        #expect(info["SUVerifyUpdateBeforeExtraction"] as? Bool == true)
        #expect(info["SUEnableAutomaticChecks"] as? Bool == true)
        #expect(AppUpdater.shared == nil)
        let appMenu = try #require(NSApp.mainMenu?.items.first?.submenu)
        #expect(!appMenu.items.contains { $0.title == "Check for Updates…" })
        #expect(appMenu.items.contains { $0.title == "Settings…" && $0.keyEquivalent == "," })
        #expect(UpdateSettingsModel(updater: nil).updater == nil)
    }

    /// Release candidates are opt-in: no channel but the default one until
    /// the setting is on.
    @Test func releaseCandidateChannelIsOptIn() throws {
        let suite = "org.neoscad.NeoSCADAppTests.updates"
        let defaults = try #require(UserDefaults(suiteName: suite))
        defer { defaults.removePersistentDomain(forName: suite) }
        #expect(UpdateSettings.allowedChannels(defaults).isEmpty)
        defaults.set(true, forKey: UpdateSettings.releaseCandidatesKey)
        #expect(UpdateSettings.allowedChannels(defaults) == ["rc"])
        defaults.set(false, forKey: UpdateSettings.releaseCandidatesKey)
        #expect(UpdateSettings.allowedChannels(defaults).isEmpty)
    }
}
