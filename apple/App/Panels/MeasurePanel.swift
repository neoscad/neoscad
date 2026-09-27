// The measure panel: `neoscad measure` on the document. The model's and
// each part's volume, surface area, bounding box and centre of mass; the
// distance between two parts; a cross-section at a plane (axis and height
// on a slider) with its area and perimeter, its outline drawn in the 3D
// view; and two points picked on the surface in the view, with the
// distance between them.
//
// A measurement renders the model once and keeps its solids in the core
// (`Measurement`), so moving the section's slider or picking points cuts
// and casts rays against the same solids without evaluating or rendering
// the document again. The numbers are for the text as it was measured:
// after an edit, Measure again.

import NeoSCADCore
import SwiftUI

/// The measure panel's state in one window.
@MainActor
@Observable
final class MeasureModel {
    var result: MeasureResult?
    var running = false
    var error: String?
    /// What the section cuts: `nil` for the model, else a part's name.
    var target: String?

    var sectionShown = false
    var axis: SectionAxis = .z
    var offset: Double = 0
    var section: SectionResult?

    var partA: String?
    var partB: String?
    var between: BetweenResult?

    /// Clicks in the view pick points on the surface (at most two).
    var picking = false
    var picks: [[Double]] = []

    /// The latest section request, so a slow cut that finishes after a
    /// newer one is dropped.
    @ObservationIgnored var sectionGeneration = 0

    /// The distance between the two picked points.
    var pickDistance: Double? {
        guard picks.count == 2 else { return nil }
        let (a, b) = (picks[0], picks[1])
        return sqrt((0..<3).map { (a[$0] - b[$0]) * (a[$0] - b[$0]) }.reduce(0, +))
    }

    /// The bounding box of what the section cuts.
    var targetBox: (min: [Double], max: [Double])? {
        guard let r = result else { return nil }
        let solid =
            target.flatMap { name in r.parts.first { $0.name == name }?.solid } ?? r.model
        guard let s = solid else { return nil }
        return (s.bboxMin, s.bboxMax)
    }

    /// The slider's range along the section's axis: the box, a hair
    /// inside so the ends still cut something.
    var offsetRange: ClosedRange<Double> {
        guard let b = targetBox else { return 0...1 }
        let i = axis.index
        let (lo, hi) = (b.min[i], b.max[i])
        guard hi > lo else { return lo...(lo + 1) }
        return lo...hi
    }
}

extension SectionAxis {
    var index: Int {
        switch self {
        case .x: 0
        case .y: 1
        case .z: 2
        }
    }
    var name: String { ["X", "Y", "Z"][index] }
}

/// A number of millimetres (or their squares and cubes) for the panel.
func mm(_ x: Double) -> String {
    x.formatted(.number.precision(.fractionLength(0...3)))
}

func vector(_ v: [Double]) -> String {
    "[" + v.map(mm).joined(separator: ", ") + "]"
}

struct MeasureView: View {
    let model: DocumentModel

    private var m: MeasureModel { model.measure }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            toolbar.padding(8)
            Divider()
            if let error = m.error {
                Label(error, systemImage: "exclamationmark.octagon")
                    .foregroundStyle(.red)
                    .padding(10)
                Spacer()
            } else if let r = m.result {
                ScrollView {
                    VStack(alignment: .leading, spacing: 10) {
                        modelBox(r)
                        if !r.parts.isEmpty { partsBox(r) }
                        if r.measurement != nil {
                            sectionBox
                            pickBox
                        }
                    }
                    .padding(8)
                }
            } else {
                Text("Measure renders the model and reports its volume, area, box and centre, with sections and distances.")
                    .font(.callout)
                    .foregroundStyle(.secondary)
                    .padding(10)
                Spacer()
            }
        }
        .frame(maxHeight: .infinity, alignment: .top)
        .background(.background.secondary)
        .accessibilityIdentifier("measure-panel")
    }

    private var toolbar: some View {
        HStack(spacing: 8) {
            Button("Measure") { model.actions.runMeasure() }
                .disabled(m.running)
                .help("Render the model and measure it")
            Toggle("Parts", isOn: Binding(get: { model.partsEnabled }, set: { model.actions.setParts($0) }))
                .toggleStyle(.checkbox)
                .help("Enable part(\"name\") { ... } in this document (--enable part)")
            Spacer()
            if m.running { ProgressView().controlSize(.small) }
        }
        .controlSize(.small)
    }

    private func modelBox(_ r: MeasureResult) -> some View {
        GroupBox("Model") {
            Grid(alignment: .leading, horizontalSpacing: 8, verticalSpacing: 3) {
                if let s = r.model {
                    stats(s)
                    if let c = r.components, let ok = r.manifold {
                        row("Pieces", "\(c)" + (ok ? "" : ", not manifold"))
                    }
                } else if let g = r.model2d {
                    row("Area", "\(mm(g.area)) mm²")
                    row("Size", vector(zip(g.bboxMax, g.bboxMin).map { $0 - $1 }) + " mm")
                    row("Contours", "\(g.contours ?? 0)")
                } else if r.exitCode != 0 {
                    Text("The model did not render" + (firstError(r.console).map { ": \($0)" } ?? "."))
                } else {
                    Text("The model is empty.")
                }
            }
            .font(.callout)
            .textSelection(.enabled)
            .frame(maxWidth: .infinity, alignment: .leading)
        }
        .accessibilityIdentifier("measure-model")
    }

    @ViewBuilder private func stats(_ s: SolidStats) -> some View {
        row("Volume", "\(mm(s.volume)) mm³")
        row("Area", "\(mm(s.area)) mm²")
        row("Size", vector(zip(s.bboxMax, s.bboxMin).map { $0 - $1 }) + " mm")
        row("Box", vector(s.bboxMin) + " – " + vector(s.bboxMax))
        row("Centroid", vector(s.centroid))
    }

    private func row(_ label: String, _ value: String) -> some View {
        GridRow {
            Text(label).foregroundStyle(.secondary)
            Text(value).monospacedDigit()
        }
    }

    private func partsBox(_ r: MeasureResult) -> some View {
        GroupBox("Parts") {
            VStack(alignment: .leading, spacing: 8) {
                ForEach(r.parts, id: \.name) { p in
                    DisclosureGroup {
                        Grid(alignment: .leading, horizontalSpacing: 8, verticalSpacing: 3) {
                            if let s = p.solid { stats(s) } else { Text("Not a solid") }
                            if let c = p.context { row("Changed by", c) }
                            if p.instances > 1 { row("Instances", "\(p.instances)") }
                        }
                        .font(.callout)
                        .textSelection(.enabled)
                    } label: {
                        Text(p.name).font(.callout.monospaced())
                    }
                }
                Divider()
                HStack {
                    partPicker("From", Binding(get: { m.partA }, set: { m.partA = $0; model.actions.measureBetween() }), r)
                    partPicker("To", Binding(get: { m.partB }, set: { m.partB = $0; model.actions.measureBetween() }), r)
                }
                .controlSize(.small)
                if let b = m.between {
                    Text(betweenText(b))
                        .font(.callout)
                        .textSelection(.enabled)
                        .accessibilityIdentifier("measure-between")
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
        }
    }

    private func partPicker(_ label: String, _ b: Binding<String?>, _ r: MeasureResult) -> some View {
        Picker(label, selection: b) {
            Text("None").tag(String?.none)
            ForEach(r.parts, id: \.name) { p in Text(p.name).tag(String?.some(p.name)) }
        }
    }

    private func betweenText(_ b: BetweenResult) -> String {
        if b.overlapping { return "\(b.a) and \(b.b) overlap by \(mm(b.overlapVolume)) mm³" }
        guard let d = b.distance else { return "\(b.a) or \(b.b) is not a solid" }
        if b.touching { return "\(b.a) and \(b.b) touch" }
        return "\(mm(d)) mm apart"
    }

    private var sectionBox: some View {
        GroupBox {
            VStack(alignment: .leading, spacing: 6) {
                if let r = m.result, !r.parts.isEmpty {
                    Picker("Cut", selection: Binding(get: { m.target }, set: { m.target = $0; model.actions.updateSection() })) {
                        Text("Model").tag(String?.none)
                        ForEach(r.parts, id: \.name) { p in Text(p.name).tag(String?.some(p.name)) }
                    }
                    .controlSize(.small)
                }
                Picker("Axis", selection: Binding(get: { m.axis }, set: { m.axis = $0; model.actions.updateSection() })) {
                    ForEach([SectionAxis.x, .y, .z], id: \.self) { a in Text(a.name).tag(a) }
                }
                .pickerStyle(.segmented)
                .labelsHidden()
                HStack {
                    Slider(
                        value: Binding(get: { m.offset }, set: { m.offset = $0; model.actions.updateSection() }),
                        in: m.offsetRange)
                    TextField(
                        "Height", value: Binding(get: { m.offset }, set: { m.offset = $0; model.actions.updateSection() }),
                        format: .number.precision(.fractionLength(0...3))
                    )
                    .frame(width: 64)
                    .textFieldStyle(.roundedBorder)
                }
                .controlSize(.small)
                if let s = m.section {
                    Grid(alignment: .leading, horizontalSpacing: 8, verticalSpacing: 3) {
                        row("Plane", s.plane)
                        row("Area", "\(mm(s.area)) mm²")
                        row("Perimeter", "\(mm(s.perimeter)) mm")
                        row("Contours", "\(s.contours)")
                    }
                    .font(.callout)
                    .textSelection(.enabled)
                    .accessibilityIdentifier("measure-section")
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            .disabled(!m.sectionShown)
        } label: {
            Toggle("Section", isOn: Binding(get: { m.sectionShown }, set: { m.sectionShown = $0; model.actions.updateSection() }))
                .toggleStyle(.checkbox)
        }
    }

    private var pickBox: some View {
        GroupBox {
            VStack(alignment: .leading, spacing: 4) {
                if m.picking {
                    Text(m.picks.count < 2 ? "Click the model in the view to pick point \(m.picks.count == 0 ? "A" : "B")." : "Click again to start over.")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
                ForEach(Array(m.picks.enumerated()), id: \.offset) { i, p in
                    Text("\(i == 0 ? "A" : "B"): \(vector(p))").font(.callout).monospacedDigit()
                }
                if let d = m.pickDistance {
                    Text("Distance: \(mm(d)) mm")
                        .font(.callout.bold())
                        .accessibilityIdentifier("measure-pick-distance")
                }
                if !m.picks.isEmpty {
                    Button("Clear") { model.actions.clearPicks() }.controlSize(.small)
                }
            }
            .textSelection(.enabled)
            .frame(maxWidth: .infinity, alignment: .leading)
        } label: {
            Toggle("Pick points", isOn: Binding(get: { m.picking }, set: { m.picking = $0; model.actions.clearPicks() }))
                .toggleStyle(.checkbox)
        }
    }
}
