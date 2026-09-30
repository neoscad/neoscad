// Serves the editor's page and script (apple/Editor/web/dist, copied next
// to the app as Editor\) at `https://app.neoscad.example/editor.html`,
// answered by the app itself: the macOS and Linux apps serve the same
// bundle from a scheme of their own, `neoscad-editor://app/`
// (apple/App/Editor/EditorSchemeHandler.swift).
//
// Not that scheme here: WebView2 under WinUI 3 never raised
// WebResourceRequested for it, registered or not, with any filter pattern
// or source kind (CI logs, September 2026: every navigation ended
// ConnectionAborted with no request seen). Requests for an https origin
// reach the handler before any network; `.example` is reserved (RFC 2606),
// so the name resolves nowhere if one ever escaped. Every response carries
// headers, so the page gets a Content-Security-Policy that admits the
// bundled script and nothing else. The page's own <meta> policy names the
// other apps' scheme (`script-src neoscad-editor:`); served here it is
// rewritten to `'self'`, or the page would refuse its own script.
// Styles need a nonce: CodeMirror writes its theme as <style> elements at
// run time, and a fresh nonce for each load is how the policy tells them
// from injected ones.
//
// This class is the part that decides what is served; the WebView2
// plumbing (WebResourceRequested) is in NeoSCAD.App/Editor/EditorHost.cs.

using System.Security.Cryptography;
using System.Text;

namespace NeoSCAD.Host;

/// <summary>One response of the editor's scheme.</summary>
public sealed record EditorResponse(byte[] Body, string ContentType, IReadOnlyDictionary<string, string> Headers);

public static class EditorPage
{
    /// <summary>The origin the page and its script are served from.</summary>
    public const string Origin = "https://app.neoscad.example";
    public const string PageUrl = Origin + "/editor.html";

    /// <summary>The page's <meta> script policy as the other apps serve it, and what it becomes here.</summary>
    const string SchemeScriptPolicy = "script-src neoscad-editor:";
    const string ScriptPolicy = "script-src 'self'";

    /// <summary>
    /// The policy for a page whose styles carry <paramref name="nonce"/>.
    /// `default-src 'none'` covers everything not listed (connections,
    /// frames, fonts, media, workers); the lint gutter's icons are `data:`
    /// SVGs. Identical to the macOS app's, so the bundle meets one policy.
    /// </summary>
    public static string ContentSecurityPolicy(string nonce) => string.Join("; ",
        "default-src 'none'",
        ScriptPolicy,
        $"style-src 'nonce-{nonce}'",
        "img-src data:",
        "base-uri 'none'",
        "form-action 'none'");

    /// <summary>A fresh nonce: 128 random bits as hex.</summary>
    public static string NewNonce() => Convert.ToHexString(RandomNumberGenerator.GetBytes(16));

    /// <summary>
    /// The MIME type of a file the page may load, or null for a name that
    /// is not served. Only plain names directly in the bundle's directory:
    /// no separators, no leading dot, so no URL can climb out of it.
    /// </summary>
    public static string? ContentType(string name)
    {
        if (name.Length == 0 || name.StartsWith('.') || name.Contains('/') || name.Contains('\\')
            || name.Contains(':'))
        {
            return null;
        }
        return Path.GetExtension(name).ToLowerInvariant() switch
        {
            ".html" => "text/html; charset=utf-8",
            ".js" => "text/javascript; charset=utf-8",
            ".css" => "text/css; charset=utf-8",
            ".txt" => "text/plain; charset=utf-8",
            _ => null,
        };
    }

    /// <summary>
    /// The file name a request URI asks for, or null when it is not on
    /// <see cref="Origin"/>.
    /// </summary>
    public static string? FileName(string uri)
    {
        if (!Uri.TryCreate(uri, UriKind.Absolute, out var u)
            || !string.Equals(u.GetLeftPart(UriPartial.Authority), Origin, StringComparison.OrdinalIgnoreCase))
        {
            return null;
        }
        return Uri.UnescapeDataString(u.AbsolutePath.TrimStart('/'));
    }

    /// <summary>
    /// The response for <paramref name="uri"/> from the bundle in
    /// <paramref name="directory"/>, or null (the view then answers 404).
    /// </summary>
    public static EditorResponse? Respond(string directory, string uri, Func<string>? nonce = null)
    {
        var name = FileName(uri);
        if (name is null) return null;
        var type = ContentType(name);
        if (type is null) return null;
        var path = Path.Combine(directory, name);
        if (!File.Exists(path)) return null;
        var body = File.ReadAllBytes(path);
        var headers = new Dictionary<string, string>
        {
            ["Content-Type"] = type,
            ["Cache-Control"] = "no-store",
            ["X-Content-Type-Options"] = "nosniff",
        };
        if (type.StartsWith("text/html", StringComparison.Ordinal))
        {
            var n = (nonce ?? NewNonce)();
            body = Encoding.UTF8.GetBytes(Encoding.UTF8.GetString(body)
                .Replace("NONCE_PLACEHOLDER", n)
                .Replace(SchemeScriptPolicy, ScriptPolicy));
            headers["Content-Security-Policy"] = ContentSecurityPolicy(n);
        }
        headers["Content-Length"] = body.Length.ToString(System.Globalization.CultureInfo.InvariantCulture);
        return new EditorResponse(body, type, headers);
    }

    /// <summary>
    /// The script WebView2 runs before the bundle on every load: the host
    /// object the bundle posts to (`bridge.js`'s `editorHost` prefers
    /// `window.NeoSCADHost`, as the web demo sets it). WebView2's own
    /// `chrome.webview.postMessage` returns nothing, so this wraps it in
    /// the promise-returning shape the bundle expects; the app never
    /// replies with a value (the macOS handler returns nil too).
    ///
    /// It also reports what would otherwise leave a blank pane with no
    /// trace: the document it ran in, script errors, rejected promises and
    /// policy violations (a script or style the page's policy refused)
    /// arrive as `log` messages, which the app writes to its log.
    /// </summary>
    public const string HostScript = """
        (() => {
          if (!window.chrome || !window.chrome.webview) return;
          const view = window.chrome.webview;
          window.NeoSCADHost = {
            postMessage(message) {
              view.postMessage(message);
              return Promise.resolve(null);
            },
          };
          const log = (level, message) => {
            try { view.postMessage({ type: "log", level, message: String(message) }); } catch (_) {}
          };
          log("info", "host script in " + location.href);
          addEventListener("error", (e) =>
            log("error", `${e.message} (${e.filename}:${e.lineno}:${e.colno})`));
          addEventListener("unhandledrejection", (e) =>
            log("error", "unhandled rejection: " + (e.reason && e.reason.stack || e.reason)));
          addEventListener("securitypolicyviolation", (e) =>
            log("error", `policy refused ${e.blockedURI || "inline"} (${e.effectiveDirective})`));
          addEventListener("DOMContentLoaded", () =>
            log("info", "DOMContentLoaded; NeoSCADEditor " + (window.NeoSCADEditor ? "present" : "missing")));
        })();
        """;
}
