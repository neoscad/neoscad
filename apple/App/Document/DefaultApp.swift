// Whether NeoSCAD is the app that opens .scad files (org.openscad.scad,
// imported in apple/project.yml), and the offer to make it so.
//
// Installing NeoSCAD beside OpenSCAD leaves Finder opening .scad files in
// whichever app LaunchServices picked first, so at launch NeoSCAD asks
// once, and Settings > General can do it at any time after "Don't ask
// again".
//
// The question is only asked by a copy of the app worth registering. A
// default handler is stored as the app's location, so a copy that is
// about to disappear must not ask:
//   - the hosted tests and Debug builds: a DerivedData build would take
//     .scad files away from the developer's installed NeoSCAD, and an
//     alert would sit over the documents the tests drive;
//   - App Translocation's randomized mount and a read-only volume (the
//     DMG, before the app is dragged to Applications), whose paths are
//     gone after quit, leaving .scad files with a handler that no longer
//     exists.
//
// What macOS does on "Make Default" (macOS 27): LaunchServices puts up its
// own confirmation, "Do you want all documents with the extension .scad to
// open with NeoSCAD?", and calls the completion handler once the user
// answers. Whatever the handler gets, NeoSCAD asks LaunchServices again
// afterwards, so "Don't Change" (an error, or no error and no change) is
// reported as "macOS didn't change it" and left there: no retry, and the
// launch offer is not repeated in the same session.

import AppKit
import UniformTypeIdentifiers
import os

/// The two LaunchServices calls the offer makes, behind a protocol so the
/// tests can answer them without reading or changing this Mac's defaults.
@MainActor
protocol DefaultAppWorkspace {
    /// The bundle identifier of the app that opens `type`, or `nil`.
    func defaultApplicationIdentifier(for type: UTType) -> String?
    /// The display name of that app, for Settings.
    func defaultApplicationName(for type: UTType) -> String?
    /// Throws when the default did not change, the user's "Don't Change"
    /// among the reasons.
    func setDefaultApplication(at application: URL, for type: UTType) async throws
}

extension NSWorkspace: DefaultAppWorkspace {
    func defaultApplicationIdentifier(for type: UTType) -> String? {
        // The identifier, not the path: a user may have several copies of
        // NeoSCAD (Applications, a build, the DMG), and any of them being
        // the default means this question has been answered.
        urlForApplication(toOpen: type).flatMap { Bundle(url: $0)?.bundleIdentifier }
    }

    func defaultApplicationName(for type: UTType) -> String? {
        urlForApplication(toOpen: type).map { FileManager.default.displayName(atPath: $0.path) }
    }

    /// `setDefaultApplication(at:toOpen:completionHandler:)`, whose
    /// handler runs once the user has answered the system's confirmation.
    func setDefaultApplication(at application: URL, for type: UTType) async throws {
        try await setDefaultApplication(at: application, toOpen: type)
    }
}

enum DefaultApp {
    /// Set when the user ticks "Don't ask again" in the launch offer. As a
    /// launch argument (`-DefaultAppDontAsk YES`) it keeps scripted
    /// launches of a release build (scripts/apple/test-updates.sh) free of
    /// the sheet without storing anything.
    static let dontAskKey = "DefaultAppDontAsk"

    /// OpenSCAD's type, as Info.plist imports it.
    static let scadType = UTType(importedAs: "org.openscad.scad")

    private static let log = Logger(subsystem: "org.neoscad.NeoSCAD", category: "DefaultApp")

    /// Where and how this copy of the app is running: the facts the
    /// decision depends on that are not preferences.
    struct Context {
        var bundleURL: URL
        var bundleIdentifier: String?
        var isDebugBuild: Bool
        var underTests: Bool

        @MainActor static var current: Context {
            #if DEBUG
                let debug = true
            #else
                let debug = false
            #endif
            return Context(
                bundleURL: Bundle.main.bundleURL,
                bundleIdentifier: Bundle.main.bundleIdentifier,
                isDebugBuild: debug,
                underTests: NSClassFromString("XCTestCase") != nil
                    || ProcessInfo.processInfo.environment["XCTestConfigurationFilePath"] != nil)
        }
    }

    /// Why this copy of the app must not be registered as the default at
    /// all, even when the user asks in Settings, or `nil`.
    static func reasonNotToRegister(appAt bundle: URL) -> String? {
        // The same places the command-line tool's link avoids, for the
        // same reason: a path that is gone after quit.
        CommandLineTool.reasonNotToLink(appAt: bundle)
    }

    /// Why the launch offer is not shown, or `nil` to show it. Every
    /// condition is here, so the tests cover the whole decision; only
    /// `workspace` is asked about the system.
    @MainActor
    static func reasonNotToAsk(
        _ context: Context, defaults: UserDefaults, workspace: DefaultAppWorkspace
    ) -> String? {
        if context.underTests { return "running under XCTest" }
        if context.isDebugBuild { return "a Debug build" }
        if context.bundleURL.path.contains("/DerivedData/") { return "a build in DerivedData" }
        if let reason = reasonNotToRegister(appAt: context.bundleURL) { return reason }
        if defaults.bool(forKey: dontAskKey) { return "the user chose \"Don't ask again\"" }
        if isDefault(context, workspace: workspace) { return "NeoSCAD is already the default" }
        return nil
    }

    @MainActor
    static func isDefault(_ context: Context, workspace: DefaultAppWorkspace) -> Bool {
        guard let own = context.bundleIdentifier else { return false }
        return workspace.defaultApplicationIdentifier(for: scadType) == own
    }

    /// Makes this copy the default. `done` gets `nil` once NeoSCAD is the
    /// default, or the reason it is not (the user's "Don't Change" in the
    /// system's confirmation, or a LaunchServices failure), on the main
    /// actor.
    @MainActor
    static func makeDefault(
        _ context: Context, workspace: DefaultAppWorkspace,
        done: @escaping @MainActor (String?) -> Void
    ) {
        if let reason = reasonNotToRegister(appAt: context.bundleURL) {
            done("This copy of NeoSCAD can't be the default because \(reason).")
            return
        }
        Task { @MainActor in
            do {
                try await workspace.setDefaultApplication(at: context.bundleURL, for: scadType)
            } catch {
                log.notice(
                    "default app for .scad not changed: \(error.localizedDescription, privacy: .public)")
                done("macOS didn't change it.")
                return
            }
            // No error but no change either: say so rather than claim a
            // success that Settings would then contradict.
            done(isDefault(context, workspace: workspace) ? nil : "macOS didn't change it.")
        }
    }
}

/// The launch offer: an alert on the first document window.
@MainActor
enum DefaultAppPrompt {
    /// Once per launch, whatever the answer, so a declined system
    /// confirmation never brings the offer straight back.
    private static var asked = false

    /// At launch (not under XCTest; NeoSCADApp.swift): wait for a document
    /// window, then ask if `DefaultApp.reasonNotToAsk` allows it.
    static func scheduleAtLaunch(
        context: DefaultApp.Context = .current, defaults: UserDefaults = .standard,
        workspace: DefaultAppWorkspace = NSWorkspace.shared
    ) {
        if let reason = DefaultApp.reasonNotToAsk(context, defaults: defaults, workspace: workspace) {
            Logger(subsystem: "org.neoscad.NeoSCAD", category: "DefaultApp")
                .info("not offering to be the default app: \(reason, privacy: .public)")
            return
        }
        attempt(remaining: 20, context: context, defaults: defaults, workspace: workspace)
    }

    /// Tries every half second for ten seconds: the first window is made
    /// after launch finishes (restored windows, an opened file or the
    /// untitled document), and while any other sheet or modal panel is up
    /// the offer waits rather than stacking a second dialog on it.
    private static func attempt(
        remaining: Int, context: DefaultApp.Context, defaults: UserDefaults,
        workspace: DefaultAppWorkspace
    ) {
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.5) {
            MainActor.assumeIsolated {
                guard !asked, remaining > 0 else { return }
                let window = NSApp.windows.first {
                    $0.isVisible && $0.windowController?.document is SCADDocument
                }
                guard let window, window.attachedSheet == nil, NSApp.modalWindow == nil else {
                    attempt(
                        remaining: remaining - 1, context: context, defaults: defaults,
                        workspace: workspace)
                    return
                }
                asked = true
                show(on: window, context: context, defaults: defaults, workspace: workspace)
            }
        }
    }

    private static func show(
        on window: NSWindow, context: DefaultApp.Context, defaults: UserDefaults,
        workspace: DefaultAppWorkspace
    ) {
        let alert = NSAlert()
        alert.messageText = "Make NeoSCAD the default app for OpenSCAD files?"
        alert.informativeText =
            "Double-clicking a .scad file in Finder will open it in NeoSCAD. "
            + "You can change this later in NeoSCAD > Settings… > General."
        alert.addButton(withTitle: "Make Default")
        alert.addButton(withTitle: "Not Now")
        alert.showsSuppressionButton = true
        alert.suppressionButton?.title = "Don't ask again"
        alert.beginSheetModal(for: window) { response in
            if alert.suppressionButton?.state == .on {
                defaults.set(true, forKey: DefaultApp.dontAskKey)
            }
            guard response == .alertFirstButtonReturn else { return }
            DefaultApp.makeDefault(context, workspace: workspace) { failure in
                guard let failure else { return }
                report(failure, on: window)
            }
        }
    }

    /// Tells the user the default is unchanged, once, with where to try
    /// again.
    static func report(_ failure: String, on window: NSWindow?) {
        let alert = NSAlert()
        alert.messageText = "NeoSCAD is not the default app for OpenSCAD files"
        alert.informativeText =
            failure + " You can make NeoSCAD the default in NeoSCAD > Settings… > General."
        alert.addButton(withTitle: "OK")
        if let window, window.isVisible, window.attachedSheet == nil {
            alert.beginSheetModal(for: window)
        } else {
            alert.runModal()
        }
    }
}
