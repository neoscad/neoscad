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
//   (5 s, and 512 MiB of memory, estimated and also measured as the
//   footprint of the whole process: in an extension, the extension's
//   requests together). A fresh core also keeps a
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

/// What a preview shows: the core's `PreviewOutcome` (the picture, the
/// source, notes, whether it timed out, the files it could not read),
/// shared with every host's file-manager preview.
public typealias QuickLookResult = PreviewOutcome

public enum QuickLookRender {
    /// Tight limits (`agent-surface.md` finding 1 applies doubly: nobody
    /// asked for this render), from the core (`preview_limits`).
    public static var limits: ResourceLimits {
        (try? previewLimits())
            ?? ResourceLimits(
                timeSeconds: 5, memoryBytes: 512 << 20, fragments: nil, slices: nil,
                list: nil, string: nil, rands: nil, triangles: nil, depth: nil)
    }

    /// How long a request may take in all, drawing included. A little over
    /// the core's own time limit, so the core normally stops itself and
    /// reports why, and the watchdog is the backstop.
    public static let deadline: TimeInterval = 6

    /// Render the file at `url` as a `width` by `height` pixel picture.
    /// Never throws: every failure becomes a result without a picture and
    /// with a note saying why.
    ///
    /// `limits` are for tests only. They run inside the app's process,
    /// whose footprint (the app and the tests running beside them) is
    /// already near 512 MiB, so the measured memory limit would stop a
    /// model for memory it never used.
    public static func render(
        fileAt url: URL, width: UInt32, height: UInt32, mode: RenderMode = .preview,
        deadline: TimeInterval = deadline, limits: ResourceLimits = limits
    ) async -> QuickLookResult {
        let data: Data
        do {
            data = try Data(contentsOf: url)
        } catch {
            return failed("", "Could not read the file: \(error.localizedDescription)")
        }
        // OpenSCAD reads files as UTF-8. Invalid sequences become U+FFFD
        // rather than failing the whole preview: the text still shows, and
        // they can only sit in strings or comments of a model that parses.
        let text = String(decoding: data, as: UTF8.self)
        return await render(
            path: url.standardizedFileURL.path, text: text, width: width, height: height,
            mode: mode, deadline: deadline, limits: limits)
    }

    /// Render `text` as the document at `path` (absolute; includes resolve
    /// against its directory): the core's `preview_picture` on a core made
    /// for this request, raced against the watchdog. `limits` as for
    /// `render(fileAt:)`.
    public static func render(
        path: String, text: String, width: UInt32, height: UInt32,
        mode: RenderMode = .preview, deadline: TimeInterval = deadline,
        limits: ResourceLimits = limits
    ) async -> QuickLookResult {
        let core: Core
        do {
            core = try Core(config: CoreConfig(resourceDir: nil, testHooks: false))
            try core.setLimits(limits: limits)
        } catch {
            return failed(text, message(error))
        }
        let options = PictureOptions(width: width, height: height, mode: mode)
        let outcome = await race(deadline: deadline) {
            Result { try core.previewPicture(path: path, text: text, options: options) }
        } onTimeout: {
            _ = try? core.cancel(path: path)
        }
        switch outcome {
        case nil:
            return (try? previewTimedOut(source: text, deadlineSeconds: UInt32(deadline)))
                ?? failed(text, "The model took longer than Quick Look allows.")
        case .failure(let error)?:
            return failed(text, message(error))
        case .success(let r)?:
            return r
        }
    }

    // MARK: Helpers

    private static func failed(_ source: String, _ note: String) -> QuickLookResult {
        (try? previewFailed(source: source, note: note))
            ?? QuickLookResult(png: nil, source: source, notes: [note], timedOut: false, unreadable: [])
    }

    private static func message(_ error: Error) -> String {
        (error as? CoreError)?.message ?? error.localizedDescription
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

/// The preview extension's page, from the core (`preview_html`): the
/// picture (an attachment, `cid:`), the notes, then the source.
public enum QuickLookPage {
    /// The attachment name the page's `<img>` refers to.
    public static let imageID = (try? previewImageId()) ?? "model.png"

    public static func html(_ r: QuickLookResult, title: String) -> String {
        (try? previewHtml(outcome: r, title: title)) ?? ""
    }
}
