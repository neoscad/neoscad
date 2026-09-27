// The document loop (docs/audits/macos-prep.md, 8f): after each pause in
// typing, each customizer edit and each change on disk to a file the model
// read, the document runs once, and that one run feeds everything the
// window shows:
//
//   edit -> (pause, `previewDelay`) -> Core.runDocument:
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
        pendingPreview?.cancel()
        pendingPreview = Task { @MainActor [weak self] in
            try? await Task.sleep(for: Self.previewDelay)
            guard !Task.isCancelled else { return }
            self?.run(.preview)
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
        if let old = corePath, old != path {
            // Saved under a new name: the old buffer would shadow the file
            // that is still on disk at the old path.
            _ = try? engine.close(old)
            coreInSync = false
        }
        corePath = path
        if !coreInSync {
            do {
                try engine.update(path, text: model.text)
                coreInSync = true
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
        requestCount += 1
        lastMode = mode
        model.report = .running(mode)
        let model = self.model
        let viewport = model.viewport
        let language = languageServer
        let request = DocumentRequest(
            mode: mode,
            overrides: model.parameterValues.map { ParameterOverride(name: $0.key, value: $0.value) }
                .sorted { $0.name < $1.name })
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
                guard !Task.isCancelled else { return }
                if r.shown { viewport.view?.requestFrame() }
                model.editor.languageClient?.deliverPublications(r.language)
                model.console = r.console
                model.report = .rendered(r.render, mode)
                self?.watcher.watch(r.files)
                self?.refreshParameters()
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
        if lastMode == .render { run(.render) } else { schedulePreview() }
    }

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
            let names = Set(groups.flatMap { $0.parameters.map(\.name) })
            if self.model.parameterGroups != groups { self.model.parameterGroups = groups }
            let kept = self.model.parameterValues.filter { names.contains($0.key) }
            if kept.count != self.model.parameterValues.count { self.model.parameterValues = kept }
        }
    }

    /// The parameter sets file of this document: OpenSCAD's, `name.json`
    /// beside `name.scad` (`ParameterWidget::getJsonFile`).
    var parameterSetsURL: URL? {
        fileURL?.deletingPathExtension().appendingPathExtension("json")
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
            saveParameterSet: { [weak self] in self?.saveParameterSet() })
    }

    func setParameter(_ name: String, _ value: ParameterValue?) {
        if let value {
            guard model.parameterValues[name] != value else { return }
            model.parameterValues[name] = value
        } else {
            guard model.parameterValues.removeValue(forKey: name) != nil else { return }
        }
        model.selectedParameterSet = nil
        schedulePreview()
    }

    func resetParameters() {
        model.selectedParameterSet = nil
        guard !model.parameterValues.isEmpty else { return }
        model.parameterValues = [:]
        schedulePreview()
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
            let defaults = Dictionary(
                model.parameterGroups.flatMap(\.parameters).map { ($0.name, $0.defaultValue) },
                uniquingKeysWith: { a, _ in a })
            var edited: [String: ParameterValue] = [:]
            for v in values where defaults[v.name] != v.value { edited[v.name] = v.value }
            model.parameterValues = edited
            model.selectedParameterSet = name
            schedulePreview()
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
