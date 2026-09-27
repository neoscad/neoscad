// The customizer beside the 3D view: the document's annotated parameters
// (OpenSCAD's customizer comments, read by the core from the text), each
// with the control OpenSCAD's customizer gives it (sliders for numbers with
// a range, spin boxes for other numbers, checkboxes, text fields, vector
// fields and dropdowns), grouped as the file groups them.
//
// Editing a value never touches the text: the document runs again with
// the edited values as `-D`-style assignments after the text (see
// Document/DocumentLoop.swift). Reset returns every value to the text's.
// Parameter sets are OpenSCAD's JSON file beside the model: choosing one
// applies it as `-p file -P set` does, and Save writes the current values
// as a set OpenSCAD can read.

import NeoSCADCore
import SwiftUI

struct CustomizerView: View {
    let model: DocumentModel

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            toolbar
                .padding(8)
            Divider()
            if model.parameterGroups.isEmpty {
                Text("No parameters. Top-level assignments before the first module or function, with customizer comments, appear here.")
                    .font(.callout)
                    .foregroundStyle(.secondary)
                    .padding(12)
                Spacer()
            } else {
                ScrollView {
                    VStack(alignment: .leading, spacing: 10) {
                        ForEach(model.parameterGroups, id: \.name) { group in
                            GroupBox {
                                VStack(alignment: .leading, spacing: 10) {
                                    ForEach(group.parameters, id: \.name) { p in
                                        ParameterRow(parameter: p, model: model)
                                    }
                                }
                                .frame(maxWidth: .infinity, alignment: .leading)
                            } label: {
                                Text(group.name).font(.headline)
                            }
                        }
                    }
                    .padding(8)
                }
            }
        }
        .frame(maxHeight: .infinity, alignment: .top)
        .background(.background.secondary)
        .accessibilityIdentifier("customizer")
    }

    private var toolbar: some View {
        HStack(spacing: 6) {
            Picker("Set", selection: setBinding) {
                Text("Design default values").tag(String?.none)
                ForEach(model.parameterSets, id: \.self) { name in
                    Text(name).tag(String?.some(name))
                }
            }
            .labelsHidden()
            .disabled(!model.parameterSetsAvailable)
            .help(
                model.parameterSetsAvailable
                    ? "Parameter sets from the JSON file beside the model"
                    : "Save the document to keep parameter sets beside it")
            Button("Save…") { model.actions.saveParameterSet() }
                .disabled(!model.parameterSetsAvailable || model.parameterGroups.isEmpty)
                .help("Save the current values as a parameter set")
            Button("Reset") { model.actions.resetParameters() }
                .disabled(model.parameterValues.isEmpty)
                .help("Return every value to the one in the text")
        }
        .controlSize(.small)
    }

    private var setBinding: Binding<String?> {
        Binding(
            get: { model.selectedParameterSet },
            set: { name in
                if let name {
                    model.actions.applyParameterSet(name)
                } else {
                    model.actions.resetParameters()
                }
            })
    }
}

private struct ParameterRow: View {
    let parameter: Parameter
    let model: DocumentModel

    var body: some View {
        VStack(alignment: .leading, spacing: 3) {
            HStack(alignment: .firstTextBaseline) {
                Text(parameter.name)
                    .font(.callout.weight(model.parameterValues[parameter.name] == nil ? .regular : .semibold))
                if model.parameterValues[parameter.name] != nil {
                    Button {
                        model.actions.setParameter(parameter.name, nil)
                    } label: {
                        Image(systemName: "arrow.uturn.backward.circle")
                    }
                    .buttonStyle(.plain)
                    .foregroundStyle(.secondary)
                    .help("Back to the text's value")
                }
            }
            if !parameter.description.isEmpty {
                Text(parameter.description)
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
            control
        }
    }

    private var value: ParameterValue { model.value(of: parameter) }

    private func set(_ v: ParameterValue) {
        model.actions.setParameter(parameter.name, v == parameter.defaultValue ? nil : v)
    }

    private var number: Double {
        if case .number(let x) = value { return x }
        return 0
    }

    @ViewBuilder private var control: some View {
        switch parameter.control {
        case .checkbox:
            Toggle(
                parameter.name,
                isOn: Binding(
                    get: { if case .bool(let b) = value { return b } else { return false } },
                    set: { set(.bool(value: $0)) })
            )
            .labelsHidden()
        case .slider(let min, let max, let step):
            HStack {
                Slider(
                    value: Binding(get: { number }, set: { set(.number(value: snap($0, step, min))) }),
                    in: min...Swift.max(min, max))
                NumberField(value: number, step: step) { set(.number(value: $0)) }
                    .frame(width: 70)
            }
        case .spinBox(let min, let max, let step):
            HStack {
                NumberField(value: number, step: step) {
                    set(.number(value: clamp($0, min, max)))
                }
                .frame(width: 90)
                Stepper(
                    "",
                    onIncrement: { set(.number(value: clamp(number + (step ?? 1), min, max))) },
                    onDecrement: { set(.number(value: clamp(number - (step ?? 1), min, max))) }
                )
                .labelsHidden()
            }
        case .text(let maxLength):
            TextField(
                parameter.name,
                text: Binding(
                    get: { if case .text(let s) = value { return s } else { return "" } },
                    set: { s in
                        let t = maxLength.map { String(decoding: s.utf8.prefix(Int($0)), as: UTF8.self) } ?? s
                        set(.text(value: t))
                    })
            )
            .labelsHidden()
            .textFieldStyle(.roundedBorder)
        case .vector(let min, let max, let step):
            let items: [Double] = {
                if case .vector(let v) = value { return v }
                return []
            }()
            HStack(spacing: 4) {
                ForEach(items.indices, id: \.self) { i in
                    NumberField(value: items[i], step: step) { x in
                        var v = items
                        v[i] = clamp(x, min, max)
                        set(.vector(value: v))
                    }
                }
            }
        case .dropdown(let options):
            Picker(
                parameter.name,
                selection: Binding(
                    get: { options.firstIndex { $0.value == value } ?? 0 },
                    set: { i in if options.indices.contains(i) { set(options[i].value) } })
            ) {
                ForEach(options.indices, id: \.self) { i in
                    Text(options[i].label).tag(i)
                }
            }
            .labelsHidden()
        }
    }

    /// A slider's value on its step grid (OpenSCAD's slider moves in
    /// steps from the minimum).
    private func snap(_ x: Double, _ step: Double?, _ min: Double) -> Double {
        guard let step, step > 0 else { return x }
        let n = ((x - min) / step).rounded()
        // Keep the decimals of the step: 0.1 steps give 0.3, not
        // 0.30000000000000004.
        let decimals = max(0, -Int(floor(log10(step))) + 1)
        let v = min + n * step
        let scale = pow(10, Double(Swift.min(decimals, 12)))
        return (v * scale).rounded() / scale
    }

    private func clamp(_ x: Double, _ min: Double?, _ max: Double?) -> Double {
        var v = x
        if let min { v = Swift.max(v, min) }
        if let max { v = Swift.min(v, max) }
        return v
    }
}

/// A number typed into a field: committed on Return or when the field
/// loses focus, so a half-typed "1." does not run the model.
private struct NumberField: View {
    let value: Double
    let step: Double?
    let commit: (Double) -> Void
    @State private var text = ""
    @FocusState private var focused: Bool

    var body: some View {
        TextField("", text: $text)
            .textFieldStyle(.roundedBorder)
            .multilineTextAlignment(.trailing)
            .monospacedDigit()
            .focused($focused)
            .onAppear { text = Self.format(value) }
            .onChange(of: value) { _, v in if !focused { text = Self.format(v) } }
            .onChange(of: focused) { _, f in if !f { submit() } }
            .onSubmit(submit)
    }

    private func submit() {
        if let x = Double(text.trimmingCharacters(in: .whitespaces)), x.isFinite {
            commit(x)
        } else {
            text = Self.format(value)
        }
    }

    static func format(_ x: Double) -> String {
        String(format: "%g", x)
    }
}
