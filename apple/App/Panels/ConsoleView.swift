// The console area under the editor: the last render's statistics, its
// diagnostics (line:column, code, message, first hint) and the console
// lines OpenSCAD would print. A stand-in for 8f's console and error
// gutter; enough to show the core's results end to end.

import NeoSCADCore
import SwiftUI

struct ConsoleView: View {
    let report: RenderReport

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            summary
                .font(.callout)
                .accessibilityIdentifier("render-summary")
            if case .rendered(let r, _) = report {
                ForEach(Array(r.diagnostics.enumerated()), id: \.offset) { _, d in
                    DiagnosticRow(diagnostic: d)
                }
                if !r.console.isEmpty {
                    ScrollView {
                        Text(r.console)
                            .font(.system(.caption, design: .monospaced))
                            .textSelection(.enabled)
                            .frame(maxWidth: .infinity, alignment: .leading)
                    }
                }
            }
            Spacer(minLength: 0)
        }
        .padding(8)
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
        .background(.background.secondary)
    }

    @ViewBuilder private var summary: some View {
        switch report {
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

    private static func fmt(_ x: Double) -> String {
        String(format: "%g", x)
    }
}

private struct DiagnosticRow: View {
    let diagnostic: Diagnostic

    var body: some View {
        let d = diagnostic
        VStack(alignment: .leading, spacing: 2) {
            HStack(alignment: .firstTextBaseline, spacing: 6) {
                Image(systemName: icon).foregroundStyle(color)
                if let s = d.span {
                    Text("\(s.startLine):\(s.startColumn)")
                        .font(.system(.callout, design: .monospaced))
                        .foregroundStyle(.secondary)
                }
                Text(d.message)
                Text(d.code).font(.caption).foregroundStyle(.tertiary)
            }
            if let hint = d.hints.first {
                Text(hint.message)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .padding(.leading, 22)
            }
        }
        .textSelection(.enabled)
    }

    private var icon: String {
        switch diagnostic.severity {
        case .error: "xmark.octagon.fill"
        case .warning: "exclamationmark.triangle.fill"
        case .deprecated: "clock.badge.exclamationmark"
        }
    }

    private var color: Color {
        switch diagnostic.severity {
        case .error: .red
        case .warning: .orange
        case .deprecated: .yellow
        }
    }
}
