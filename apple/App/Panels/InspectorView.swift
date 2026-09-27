// The inspector beside the 3D view: the customizer, the check panel and
// the measure panel, one at a time under a segmented control (View >
// Customizer, Check and Measure choose one; choosing the one shown hides
// the inspector).

import SwiftUI

struct InspectorView: View {
    let model: DocumentModel

    var body: some View {
        VStack(spacing: 0) {
            Picker("Panel", selection: Binding(get: { model.inspector }, set: { model.inspector = $0 })) {
                ForEach(InspectorTab.allCases, id: \.self) { t in Text(t.rawValue).tag(t) }
            }
            .pickerStyle(.segmented)
            .labelsHidden()
            .padding(6)
            Divider()
            switch model.inspector {
            case .customizer: CustomizerView(model: model)
            case .check: CheckView(model: model)
            case .measure: MeasureView(model: model)
            }
        }
        .background(.background.secondary)
    }
}
