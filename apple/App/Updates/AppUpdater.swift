// The app's updater: Sparkle 2 (docs/release.md, "The macOS app's
// updates"; docs/audits/auto-update.md).
//
// Sparkle reads the appcast at SUFeedURL, checks its EdDSA signature and
// the DMG's against SUPublicEDKey, and offers, downloads and installs the
// update. Both keys are in Info.plist, filled from the build settings
// NEOSCAD_SPARKLE_FEED_URL and NEOSCAD_SPARKLE_PUBLIC_KEY (apple/project.yml).
//
// A build without a public key (every development build, until the owner
// creates the key) has no updater: Sparkle started without a key would
// show the user an alert to "contact the developer" a few seconds after
// launch, so the updater is simply never created, and the menu item and
// Settings say so.

import AppKit
import Sparkle

/// The two choices the owner decided on (docs/audits/auto-update.md,
/// "Decisions"): automatic checks, on by default (Sparkle keeps that one
/// itself, as SUEnableAutomaticChecks in the app's defaults), and the
/// opt-in release-candidate channel, kept here.
enum UpdateSettings {
    static let releaseCandidatesKey = "ReceiveReleaseCandidates"

    /// The appcast's channel for release candidates
    /// (`<sparkle:channel>rc</sparkle:channel>`, written by
    /// scripts/release/appcast.py). Items without a channel are stable,
    /// and every updater sees those.
    static let releaseCandidateChannel = "rc"

    static func receiveReleaseCandidates(_ defaults: UserDefaults = .standard) -> Bool {
        defaults.bool(forKey: releaseCandidatesKey)
    }

    /// The channels Sparkle may offer besides the default (stable) one.
    static func allowedChannels(_ defaults: UserDefaults = .standard) -> Set<String> {
        receiveReleaseCandidates(defaults) ? [releaseCandidateChannel] : []
    }
}

@MainActor
final class AppUpdater: NSObject, SPUUpdaterDelegate {
    /// `nil` when this build has no usable public key.
    static let shared: AppUpdater? = {
        guard isConfigured(Bundle.main.infoDictionary ?? [:]) else { return nil }
        return AppUpdater()
    }()

    /// Whether an Info.plist carries what Sparkle needs: a feed URL and an
    /// EdDSA public key that is base64 of 32 bytes. A malformed key is
    /// treated as none: Sparkle would refuse to start with it and alert
    /// the user, which a build mistake should not cause.
    nonisolated static func isConfigured(_ info: [String: Any]) -> Bool {
        guard let feed = info["SUFeedURL"] as? String, URL(string: feed)?.scheme != nil,
            let key = info["SUPublicEDKey"] as? String,
            let bytes = Data(base64Encoded: key), bytes.count == 32
        else { return false }
        return true
    }

    private var controller: SPUStandardUpdaterController!

    private override init() {
        super.init()
        controller = SPUStandardUpdaterController(
            startingUpdater: false, updaterDelegate: self, userDriverDelegate: nil)
        // docs/privacy.md: the request carries no version or OS. Sparkle's
        // default User-Agent is "NeoSCAD/<version> Sparkle/<version>", and
        // system profiling is off by default (SUSendProfileInfo).
        controller.updater.userAgentString = "neoscad"
    }

    /// Starts Sparkle's schedule: the first check is due a day after the
    /// last one (or soon after the first launch).
    func start() {
        controller.startUpdater()
    }

    var updater: SPUUpdater { controller.updater }

    /// The menu item's target and action; Sparkle's controller validates
    /// it (disabled while a check is already showing).
    var menuTarget: AnyObject { controller }
    static let checkAction = #selector(SPUStandardUpdaterController.checkForUpdates(_:))

    // MARK: SPUUpdaterDelegate

    func allowedChannels(for updater: SPUUpdater) -> Set<String> {
        UpdateSettings.allowedChannels()
    }
}
