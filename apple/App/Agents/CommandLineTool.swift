// The `neoscad` command-line tool the app carries (Contents/Helpers/neoscad;
// docs/mcp.md, "Setup from the apps"), and the stable link to it that
// agent clients are configured with:
//
//     ~/Library/Application Support/NeoSCAD/bin/neoscad -> <app>/Contents/Helpers/neoscad
//
// Why a link and not the bundle path: a client's config outlives the app's
// location. Users move the app, and run it from Downloads before moving
// it. The link is refreshed at every launch, so a config written once
// keeps working after a move. Sparkle replaces the bundle in place, so an
// update keeps the path anyway. Why an absolute path at all: GUI clients
// on macOS (Claude Desktop) start servers with launchd's PATH, which holds
// neither Homebrew nor ~/.local/bin, so a bare `neoscad` fails there.
//
// The link is left alone, rather than pointed somewhere that is about to
// disappear, when this copy of the app runs from:
//   - a read-only volume (the DMG, before it is dragged to Applications);
//   - App Translocation's randomized mount (a quarantined app opened where
//     it was downloaded), whose path is gone after quit;
//   - the hosted tests (a DerivedData build), which must not repoint the
//     developer's own link.
// A regular file at the link's path is not ours (a user's own script, say)
// and is never replaced.

import Foundation
import os

enum CommandLineTool {
    /// A user default that moves the link's directory: for the release
    /// check of a locally built app (docs/release.md) and the tests, which
    /// must not touch the real one. Read from the launch arguments
    /// (`-NeoSCADToolLinkDirectory DIR`) or the app's defaults.
    static let directoryDefaultsKey = "NeoSCADToolLinkDirectory"

    private static let log = Logger(subsystem: "org.neoscad.NeoSCAD", category: "CommandLineTool")

    /// `~/Library/Application Support/NeoSCAD/bin`, unless the default
    /// above names another directory.
    static func linkDirectory(_ defaults: UserDefaults = .standard) -> URL {
        if let custom = defaults.string(forKey: directoryDefaultsKey), !custom.isEmpty {
            return URL(fileURLWithPath: custom, isDirectory: true)
        }
        let support = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask)[0]
        return support.appendingPathComponent("NeoSCAD/bin", isDirectory: true)
    }

    /// The link agent configs name. Its path, not its target: this is what
    /// the sheet writes into Claude Desktop's config and the rest.
    static func linkPath(_ defaults: UserDefaults = .standard) -> URL {
        linkDirectory(defaults).appendingPathComponent("neoscad")
    }

    /// The bundle's own tool, if this build has one.
    static func bundledTool(in bundle: Bundle = .main) -> URL? {
        let tool = bundle.bundleURL.appendingPathComponent("Contents/Helpers/neoscad")
        return FileManager.default.isExecutableFile(atPath: tool.path) ? tool : nil
    }

    enum Outcome: Equatable {
        /// There was no link; it was made.
        case created
        /// The link pointed elsewhere (an older location); it was moved.
        case updated(previous: String)
        /// It already pointed here.
        case current
        /// Left alone, with the reason.
        case skipped(String)
    }

    /// Why this copy of the app must not be the link's target, or `nil`.
    static func reasonNotToLink(appAt bundle: URL) -> String? {
        if bundle.path.contains("/AppTranslocation/") {
            return "the app runs from App Translocation's temporary location"
        }
        if let readOnly = try? bundle.resourceValues(forKeys: [.volumeIsReadOnlyKey]).volumeIsReadOnly,
            readOnly
        {
            return "the app runs from a read-only volume"
        }
        return nil
    }

    /// Makes `directory/neoscad` a symbolic link to `tool`. The new link
    /// is made beside the old one and renamed over it, so a client
    /// starting the tool at that moment finds one or the other, never
    /// nothing.
    static func refreshLink(to tool: URL, in directory: URL) throws -> Outcome {
        let fm = FileManager.default
        let link = directory.appendingPathComponent("neoscad")
        let target = tool.path
        var previous: String?
        if let attributes = try? fm.attributesOfItem(atPath: link.path) {
            guard attributes[.type] as? FileAttributeType == .typeSymbolicLink else {
                return .skipped("\(link.path) is not a link NeoSCAD made")
            }
            let current = try fm.destinationOfSymbolicLink(atPath: link.path)
            if current == target { return .current }
            previous = current
        }
        try fm.createDirectory(at: directory, withIntermediateDirectories: true)
        let temporary = directory.appendingPathComponent(".neoscad.\(ProcessInfo.processInfo.processIdentifier)")
        try? fm.removeItem(at: temporary)
        try fm.createSymbolicLink(atPath: temporary.path, withDestinationPath: target)
        if rename(temporary.path, link.path) != 0 {
            let error = POSIXError(POSIXErrorCode(rawValue: errno) ?? .EIO)
            try? fm.removeItem(at: temporary)
            throw error
        }
        return previous.map { .updated(previous: $0) } ?? .created
    }

    /// At launch (not under XCTest): point the link at this app's tool, off
    /// the main thread. Failures are logged, never shown: the link only
    /// matters once the user connects an agent, and the sheet checks it
    /// then.
    static func refreshAtLaunch(bundle: Bundle = .main) {
        guard let tool = bundledTool(in: bundle) else {
            log.info("no bundled command-line tool; the link is left alone")
            return
        }
        if let reason = reasonNotToLink(appAt: bundle.bundleURL) {
            log.info("command-line tool link left alone: \(reason, privacy: .public)")
            return
        }
        let directory = linkDirectory()
        DispatchQueue.global(qos: .utility).async {
            do {
                let outcome = try refreshLink(to: tool, in: directory)
                log.info("command-line tool link: \(String(describing: outcome), privacy: .public)")
            } catch {
                log.notice("could not refresh the command-line tool link: \(error.localizedDescription, privacy: .public)")
            }
        }
    }
}
