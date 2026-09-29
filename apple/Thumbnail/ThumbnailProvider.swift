// Finder's thumbnail of a .scad file: the model from OpenSCAD's default
// (diagonal) camera, at the size and scale Finder asks for. The rendering
// lives in NeoSCADCore (Core/QuickLook.swift), under Quick Look's limits.
//
// A file with no picture (errors, a timeout, a file too large) gets an
// error back, and Finder falls back to the document icon. A half-drawn or
// made-up image would pretend to be the model.

import CoreGraphics
import Foundation
import ImageIO
import NeoSCADCore
import OSLog
import QuickLookThumbnailing

/// One line per thumbnail (`log show --predicate 'subsystem ==
/// "org.neoscad.NeoSCAD.Thumbnail"'`).
private let log = Logger(subsystem: "org.neoscad.NeoSCAD.Thumbnail", category: "thumbnail")

final class ThumbnailProvider: QLThumbnailProvider {
    /// Larger thumbnails cost render time and memory for no visible gain:
    /// Finder's largest icon is 512 points (1024 pixels at 2x).
    private static let maxPixels: CGFloat = 1024

    override func provideThumbnail(
        for request: QLFileThumbnailRequest,
        _ handler: @escaping (QLThumbnailReply?, Error?) -> Void
    ) {
        // Square: a model has no page shape, and a square fills Finder's
        // icon grid evenly.
        let side = max(1, min(request.maximumSize.width, request.maximumSize.height))
        let pixels = UInt32(min(max(side * request.scale, 16), Self.maxPixels).rounded())
        let url = request.fileURL
        let reply = UnsafeSendable(handler)
        Task {
            let start = Date()
            let result = await QuickLookRender.render(fileAt: url, width: pixels, height: pixels)
            log.info(
                "thumbnail \(url.lastPathComponent, privacy: .private) at \(pixels, privacy: .public) px: \(result.png?.count ?? 0, privacy: .public) PNG bytes, \(Date().timeIntervalSince(start), format: .fixed(precision: 2), privacy: .public) s, timed out \(result.timedOut, privacy: .public), \(result.unreadable.count, privacy: .public) unreadable files, notes: \(result.notes.joined(separator: " | "), privacy: .private)"
            )
            // `ThumbnailProvider`, not `Self`: a dynamic `Self` captured by
            // the task is a non-Sendable metatype to Xcode 26's compiler.
            guard let png = result.png, let image = ThumbnailProvider.image(png) else {
                reply.value(nil, ThumbnailError(notes: result.notes))
                return
            }
            let thumbnail = QLThumbnailReply(contextSize: CGSize(width: side, height: side)) {
                context in
                // The context arrives scaled to the request's scale; its
                // clip is the whole bitmap in the current user space.
                context.interpolationQuality = .high
                context.draw(image, in: context.boundingBoxOfClipPath)
                return true
            }
            thumbnail.extensionBadge = "SCAD"
            reply.value(thumbnail, nil)
        }
    }

    private static func image(_ png: Data) -> CGImage? {
        guard let source = CGImageSourceCreateWithData(png as CFData, nil) else { return nil }
        return CGImageSourceCreateImageAtIndex(source, 0, nil)
    }
}

/// Why there is no thumbnail (Finder logs it; the user sees the icon).
struct ThumbnailError: LocalizedError {
    var notes: [String]
    var errorDescription: String? {
        notes.isEmpty ? "The model has nothing to draw." : notes.joined(separator: " ")
    }
}

/// Quick Look's completion handler is not annotated `@Sendable`, but it is
/// documented to be called once from any thread.
private struct UnsafeSendable<T>: @unchecked Sendable {
    let value: T
    init(_ value: T) { self.value = value }
}
