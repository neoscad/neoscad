// What the Quick Look extensions draw (docs/audits/macos-prep.md, 8h):
// one entry point, `QuickLookRender.render`, that the preview and the
// thumbnail extension both call, kept here in NeoSCADCore so the unit
// tests (apple/Tests) exercise it without a running extension.
//
// Finder calls the extensions unasked, for every .scad file a window
// shows, so a model must never be able to hang Finder or blow up the
// extension's memory:
//
// - Each request gets a core of its own, under `QuickLookRender.limits`
//   (5 s and 512 MiB of estimated memory). A fresh core also keeps a
//   long-lived extension process from accumulating geometry caches, and
//   two requests for the same file (a thumbnail at two sizes) cannot
//   cancel each other as two requests on one core's document would.
// - A watchdog answers at the deadline whatever the core is doing: some
//   steps (one large boolean) do not check for cancellation, and the reply
//   must not wait for them. The core is cancelled and finishes in the
//   background.
//
// The extensions are sandboxed: they may read the previewed file and
// nothing else. A relative `include`, `use` or `import()` of a sibling
// file then fails as if the file were missing, so the model renders
// without it and the result carries a note naming the files. The bundled
// MCAD and fonts are compiled into the core and still work.

import Foundation

/// The outcome of a Quick Look request.
public struct QuickLookResult: Sendable {
    /// The model's picture (an opaque PNG), or `nil` when there is none:
    /// the model failed, timed out or was too large to try.
    public var png: Data?
    /// The file's text, for the preview to show beneath the picture.
    public var source: String
    /// Short sentences for the user: files that could not be read, the
    /// first error, a timeout.
    public var notes: [String]
    /// Whether the deadline passed before the picture was ready.
    public var timedOut: Bool
    /// Files the model uses that could not be read, as the model names
    /// them (in the sandbox, usually its siblings).
    public var unreadable: [String]
}

public enum QuickLookRender {
    /// Tight limits (`agent-surface.md` finding 1 applies doubly: nobody
    /// asked for this render). The rest are the agent defaults.
    public static var limits: ResourceLimits {
        var l = (try? defaultLimits())
            ?? ResourceLimits(
                timeSeconds: nil, memoryBytes: nil, fragments: nil, slices: nil,
                list: nil, string: nil, rands: nil, triangles: nil)
        l.timeSeconds = 5
        l.memoryBytes = 512 << 20
        return l
    }

    /// How long a request may take in all, drawing included. A little over
    /// the core's own time limit, so the core normally stops itself and
    /// reports why, and the watchdog is the backstop.
    public static let deadline: TimeInterval = 6

    /// Files larger than this are shown as text only: parsing megabytes
    /// of generated code is not worth a Finder hiccup.
    public static let maxSourceBytes = 4 << 20

    /// Render the file at `url` as a `width` by `height` pixel picture.
    /// Never throws: every failure becomes a result without a picture and
    /// with a note saying why.
    public static func render(
        fileAt url: URL, width: UInt32, height: UInt32, mode: RenderMode = .preview,
        deadline: TimeInterval = deadline
    ) async -> QuickLookResult {
        let data: Data
        do {
            data = try Data(contentsOf: url)
        } catch {
            return QuickLookResult(
                png: nil, source: "", notes: ["Could not read the file: \(error.localizedDescription)"],
                timedOut: false, unreadable: [])
        }
        // OpenSCAD reads files as UTF-8. Invalid sequences become U+FFFD
        // rather than failing the whole preview: the text still shows, and
        // they can only sit in strings or comments of a model that parses.
        let text = String(decoding: data, as: UTF8.self)
        if data.count > maxSourceBytes {
            return QuickLookResult(
                png: nil, source: text,
                notes: ["This file is too large to render in Quick Look."],
                timedOut: false, unreadable: [])
        }
        return await render(
            path: url.standardizedFileURL.path, text: text, width: width, height: height,
            mode: mode, deadline: deadline)
    }

    /// Render `text` as the document at `path` (absolute; includes resolve
    /// against its directory).
    public static func render(
        path: String, text: String, width: UInt32, height: UInt32,
        mode: RenderMode = .preview, deadline: TimeInterval = deadline
    ) async -> QuickLookResult {
        let base = QuickLookResult(
            png: nil, source: text, notes: [], timedOut: false, unreadable: [])
        let core: Core
        do {
            core = try Core(config: CoreConfig(resourceDir: nil, testHooks: false))
            try core.setLimits(limits: limits)
            _ = try core.open(path: path, text: text)
        } catch {
            return with(base, note: message(error))
        }
        let options = PictureOptions(width: width, height: height, mode: mode)
        let outcome = await race(deadline: deadline) {
            Result { try core.picture(path: path, options: options) }
        } onTimeout: {
            _ = try? core.cancel(path: path)
        }
        switch outcome {
        case nil:
            var r = base
            r.timedOut = true
            r.notes.append(
                "The model took longer than Quick Look allows (\(Int(deadline)) s). "
                    + "Open it in NeoSCAD to render it.")
            return r
        case .failure(let error)?:
            return with(base, note: message(error))
        case .success(let p)?:
            var r = base
            r.png = p.png.map { Data($0) }
            r.unreadable = unreadableFiles(
                p.diagnostics, console: p.console,
                relativeTo: (path as NSString).deletingLastPathComponent)
            if !r.unreadable.isEmpty {
                // Missing and sandbox-blocked files fail alike, so the note
                // names both causes rather than guessing.
                r.notes.append(
                    "Skipped files Quick Look could not read: "
                        + r.unreadable.joined(separator: ", ")
                        + ". Quick Look can open only the previewed file; "
                        + "open it in NeoSCAD to see the whole model.")
            }
            if p.exitCode != 0 {
                let first = p.diagnostics.first { $0.severity == .error }
                if first?.code == "resource-limit" {
                    r.notes.append(
                        "The model is too large for Quick Look: \(first?.message ?? "a limit was reached"). "
                            + "Open it in NeoSCAD to render it.")
                } else {
                    r.notes.append(first.map(describe) ?? "The model has errors.")
                }
            } else if p.empty {
                r.notes.append("The model is empty.")
            }
            return r
        }
    }

    // MARK: Helpers

    /// Files a model could not read, from the loader's diagnostics
    /// (`include`, `use`) and the console lines of `import()` and library
    /// files, in order and without repeats. `import()` names its file by
    /// absolute path, as OpenSCAD does; one inside `directory` is shown
    /// relative to it, as the model wrote it.
    static func unreadableFiles(
        _ diagnostics: [Diagnostic], console: String, relativeTo directory: String
    ) -> [String] {
        var names: [String] = []
        let prefix = directory.hasSuffix("/") ? directory : directory + "/"
        func add(_ line: String) {
            guard var name = quoted(line) else { return }
            if name.hasPrefix(prefix) { name.removeFirst(prefix.count) }
            if !names.contains(name) { names.append(name) }
        }
        for d in diagnostics where d.code == "include-not-found" || d.code == "library-not-found" {
            add(d.message)
        }
        for line in console.split(separator: "\n") where line.contains("Can't open") {
            add(String(line))
        }
        return names
    }

    /// The first `'...'` in `s` after the apostrophe of "Can't", which
    /// every one of these messages starts with ("Can't find include file
    /// 'parts.scad'.", "Can't open import file 'x.stl', import() at ...").
    private static func quoted(_ s: String) -> String? {
        let from = s.range(of: "Can't")?.upperBound ?? s.startIndex
        guard let start = s[from...].firstIndex(of: "'") else { return nil }
        let rest = s[s.index(after: start)...]
        guard let end = rest.firstIndex(of: "'") else { return nil }
        return String(rest[..<end])
    }

    private static func describe(_ d: Diagnostic) -> String {
        if let line = d.line { return "Error on line \(line): \(d.message)" }
        return "Error: \(d.message)"
    }

    private static func message(_ error: Error) -> String {
        (error as? CoreError)?.message ?? error.localizedDescription
    }

    private static func with(_ r: QuickLookResult, note: String) -> QuickLookResult {
        var r = r
        r.notes.append(note)
        return r
    }

    /// `work` on a background thread, or `nil` once `deadline` seconds
    /// pass (after calling `onTimeout`), whichever comes first. Unlike a
    /// task group, this returns at the deadline even if `work` never
    /// looks at cancellation.
    private static func race<T: Sendable>(
        deadline: TimeInterval,
        _ work: @escaping @Sendable () -> T,
        onTimeout: @escaping @Sendable () -> Void
    ) async -> T? {
        await withCheckedContinuation { (continuation: CheckedContinuation<T?, Never>) in
            let once = Once(continuation)
            DispatchQueue.global(qos: .userInitiated).async {
                once.resume(work())
            }
            DispatchQueue.global(qos: .userInitiated).asyncAfter(deadline: .now() + deadline) {
                if once.resume(nil) { onTimeout() }
            }
        }
    }
}

/// Resumes a continuation at most once, from whichever thread gets there
/// first.
private final class Once<T: Sendable>: @unchecked Sendable {
    private let lock = NSLock()
    private var continuation: CheckedContinuation<T?, Never>?

    init(_ continuation: CheckedContinuation<T?, Never>) {
        self.continuation = continuation
    }

    /// Whether this call was the one that resumed.
    @discardableResult
    func resume(_ value: T?) -> Bool {
        lock.lock()
        let c = continuation
        continuation = nil
        lock.unlock()
        c?.resume(returning: value)
        return c != nil
    }
}

/// The preview extension's page: the picture, the notes, then the source.
/// HTML because Quick Look scrolls it and selects its text, and a long
/// file's source needs both; the picture travels as an attachment
/// (`cid:`), not inlined as base64.
public enum QuickLookPage {
    /// The attachment name the page's `<img>` refers to.
    public static let imageID = "model.png"

    /// Source beyond this many characters is cut, with a note: WebKit lays
    /// out every line of a `<pre>`, and a preview is for a glance.
    public static let maxSourceCharacters = 200_000

    public static func html(_ r: QuickLookResult, title: String) -> String {
        var notes = r.notes
        var source = r.source
        if source.count > maxSourceCharacters {
            source = String(source.prefix(maxSourceCharacters))
            notes.append("The source is cut short here; open the file to see all of it.")
        }
        let image =
            r.png == nil
            ? ""
            : "<div class=\"model\"><img src=\"cid:\(imageID)\" alt=\"\(escape(title))\"></div>"
        let noteList =
            notes.isEmpty
            ? ""
            : "<ul class=\"notes\">\(notes.map { (n: String) -> String in "<li>\(escape(n))</li>" }.joined())</ul>"
        return """
            <!DOCTYPE html>
            <html><head><meta charset="utf-8"><title>\(escape(title))</title>
            <style>
            :root { color-scheme: light dark; }
            body { margin: 0; font: 13px -apple-system, sans-serif; }
            .model img { max-width: 100%; height: auto; display: block; margin: 0 auto; }
            .notes { margin: 8px 12px; padding: 6px 10px 6px 28px; border-radius: 6px;
                     background: rgba(255, 196, 0, 0.18); }
            pre { margin: 0; padding: 12px; font: 12px ui-monospace, Menlo, monospace;
                  white-space: pre-wrap; overflow-wrap: anywhere; tab-size: 4; }
            </style></head>
            <body>\(image)\(noteList)<pre>\(escape(source))</pre></body></html>
            """
    }

    static func escape(_ s: String) -> String {
        var out = ""
        out.reserveCapacity(s.utf8.count)
        for c in s {
            switch c {
            case "&": out += "&amp;"
            case "<": out += "&lt;"
            case ">": out += "&gt;"
            case "\"": out += "&quot;"
            default: out.append(c)
            }
        }
        return out
    }
}
