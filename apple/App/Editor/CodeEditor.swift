// The editor in SwiftUI: the document's EditorController's web view.

import AppKit
import NeoSCADCore
import SwiftUI

struct CodeEditor: NSViewRepresentable {
    let controller: EditorController

    func makeNSView(context: Context) -> EditorWebView {
        controller.webView
    }

    func updateNSView(_ view: EditorWebView, context: Context) {}
}

extension EditorDiagnostic {
    /// The core's diagnostics about the file at `path` as lint markers, in
    /// the UTF-16 offsets of `lines` (the text the core rendered).
    /// Diagnostics without a position, or about another file (an included
    /// one), stay in the console only: there is nothing here to mark.
    static func from(_ diagnostics: [Diagnostic], path: String, lines: SourceLines) -> [Self] {
        diagnostics.compactMap { d in
            guard d.file == path else { return nil }
            let (from, to): (Int, Int)
            if let s = d.span {
                from = lines.utf16Offset(line: Int(s.startLine), column: Int(s.startColumn))
                to = lines.utf16Offset(line: Int(s.endLine), column: Int(s.endColumn))
            } else if let line = d.line {
                // A line and no span: the whole line.
                from = lines.utf16Offset(line: Int(line), column: 1)
                to = lines.utf16Offset(line: Int(line), column: Int.max / 2)
            } else {
                return nil
            }
            var message = d.message
            for hint in d.hints { message += "\n" + hint.message }
            let actions: [(name: String, edit: UTF16Edit)] = d.hints.compactMap { hint in
                guard let r = hint.replacement else { return nil }
                let s = r.span
                let edit = UTF16Edit(
                    from: lines.utf16Offset(line: Int(s.startLine), column: Int(s.startColumn)),
                    to: lines.utf16Offset(line: Int(s.endLine), column: Int(s.endColumn)),
                    insert: r.text)
                let name = r.text.isEmpty ? "Remove" : "Replace with \u{201C}\(r.text)\u{201D}"
                return (name, edit)
            }
            return EditorDiagnostic(
                from: from, to: max(from, to), severity: severity(d.severity),
                message: message, source: d.code, actions: actions)
        }
    }

    /// OpenSCAD's three kinds in CodeMirror's terms. A deprecation is not a
    /// fault in the model, so it shows as information, apart from warnings.
    private static func severity(_ s: Severity) -> String {
        switch s {
        case .error: "error"
        case .warning: "warning"
        case .deprecated: "info"
        }
    }
}
