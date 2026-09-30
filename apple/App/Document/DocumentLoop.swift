// The document loop (docs/audits/macos-prep.md, 8f): after each pause in
// typing, each customizer edit and each change on disk to a file the model
// read, the document runs once, and that one run feeds everything the
// window shows:
//
//   edit -> (pause, the loop's delay) -> Core.runDocument:
//     evaluate -> the diagnostics to the editor's language server, which
//        publishes them as the markers (for the page's version with this
//        text), before any geometry is built
//     build -> the geometry stage's warnings to the markers too, if any
//           -> the model into the 3D view (unless a newer run started)
//     -> the console's lines, the customizer's parameters, the watched files
//
// Until 8f each pause ran a preview for the view and a separate evaluation
// for the markers; now there is one evaluation, and the markers also carry
// the geometry stage's warnings. A newer run supersedes an older one at
// every stage: the waiting one is cancelled here, the running one by the
// session (the core cancels a document's older requests when a new one
// starts), a finished-but-stale upload is skipped by the core, and a stale
// result is not shown here.
//
// When to run, which run is current and what the customizer has edited is
// the core's `DocumentController` (`crates/client/src/document_loop.rs`,
// shared with the Linux and Windows apps); this file keeps the timer and
// makes the core calls the controller's plans ask for.
//
// Memory. A run's evaluation and geometry can allocate hundreds of
// megabytes, all freed when it ends; the app's Info.plist turns off the
// allocator's cache of freed large blocks so that they go back to the
// system (apple/project.yml, `LSEnvironment`).

import AppKit
import NeoSCADCore

extension SCADDocument {
    /// A run once typing pauses; each keystroke restarts the wait.
    func schedulePreview() {
        guard !isClosed else { return }
        try? loop.schedule(nowMs: Self.nowMs())
        armTimer()
    }

    /// The host's one timer: sleep until the loop's next due run, then run
    /// what is due (or sleep again if the wait was restarted meanwhile).
    func armTimer() {
        pendingPreview?.cancel()
        guard let due = (try? loop.nextDueMs()) ?? nil else {
            pendingPreview = nil
            return
        }
        let now = Self.nowMs()
        let wait = due > now ? due - now : 0
        pendingPreview = Task { @MainActor [weak self] in
            try? await Task.sleep(for: .milliseconds(wait))
            guard !Task.isCancelled, let self else { return }
            if let mode = (try? self.loop.due(nowMs: Self.nowMs())) ?? nil {
                self.run(mode)
            } else {
                self.armTimer()
            }
        }
    }

    /// Run the document once (see the file's documentation): the text as
    /// it is now, with the customizer's values, in `mode`.
    func run(_ mode: RenderMode) {
        guard !isClosed else { return }
        pendingPreview?.cancel()
        pendingPreview = nil
        let engine: Engine
        switch CoreService.shared {
        case .success(let e): engine = e
        case .failure(let e):
            model.report = .failed("The core did not start: \(e.message)")
            return
        }
        let path = fileURL?.path ?? untitledPath
        guard let plan = (try? loop.beginRun(mode: mode, path: path)) ?? nil else { return }
        if let old = plan.close {
            // Saved under a new name: the old buffer would shadow the file
            // that is still on disk at the old path.
            _ = try? engine.close(old)
        }
        if plan.sendText {
            do {
                try engine.update(path, text: model.text)
                try loop.textSent()
            } catch let e as CoreError {
                model.report = .failed(e.message)
                return
            } catch {
                model.report = .failed("\(error)")
                return
            }
        }
        // The page's language client sends this text now, so the run's
        // markers find the version they belong to without waiting for the
        // client's own pause.
        model.editor.syncLanguage()
        renderTask?.cancel()
        model.report = .running(mode)
        let model = self.model
        let viewport = model.viewport
        let language = languageServer
        let request = plan.request
        let loop = self.loop
        renderTask = Task { @MainActor [weak self] in
            do {
                // The evaluation's markers arrive before the geometry is
                // built (a large preview can take seconds more).
                let early: @Sendable ([String]) -> Void = { [weak client = model.editor.languageClient] messages in
                    DispatchQueue.main.async {
                        MainActor.assumeIsolated { client?.deliverPublications(messages) }
                    }
                }
                let r = try await engine.runDocument(
                    path, request: request, viewport: viewport.viewport, language: language,
                    early: early)
                guard !Task.isCancelled, (try? loop.isCurrent(generation: plan.generation)) == true
                else { return }
                if r.shown { viewport.view?.requestFrame() }
                model.editor.languageClient?.deliverPublications(r.language)
                model.console = r.console
                model.report = .rendered(r.render, mode)
                self?.watcher.watch(r.files)
                self?.refreshParameters()
                // Auto check follows renders, not previews (Panels/
                // CheckPanel.swift says why).
                if mode == .render, model.check.auto { self?.runCheck() }
            } catch CoreError.Cancelled {
                // A newer run took over; it reports instead.
            } catch let e as CoreError {
                model.report = .failed(e.message)
            } catch {
                model.report = .failed("\(error)")
            }
        }
    }

    /// A file the last run read changed on disk (an include saved by
    /// another editor, a regenerated import): run the last mode again.
    func filesChanged() {
        guard !isClosed else { return }
        if let mode = (try? loop.filesChanged(nowMs: Self.nowMs())) ?? nil { run(mode) } else { armTimer() }
    }

    /// The customizer's values as the core takes them, in a stable order.
    var overrides: [ParameterOverride] { (try? loop.overrides()) ?? [] }

    // MARK: The customizer

    /// Read the parameters of the current text (after each run: the text
    /// is the run's), dropping edited values of parameters that are gone.
    func refreshParameters() {
        guard !isClosed, let path = corePath, case .success(let engine) = CoreService.shared
        else { return }
        parameterTask?.cancel()
        parameterTask = Task { @MainActor [weak self] in
            guard let groups = try? await engine.parameters(path), !Task.isCancelled,
                let self, !self.isClosed
            else { return }
            if self.model.parameterGroups != groups { self.model.parameterGroups = groups }
            // Values of parameters that are gone are dropped by the loop,
            // whose observer updates the model.
            _ = try? self.loop.parametersRead(groups: groups)
        }
    }

    /// The parameter sets file of this document: OpenSCAD's, `name.json`
    /// beside `name.scad` (`ParameterWidget::getJsonFile`).
    var parameterSetsURL: URL? {
        guard let path = fileURL?.path, let json = try? parameterSetPath(docPath: path) else { return nil }
        return URL(fileURLWithPath: json)
    }

    func refreshParameterSets() {
        model.parameterSetsAvailable = parameterSetsURL != nil
        guard let url = parameterSetsURL, case .success(let engine) = CoreService.shared else {
            model.parameterSets = []
            return
        }
        model.parameterSets = (try? engine.core.parameterSets(jsonPath: url.path)) ?? []
    }

    /// Wire the panels' actions to this document.
    func connectPanels() {
        model.actions = DocumentActions(
            jump: { [weak self] range in self?.jump(to: range) },
            setParameter: { [weak self] name, value in self?.setParameter(name, value) },
            resetParameters: { [weak self] in self?.resetParameters() },
            applyParameterSet: { [weak self] name in self?.applyParameterSet(name) },
            saveParameterSet: { [weak self] in self?.saveParameterSet() },
            setParts: { [weak self] on in self?.setParts(on) },
            runCheck: { [weak self] in self?.runCheck() },
            selectFinding: { [weak self] id in self?.selectFinding(id) },
            runMeasure: { [weak self] in self?.runMeasure() },
            measureBetween: { [weak self] in self?.measureBetween() },
            updateSection: { [weak self] in self?.updateSection() },
            clearPicks: { [weak self] in self?.clearPicks() })
        model.viewport.onClick = { [weak self] point in self?.pick(at: point) ?? false }
    }

    func setParameter(_ name: String, _ value: ParameterValue?) {
        if (try? loop.setParameter(name: name, value: value, nowMs: Self.nowMs())) == true { armTimer() }
    }

    func resetParameters() {
        if (try? loop.resetParameters(nowMs: Self.nowMs())) == true { armTimer() }
    }

    /// Apply a set as OpenSCAD's `-p file -P name` does (values checked
    /// and clamped, unnamed parameters back to the text's).
    func applyParameterSet(_ name: String) {
        guard let url = parameterSetsURL, let path = corePath,
            case .success(let engine) = CoreService.shared
        else { return }
        do {
            let values = try engine.core.applyParameterSet(
                path: path, jsonPath: url.path, name: name)
            try loop.parameterSetApplied(
                name: name, values: values, groups: model.parameterGroups, nowMs: Self.nowMs())
            armTimer()
        } catch let e as CoreError {
            model.report = .failed(e.message)
        } catch {}
    }

    /// Ask for a name and save the current values as that set.
    func saveParameterSet() {
        guard let url = parameterSetsURL else { return }
        let alert = NSAlert()
        alert.messageText = "Save Parameter Set"
        alert.informativeText = "The set is saved in \(url.lastPathComponent), next to the model."
        let field = NSTextField(frame: NSRect(x: 0, y: 0, width: 240, height: 24))
        field.stringValue = model.selectedParameterSet ?? "Set \(model.parameterSets.count + 1)"
        alert.accessoryView = field
        alert.addButton(withTitle: "Save")
        alert.addButton(withTitle: "Cancel")
        let finish: (NSApplication.ModalResponse) -> Void = { [weak self] response in
            guard response == .alertFirstButtonReturn else { return }
            let name = field.stringValue.trimmingCharacters(in: .whitespaces)
            guard !name.isEmpty else { return }
            self?.saveParameterSet(named: name)
        }
        if let window = windowControllers.first?.window {
            alert.beginSheetModal(for: window, completionHandler: finish)
        } else {
            finish(alert.runModal())
        }
    }

    func saveParameterSet(named name: String) {
        guard let url = parameterSetsURL, let path = corePath,
            case .success(let engine) = CoreService.shared
        else { return }
        do {
            try engine.core.saveParameterSet(
                path: path, jsonPath: url.path, name: name,
                values: model.parameterValues.map { ParameterOverride(name: $0.key, value: $0.value) })
            refreshParameterSets()
            model.selectedParameterSet = name
        } catch let e as CoreError {
            model.report = .failed(e.message)
        } catch {}
    }

    // MARK: The console

    /// Show a console line's span: in this document's editor when it is
    /// this document, otherwise in the file's own window (a document, or a
    /// read-only viewer for a library).
    func jump(to range: SourceRange) {
        if range.path == corePath {
            model.editor.reveal(
                line: Int(range.startLine), character: Int(range.startCharacter),
                endLine: Int(range.endLine), endCharacter: Int(range.endCharacter))
            windowControllers.first?.window?.makeFirstResponder(model.editor.webView)
        } else {
            LocationOpener.open(
                uri: URL(fileURLWithPath: range.path).absoluteString,
                line: Int(range.startLine), character: Int(range.startCharacter),
                from: windowControllers.first?.window)
        }
    }
}
