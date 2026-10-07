// NeoSCAD's language extensions the app runs every document with, from
// Settings > Language. Off by default, as on the command line: with them
// off a file means exactly what it means in OpenSCAD
// (docs/language-extensions.md, section 1). Only constrained sketches
// (`--enable sketch`, docs/sketch.md) are here; `part()` keeps its
// per-window toggle in the check and measure panels.

import Foundation

enum LanguageSettings {
    /// Posted when a setting changes: open documents pass the new names
    /// to their runs and language servers and run again.
    static let didChange = Notification.Name("NeoSCADLanguageSettingsDidChange")

    private static let sketchesKey = "EnableSketches"

    /// Constrained sketches: `sketch() { ... }` with its entities and
    /// constraints.
    static var sketches: Bool {
        get { UserDefaults.standard.bool(forKey: sketchesKey) }
        set {
            guard newValue != sketches else { return }
            UserDefaults.standard.set(newValue, forKey: sketchesKey)
            NotificationCenter.default.post(name: didChange, object: nil)
        }
    }

    /// The `--enable` names every document's runs, checks, exports and
    /// language server take.
    static var enable: [String] { sketches ? ["sketch"] : [] }
}
