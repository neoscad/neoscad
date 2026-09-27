// The Quick Look preview of a .scad file (space bar in Finder): the model
// drawn from OpenSCAD's default camera, any notes (files the sandbox kept
// out, errors, a timeout), and the source beneath. The rendering and the
// page live in NeoSCADCore (Core/QuickLook.swift), where the unit tests
// reach them; this class only adapts them to Quick Look's reply.
//
// A data-based preview (QLIsDataBasedPreview in the Info.plist): Quick
// Look shows the HTML itself, so the extension draws no views and needs
// no main-thread work.

import Foundation
import NeoSCADCore
import OSLog
import QuickLookUI
import UniformTypeIdentifiers

/// One line per preview, so a slow or failing preview can be told apart
/// from Quick Look not calling the extension at all (`log show
/// --predicate 'subsystem == "org.neoscad.NeoSCAD.QuickLook"'`).
private let log = Logger(subsystem: "org.neoscad.NeoSCAD.QuickLook", category: "preview")

final class PreviewProvider: QLPreviewProvider, QLPreviewingController {
    /// The picture's pixels: twice the page's 800-point width, so it is
    /// sharp on a Retina display; the page scales it to fit.
    private static let pictureSize: (UInt32, UInt32) = (1600, 1200)

    func providePreview(for request: QLFilePreviewRequest) async throws -> QLPreviewReply {
        let url = request.fileURL
        let start = Date()
        let result = await QuickLookRender.render(
            fileAt: url, width: Self.pictureSize.0, height: Self.pictureSize.1)
        log.info(
            "preview \(url.lastPathComponent, privacy: .private): \(result.png?.count ?? 0, privacy: .public) PNG bytes, \(Date().timeIntervalSince(start), format: .fixed(precision: 2), privacy: .public) s, timed out \(result.timedOut, privacy: .public), \(result.unreadable.count, privacy: .public) unreadable files, notes: \(result.notes.joined(separator: " | "), privacy: .private)"
        )
        let html = QuickLookPage.html(result, title: url.lastPathComponent)
        let png = result.png
        let size = CGSize(width: 800, height: png == nil ? 600 : 900)
        return QLPreviewReply(dataOfContentType: .html, contentSize: size) { reply in
            if let png {
                reply.attachments = [
                    QuickLookPage.imageID: QLPreviewReplyAttachment(data: png, contentType: .png)
                ]
            }
            return Data(html.utf8)
        }
    }
}
