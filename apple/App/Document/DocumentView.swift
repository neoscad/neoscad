// A document window's content: the editor above the console on the left,
// the 3D view on the right.

import SwiftUI

struct DocumentView: View {
    let model: DocumentModel

    var body: some View {
        HSplitView {
            VSplitView {
                CodeEditor(controller: model.editor)
                    .frame(minWidth: 320, minHeight: 200)
                ConsoleView(report: model.report)
                    .frame(minHeight: 90, idealHeight: 160)
            }
            .frame(minWidth: 320, idealWidth: 420)
            ViewportView(controller: model.viewport)
                .frame(minWidth: 320, idealWidth: 640, minHeight: 240)
        }
    }
}
