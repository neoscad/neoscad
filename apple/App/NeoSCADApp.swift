// The application: AppKit's document architecture with SwiftUI content.
//
// Why NSDocument and not SwiftUI's DocumentGroup (docs/audits/macos-prep.md,
// "Documents"): the architecture asks for autosave in place, versions and
// window tabs, and in 8d the text will live in a web view (CodeMirror),
// so the document must decide when to pull the text for saving.
// NSDocument gives that control directly; DocumentGroup's autosave and
// versions behaviour on macOS was not verified. Each window's content is
// a SwiftUI view in an NSHostingController (Document/SCADDocument.swift).
//
// The menu bar is built in code (MainMenu.swift) rather than in a
// storyboard: it is short, reviewable as text, and XcodeGen needs no
// Interface Builder files.

import AppKit
import NeoSCADCore

@main
@MainActor
final class AppDelegate: NSObject, NSApplicationDelegate {
    /// NSApplication holds its delegate weakly.
    private static var instance: AppDelegate?

    static func main() {
        let app = NSApplication.shared
        let delegate = AppDelegate()
        instance = delegate
        app.delegate = delegate
        app.setActivationPolicy(.regular)
        app.mainMenu = MainMenu.make()
        app.run()
    }

    func applicationDidFinishLaunching(_ notification: Notification) {
        NSApp.activate()
        // Sparkle's schedule (App/Updates/AppUpdater.swift). Not under
        // XCTest: the hosted app tests must make no network request, and
        // a check's window would sit over the documents they drive.
        if NSClassFromString("XCTestCase") == nil {
            AppUpdater.shared?.start()
            // The stable link agent clients run the bundled CLI through
            // (App/Agents/CommandLineTool.swift). Not under XCTest: a
            // DerivedData build must not repoint the developer's own link.
            CommandLineTool.refreshAtLaunch()
        }
    }

    @objc func showSettings(_ sender: Any?) {
        SettingsWindowController.shared.show()
    }

    /// A new untitled document on launch without files to open, as a text
    /// editor does.
    func applicationShouldOpenUntitledFile(_ sender: NSApplication) -> Bool {
        true
    }

    // MARK: The editor's font (View menu), for every window

    @objc func increaseEditorFontSize(_ sender: Any?) {
        EditorSettings.fontSize += 1
    }

    @objc func decreaseEditorFontSize(_ sender: Any?) {
        EditorSettings.fontSize -= 1
    }

    @objc func resetEditorFontSize(_ sender: Any?) {
        EditorSettings.fontSize = EditorSettings.defaultFontSize
    }

    /// The standard About panel (name, icon and "Version x (build)" from
    /// Info.plist), with the core's version as credits.
    @objc func showAboutPanel(_ sender: Any?) {
        var options: [NSApplication.AboutPanelOptionKey: Any] = [:]
        if let v = try? coreVersion() {
            options[.credits] = NSAttributedString(
                string: "NeoSCAD core \(v)",
                attributes: [
                    .font: NSFont.systemFont(ofSize: NSFont.smallSystemFontSize),
                    .foregroundColor: NSColor.secondaryLabelColor,
                ])
        }
        NSApp.orderFrontStandardAboutPanel(options: options)
    }
}

/// The one Rust core of the process, shared by every document so they
/// share its caches. `nil` result (with the reason) if it could not start.
enum CoreService {
    static let shared: Result<Engine, CoreError> = {
        do {
            return .success(try Engine(resourceDirectory: Bundle.main.resourcePath))
        } catch let e as CoreError {
            return .failure(e)
        } catch {
            return .failure(.Failed(message: "\(error)"))
        }
    }()
}
