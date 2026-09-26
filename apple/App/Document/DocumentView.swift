// A document window's content: the editor above, the console below.

import SwiftUI

struct DocumentView: View {
    @Bindable var model: DocumentModel

    var body: some View {
        VSplitView {
            PlainTextEditor(text: $model.text)
                .frame(minWidth: 400, minHeight: 200)
            ConsoleView(report: model.report)
                .frame(minHeight: 90, idealHeight: 180)
        }
    }
}
