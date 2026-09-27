// The console under the editor: the last run's summary (its time, and a
// render's geometry numbers) and every line OpenSCAD would print, echo
// output and warnings interleaved as they happened. Lines can be filtered
// by kind and by text, the list collapses to its summary, and a line that
// points into a file jumps there when clicked (this document's lines
// select their span in the editor; another file's open its window).

import NeoSCADCore
import SwiftUI

struct ConsoleView: View {
    let model: DocumentModel
    @State private var shown: Set<ConsoleFilter> = Set(ConsoleFilter.allCases)
    @State private var search = ""

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            header
                .padding(.horizontal, 8)
                .padding(.vertical, 5)
            if !model.consoleCollapsed {
                Divider()
                lines
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
        .background(.background.secondary)
    }

    private var header: some View {
        HStack(spacing: 8) {
            Button {
                model.consoleCollapsed.toggle()
            } label: {
                Image(systemName: model.consoleCollapsed ? "chevron.right" : "chevron.down")
                    .frame(width: 12)
            }
            .buttonStyle(.plain)
            .accessibilityLabel(model.consoleCollapsed ? "Show console" : "Hide console")
            summary
                .font(.callout)
                .lineLimit(1)
                .layoutPriority(1)
                .accessibilityIdentifier("render-summary")
            Spacer(minLength: 8)
            if !model.consoleCollapsed {
                ForEach(ConsoleFilter.allCases, id: \.self) { f in
                    let n = model.console.filter { f.matches($0.kind) }.count
                    Toggle(isOn: binding(f)) {
                        Label("\(n)", systemImage: f.symbol).monospacedDigit()
                    }
                    .toggleStyle(.button)
                    .controlSize(.small)
                    .fixedSize()
                    .help("Show \(f.title.lowercased()) lines")
                    .accessibilityLabel("\(f.title) \(n)")
                }
                TextField("Filter", text: $search)
                    .textFieldStyle(.roundedBorder)
                    .controlSize(.small)
                    .frame(minWidth: 60, maxWidth: 140)
            }
        }
    }

    private func binding(_ f: ConsoleFilter) -> Binding<Bool> {
        Binding(
            get: { shown.contains(f) },
            set: { on in
                if on { shown.insert(f) } else { shown.remove(f) }
            })
    }

    private var visible: [(Int, ConsoleLine)] {
        let needle = search.trimmingCharacters(in: .whitespaces)
        return model.console.enumerated().filter { _, line in
            ConsoleFilter.allCases.contains { shown.contains($0) && $0.matches(line.kind) }
                && (needle.isEmpty || line.text.localizedCaseInsensitiveContains(needle))
        }
    }

    private var lines: some View {
        ScrollView {
            LazyVStack(alignment: .leading, spacing: 1) {
                ForEach(visible, id: \.0) { _, line in
                    ConsoleRow(line: line) { range in model.actions.jump(range) }
                }
            }
            .padding(.horizontal, 8)
            .padding(.vertical, 4)
            .frame(maxWidth: .infinity, alignment: .leading)
        }
    }

    @ViewBuilder private var summary: some View {
        switch model.report {
        case .idle:
            Text("Design > Preview (F5) previews the model; Render (F6) renders it.")
                .foregroundStyle(.secondary)
        case .running(let mode):
            HStack(spacing: 6) {
                ProgressView().controlSize(.small)
                Text(mode == .preview ? "Previewing…" : "Rendering…")
            }
        case .failed(let message):
            Label(message, systemImage: "exclamationmark.octagon")
                .foregroundStyle(.red)
        case .rendered(let r, let mode):
            Text(Self.describe(r, mode: mode))
                .help(Self.timings(r.timings))
        }
    }

    /// One line: time, then the geometry's numbers (a preview has none:
    /// it builds the CSG products, not one mesh).
    static func describe(_ r: RenderResult, mode: RenderMode = .render) -> String {
        let ms = String(format: "%.1f ms", r.timings.totalMs)
        if mode == .preview {
            return r.exitCode == 0 ? "Previewed in \(ms)." : "Preview failed (\(ms))."
        }
        guard r.exitCode == 0 else { return "Render failed (\(ms))." }
        guard let g = r.geometry else { return "Rendered in \(ms): empty result." }
        let size = zip(g.bboxMax, g.bboxMin).map { fmt($0 - $1) }.joined(separator: " × ")
        var parts = ["\(g.dimensions)D", "bbox \(size)"]
        if let v = g.volume { parts.append("volume \(fmt(v))") }
        parts.append("area \(fmt(g.area))")
        if let t = g.triangles { parts.append("\(t) triangles") }
        if let c = g.components { parts.append("\(c) component\(c == 1 ? "" : "s")") }
        if let m = g.manifold { parts.append(m ? "manifold" : "not manifold") }
        return "Rendered in \(ms): " + parts.joined(separator: ", ")
    }

    /// The stages' times, for the summary's tooltip.
    static func timings(_ t: Timings) -> String {
        String(
            format: "Parse %.1f ms, evaluate %.1f ms, geometry %.1f ms; total %.1f ms",
            t.parseMs, t.evaluateMs, t.geometryMs, t.totalMs)
    }

    private static func fmt(_ x: Double) -> String {
        String(format: "%g", x)
    }
}

/// The console's filters: what kinds of line each shows.
enum ConsoleFilter: CaseIterable {
    case errors, warnings, echo, other

    var title: String {
        switch self {
        case .errors: "Errors"
        case .warnings: "Warnings"
        case .echo: "Echo"
        case .other: "Other"
        }
    }

    var symbol: String {
        switch self {
        case .errors: "xmark.octagon"
        case .warnings: "exclamationmark.triangle"
        case .echo: "text.bubble"
        case .other: "info.circle"
        }
    }

    func matches(_ kind: ConsoleKind) -> Bool {
        switch self {
        case .errors: kind == .error || kind == .trace
        case .warnings: kind == .warning || kind == .deprecated
        case .echo: kind == .echo
        case .other: kind == .info
        }
    }
}

private struct ConsoleRow: View {
    let line: ConsoleLine
    let jump: (SourceRange) -> Void

    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: 6) {
            Image(systemName: icon)
                .foregroundStyle(color)
                .frame(width: 14)
            Text(line.text)
                .font(.system(.caption, design: .monospaced))
                .foregroundStyle(line.kind == .info ? .secondary : .primary)
                .textSelection(.enabled)
                .frame(maxWidth: .infinity, alignment: .leading)
            if line.location != nil {
                Image(systemName: "arrow.right.circle")
                    .foregroundStyle(.tertiary)
                    .help("Show in the editor")
            }
        }
        .contentShape(Rectangle())
        .onTapGesture {
            if let at = line.location { jump(at) }
        }
        .accessibilityElement(children: .combine)
        .accessibilityAddTraits(line.location != nil ? .isButton : [])
    }

    private var icon: String {
        switch line.kind {
        case .error: "xmark.octagon.fill"
        case .warning: "exclamationmark.triangle.fill"
        case .deprecated: "clock.badge.exclamationmark"
        case .echo: "text.bubble"
        case .trace: "arrow.turn.down.right"
        case .info: "info.circle"
        }
    }

    private var color: Color {
        switch line.kind {
        case .error, .trace: .red
        case .warning: .orange
        case .deprecated: .yellow
        case .echo: .accentColor
        case .info: .secondary
        }
    }
}
