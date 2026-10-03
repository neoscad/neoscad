// A document window's content: the editor above the console on the left,
// the 3D view in the middle, and the inspector (customizer, check,
// measure) on the right (View > Customizer, Check and Measure). Over them,
// while the file on disk and the document disagree, a bar says so
// (SCADDocument+Disk.swift), and while an agent's edit waits for the
// user's answer, another asks (SCADDocument+Agent.swift).

import SwiftUI

struct DocumentView: View {
    let model: DocumentModel

    var body: some View {
        VStack(spacing: 0) {
            if let notice = model.diskNotice {
                DiskNoticeBar(notice: notice, actions: model.actions)
                Divider()
            }
            if let approval = model.agentApproval {
                AgentApprovalBar(approval: approval)
                Divider()
            }
            HSplitView {
                VSplitView {
                    CodeEditor(controller: model.editor)
                        .frame(minWidth: 320, minHeight: 200)
                    ConsoleView(model: model)
                        .frame(minHeight: model.consoleCollapsed ? 30 : 90, idealHeight: 160,
                            maxHeight: model.consoleCollapsed ? 30 : .infinity)
                }
                .frame(minWidth: 320, idealWidth: 420)
                ViewportView(controller: model.viewport)
                    .frame(minWidth: 320, idealWidth: 600, minHeight: 240)
                    .overlay(alignment: .topLeading) {
                        if model.agentMarkCount > 0 {
                            AgentMarksChip(count: model.agentMarkCount) {
                                model.actions.clearAgentMarks()
                            }
                        }
                    }
                if model.customizerShown {
                    InspectorView(model: model)
                        .frame(minWidth: 240, idealWidth: 300, maxWidth: 400)
                }
            }
        }
    }
}

/// "The file changed on disk": a bar rather than a sheet, so the user can
/// look at both before choosing, and nothing waits on the answer.
struct DiskNoticeBar: View {
    let notice: DiskNotice
    let actions: DocumentActions

    var body: some View {
        HStack(spacing: 12) {
            Image(systemName: "exclamationmark.triangle.fill")
                .foregroundStyle(.yellow)
            VStack(alignment: .leading, spacing: 2) {
                Text(title).bold()
                Text(message).foregroundStyle(.secondary)
            }
            Spacer()
            switch notice {
            case .changed(let reloadable):
                Button("Keep Mine") { actions.keepMine() }
                if reloadable {
                    // No default-button shortcut: Return is typed into the
                    // editor, and must never throw the user's changes away.
                    Button("Reload") { actions.reloadFromDisk() }
                }
            case .missing:
                Button("Dismiss") { actions.keepMine() }
            }
        }
        .font(.callout)
        .padding(.horizontal, 12)
        .padding(.vertical, 8)
        .background(.bar)
    }

    private var title: String {
        switch notice {
        case .changed: "The file changed on disk"
        case .missing: "The file was deleted"
        }
    }

    private var message: String {
        switch notice {
        case .changed(true):
            "Another program changed it while you had unsaved changes. Reload takes its version (Undo brings yours back)."
        case .changed(false):
            "Another program changed it, and it is no longer UTF-8 text."
        case .missing:
            "Saving will write it again."
        }
    }
}
