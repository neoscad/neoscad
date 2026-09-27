// The editor in SwiftUI: the document's EditorController's web view.

import AppKit
import SwiftUI

struct CodeEditor: NSViewRepresentable {
    let controller: EditorController

    func makeNSView(context: Context) -> EditorWebView {
        controller.webView
    }

    func updateNSView(_ view: EditorWebView, context: Context) {}
}
