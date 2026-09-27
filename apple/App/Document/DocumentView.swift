// A document window's content: the editor above the console on the left,
// the 3D view in the middle, and the inspector (customizer, check,
// measure) on the right (View > Customizer, Check and Measure).

import SwiftUI

struct DocumentView: View {
    let model: DocumentModel

    var body: some View {
        HSplitView {
            VSplitView {
                CodeEditor(controller: model.editor)
                    .frame(minWidth: 320, minHeight: 200)
                ConsoleView(model: model)
                    .frame(minHeight: model.consoleCollapsed ? 30 : 90, idealHeight: 160,
                        maxHeight: model.consoleCollapsed ? 30 : .infinity)
            }
            .frame(minWidth: 320, idealWidth: 420)
            ViewportView(controller: model.viewport)
                .frame(minWidth: 320, idealWidth: 600, minHeight: 240)
            if model.customizerShown {
                InspectorView(model: model)
                    .frame(minWidth: 240, idealWidth: 300, maxWidth: 400)
            }
        }
    }
}
