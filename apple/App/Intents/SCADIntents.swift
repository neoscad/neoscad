// Shortcuts actions (App Intents): render a `.scad` file to a mesh or
// drawing file, take a snapshot sheet of it, and check it for printing.
// Each wraps one core call (docs/audits/macos-prep.md, "App Intents"),
// run by the app's one core, under the agent resource limits the core
// starts with (`defaultLimits()`): a shortcut runs files nobody is
// watching, so a runaway model must stop rather than take the machine.
//
// The work is in `IntentRunner`, apart from the intents' declarations,
// so the tests run it without Shortcuts. Input files are read where they
// are (their includes resolve beside them); a file Shortcuts hands over
// as data only (no URL) is written to a temporary folder first, and then
// relative includes cannot resolve. Outputs go to a temporary folder of
// the app's and are returned as files for the next action.
//
// The file parameters accept plain text, which `org.openscad.scad`
// conforms to: the metadata processor refuses a type it cannot resolve at
// build time ("Could not determine the identifier ... please use a UTType
// defined by UniformTypeIdentifiers.framework"), and the app's own
// imported type is one.
//
// `@IntentParameter` (what `@Parameter` names) is spelled out because NeoSCADCore has a
// `Parameter` of its own (the customizer's), which the bare attribute
// would find first.

import AppIntents
import Foundation
import NeoSCADCore
import UniformTypeIdentifiers

/// The formats a shortcut can render to.
enum IntentExportFormat: String, AppEnum {
    case stl, binstl, threemf, obj, off, svg, dxf, pdf

    static let typeDisplayRepresentation: TypeDisplayRepresentation = "Export Format"
    static let caseDisplayRepresentations: [IntentExportFormat: DisplayRepresentation] = [
        .stl: "STL (ASCII)",
        .binstl: "STL (binary)",
        .threemf: "3MF",
        .obj: "OBJ",
        .off: "OFF",
        .svg: "SVG",
        .dxf: "DXF",
        .pdf: "PDF",
    ]

    var coreID: String { self == .threemf ? "3mf" : rawValue }
    var fileExtension: String {
        switch self {
        case .stl, .binstl: "stl"
        case .threemf: "3mf"
        default: rawValue
        }
    }
}

/// Why an intent failed, as Shortcuts shows it.
struct IntentFailure: Error, CustomLocalizedStringResourceConvertible, Equatable {
    let message: String
    var localizedStringResource: LocalizedStringResource { "\(message)" }
}

/// The intents' work, without Shortcuts.
enum IntentRunner {
    static func engine() throws -> Engine {
        switch CoreService.shared {
        case .success(let e): return e
        case .failure(let e): throw IntentFailure(message: "The NeoSCAD core did not start: \(e.message)")
        }
    }

    /// A fresh folder for one intent's output.
    static func outputFolder() throws -> URL {
        let dir = FileManager.default.temporaryDirectory
            .appendingPathComponent("NeoSCAD-Shortcuts", isDirectory: true)
            .appendingPathComponent(UUID().uuidString, isDirectory: true)
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        return dir
    }

    /// The model's path on disk: the file itself, or a copy of its data.
    static func modelPath(url: URL?, data: Data, filename: String) throws -> String {
        if let url, url.isFileURL, FileManager.default.fileExists(atPath: url.path) {
            return url.standardizedFileURL.path
        }
        let name = filename.isEmpty ? "model.scad" : filename
        let dest = try outputFolder().appendingPathComponent(name)
        try data.write(to: dest)
        return dest.path
    }

    /// The first error lines of a failed request, as the failure's text.
    static func failure(_ what: String, console: String) -> IntentFailure {
        let lines = console.split(separator: "\n").map(String.init)
        let errors = lines.filter { $0.hasPrefix("ERROR") || $0.contains("Parser error") }
        let reason = (errors.isEmpty ? lines.filter { !$0.hasPrefix("WARNING") } : errors)
            .prefix(4).joined(separator: "\n")
        return IntentFailure(message: reason.isEmpty ? "\(what) failed." : "\(what) failed: \(reason)")
    }

    /// Render `path` and write it as `format`; the written file.
    static func render(path: String, format: IntentExportFormat) async throws -> URL {
        let engine = try engine()
        let base = URL(fileURLWithPath: path).deletingPathExtension().lastPathComponent
        let out = try outputFolder().appendingPathComponent("\(base).\(format.fileExtension)")
        let r = try await engine.exportFile(
            path, to: out.path, options: ExportOptions(format: format.coreID))
        guard r.exitCode == 0 else { throw failure("Rendering \(base)", console: r.console) }
        return out
    }

    /// A contact sheet of `path` as PNG; the written file.
    static func snapshot(path: String, size: Int) async throws -> URL {
        let engine = try engine()
        let side = UInt32(min(max(size, 64), 4096))
        let base = URL(fileURLWithPath: path).deletingPathExtension().lastPathComponent
        let r = try await engine.snapshotFile(
            path, options: SnapshotOptions(width: side, height: side))
        guard r.exitCode == 0, let png = r.png else {
            throw failure("The snapshot of \(base)", console: r.console)
        }
        let out = try outputFolder().appendingPathComponent("\(base)-snapshot.png")
        try Data(png).write(to: out)
        return out
    }

    /// `neoscad check`'s report of `path`: its summary line and each
    /// finding with its fix.
    static func check(path: String, nozzle: Double?, bed: [Double]?) async throws -> (
        report: String, errors: Int
    ) {
        let engine = try engine()
        var options = try defaultCheckOptions()
        if let n = nozzle, n > 0 {
            options.nozzle = n
            options.minWall = 2 * n
        }
        options.bed = bed
        let r = try await engine.check(path, options: options)
        if r.failed {
            let base = URL(fileURLWithPath: path).lastPathComponent
            throw failure("Checking \(base)", console: r.console)
        }
        return (r.text.trimmingCharacters(in: .whitespacesAndNewlines), Int(r.errors))
    }
}

/// The input file's path, reading it where it is when Shortcuts gives a
/// URL (with its security scope held while the core reads it).
private func withModel<T>(_ file: IntentFile, _ body: (String) async throws -> T) async throws -> T {
    let url = file.fileURL
    let scoped = url?.startAccessingSecurityScopedResource() ?? false
    defer { if scoped { url?.stopAccessingSecurityScopedResource() } }
    let path = try IntentRunner.modelPath(url: url, data: file.data, filename: file.filename)
    return try await body(path)
}

private func rethrowing<T>(_ body: () async throws -> T) async throws -> T {
    do {
        return try await body()
    } catch let e as CoreError {
        throw IntentFailure(message: e.message)
    }
}

struct RenderSCADIntent: AppIntent {
    static let title: LocalizedStringResource = "Render OpenSCAD File"
    static let description = IntentDescription(
        "Renders an OpenSCAD (.scad) file and exports the model as STL, 3MF, OBJ, OFF, or as SVG, DXF or PDF for a 2D design.")

    @IntentParameter(title: "File", supportedContentTypes: [.plainText])
    var file: IntentFile

    @IntentParameter(title: "Format", default: .binstl)
    var format: IntentExportFormat

    static var parameterSummary: some ParameterSummary {
        Summary("Render \(\.$file) as \(\.$format)")
    }

    func perform() async throws -> some IntentResult & ReturnsValue<IntentFile> {
        let out = try await rethrowing {
            try await withModel(file) { try await IntentRunner.render(path: $0, format: format) }
        }
        return .result(value: IntentFile(fileURL: out, filename: out.lastPathComponent))
    }
}

struct SnapshotSCADIntent: AppIntent {
    static let title: LocalizedStringResource = "Snapshot OpenSCAD File"
    static let description = IntentDescription(
        "Renders an OpenSCAD (.scad) file and draws it from four standard views as a PNG image.")

    @IntentParameter(title: "File", supportedContentTypes: [.plainText])
    var file: IntentFile

    @IntentParameter(title: "Size", description: "The image's width and height in pixels.", default: 1024, inclusiveRange: (64, 4096))
    var size: Int

    static var parameterSummary: some ParameterSummary {
        Summary("Snapshot \(\.$file)") { \.$size }
    }

    func perform() async throws -> some IntentResult & ReturnsValue<IntentFile> {
        let out = try await rethrowing {
            try await withModel(file) { try await IntentRunner.snapshot(path: $0, size: size) }
        }
        return .result(value: IntentFile(fileURL: out, filename: out.lastPathComponent, type: .png))
    }
}

struct CheckSCADIntent: AppIntent {
    static let title: LocalizedStringResource = "Check OpenSCAD File for Printing"
    static let description = IntentDescription(
        "Checks an OpenSCAD (.scad) model for 3D printing: thin walls, overhangs, floating pieces and more. Returns the report.")

    @IntentParameter(title: "File", supportedContentTypes: [.plainText])
    var file: IntentFile

    @IntentParameter(title: "Nozzle (mm)", default: 0.4)
    var nozzle: Double

    static var parameterSummary: some ParameterSummary {
        Summary("Check \(\.$file)") { \.$nozzle }
    }

    func perform() async throws -> some IntentResult & ReturnsValue<String> & ProvidesDialog {
        let (report, _) = try await rethrowing {
            try await withModel(file) {
                try await IntentRunner.check(path: $0, nozzle: nozzle, bed: nil)
            }
        }
        let first = report.split(separator: "\n").first.map(String.init) ?? report
        return .result(value: report, dialog: "\(first)")
    }
}

/// The actions as Shortcuts lists them, with phrases for Siri and
/// Spotlight.
struct NeoSCADShortcuts: AppShortcutsProvider {
    static var appShortcuts: [AppShortcut] {
        AppShortcut(
            intent: RenderSCADIntent(),
            phrases: ["Render an OpenSCAD file with \(.applicationName)"],
            shortTitle: "Render SCAD",
            systemImageName: "cube")
        AppShortcut(
            intent: SnapshotSCADIntent(),
            phrases: ["Snapshot an OpenSCAD file with \(.applicationName)"],
            shortTitle: "Snapshot SCAD",
            systemImageName: "photo")
        AppShortcut(
            intent: CheckSCADIntent(),
            phrases: ["Check an OpenSCAD file with \(.applicationName)"],
            shortTitle: "Check SCAD",
            systemImageName: "checkmark.seal")
    }
}
