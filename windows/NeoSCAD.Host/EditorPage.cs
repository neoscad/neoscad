// Serves the editor's page and script (apple/Editor/web/dist, copied next
// to the app as Editor\) under a scheme of the app's own,
// `neoscad-editor://app/editor.html`, as the macOS app does
// (apple/App/Editor/EditorSchemeHandler.swift). The bundle is the same
// file; only who serves it differs.
//
// A custom scheme rather than a file URL or a virtual host mapped to the
// folder: the page gets a stable origin of its own, nothing can reach the
// network, and each response carries headers, so the page gets a
// Content-Security-Policy that admits the bundled script and nothing
// else. The page's own <meta> policy says `script-src neoscad-editor:`, so
// a page served from any other origin would not even run its script.
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
    public const string Scheme = "neoscad-editor";
    public const string PageUrl = Scheme + "://app/editor.html";

    /// <summary>
    /// The policy for a page whose styles carry <paramref name="nonce"/>.
    /// `default-src 'none'` covers everything not listed (connections,
    /// frames, fonts, media, workers); the lint gutter's icons are `data:`
    /// SVGs. Identical to the macOS app's, so the bundle meets one policy.
    /// </summary>
    public static string ContentSecurityPolicy(string nonce) => string.Join("; ",
        "default-src 'none'",
        $"script-src {Scheme}:",
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
    /// The file name a request URI asks for, or null when it is not the
    /// scheme's `app` host.
    /// </summary>
    public static string? FileName(string uri)
    {
        if (!Uri.TryCreate(uri, UriKind.Absolute, out var u)
            || !string.Equals(u.Scheme, Scheme, StringComparison.OrdinalIgnoreCase)
            || !string.Equals(u.Host, "app", StringComparison.OrdinalIgnoreCase))
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
            body = Encoding.UTF8.GetBytes(Encoding.UTF8.GetString(body).Replace("NONCE_PLACEHOLDER", n));
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
        })();
        """;
}
