// The agent's controls in a document window (docs/audits/agent-connection-
// desktop.md, "UI spec"):
//
// - the toolbar control: the sparkle and "Connect your AI agent" while no
//   agent is connected; the agent's name with a green dot once one is
//   (several: "2 agents"); the dot pulses while an agent works, and the
//   tooltip says what it does ("Claude Code is editing");
// - its popover: the status, each agent with Disconnect, the last few
//   things they did, "Ask before applying edits", and the way to the
//   setup sheet;
// - the bar over the editor that asks before an agent's edit is applied
//   (only when the user chose so), and the chip over the 3D view while the
//   agent's marks show, which clears them.
//
// Agents' names are their own claims (MCP's clientInfo), shown as labels.

import AppKit
import NeoSCADCore
import SwiftUI

struct AgentToolbarControl: View {
    let service: AgentService
    @State private var showing = false

    var body: some View {
        Button {
            if service.allowed || service.isConnected {
                showing.toggle()
            } else {
                // Nothing to show yet: straight to setting it up.
                AgentSetupPresenter.show()
            }
        } label: {
            HStack(spacing: 5) {
                if service.isConnected {
                    // A shape, not a symbol: a toolbar button draws its
                    // symbols as templates, which would drop the green.
                    StatusDot(working: service.isWorking)
                } else {
                    Image(systemName: "sparkles")
                        .accessibilityHidden(true)
                }
                Text(service.shortLabel)
                    .lineLimit(1)
            }
            .padding(.horizontal, 10)
            .padding(.vertical, 4)
            .contentShape(Capsule())
        }
        // A style of its own: the toolbar's bordered buttons draw their
        // label as a template, which would turn the green dot grey.
        .buttonStyle(AgentToolbarButtonStyle())
        .help(service.statusLine)
        .accessibilityLabel(service.statusLine)
        .accessibilityIdentifier("agent-toolbar-control")
        .popover(isPresented: $showing, arrowEdge: .bottom) {
            AgentPopover(service: service) { showing = false }
        }
    }
}

/// The popover under the toolbar control.
struct AgentPopover: View {
    @Bindable var service: AgentService
    var close: () -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            AgentStatusList(service: service)
            if !service.recent.isEmpty {
                Divider()
                VStack(alignment: .leading, spacing: 4) {
                    Text("Recently").font(.caption).foregroundStyle(.secondary)
                    ForEach(service.recent) { a in
                        HStack(alignment: .firstTextBaseline) {
                            Text(a.text).lineLimit(1)
                            Spacer(minLength: 8)
                            Text(a.at, style: .time).foregroundStyle(.secondary)
                        }
                        .font(.callout)
                    }
                }
            }
            Divider()
            Toggle("Ask before applying edits", isOn: $service.askBeforeApplying)
                .toggleStyle(.checkbox)
            HStack {
                Button("Set Up Agents…") {
                    close()
                    AgentSetupPresenter.show()
                }
                Spacer()
                Button("Settings…") {
                    close()
                    SettingsWindowController.shared.show(tab: .agents)
                }
            }
        }
        .padding(14)
        .frame(width: 320)
    }
}

/// Who is connected, with Disconnect: shared by the popover, the sheet and
/// Settings.
struct AgentStatusList: View {
    let service: AgentService

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            if !service.allowed {
                Label("AI agents are off.", systemImage: "sparkles")
                    .foregroundStyle(.secondary)
            } else if let error = service.status?.error {
                Label(error, systemImage: "exclamationmark.triangle.fill")
                    .foregroundStyle(.red)
                    .fixedSize(horizontal: false, vertical: true)
            } else if service.clients.isEmpty {
                Label("Waiting for an agent", systemImage: "sparkles")
                Text("Agents set up below connect when they start a session.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            } else {
                ForEach(service.clients, id: \.id) { client in
                    HStack(spacing: 8) {
                        Image(systemName: "circle.fill")
                            .font(.system(size: 7))
                            .foregroundStyle(.green)
                            .symbolEffect(.pulse, options: .repeating, isActive: client.activity != nil)
                        // "Claude Code connected", "Claude Code is editing".
                        Text(
                            agentStatusLine(
                                status: AgentStatus(
                                    allowed: true, listening: true, address: nil, clients: [client],
                                    error: nil))
                        )
                        .lineLimit(1)
                        Spacer()
                        Button("Disconnect") { service.disconnect(client) }
                            .controlSize(.small)
                    }
                }
            }
        }
    }
}

/// "Claude Code wants to change line 12-14": over the editor, while an
/// agent's edit waits for the user's answer. Neither button is the default:
/// Return is typed into the editor.
struct AgentApprovalBar: View {
    let approval: AgentApproval

    var body: some View {
        HStack(spacing: 12) {
            Image(systemName: "sparkles").foregroundStyle(.tint)
            Text("\(approval.client ?? "An agent") wants to change \(approval.summary).")
                .lineLimit(2)
            Spacer()
            Button("Reject") { approval.resolve(false) }
            Button("Apply") { approval.resolve(true) }
        }
        .font(.callout)
        .padding(.horizontal, 12)
        .padding(.vertical, 8)
        .background(.bar)
        .accessibilityIdentifier("agent-approval")
    }
}

/// Over the 3D view while the agent's marks show.
struct AgentMarksChip: View {
    let count: Int
    let clear: () -> Void

    var body: some View {
        HStack(spacing: 6) {
            Image(systemName: "sparkles")
            Text(count == 1 ? "1 agent mark" : "\(count) agent marks")
            Button(action: clear) {
                Image(systemName: "xmark.circle.fill")
            }
            .buttonStyle(.plain)
            .help("Clear the agent's marks")
            .accessibilityLabel("Clear the agent's marks")
        }
        .font(.caption)
        .padding(.horizontal, 8)
        .padding(.vertical, 4)
        .background(.regularMaterial, in: Capsule())
        .padding(8)
    }
}

/// The connected agent's green dot; it pulses while the agent works, and
/// holds still with Reduce Motion on.
struct StatusDot: View {
    let working: Bool
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var dim = false

    var body: some View {
        Circle()
            .fill(.green)
            .frame(width: 8, height: 8)
            .opacity(working && dim ? 0.35 : 1)
            .animation(
                working && !reduceMotion ? .easeInOut(duration: 0.7).repeatForever() : .default,
                value: dim
            )
            .onChange(of: working, initial: true) { _, on in dim = on && !reduceMotion }
            .accessibilityHidden(true)
    }
}

/// A capsule like the toolbar's own buttons, which keeps the label's
/// colours.
struct AgentToolbarButtonStyle: ButtonStyle {
    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .foregroundStyle(.primary)
            .background(
                Capsule().fill(Color.primary.opacity(configuration.isPressed ? 0.16 : 0.07)))
    }
}
