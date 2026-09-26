// The 3D view in SwiftUI: the document's MetalView, or why there is none.

import AppKit
import SwiftUI

struct ViewportView: View {
    let controller: ViewportController

    var body: some View {
        if let error = controller.error {
            Text(error)
                .foregroundStyle(.secondary)
                .frame(maxWidth: .infinity, maxHeight: .infinity)
        } else {
            MetalViewRepresentable(controller: controller)
        }
    }
}

private struct MetalViewRepresentable: NSViewRepresentable {
    let controller: ViewportController

    func makeNSView(context: Context) -> MetalView {
        MetalView(controller: controller)
    }

    func updateNSView(_ view: MetalView, context: Context) {}

    static func dismantleNSView(_ view: MetalView, coordinator: ()) {
        view.detachLayer()
    }
}
