// File > Examples: the examples every NeoSCAD app offers, from the core
// (`examples`, `example_source`; the files are the web demo's
// `web/examples/`, embedded in the core at build time). Opening one makes a
// new untitled document with its text, as OpenSCAD's File > Examples
// does, so editing it never touches a file the user did not choose. Heavy
// examples (seconds of geometry) open without running; Preview or Render
// runs them.

import AppKit
import NeoSCADCore

@MainActor
final class ExampleMenu: NSObject {
    static let shared = ExampleMenu()

    /// The examples, in the core's order.
    static var all: [Example] { (try? examples()) ?? [] }

    /// The submenu, one item per example; the item's represented object is
    /// the example's id.
    func menu() -> NSMenu {
        let m = NSMenu(title: "Examples")
        for e in Self.all {
            let item = NSMenuItem(title: e.title, action: #selector(open(_:)), keyEquivalent: "")
            item.target = self
            item.representedObject = e.id
            if !e.note.isEmpty { item.toolTip = e.note }
            m.addItem(item)
        }
        return m
    }

    @objc func open(_ sender: NSMenuItem) {
        guard let id = sender.representedObject as? String else { return }
        do {
            _ = try Self.open(id: id)
        } catch {
            NSAlert(error: error).runModal()
        }
    }

    /// A new untitled document holding the example `id`.
    @discardableResult
    static func open(id: String) throws -> SCADDocument? {
        guard let example = all.first(where: { $0.id == id }) else { return nil }
        let text = try exampleSource(id: id)
        let controller = NSDocumentController.shared
        guard let doc = try controller.makeUntitledDocument(ofType: controller.defaultType ?? "") as? SCADDocument
        else { return nil }
        doc.displayName = (example.fileName as NSString).deletingPathExtension
        doc.model.partsEnabled = example.parts
        _ = try? doc.loop.setParts(on: example.parts)
        doc.runsWhenTextIsReplaced = example.autorun
        doc.model.text = text
        doc.runsWhenTextIsReplaced = true
        controller.addDocument(doc)
        doc.makeWindowControllers()
        doc.showWindows()
        return doc
    }
}
