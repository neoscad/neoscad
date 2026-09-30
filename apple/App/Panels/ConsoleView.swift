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

    /// One line: time, then the geometry's numbers (the core's
    /// `describe_render`, which every host shows).
    static func describe(_ r: RenderResult, mode: RenderMode = .render) -> String {
        (try? describeRender(result: r, mode: mode)) ?? ""
    }

    /// The stages' times, for the summary's tooltip.
    static func timings(_ t: Timings) -> String {
        (try? describeTimings(timings: t)) ?? ""
    }
}

/// The console's filters: the core's groups (`console_groups`), with the
/// symbols this platform draws them with.
typealias ConsoleFilter = ConsoleGroup

extension ConsoleGroup {
    /// The groups in the order the core lists them.
    static let allCases: [ConsoleGroup] = ((try? consoleGroups()) ?? []).map(\.group)

    var title: String {
        ((try? consoleGroups()) ?? []).first { $0.group == self }?.title ?? ""
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
        (try? consoleGroup(kind: kind)) == self
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
