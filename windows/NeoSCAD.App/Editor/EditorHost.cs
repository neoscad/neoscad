// The editor pane: the CodeMirror 6 bundle the macOS app and the web demo
// use (apple/Editor/web, unchanged), in WebView2. The Windows counterpart
// of apple/App/Editor/EditorController.swift and EditorSchemeHandler.swift;
// the protocol is described in NeoSCAD.Host/EditorProtocol.cs.
//
// Serving. The page loads from `neoscad-editor://app/editor.html`, a scheme
// registered with the WebView2 environment and answered from the app's
// Editor\ folder by WebResourceRequested (NeoSCAD.Host/EditorPage.cs
// decides what is served, with the Content-Security-Policy header). Every
// other request is refused, and navigation away from the page is
// cancelled, so the page never reaches the network.
//
// Messages. The bundle posts to `window.NeoSCADHost` when a page sets it
// (bridge.js's `editorHost`); a script added before any page script
// (EditorPage.HostScript) sets it to a wrapper over WebView2's
// `chrome.webview.postMessage`. The app calls the page's
// `window.NeoSCADEditor` with ExecuteScriptAsync, every argument a JSON
// literal.

using Microsoft.UI.Xaml.Controls;
using Microsoft.Web.WebView2.Core;
using NeoSCAD.Host;

namespace NeoSCAD.App.Editor;

public sealed class EditorHost
{
    readonly WebView2 view;
    readonly DocumentSession document;
    readonly EditorSync sync = new();
    readonly string bundle = Path.Combine(AppContext.BaseDirectory, "Editor");
    bool ready;

    /// <summary>A key the app's menu owns, pressed in the editor ("preview", "render").</summary>
    public event Action<string>? Command;

    /// <summary>Why the editor did not start (no WebView2 runtime, no bundle).</summary>
    public string? Error { get; private set; }

    public EditorHost(WebView2 view, DocumentSession document)
    {
        this.view = view;
        this.document = document;
        sync.Apply = document.EditorChanged;
        sync.RequestText = RequestText;
        document.TextLoaded += text => _ = Load(text);
        document.LanguageSyncRequested += () => _ = Call(EditorScript.LspSync());
        if (document.Language is { } language) language.Deliver = m => _ = Call(EditorScript.LspReceive(m));
    }

    public async Task StartAsync()
    {
        if (!File.Exists(Path.Combine(bundle, "editor.html")))
        {
            Error = "The editor's files are missing (build apple/Editor/web first).";
            return;
        }
        try
        {
            var options = new CoreWebView2EnvironmentOptions();
            // The scheme is a standard one with its own origin
            // (`neoscad-editor://app`), so the page and its script share it
            // and the policy's `script-src neoscad-editor:` admits the bundle.
            options.CustomSchemeRegistrations.Add(new CoreWebView2CustomSchemeRegistration(EditorPage.Scheme)
            {
                // A Win32 BOOL in the WinRT projection (an int), not a bool.
                TreatAsSecure = 1,
                HasAuthorityComponent = true,
            });
            var env = await CoreWebView2Environment.CreateWithOptionsAsync(null, UserDataFolder(), options);
            await view.EnsureCoreWebView2Async(env);
        }
        catch (Exception e) when (e is System.Runtime.InteropServices.COMException or FileNotFoundException)
        {
            Error = $"The editor needs the WebView2 runtime: {e.Message}";
            return;
        }
        var core = view.CoreWebView2;
        var settings = core.Settings;
        // Cut, copy and paste stay on the context menu; the browser's own
        // keys (reload, find, print), zoom and status bar would each act on
        // the page rather than the document, so they are off.
        settings.AreDefaultContextMenusEnabled = true;
        settings.AreDevToolsEnabled = System.Diagnostics.Debugger.IsAttached;
        settings.IsStatusBarEnabled = false;
        settings.AreBrowserAcceleratorKeysEnabled = false;
        settings.IsZoomControlEnabled = false;
        core.AddWebResourceRequestedFilter($"{EditorPage.Scheme}:*", CoreWebView2WebResourceContext.All);
        core.WebResourceRequested += OnResourceRequested;
        core.NavigationStarting += (_, e) =>
        {
            if (!e.Uri.StartsWith(EditorPage.Scheme + ":", StringComparison.OrdinalIgnoreCase)) e.Cancel = true;
        };
        core.NewWindowRequested += (_, e) => e.Handled = true;
        core.WebMessageReceived += (_, e) => Receive(e.WebMessageAsJson);
        core.ProcessFailed += (_, _) =>
        {
            // The page's process died: the document's copy of the text is
            // complete, so reload; `ready` shows the text again. Only the
            // undo history is lost.
            ready = false;
            sync.LoadSent();
            core.Navigate(EditorPage.PageUrl);
        };
        await core.AddScriptToExecuteOnDocumentCreatedAsync(EditorPage.HostScript);
        core.Navigate(EditorPage.PageUrl);
    }

    static string UserDataFolder() => Path.Combine(
        Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData), "NeoSCAD", "WebView2");

    void OnResourceRequested(CoreWebView2 sender, CoreWebView2WebResourceRequestedEventArgs e)
    {
        var response = EditorPage.Respond(bundle, e.Request.Uri);
        var env = view.CoreWebView2.Environment;
        if (response is null)
        {
            e.Response = env.CreateWebResourceResponse(null, 404, "Not Found", "");
            return;
        }
        var headers = string.Join("\r\n", response.Headers.Select(h => $"{h.Key}: {h.Value}"));
        var stream = new MemoryStream(response.Body).AsRandomAccessStream();
        e.Response = env.CreateWebResourceResponse(stream, 200, "OK", headers);
    }

    void Receive(string json)
    {
        switch (EditorMessage.Parse(json))
        {
            case EditorMessage.Ready:
                ready = true;
                _ = Load(document.Text);
                break;
            case EditorMessage.Changes c:
                sync.Changes(c);
                break;
            case EditorMessage.Command c:
                Command?.Invoke(c.Name);
                break;
            case EditorMessage.Lsp l:
                document.Language?.Send(l.Message);
                break;
            case EditorMessage.Log l:
                System.Diagnostics.Debug.WriteLine($"editor {l.Level}: {l.Text}");
                break;
            default:
                break;
        }
    }

    /// <summary>Show <paramref name="text"/> (a file was read): a new undo history.</summary>
    async Task Load(string text)
    {
        if (!ready) return; // `ready` loads the current text
        sync.LoadSent();
        var reply = await Call(EditorScript.Load(text, document.LanguageUri, false));
        if (reply is not null && EditorReply.Version(reply) is { } v) sync.Loaded(v);
    }

    async void RequestText()
    {
        var reply = await Call(EditorScript.Text());
        if (reply is null || EditorReply.Text(reply) is not { } t) return;
        document.EditorReplacedText(t.Text);
        sync.Resynced(t.Version);
    }

    async Task<string?> Call(string script)
    {
        if (!ready || view.CoreWebView2 is null) return null;
        try
        {
            return await view.CoreWebView2.ExecuteScriptAsync(script);
        }
        catch (Exception e) when (e is System.Runtime.InteropServices.COMException or InvalidOperationException)
        {
            System.Diagnostics.Debug.WriteLine($"editor call failed: {e.Message}");
            return null;
        }
    }

    public void Focus() => _ = Call(EditorScript.Focus());
}
