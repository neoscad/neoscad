// The editor's web view: a WKWebView that routes the Edit menu to
// CodeMirror.
//
// Keys and menus. While the editor has focus, WebKit offers each key to
// the page, menu key equivalents included: CodeMirror's keymap takes ⌘Z,
// ⇧⌘Z, ⌘F, ⌘A, ⌘/ and the rest (marking them handled), and a key it
// declines (⌘S, ⌘W, the View menu's ⌘1...) goes on to the menu. F5 and F6
// the editor forwards as commands itself (src/editor.js, `appKeys`).
// AppTests/EditorTests.swift checks each layer, and its EditorKeyTests
// the whole path, with keys sent through NSApp.
//
// Choosing a menu item with the mouse sends its action down the responder
// chain instead, to this view first. Undo, Redo, Select All and Find must
// then reach CodeMirror too, so they are implemented here. WebKit's own
// versions would act on the page's DOM behind CodeMirror's back: its undo
// would replay DOM edits CodeMirror had already taken in.
//
// Undo manager: none. WebKit registers its native editing steps with the
// responder chain's undo manager, which for a document window is the
// document's; each keystroke would then count as an NSDocument change as
// well as the editor's own report of it, and the document's Undo would
// replay those native steps. The editor's history is the only one.

import AppKit
import WebKit

final class EditorWebView: WKWebView {
    weak var controller: EditorController?

    override var undoManager: UndoManager? { nil }

    @objc func undo(_ sender: Any?) {
        controller?.undo()
    }

    @objc func redo(_ sender: Any?) {
        controller?.redo()
    }

    override func selectAll(_ sender: Any?) {
        controller?.selectAll()
    }

    @objc func performFindPanelAction(_ sender: Any?) {
        controller?.openSearch()
    }

    override func validateUserInterfaceItem(_ item: any NSValidatedUserInterfaceItem) -> Bool {
        switch item.action {
        case #selector(undo(_:)):
            return (controller?.undoDepth ?? 0) > 0
        case #selector(redo(_:)):
            return (controller?.redoDepth ?? 0) > 0
        case #selector(selectAll(_:)), #selector(performFindPanelAction(_:)):
            return controller?.isReady ?? false
        default:
            return super.validateUserInterfaceItem(item)
        }
    }
}
