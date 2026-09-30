// The editor pane: the CodeMirror 6 bundle the macOS app and the web demo
// use (apple/Editor/web, unchanged), in WebView2. The Windows counterpart
// of apple/App/Editor/EditorController.swift and EditorSchemeHandler.swift;
// the protocol is described in NeoSCAD.Host/EditorProtocol.cs.
//
// Serving. The page loads from `https://app.neoscad.example/editor.html`,
// answered from the app's Editor\ folder by WebResourceRequested (NeoSCAD.Host/EditorPage.cs
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
//
// Diagnostics. Every step of the start-up goes to AppLog (on with
// `--log FILE`): the environment, CoreWebView2Initialized with its
// exception, each navigation with its WebErrorStatus, each request
// answered, an external-scheme launch, a failed page process, the page's
// own script errors and policy violations (EditorPage.HostScript) and the
// ready message. It is how the custom scheme was found never to reach the
// handler (EditorPage.cs).

using System.Runtime.InteropServices.WindowsRuntime;
using Microsoft.UI.Xaml.Controls;
using Microsoft.Web.WebView2.Core;
using NeoSCAD.Host;
using Windows.Storage.Streams;

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

    /// <summary>
    /// No WebView2 runtime is installed: the window offers the download
    /// (<see cref="WebViewRuntime"/>) rather than only showing <see cref="Error"/>.
    /// </summary>
    public bool RuntimeMissing { get; private set; }

    public EditorHost(WebView2 view, DocumentSession document)
    {
        this.view = view;
        this.document = document;
        sync.Apply = document.EditorChanged;
        sync.RequestText = RequestText;
        document.TextLoaded += text => _ = Load(text);
        document.LanguageSyncRequested += () => _ = Call(EditorScript.LspSync());
        // Not gated on `ready`: the page's client sends `initialize` before
        // the editor says ready, and its answer arrives in between (CI logs:
        // a reply dropped there made the first request time out).
        if (document.Language is { } language) language.Deliver = m => _ = Call(EditorScript.LspReceive(m), whenReady: false);
    }

    public async Task StartAsync()
    {
        AppLog.Write($"editor: bundle {bundle}");
        if (!File.Exists(Path.Combine(bundle, "editor.html")))
        {
            Error = "The editor's files are missing (build apple/Editor/web first).";
            AppLog.Write($"editor: {Error}");
            return;
        }
        view.CoreWebView2Initialized += (_, e) =>
        {
            if (e.Exception is { } x) AppLog.Write("editor: CoreWebView2Initialized failed", x);
            else AppLog.Write("editor: CoreWebView2Initialized");
        };
        // Asked first, because without a runtime CreateWithOptionsAsync fails
        // with a bare COM error that tells the user nothing they can act on.
        var runtime = WebViewRuntime.Installed(() => CoreWebView2Environment.GetAvailableBrowserVersionString());
        if (runtime is null)
        {
            RuntimeMissing = true;
            Error = WebViewRuntime.MissingMessage;
            AppLog.Write("editor: no WebView2 runtime is installed");
            return;
        }
        AppLog.Write($"editor: WebView2 runtime {runtime}");
        try
        {
            var options = new CoreWebView2EnvironmentOptions();
            var folder = UserDataFolder();
            AppLog.Write($"editor: creating the WebView2 environment, user data {folder}");
            var env = await CoreWebView2Environment.CreateWithOptionsAsync(null, folder, options);
            AppLog.Write($"editor: environment {env.BrowserVersionString}, user data {env.UserDataFolder}");
            await view.EnsureCoreWebView2Async(env);
        }
        catch (Exception e) when (e is System.Runtime.InteropServices.COMException or FileNotFoundException)
        {
            Error = $"The editor needs the WebView2 runtime: {e.Message}";
            AppLog.Write("editor: WebView2 did not start", e);
            return;
        }
        catch (Exception e)
        {
            // Anything else (an unwritable user-data folder, an environment
            // that does not match one already running) used to vanish:
            // MainWindow discards StartAsync's task, so the pane stayed
            // white with no message. Now it is shown and logged.
            Error = $"The editor did not start: {e.Message}";
            AppLog.Write("editor: WebView2 did not start", e);
            return;
        }
        var core = view.CoreWebView2;
        AppLog.Write($"editor: CoreWebView2 ready, browser process {core.BrowserProcessId}");
        var settings = core.Settings;
        // Cut, copy and paste stay on the context menu; the browser's own
        // keys (reload, find, print), zoom and status bar would each act on
        // the page rather than the document, so they are off.
        settings.AreDefaultContextMenusEnabled = true;
        settings.AreDevToolsEnabled = System.Diagnostics.Debugger.IsAttached;
        settings.IsStatusBarEnabled = false;
        settings.AreBrowserAcceleratorKeysEnabled = false;
        settings.IsZoomControlEnabled = false;
        // Requests for the page's origin, from any frame or worker.
        core.AddWebResourceRequestedFilter($"{EditorPage.Origin}/*", CoreWebView2WebResourceContext.All,
            CoreWebView2WebResourceRequestSourceKinds.All);
        core.WebResourceRequested += OnResourceRequested;
        core.NavigationStarting += (_, e) =>
        {
            if (!e.Uri.StartsWith(EditorPage.Origin + "/", StringComparison.OrdinalIgnoreCase)) e.Cancel = true;
            AppLog.Write($"editor: NavigationStarting {e.NavigationId} {e.Uri}{(e.Cancel ? " (cancelled)" : "")}");
        };
        core.ContentLoading += (_, e) =>
            AppLog.Write($"editor: ContentLoading {e.NavigationId}{(e.IsErrorPage ? " (an error page)" : "")}");
        core.DOMContentLoaded += (_, e) => AppLog.Write($"editor: DOMContentLoaded {e.NavigationId}");
        core.NavigationCompleted += (_, e) => AppLog.Write(
            $"editor: NavigationCompleted {e.NavigationId} success={e.IsSuccess} " +
            $"status={e.WebErrorStatus} http={e.HttpStatusCode}");
        // A navigation to a scheme the environment does not know becomes an
        // external launch rather than a page, which leaves the pane white:
        // logged, because that is what a lost scheme registration looks
        // like, and refused, because the editor never starts a program.
        core.LaunchingExternalUriScheme += (_, e) =>
        {
            AppLog.Write($"editor: LaunchingExternalUriScheme {e.Uri} (refused)");
            e.Cancel = true;
        };
        core.NewWindowRequested += (_, e) => e.Handled = true;
        core.WebMessageReceived += (_, e) => Receive(e.WebMessageAsJson);
        core.ProcessFailed += (_, e) =>
        {
            AppLog.Write($"editor: ProcessFailed {e.ProcessFailedKind} reason={e.Reason} " +
                         $"exit={e.ExitCode} {e.ProcessDescription}");
            // The page's process died: the document's copy of the text is
            // complete, so reload; `ready` shows the text again. Only the
            // undo history is lost.
            ready = false;
            sync.LoadSent();
            core.Navigate(EditorPage.PageUrl);
        };
        await core.AddScriptToExecuteOnDocumentCreatedAsync(EditorPage.HostScript);
        AppLog.Write($"editor: navigating to {EditorPage.PageUrl}");
        core.Navigate(EditorPage.PageUrl);
        _ = ReportIfNotReady();
    }

    /// <summary>Log once if the page has not said `ready` 15 s after the first navigation.</summary>
    async Task ReportIfNotReady()
    {
        await Task.Delay(TimeSpan.FromSeconds(15));
        if (!ready) AppLog.Write("editor: no ready message 15 s after navigating");
    }

    static string UserDataFolder() => Path.Combine(
        Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData), "NeoSCAD", "WebView2");

    /// <summary>
    /// Answer a request for the scheme from the bundle. The body goes in a
    /// Windows Runtime InMemoryRandomAccessStream, filled under a deferral,
    /// rather than a managed MemoryStream behind AsRandomAccessStream:
    /// WebView2 reads the body after the handler returns, and that managed
    /// wrapper is the one ingredient of a WinUI 3 report of this same
    /// handler hanging the app (MicrosoftEdge/WebView2Feedback#806).
    /// </summary>
    async void OnResourceRequested(CoreWebView2 sender, CoreWebView2WebResourceRequestedEventArgs e)
    {
        var uri = e.Request.Uri;
        AppLog.Write($"editor: request {uri} ({e.RequestedSourceKind})");
        var deferral = e.GetDeferral();
        try
        {
            var response = EditorPage.Respond(bundle, uri);
            var env = sender.Environment;
            if (response is null)
            {
                AppLog.Write($"editor: request {uri} -> 404");
                e.Response = env.CreateWebResourceResponse(null, 404, "Not Found", "");
                return;
            }
            var headers = string.Join("\r\n", response.Headers.Select(h => $"{h.Key}: {h.Value}"));
            var stream = new InMemoryRandomAccessStream();
            await stream.WriteAsync(response.Body.AsBuffer());
            stream.Seek(0);
            e.Response = env.CreateWebResourceResponse(stream, 200, "OK", headers);
            AppLog.Write($"editor: request {uri} -> 200, {response.Body.Length} bytes {response.ContentType}");
        }
        catch (Exception x)
        {
            // An exception out of an async void handler would take the app
            // down; the page gets no response instead, and the log says why.
            AppLog.Write($"editor: request {uri} failed", x);
        }
        finally
        {
            deferral.Complete();
        }
    }

    void Receive(string json)
    {
        switch (EditorMessage.Parse(json))
        {
            case EditorMessage.Ready:
                AppLog.Write("editor: ready");
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
                AppLog.Write($"editor page {l.Level}: {l.Text}");
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
        AppLog.Write($"editor: text loaded ({text.Length} UTF-16 units), reply {(reply is null ? "none" : "received")}");
    }

    async void RequestText()
    {
        var reply = await Call(EditorScript.Text());
        if (reply is null || EditorReply.Text(reply) is not { } t) return;
        document.EditorReplacedText(t.Text);
        sync.Resynced(t.Version);
    }

    async Task<string?> Call(string script, bool whenReady = true)
    {
        if ((whenReady && !ready) || view.CoreWebView2 is null) return null;
        try
        {
            return await view.CoreWebView2.ExecuteScriptAsync(script);
        }
        catch (Exception e) when (e is System.Runtime.InteropServices.COMException or InvalidOperationException)
        {
            System.Diagnostics.Debug.WriteLine($"editor call failed: {e.Message}");
            AppLog.Write("editor: call failed", e);
            return null;
        }
    }

    public void Focus() => _ = Call(EditorScript.Focus());
}
