// The editor's preferences shared by every window: the font size, which
// View > Bigger (⌘+) and Smaller (⌘−) change. The face is SF Mono, the
// system's monospaced font (src/theme.js).

import AppKit

enum EditorSettings {
    static let fontSizeDidChange = Notification.Name("NeoSCADEditorFontSizeDidChange")

    private static let key = "EditorFontSize"
    static let defaultFontSize = 12.0
    static let sizes = 8.0...36.0

    /// In CSS pixels, which are points on macOS.
    static var fontSize: Double {
        get {
            let v = UserDefaults.standard.double(forKey: key)
            return v == 0 ? defaultFontSize : min(max(v, sizes.lowerBound), sizes.upperBound)
        }
        set {
            let v = min(max(newValue.rounded(), sizes.lowerBound), sizes.upperBound)
            guard v != fontSize else { return }
            UserDefaults.standard.set(v, forKey: key)
            NotificationCenter.default.post(name: fontSizeDidChange, object: nil)
        }
    }
}
