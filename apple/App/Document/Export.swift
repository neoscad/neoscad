// File > Export: the document's model to a mesh or drawing file (STL,
// 3MF, OBJ, OFF, SVG, DXF, PDF), an image of the 3D view as it is, or a
// contact sheet of standard views (`neoscad snapshot`).
//
// A save panel with the format in a popup and the format's options below
// it (3MF's colours; the image's size). The export renders the model's
// current text with the customizer's values and the parts toggle, detached
// from the document's runs (crates/ffi/src/inspect.rs), so it survives
// typing. It shows its stage in a sheet with a Cancel button.
//
// No silent failures (docs/audits/agent-surface.md, finding 5): an export
// that writes nothing (a 2D model to a 3D format, an empty model, a folder
// that cannot be written) ends in an alert with the core's reason, never
// in nothing. `ExportOutcome` is that decision as data, so the tests can
// check it without a person to read the alert.

import AppKit
import NeoSCADCore
import SwiftUI
import UniformTypeIdentifiers

/// What File > Export can write.
enum ExportFormat: String, CaseIterable, Identifiable {
    case binaryStl, asciiStl, threeMF, obj, off, svg, dxf, pdf, viewImage, snapshot

    var id: String { rawValue }

    var title: String {
        switch self {
        case .binaryStl: "STL (binary)"
        case .asciiStl: "STL (ASCII)"
        case .threeMF: "3MF"
        case .obj: "OBJ"
        case .off: "OFF"
        case .svg: "SVG"
        case .dxf: "DXF"
        case .pdf: "PDF"
        case .viewImage: "PNG image of the view"
        case .snapshot: "PNG snapshot sheet"
        }
    }

    /// The core's format id (`--export-format`); `nil` for the images.
    var coreID: String? {
        switch self {
        case .binaryStl: "binstl"
        case .asciiStl: "stl"
        case .threeMF: "3mf"
        case .obj: "obj"
        case .off: "off"
        case .svg: "svg"
        case .dxf: "dxf"
        case .pdf: "pdf"
        case .viewImage, .snapshot: nil
        }
    }

    var fileExtension: String {
        switch self {
        case .binaryStl, .asciiStl: "stl"
        case .threeMF: "3mf"
        case .obj: "obj"
        case .off: "off"
        case .svg: "svg"
        case .dxf: "dxf"
        case .pdf: "pdf"
        case .viewImage, .snapshot: "png"
        }
    }

    var contentType: UTType {
        UTType(filenameExtension: fileExtension) ?? .data
    }

    /// The model's dimension the format needs (`nil`: any, for images).
    var dimension: Int? {
        switch self {
        case .svg, .dxf, .pdf: 2
        case .viewImage, .snapshot: nil
        default: 3
        }
    }
}

/// The export's settings, as the panel's accessory edits them; the app
/// remembers them between exports.
struct ExportSettings: Equatable {
    var format: ExportFormat = .binaryStl
    var threeMFColorMode: ThreeMfColorMode = .model
    /// `#rrggbb`, for one colour on everything.
    var threeMFColor = "#f9d72c"
    var threeMFMaterial: ThreeMfMaterial = .baseMaterial
    var imageWidth: UInt32 = 1600
    var imageHeight: UInt32 = 1200

    var options: ExportOptions {
        ExportOptions(
            format: format.coreID,
            threemfColorMode: format == .threeMF ? threeMFColorMode : nil,
            threemfColor: format == .threeMF && threeMFColorMode == .selectedOnly ? threeMFColor : nil,
            threemfMaterial: format == .threeMF ? threeMFMaterial : nil)
    }

    static func load(_ d: UserDefaults = .standard) -> ExportSettings {
        var s = ExportSettings()
        if let f = d.string(forKey: "export.format").flatMap(ExportFormat.init) { s.format = f }
        switch d.string(forKey: "export.3mf.colorMode") {
        case "none": s.threeMFColorMode = .noColor
        case "selected-only": s.threeMFColorMode = .selectedOnly
        default: break
        }
        if let c = d.string(forKey: "export.3mf.color") { s.threeMFColor = c }
        if d.string(forKey: "export.3mf.material") == "color" { s.threeMFMaterial = .color }
        let size = { (k: String) in (d.object(forKey: k) as? Int).flatMap { UInt32(exactly: $0) } }
        if let w = size("export.image.width"), (16...8192).contains(w) { s.imageWidth = w }
        if let h = size("export.image.height"), (16...8192).contains(h) { s.imageHeight = h }
        return s
    }

    func save(_ d: UserDefaults = .standard) {
        d.set(format.rawValue, forKey: "export.format")
        d.set(
            threeMFColorMode == .noColor
                ? "none" : threeMFColorMode == .selectedOnly ? "selected-only" : "model",
            forKey: "export.3mf.colorMode")
        d.set(threeMFColor, forKey: "export.3mf.color")
        d.set(threeMFMaterial == .color ? "color" : "basematerial", forKey: "export.3mf.material")
        d.set(Int(imageWidth), forKey: "export.image.width")
        d.set(Int(imageHeight), forKey: "export.image.height")
    }
}

/// An alert's text: what failed and why.
struct ExportAlert: Equatable {
    var title: String
    var message: String
}

/// How an export ended.
enum ExportOutcome: Equatable {
    case written(URL, bytes: UInt64)
    case cancelled
    case failed(ExportAlert)

    /// The core's result for a file export: written, or failed with the
    /// console's error lines (every line when there is no `ERROR:` line,
    /// such as "Current top level object is not a 3D object.").
    static func of(_ r: ExportResult, to url: URL) -> ExportOutcome {
        if r.exitCode == 0 { return .written(url, bytes: r.bytes) }
        let lines = r.console.split(separator: "\n").map(String.init)
        let errors = lines.filter { $0.hasPrefix("ERROR") }
        let reason = (errors.isEmpty ? lines.filter { !$0.hasPrefix("WARNING") } : errors)
            .suffix(6).joined(separator: "\n")
        return .failed(
            ExportAlert(
                title: "“\(url.lastPathComponent)” was not exported",
                message: reason.isEmpty ? "The export failed (exit code \(r.exitCode))." : reason))
    }

    static func of(_ error: Error, to url: URL) -> ExportOutcome {
        if case CoreError.Cancelled = error { return .cancelled }
        if error is CancellationError { return .cancelled }
        return .failed(
            ExportAlert(title: "“\(url.lastPathComponent)” was not exported", message: describe(error)))
    }
}

extension SCADDocument {
    /// The format the panel starts on: the last one used, unless the last
    /// render was 2D and it is a 3D format (or the other way round).
    func initialExportSettings() -> ExportSettings {
        var s = ExportSettings.load()
        if case .rendered(let r, _) = model.report, let g = r.geometry,
            let want = s.format.dimension, Int(g.dimensions) != want
        {
            s.format = g.dimensions == 2 ? .svg : .binaryStl
        }
        return s
    }

    /// File > Export…
    @objc func exportDocument(_ sender: Any?) {
        guard let window = windowControllers.first?.window else { return }
        let state = ExportPanelState(settings: initialExportSettings())
        let panel = NSSavePanel()
        panel.title = "Export"
        panel.prompt = "Export"
        panel.canCreateDirectories = true
        panel.isExtensionHidden = false
        let base = (fileURL?.deletingPathExtension().lastPathComponent)
            ?? URL(fileURLWithPath: untitledPath).deletingPathExtension().lastPathComponent
        func follow(_ f: ExportFormat) {
            panel.allowedContentTypes = [f.contentType]
            let name = (panel.nameFieldStringValue as NSString).deletingPathExtension
            panel.nameFieldStringValue = (name.isEmpty ? base : name) + "." + f.fileExtension
        }
        panel.nameFieldStringValue = base
        if let dir = fileURL?.deletingLastPathComponent() { panel.directoryURL = dir }
        follow(state.settings.format)
        state.formatChanged = { follow($0) }
        let accessory = NSHostingView(rootView: ExportAccessory(state: state))
        accessory.frame.size = accessory.fittingSize
        panel.accessoryView = accessory
        panel.beginSheetModal(for: window) { [weak self] response in
            guard response == .OK, let url = panel.url else { return }
            state.settings.save()
            self?.export(to: url, settings: state.settings)
        }
    }

    /// Export with a progress sheet, then an alert if it failed.
    func export(to url: URL, settings: ExportSettings) {
        exportTask?.cancel()
        let progress = ExportProgress()
        let sheet = progressSheet(progress)
        exportTask = Task { @MainActor [weak self] in
            guard let self else { return }
            let outcome = await self.performExport(to: url, settings: settings) { stage in
                DispatchQueue.main.async { MainActor.assumeIsolated { progress.stage = stage } }
            }
            if let sheet { sheet.sheetParent?.endSheet(sheet) }
            if case .failed(let a) = outcome { self.present(a) }
        }
        progress.cancel = { [weak self] in self?.exportTask?.cancel() }
    }

    /// Write `url` as `settings` ask: the export itself, without any UI
    /// (the tests call this).
    func performExport(
        to url: URL, settings: ExportSettings, stage: (@Sendable (String) -> Void)? = nil
    ) async -> ExportOutcome {
        do {
            switch settings.format {
            case .viewImage:
                guard let viewport = model.viewport.viewport else {
                    return .failed(
                        ExportAlert(
                            title: "“\(url.lastPathComponent)” was not exported",
                            message: model.viewport.error ?? "There is no 3D view."))
                }
                let (w, h) = (settings.imageWidth, settings.imageHeight)
                let png = try await Task.detached { try viewport.image(width: w, height: h) }.value
                try Data(png).write(to: url, options: .atomic)
                return .written(url, bytes: UInt64(png.count))
            case .snapshot:
                let (engine, path) = try panelPath()
                stage?("draw")
                let r = try await engine.snapshotFile(
                    path, options: SnapshotOptions(width: settings.imageWidth, height: settings.imageHeight),
                    run: runOptions)
                guard r.exitCode == 0, let png = r.png else {
                    let reason = firstError(r.console) ?? r.console
                    return .failed(
                        ExportAlert(
                            title: "“\(url.lastPathComponent)” was not exported",
                            message: reason.isEmpty ? "The model did not render." : reason))
                }
                try Data(png).write(to: url, options: .atomic)
                return .written(url, bytes: UInt64(png.count))
            default:
                let (engine, path) = try panelPath()
                let r = try await engine.exportFile(
                    path, to: url.path, options: settings.options, run: runOptions, stage: stage)
                return .of(r, to: url)
            }
        } catch {
            return .of(error, to: url)
        }
    }

    private func present(_ a: ExportAlert) {
        let alert = NSAlert()
        alert.alertStyle = .warning
        alert.messageText = a.title
        alert.informativeText = a.message
        if let window = windowControllers.first?.window {
            alert.beginSheetModal(for: window)
        } else {
            alert.runModal()
        }
    }

    /// A sheet with the export's stage and a Cancel button.
    private func progressSheet(_ progress: ExportProgress) -> NSWindow? {
        guard let window = windowControllers.first?.window else { return nil }
        let sheet = NSWindow(contentViewController: NSHostingController(rootView: ExportProgressView(progress: progress)))
        sheet.styleMask = [.titled]
        window.beginSheet(sheet)
        return sheet
    }
}

/// What the save panel's accessory edits.
@MainActor
@Observable
final class ExportPanelState {
    var settings: ExportSettings {
        didSet { if settings.format != oldValue.format { formatChanged?(settings.format) } }
    }
    @ObservationIgnored var formatChanged: ((ExportFormat) -> Void)?

    init(settings: ExportSettings) { self.settings = settings }
}

private struct ExportAccessory: View {
    let state: ExportPanelState

    var body: some View {
        let s = Binding(get: { state.settings }, set: { state.settings = $0 })
        Form {
            Picker("Format:", selection: s.format) {
                ForEach(ExportFormat.allCases) { f in Text(f.title).tag(f) }
            }
            switch state.settings.format {
            case .threeMF:
                Picker("Colours:", selection: s.threeMFColorMode) {
                    Text("From the model").tag(ThreeMfColorMode.model)
                    Text("One colour").tag(ThreeMfColorMode.selectedOnly)
                    Text("None").tag(ThreeMfColorMode.noColor)
                }
                if state.settings.threeMFColorMode == .selectedOnly {
                    ColorPicker("Colour:", selection: colorBinding(s.threeMFColor), supportsOpacity: false)
                }
                if state.settings.threeMFColorMode != .noColor {
                    Picker("Stored as:", selection: s.threeMFMaterial) {
                        Text("Base materials").tag(ThreeMfMaterial.baseMaterial)
                        Text("Colour group").tag(ThreeMfMaterial.color)
                    }
                }
            case .viewImage, .snapshot:
                HStack {
                    TextField("Size:", value: s.imageWidth, format: .number.grouping(.never))
                        .frame(width: 120)
                    Text("×")
                    TextField("", value: s.imageHeight, format: .number.grouping(.never))
                        .frame(width: 60)
                        .labelsHidden()
                    Text("pixels")
                }
            default:
                EmptyView()
            }
        }
        .padding(12)
        .frame(width: 380)
    }

    /// `#rrggbb` as a colour for the picker, and back.
    private func colorBinding(_ hex: Binding<String>) -> Binding<Color> {
        Binding(
            get: {
                let v = UInt32(hex.wrappedValue.dropFirst(), radix: 16) ?? 0xf9d72c
                return Color(
                    red: Double(v >> 16 & 0xff) / 255, green: Double(v >> 8 & 0xff) / 255,
                    blue: Double(v & 0xff) / 255)
            },
            set: { c in
                let n = NSColor(c).usingColorSpace(.sRGB) ?? .black
                let b = { (x: CGFloat) in Int((x * 255).rounded()).clamped(0, 255) }
                hex.wrappedValue = String(
                    format: "#%02x%02x%02x", b(n.redComponent), b(n.greenComponent), b(n.blueComponent))
            })
    }
}

extension Int {
    fileprivate func clamped(_ lo: Int, _ hi: Int) -> Int { Swift.min(Swift.max(self, lo), hi) }
}

/// An export's progress, for its sheet.
@MainActor
@Observable
final class ExportProgress {
    var stage = "parse"
    @ObservationIgnored var cancel: () -> Void = {}
}

private struct ExportProgressView: View {
    let progress: ExportProgress

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            Text("Exporting…").font(.headline)
            ProgressView().progressViewStyle(.linear)
            HStack {
                Text(label).font(.callout).foregroundStyle(.secondary)
                Spacer()
                Button("Cancel") { progress.cancel() }.keyboardShortcut(.cancelAction)
            }
        }
        .padding(16)
        .frame(width: 320)
    }

    private var label: String {
        switch progress.stage {
        case "parse": "Reading the files"
        case "evaluate": "Evaluating"
        case "geometry": "Building the geometry and writing"
        case "draw": "Drawing"
        default: progress.stage
        }
    }
}
