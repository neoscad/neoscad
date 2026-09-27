// Serves the editor's page and script (apple/Editor/web/dist, copied into
// the app as Resources/Editor) under a scheme of the app's own,
// `neoscad-editor://app/editor.html`.
//
// A custom scheme rather than a file URL: the page gets a stable origin of
// its own, nothing can reach the network (the web view knows no other
// scheme to load from), and responses carry headers, so the page gets a
// Content-Security-Policy that admits the bundled script and nothing else.
// Styles need a nonce: CodeMirror writes its theme as <style> elements at
// run time, and the nonce (fresh for each page load) is how the policy
// tells them from injected ones. Everything else CodeMirror styles goes
// through CSSOM (`element.style`), which the policy does not govern.

import Foundation
import WebKit

final class EditorSchemeHandler: NSObject, WKURLSchemeHandler {
    static let scheme = "neoscad-editor"
    static let pageURL = URL(string: "\(scheme)://app/editor.html")!

    /// The policy for a page whose styles carry `nonce`. `default-src
    /// 'none'` covers everything not listed: connections, frames, fonts,
    /// media, workers. The lint gutter's icons are `data:` SVGs.
    static func contentSecurityPolicy(nonce: String) -> String {
        [
            "default-src 'none'",
            "script-src \(scheme):",
            "style-src 'nonce-\(nonce)'",
            "img-src data:",
            "base-uri 'none'",
            "form-action 'none'",
        ].joined(separator: "; ")
    }

    /// The bundle's files; `nil` if the app was built without them.
    let directory: URL?

    init(directory: URL? = Bundle.main.url(forResource: "Editor", withExtension: nil)) {
        self.directory = directory
    }

    func webView(_ webView: WKWebView, start task: any WKURLSchemeTask) {
        guard let url = task.request.url, url.host == "app",
            let (data, type) = file(at: url.path)
        else {
            task.didFailWithError(URLError(.fileDoesNotExist))
            return
        }
        var body = data
        var headers = [
            "Content-Type": type,
            "Cache-Control": "no-store",
            "X-Content-Type-Options": "nosniff",
        ]
        if type.hasPrefix("text/html") {
            let nonce = UUID().uuidString.replacingOccurrences(of: "-", with: "")
            let html = String(decoding: data, as: UTF8.self)
                .replacingOccurrences(of: "NONCE_PLACEHOLDER", with: nonce)
            body = Data(html.utf8)
            headers["Content-Security-Policy"] = Self.contentSecurityPolicy(nonce: nonce)
        }
        headers["Content-Length"] = String(body.count)
        let response = HTTPURLResponse(
            url: url, statusCode: 200, httpVersion: "HTTP/1.1", headerFields: headers)!
        task.didReceive(response)
        task.didReceive(body)
        task.didFinish()
    }

    func webView(_ webView: WKWebView, stop task: any WKURLSchemeTask) {
        // Every response is sent at once in `start`; nothing to stop.
    }

    /// The file for a URL path and its MIME type. Only plain names directly
    /// in the bundle's directory are served, so no path can climb out of it.
    func file(at path: String) -> (Data, String)? {
        guard let directory else { return nil }
        let name = String(path.drop(while: { $0 == "/" }))
        guard !name.isEmpty, !name.contains("/"), !name.hasPrefix(".") else { return nil }
        let type: String
        switch (name as NSString).pathExtension {
        case "html": type = "text/html; charset=utf-8"
        case "js": type = "text/javascript; charset=utf-8"
        case "css": type = "text/css; charset=utf-8"
        case "txt": type = "text/plain; charset=utf-8"
        default: return nil
        }
        guard let data = try? Data(contentsOf: directory.appendingPathComponent(name)) else {
            return nil
        }
        return (data, type)
    }
}
