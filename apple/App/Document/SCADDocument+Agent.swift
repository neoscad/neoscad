// An AI agent working on this document (Agents/DocumentsAgentHost.swift
// calls these on the main actor, one per request).
//
// - read: the editor's text (unsaved changes included), its revision
//   (`agentRevision`), the selection, the customizer's values and the last
//   run.
// - edit: only on the revision the agent read; applied by the editor's
//   `agentEdit` as one undoable, highlighted step, with the user's
//   selection kept. When the user chose "Ask before applying", a bar over
//   the editor waits for Apply or Reject first.
// - reveal, camera, capture, annotate: the editor's `revealRange`, and the
//   viewport's `applyAgentCamera`, `captureAsShown` and agent layer of
//   marks, which the user clears from the view.
//
// Nothing here saves, exports or runs anything the agent names: an edit
// changes the buffer, which autosave then treats like the user's typing.

import AppKit
import NeoSCADCore

/// An agent's edit waiting for the user (the bar over the editor).
@MainActor
final class AgentApproval: Identifiable {
    /// The agent's self-reported name, or nil.
    let client: String?
    /// What changes ("line 12-14").
    let summary: String
    private var decide: ((Bool) -> Void)?

    init(client: String?, summary: String, decide: @escaping (Bool) -> Void) {
        self.client = client
        self.summary = summary
        self.decide = decide
    }

    /// Apply (true) or reject; only the first answer counts.
    func resolve(_ apply: Bool) {
        let d = decide
        decide = nil
        d?(apply)
    }
}

extension SCADDocument {
    // MARK: Window

    /// The toolbar, and telling the agent link about this window.
    func attachAgentControls(to window: NSWindow) {
        let toolbar = DocumentToolbar()
        self.toolbar = toolbar
        toolbar.install(in: window)
        AgentService.shared.documentOpened(self)
        keyObserver = NotificationCenter.default.addObserver(
            forName: NSWindow.didBecomeKeyNotification, object: window, queue: .main
        ) { [weak self] _ in
            MainActor.assumeIsolated {
                guard let self else { return }
                AgentService.shared.documentFocused(self)
            }
        }
        if window.isKeyWindow { AgentService.shared.documentFocused(self) }
    }

    func detachAgentControls() {
        if let keyObserver { NotificationCenter.default.removeObserver(keyObserver) }
        keyObserver = nil
        model.agentApproval?.resolve(false)
        AgentService.shared.documentClosed(self)
    }

    // MARK: Requests

    func agentRead() async -> AgentDocumentState {
        // The text and its revision together, before anything waits.
        let version = agentRevision
        let text = model.text
        var selection: EditorSelection?
        if model.editor.isReady,
            let s = try? await model.editor.call("return NeoSCADEditor.selectionPositions()")
                as? [String: Any],
            let anchor = Self.position(s["anchor"]), let head = Self.position(s["head"])
        {
            selection = EditorSelection(anchor: anchor, head: head)
        }
        return AgentDocumentState(
            version: version, text: text, selection: selection, overrides: overrides,
            parts: model.partsEnabled, run: runStatus, console: model.console)
    }

    private static func position(_ any: Any?) -> EditorPosition? {
        guard let a = any as? [Any], a.count == 2, let l = a[0] as? Int, let c = a[1] as? Int,
            l >= 0, c >= 0
        else { return nil }
        return EditorPosition(line: UInt32(l), character: UInt32(c))
    }

    /// The last run as the console's summary shows it.
    private var runStatus: AgentRunStatus {
        switch model.report {
        case .idle:
            AgentRunStatus(mode: nil, summary: "", running: false)
        case .running(let mode):
            AgentRunStatus(mode: mode, summary: "", running: true)
        case .rendered(let r, let mode):
            AgentRunStatus(mode: mode, summary: ConsoleView.describe(r, mode: mode), running: false)
        case .failed(let message):
            AgentRunStatus(mode: lastMode, summary: message, running: false)
        }
    }

    func agentApply(_ edit: AgentEditRequest) async throws -> AgentEditOutcome {
        guard edit.version == agentRevision else { return .stale(version: agentRevision) }
        if AgentService.shared.askBeforeApplying {
            guard await askToApply(edit) else { return .declined }
            // The user may have typed while deciding.
            guard edit.version == agentRevision else { return .stale(version: agentRevision) }
        }
        if edit.edits.isEmpty { return .applied(version: agentRevision) }
        let editor = model.editor
        // A load or a resynchronisation in progress settles within a
        // moment; until then the copy's revision is not the editor's.
        try await waitForEditor { editor.isReady && editor.version != nil }
        guard let expected = editor.version, edit.version == agentRevision else {
            return .stale(version: agentRevision)
        }
        let edits: [[String: Any]] = edit.edits.map {
            [
                "from": [Int($0.from.line), Int($0.from.character)],
                "to": [Int($0.to.line), Int($0.to.character)],
                "insert": $0.insert,
            ]
        }
        let json = String(decoding: try JSONSerialization.data(withJSONObject: edits), as: UTF8.self)
        let answer: [String: Any]
        do {
            answer =
                try await editor.call(
                    "return NeoSCADEditor.agentEdit(JSON.parse(edits), 8000, expect)",
                    ["edits": json, "expect": expected]) as? [String: Any] ?? [:]
        } catch {
            throw AgentHostError.Refused(message: "The editor refused the edit: \(error.localizedDescription)")
        }
        // The editor reports the change (or the keystroke that made the
        // edit stale) as an ordinary change message, which moves the
        // revision on; wait for it so the answer carries the new number.
        if let landed = answer["version"] as? Int {
            try? await waitForEditor(seconds: 3) { editor.version == landed }
        }
        if answer["stale"] as? Bool == true { return .stale(version: agentRevision) }
        return .applied(version: agentRevision)
    }

    /// Wait (without blocking the main thread) until `condition` holds.
    private func waitForEditor(seconds: Double = 5, _ condition: () -> Bool) async throws {
        let deadline = Date().addingTimeInterval(seconds)
        while !condition() {
            if isClosed { throw AgentHostError.Refused(message: "That document was closed in NeoSCAD.") }
            if Date() > deadline {
                throw AgentHostError.Refused(message: "NeoSCAD's editor is not ready; try again.")
            }
            try await Task.sleep(for: .milliseconds(10))
        }
    }

    /// Show the approval bar and wait for the user's answer. A newer edit
    /// replaces an unanswered one, which counts as rejected; closing the
    /// window or turning agents off rejects it too.
    private func askToApply(_ edit: AgentEditRequest) async -> Bool {
        model.agentApproval?.resolve(false)
        NSApp.requestUserAttention(.informationalRequest)
        return await withTaskCancellationHandler {
            await withCheckedContinuation { (done: CheckedContinuation<Bool, Never>) in
                let approval = AgentApproval(client: edit.client, summary: edit.summary) {
                    [weak self] apply in
                    done.resume(returning: apply)
                    self?.model.agentApproval = nil
                }
                model.agentApproval = approval
            }
        } onCancel: {
            Task { @MainActor [weak self] in self?.model.agentApproval?.resolve(false) }
        }
    }

    func agentReveal(from: EditorPosition, to: EditorPosition) {
        model.editor.reveal(
            line: Int(from.line), character: Int(from.character),
            endLine: Int(to.line), endCharacter: Int(to.character))
    }

    private var agentViewport: Viewport {
        get throws {
            guard let v = model.viewport.viewport else {
                throw AgentHostError.Refused(
                    message: model.viewport.error ?? "This window has no 3D view.")
            }
            return v
        }
    }

    func agentCamera(_ change: AgentCameraChange) throws -> AgentCamera {
        let camera = try agentViewport.applyAgentCamera(change: change)
        model.viewport.view?.requestFrame()
        return camera
    }

    /// The view to capture, once a preview that is due (the agent's own
    /// edit schedules one) or running has finished, so the picture shows
    /// the text the agent just wrote.
    func agentViewForCapture() async throws -> Viewport {
        let viewport = try agentViewport
        // Bounded: a user typing all the while keeps rescheduling.
        for _ in 0..<20 {
            if let pending = pendingPreview {
                await pending.value
            } else if case .running = model.report, let task = renderTask {
                await task.value
            } else {
                break
            }
            if isClosed { break }
        }
        return viewport
    }

    func agentAnnotate(lines: [ViewLine], markers: [ViewMarker]) throws {
        try agentViewport.setAgentAnnotations(lines: lines, markers: markers)
        model.agentMarkCount = lines.count + markers.count
        model.viewport.view?.requestFrame()
    }

    /// The view's "Clear" on the agent's marks.
    func clearAgentMarks() {
        try? model.viewport.viewport?.setAgentAnnotations(lines: [], markers: [])
        model.agentMarkCount = 0
        model.viewport.view?.requestFrame()
    }
}
