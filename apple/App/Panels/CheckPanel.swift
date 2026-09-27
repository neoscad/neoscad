// The check panel: `neoscad check` on the document (thin walls,
// overhangs, floating pieces, bed fit, parts that intersect), with the
// printer's numbers as settings, and the findings listed with their
// severity, code, message and fix. Selecting a finding marks it in the 3D
// view (a numbered ring at its worst point and its box, as `snapshot
// --issues` numbers them) and turns the view to it.
//
// The settings are the app's, not the document's (one printer serves
// every model): a preset for a few common printers, or custom numbers,
// kept in the user defaults. "Auto" re-runs the check after each render
// (F6), not after each preview: a check renders the model in full, which
// after every pause in typing would cost what a render costs.

import NeoSCADCore
import SwiftUI

/// A printer's numbers for `check`.
struct PrinterPreset: Identifiable, Hashable {
    let id: String
    let name: String
    let nozzle: Double
    /// Width, depth, height in mm.
    let bed: [Double]

    /// Build volumes as the makers publish them, written from memory and
    /// not checked against their spec sheets in this change (a followup);
    /// every one ships with a 0.4 mm nozzle. Custom covers anything else.
    static let all: [PrinterPreset] = [
        PrinterPreset(id: "prusa-mk4", name: "Prusa MK4", nozzle: 0.4, bed: [250, 210, 220]),
        PrinterPreset(id: "prusa-mini", name: "Prusa MINI+", nozzle: 0.4, bed: [180, 180, 180]),
        PrinterPreset(id: "bambu-x1", name: "Bambu Lab X1 / P1", nozzle: 0.4, bed: [256, 256, 256]),
        PrinterPreset(id: "bambu-a1-mini", name: "Bambu Lab A1 mini", nozzle: 0.4, bed: [180, 180, 180]),
        PrinterPreset(id: "ender-3", name: "Creality Ender-3", nozzle: 0.4, bed: [220, 220, 250]),
        PrinterPreset(id: "voron-350", name: "Voron 2.4 (350)", nozzle: 0.4, bed: [350, 350, 340]),
    ]

    static func named(_ id: String) -> PrinterPreset? { all.first { $0.id == id } }
}

/// The check's settings as the panel edits them, kept in the user
/// defaults under `check.*`.
struct CheckSettingsValues: Equatable {
    /// A preset's id, or `custom`.
    var preset: String
    var nozzle: Double
    var minWall: Double
    var maxOverhang: Double
    var useBed: Bool
    var bed: [Double]

    static let custom = "custom"

    /// `check`'s own defaults (no bed), from the core.
    static var defaults: CheckSettingsValues {
        let d = (try? defaultCheckOptions())
            ?? CheckOptions(
                nozzle: 0.4, minWall: 0.8, maxOverhang: 45, bed: nil, bedTolerance: 0.05,
                maxFindings: 10)
        return CheckSettingsValues(
            preset: custom, nozzle: d.nozzle, minWall: d.minWall, maxOverhang: d.maxOverhang,
            useBed: false, bed: [220, 220, 250])
    }

    /// Take a preset's nozzle and bed; walls follow as two perimeters.
    mutating func apply(_ p: PrinterPreset) {
        preset = p.id
        nozzle = p.nozzle
        minWall = 2 * p.nozzle
        useBed = true
        bed = p.bed
    }

    /// What the core runs with. The bed tolerance and the findings per
    /// code stay `check`'s defaults.
    var options: CheckOptions {
        let d = (try? defaultCheckOptions())
        return CheckOptions(
            nozzle: nozzle, minWall: minWall, maxOverhang: maxOverhang,
            bed: useBed ? bed : nil, bedTolerance: d?.bedTolerance ?? 0.05,
            maxFindings: d?.maxFindings ?? 10)
    }

    // MARK: Persistence

    static func load(_ defaults: UserDefaults = .standard) -> CheckSettingsValues {
        var v = Self.defaults
        if let p = defaults.string(forKey: "check.preset") { v.preset = p }
        let number = { (key: String) in defaults.object(forKey: key) as? Double }
        if let x = number("check.nozzle"), x > 0 { v.nozzle = x }
        if let x = number("check.minWall"), x > 0 { v.minWall = x }
        if let x = number("check.maxOverhang"), (0...90).contains(x) { v.maxOverhang = x }
        v.useBed = defaults.bool(forKey: "check.useBed")
        if let b = defaults.array(forKey: "check.bed") as? [Double], b.count == 3,
            b.allSatisfy({ $0 > 0 })
        {
            v.bed = b
        }
        return v
    }

    func save(_ defaults: UserDefaults = .standard) {
        defaults.set(preset, forKey: "check.preset")
        defaults.set(nozzle, forKey: "check.nozzle")
        defaults.set(minWall, forKey: "check.minWall")
        defaults.set(maxOverhang, forKey: "check.maxOverhang")
        defaults.set(useBed, forKey: "check.useBed")
        defaults.set(bed, forKey: "check.bed")
    }
}

/// The check panel's state in one window.
@MainActor
@Observable
final class CheckModel {
    var settings = CheckSettingsValues.load() {
        didSet { if settings != oldValue { settings.save() } }
    }
    /// Re-run after each render.
    var auto = UserDefaults.standard.bool(forKey: "check.auto") {
        didSet { UserDefaults.standard.set(auto, forKey: "check.auto") }
    }
    var report: CheckReport?
    var running = false
    /// Why the last check could not run (not a finding: the core's error).
    var error: String?
    /// The finding shown in the view, by id.
    var selected: UInt32?
}

struct CheckView: View {
    let model: DocumentModel
    @State private var settingsShown = true

    private var check: CheckModel { model.check }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            toolbar.padding(8)
            Divider()
            DisclosureGroup("Printer", isExpanded: $settingsShown) {
                settings.padding(.top, 4)
            }
            .padding(8)
            Divider()
            results
        }
        .frame(maxHeight: .infinity, alignment: .top)
        .background(.background.secondary)
        .accessibilityIdentifier("check-panel")
    }

    private var toolbar: some View {
        HStack(spacing: 8) {
            Button("Check") { model.actions.runCheck() }
                .disabled(check.running)
                .help("Check the model for 3D printing (renders it in full)")
            Toggle("Auto", isOn: Binding(get: { check.auto }, set: { check.auto = $0 }))
                .toggleStyle(.checkbox)
                .help("Check again after each render (F6)")
            Toggle("Parts", isOn: Binding(get: { model.partsEnabled }, set: { model.actions.setParts($0) }))
                .toggleStyle(.checkbox)
                .help("Enable part(\"name\") { ... } in this document (--enable part)")
            Spacer()
            if check.running { ProgressView().controlSize(.small) }
        }
        .controlSize(.small)
    }

    private var settings: some View {
        let s = Binding(get: { check.settings }, set: { check.settings = $0 })
        return VStack(alignment: .leading, spacing: 6) {
            Picker("Printer", selection: presetBinding) {
                ForEach(PrinterPreset.all) { p in Text(p.name).tag(p.id) }
                Divider()
                Text("Custom").tag(CheckSettingsValues.custom)
            }
            Grid(alignment: .leading, horizontalSpacing: 6, verticalSpacing: 4) {
                number("Nozzle", s.nozzle, "mm", "Walls thinner than this cannot print: an error")
                number("Min wall", s.minWall, "mm", "Walls thinner than this are a warning")
                number("Max overhang", s.maxOverhang, "°", "Steepest overhang from vertical without support")
            }
            Toggle("Bed", isOn: custom(s.useBed)).toggleStyle(.checkbox)
            if check.settings.useBed {
                HStack(spacing: 4) {
                    ForEach(0..<3, id: \.self) { i in
                        TextField(
                            ["W", "D", "H"][i], value: custom(s.bed[i]),
                            format: .number.precision(.fractionLength(0...1))
                        )
                        .frame(width: 52)
                    }
                    Text("mm").foregroundStyle(.secondary)
                }
            }
        }
        .controlSize(.small)
        .textFieldStyle(.roundedBorder)
    }

    /// Editing a number by hand makes the settings custom.
    private func custom<T>(_ b: Binding<T>) -> Binding<T> {
        Binding(
            get: { b.wrappedValue },
            set: {
                b.wrappedValue = $0
                check.settings.preset = CheckSettingsValues.custom
            })
    }

    private func number(_ label: String, _ value: Binding<Double>, _ unit: String, _ help: String)
        -> some View
    {
        GridRow {
            Text(label)
            TextField(label, value: custom(value), format: .number.precision(.fractionLength(0...2)))
                .frame(width: 60)
            Text(unit).foregroundStyle(.secondary)
        }
        .help(help)
    }

    private var presetBinding: Binding<String> {
        Binding(
            get: { check.settings.preset },
            set: { id in
                if let p = PrinterPreset.named(id) {
                    check.settings.apply(p)
                } else {
                    check.settings.preset = CheckSettingsValues.custom
                }
            })
    }

    @ViewBuilder private var results: some View {
        if let error = check.error {
            Label(error, systemImage: "exclamationmark.octagon")
                .foregroundStyle(.red)
                .padding(10)
            Spacer()
        } else if let r = check.report {
            Text(summary(r))
                .font(.callout)
                .padding(.horizontal, 10)
                .padding(.vertical, 6)
                .accessibilityIdentifier("check-summary")
            List(r.findings, id: \.id, selection: selection) { f in
                FindingRow(finding: f)
            }
            .listStyle(.inset)
            .accessibilityIdentifier("check-findings")
        } else {
            Text("Check looks for walls too thin to print, overhangs, pieces off the bed and more.")
                .font(.callout)
                .foregroundStyle(.secondary)
                .padding(10)
            Spacer()
        }
    }

    private var selection: Binding<UInt32?> {
        Binding(get: { check.selected }, set: { model.actions.selectFinding($0) })
    }

    private func summary(_ r: CheckReport) -> String {
        if r.failed {
            return "The model did not render" + (firstError(r.console).map { ": \($0)" } ?? ".")
        }
        var s = "\(r.errors) errors, \(r.warnings) warnings, \(r.info) info"
        if let w = r.minWall { s += " · thinnest wall \(w.formatted()) mm" }
        let more = r.truncated.reduce(0) { $0 + Int($1.count) }
        if more > 0 { s += " · \(more) more not listed" }
        return s
    }
}

private struct FindingRow: View {
    let finding: CheckFinding

    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: 6) {
            Image(systemName: symbol)
                .foregroundStyle(color)
                .accessibilityLabel(severity)
            VStack(alignment: .leading, spacing: 2) {
                HStack(spacing: 4) {
                    Text("\(finding.id).").monospacedDigit().foregroundStyle(.secondary)
                    Text(finding.code).font(.callout.monospaced())
                    if let p = finding.part { Text("in \(p)").foregroundStyle(.secondary) }
                }
                Text(finding.message).font(.callout)
                Text("Fix: \(finding.fix)").font(.caption).foregroundStyle(.secondary)
            }
        }
        .textSelection(.enabled)
        .padding(.vertical, 2)
    }

    private var severity: String {
        switch finding.severity {
        case .error: "Error"
        case .warning: "Warning"
        case .info: "Info"
        }
    }

    private var symbol: String {
        switch finding.severity {
        case .error: "xmark.octagon.fill"
        case .warning: "exclamationmark.triangle.fill"
        case .info: "info.circle.fill"
        }
    }

    private var color: Color {
        switch finding.severity {
        case .error: .red
        case .warning: .orange
        case .info: .blue
        }
    }
}
