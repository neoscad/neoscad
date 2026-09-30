// The editor's language server: the core's (crates/lsp, through the
// generated `LanguageServer`), one per window, reached from the page's
// @codemirror/lsp-client through the bridge's `lsp` message. The port of
// apple/App/Editor/LanguageClient.swift.
//
// Threads. Messages are handled in order, off the UI thread (the first
// request on a BOSL2 model indexes the library), one at a time on a chain
// of tasks; answers come back on the UI thread in the same order.
//
// Diagnostics. A window's server is made with `hostDiagnostics`: it never
// evaluates, and its markers come from the document's own runs
// (`DocumentSession` hands each run's publications to `Deliver`). When the
// server says diagnostics are pending (a text version arrived after its
// run was supplied), they are published after a short pause.

using NeoSCAD.Native;

namespace NeoSCAD.Host;

public sealed class LanguageBridge
{
    /// <summary>How long messages must pause before pending diagnostics go out.</summary>
    public static readonly TimeSpan Debounce = TimeSpan.FromMilliseconds(150);

    readonly LanguageServer? server;
    readonly IUiDispatcher ui;
    readonly IUiTimer diagnosticsTimer;
    readonly object chainLock = new();
    Task chain = Task.CompletedTask;
    bool stopped;

    /// <summary>Where the server's messages go (the page's client).</summary>
    public Action<string> Deliver { get; set; } = _ => { };

    /// <param name="server">Null when the core did not start: requests
    /// then get JSON-RPC's "method not found", so the page's client never
    /// waits for an answer that is not coming.</param>
    public LanguageBridge(LanguageServer? server, IUiDispatcher ui, IUiTimer diagnosticsTimer)
    {
        this.server = server;
        this.ui = ui;
        this.diagnosticsTimer = diagnosticsTimer;
    }

    public LanguageServer? Server => server;

    /// <summary>A message from the page for the server.</summary>
    public void Send(string message)
    {
        if (stopped) return;
        if (server is null)
        {
            if (JsonRpc.NotFoundReply(message) is { } reply) Deliver(reply);
            return;
        }
        lock (chainLock)
        {
            chain = chain.ContinueWith(_ =>
            {
                string[] output;
                bool pending;
                try
                {
                    output = server.Handle(message);
                    pending = server.DiagnosticsPending();
                }
                catch (CoreException)
                {
                    return;
                }
                ui.Post(() =>
                {
                    if (stopped) return;
                    foreach (var m in output) Deliver(m);
                    if (pending) diagnosticsTimer.Start(Debounce, PublishDiagnostics);
                });
            }, TaskScheduler.Default);
        }
    }

    /// <summary>Publications from a document run (`DocumentResult.Language`).</summary>
    public void DeliverPublications(IEnumerable<string> messages)
    {
        if (stopped) return;
        foreach (var m in messages) Deliver(m);
    }

    void PublishDiagnostics()
    {
        if (stopped || server is null) return;
        Task.Run(() =>
        {
            string[] output;
            try
            {
                output = server.PublishDiagnostics();
            }
            catch (CoreException)
            {
                return;
            }
            ui.Post(() =>
            {
                if (stopped) return;
                foreach (var m in output) Deliver(m);
            });
        });
    }

    /// <summary>The window closed: deliver nothing more.</summary>
    public void Stop()
    {
        stopped = true;
        diagnosticsTimer.Stop();
    }
}

public static class JsonRpc
{
    /// <summary>
    /// The error reply to a request (a message with an `id` and a
    /// `method`) when there is no server; null for a notification.
    /// </summary>
    public static string? NotFoundReply(string message)
    {
        try
        {
            using var doc = System.Text.Json.JsonDocument.Parse(message);
            var m = doc.RootElement;
            if (m.ValueKind != System.Text.Json.JsonValueKind.Object
                || !m.TryGetProperty("id", out var id) || !m.TryGetProperty("method", out _))
            {
                return null;
            }
            return "{\"jsonrpc\":\"2.0\",\"id\":" + id.GetRawText()
                + ",\"error\":{\"code\":-32601,\"message\":\"No language server yet\"}}";
        }
        catch (System.Text.Json.JsonException)
        {
            return null;
        }
    }
}
