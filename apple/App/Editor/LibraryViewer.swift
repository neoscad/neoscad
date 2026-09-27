// Going to a definition in another file.
//
// Where it opens (the 8e decision). A file of the user's own (on disk,
// writable, outside the library directories) opens as a document like any
// other, editable, in its own window or tab as the system's tab setting
// says. A library file (under `OPENSCADPATH`, the user's library folder or
// the bundled libraries, or anything not writable) opens read-only in a
// viewer tab beside the window it was reached from, as Xcode shows
// framework headers: editing BOSL2 in place from a jump would change every
// model that uses it. The bundled MCAD exists only in the core's memory,
// so the viewer takes its text from the core, not the disk.
//
// A viewer has an editor and a language server of its own, so hover and
// go to definition work inside library code too, and one viewer per file:
// jumping to a file already shown selects its tab and moves the cursor.

import AppKit
import NeoSCADCore

@MainActor
enum LocationOpener {
    /// Show `uri` at a 0-based line and UTF-16 column, reached from
    /// `window`.
    static func open(uri: String, line: Int, character: Int, from window: NSWindow?) {
        guard let url = URL(string: uri), url.isFileURL else { return }
        let path = url.path
        if isUsersOwn(path) {
            NSDocumentController.shared.openDocument(withContentsOf: url, display: true) {
                document, _, _ in
                (document as? SCADDocument)?.model.editor.reveal(line: line, character: character)
            }
        } else {
            LibraryViewer.show(path: path, line: line, character: character, beside: window)
        }
    }

    /// The library directories, as the core searches them.
    static var libraryDirectories: [String] {
        guard case .success(let engine) = CoreService.shared else { return [] }
        return (try? engine.core.libraryDirs()) ?? []
    }

    /// Whether `path` is one of the user's files rather than a library's.
    static func isUsersOwn(_ path: String) -> Bool {
        let fm = FileManager.default
        guard fm.fileExists(atPath: path), fm.isWritableFile(atPath: path) else { return false }
        return !libraryDirectories.contains { path.hasPrefix($0.hasSuffix("/") ? $0 : $0 + "/") }
    }

    /// `path` as a reader knows it: relative to its library directory
    /// (`BOSL2/shapes3d.scad`), or in full.
    static func displayPath(_ path: String) -> String {
        for dir in libraryDirectories {
            let d = dir.hasSuffix("/") ? dir : dir + "/"
            if path.hasPrefix(d) { return String(path.dropFirst(d.count)) }
        }
        return (path as NSString).abbreviatingWithTildeInPath
    }
}

/// A library file, read-only, in a window (a tab) of its own.
@MainActor
final class LibraryViewer: NSWindowController, NSWindowDelegate {
    /// The open viewers, by path.
    private(set) static var viewers: [String: LibraryViewer] = [:]

    let path: String
    let editor = EditorController()

    /// Show `path` at a location: its viewer if one is open, else a new
    /// one tabbed with `window`. Nil when the core cannot read the file.
    @discardableResult
    static func show(path: String, line: Int, character: Int, beside window: NSWindow?)
        -> LibraryViewer?
    {
        if let v = viewers[path] {
            v.window?.makeKeyAndOrderFront(nil)
            v.editor.reveal(line: line, character: character)
            return v
        }
        guard case .success(let engine) = CoreService.shared,
            let text = try? engine.core.readFile(path: path)
        else {
            NSSound.beep()
            return nil
        }
        let v = LibraryViewer(path: path, text: text, engine: engine)
        viewers[path] = v
        if let window, let mine = v.window {
            mine.tabbingIdentifier = window.tabbingIdentifier
            window.addTabbedWindow(mine, ordered: .above)
            mine.makeKeyAndOrderFront(nil)
        } else {
            v.showWindow(nil)
        }
        v.editor.reveal(line: line, character: character)
        return v
    }

    private init(path: String, text: String, engine: Engine) {
        self.path = path
        let window = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 760, height: 760),
            styleMask: [.titled, .closable, .miniaturizable, .resizable],
            backing: .buffered, defer: false)
        window.title = (path as NSString).lastPathComponent
        window.subtitle = "Read Only · \(LocationOpener.displayPath(path))"
        if FileManager.default.fileExists(atPath: path) {
            window.representedURL = URL(fileURLWithPath: path)
        }
        super.init(window: window)
        window.delegate = self
        window.isReleasedWhenClosed = false
        editor.readOnly = true
        editor.text = { text }
        editor.documentURI = { URL(fileURLWithPath: path).absoluteString }
        editor.openLocation = { [weak self] uri, line, character in
            LocationOpener.open(uri: uri, line: line, character: character, from: self?.window)
        }
        if let server = try? engine.core.languageServer(hostDiagnostics: false) {
            editor.connect(server)
        }
        window.contentView = editor.webView
        window.initialFirstResponder = editor.webView
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) {
        fatalError("not from a nib")
    }

    func windowWillClose(_ notification: Notification) {
        editor.detach()
        Self.viewers[path] = nil
    }
}
