// The editor bridge's messages, as the Windows app reads and writes them.
// The protocol is the CodeMirror bundle's (apple/Editor/web/src/editor.js
// and bridge.js), documented at its macOS end
// (apple/App/Editor/EditorController.swift). The same bundle runs in both
// apps and the web demo; nothing here changes it.
//
//   page -> app (WebView2's WebMessageReceived, one JSON object each):
//     {type: "ready"}
//     {type: "changes", base, version, edits: [[from, to, insert]...], kind,
//      length, undoDepth, redoDepth}      offsets in UTF-16 units
//     {type: "command", name}             "preview" (F5), "render" (F6), and
//                                         the menu chords the app's key
//                                         script forwards (Shortcuts.cs)
//     {type: "lsp", message}              JSON-RPC for the language server
//     {type: "open", uri, line, character}
//     {type: "log", level, message}
//
//   app -> page: `NeoSCADEditor.<function>(...)` through ExecuteScriptAsync.
//   WKWebView passes named arguments; WebView2 takes a script, so every
//   argument is spliced in as a JSON literal (never raw text), which is a
//   valid JavaScript expression for any string.

using System.Text.Json;

namespace NeoSCAD.Host;

/// <summary>A message from the editor page.</summary>
public abstract record EditorMessage
{
    public sealed record Ready : EditorMessage;

    /// <summary>
    /// One CodeMirror transaction. <see cref="Edits"/> is null when the
    /// message was malformed: the app then asks for the whole text.
    /// </summary>
    public sealed record Changes(
        long? Base, long? Version, IReadOnlyList<(ulong From, ulong To, string Insert)>? Edits,
        EditKind Kind, long? Length, int? UndoDepth, int? RedoDepth) : EditorMessage;

    public sealed record Command(string Name) : EditorMessage;
    public sealed record Lsp(string Message) : EditorMessage;
    public sealed record Open(string Uri, int Line, int Character) : EditorMessage;
    public sealed record Log(string Level, string Text) : EditorMessage;
    public sealed record Unknown(string Type) : EditorMessage;

    /// <summary>
    /// Parse WebView2's `WebMessageAsJson`; null for anything that is not
    /// an object with a string `type`.
    /// </summary>
    public static EditorMessage? Parse(string json)
    {
        JsonDocument doc;
        try
        {
            doc = JsonDocument.Parse(json);
        }
        catch (JsonException)
        {
            return null;
        }
        using (doc)
        {
            var m = doc.RootElement;
            if (m.ValueKind != JsonValueKind.Object
                || !m.TryGetProperty("type", out var t) || t.ValueKind != JsonValueKind.String)
            {
                return null;
            }
            var type = t.GetString()!;
            return type switch
            {
                "ready" => new Ready(),
                "changes" => ParseChanges(m),
                "command" => Str(m, "name") is { } n ? new Command(n) : null,
                "lsp" => Str(m, "message") is { } l ? new Lsp(l) : null,
                "open" => Str(m, "uri") is { } u
                    ? new Open(u, (int)(Int(m, "line") ?? 0), (int)(Int(m, "character") ?? 0))
                    : null,
                "log" => new Log(Str(m, "level") ?? "error", Str(m, "message") ?? ""),
                _ => new Unknown(type),
            };
        }
    }

    static Changes ParseChanges(JsonElement m)
    {
        var kind = Str(m, "kind") switch
        {
            "undo" => EditKind.Undo,
            "redo" => EditKind.Redo,
            _ => EditKind.Edit,
        };
        List<(ulong, ulong, string)>? edits = null;
        if (m.TryGetProperty("edits", out var raw) && raw.ValueKind == JsonValueKind.Array)
        {
            edits = [];
            foreach (var e in raw.EnumerateArray())
            {
                if (e.ValueKind != JsonValueKind.Array || e.GetArrayLength() != 3
                    || !e[0].TryGetInt64(out var from) || !e[1].TryGetInt64(out var to)
                    || e[2].ValueKind != JsonValueKind.String || from < 0 || to < 0)
                {
                    edits = null;
                    break;
                }
                edits.Add(((ulong)from, (ulong)to, e[2].GetString()!));
            }
        }
        return new Changes(Int(m, "base"), Int(m, "version"), edits, kind, Int(m, "length"),
            (int?)Int(m, "undoDepth"), (int?)Int(m, "redoDepth"));
    }

    static string? Str(JsonElement m, string name) =>
        m.TryGetProperty(name, out var v) && v.ValueKind == JsonValueKind.String ? v.GetString() : null;

    static long? Int(JsonElement m, string name) =>
        m.TryGetProperty(name, out var v) && v.ValueKind == JsonValueKind.Number && v.TryGetInt64(out var i)
            ? i
            : null;
}

/// <summary>How a transaction changed the text, for the dirty state.</summary>
public enum EditKind
{
    Edit,
    Undo,
    Redo,
}

/// <summary>Scripts that call the page's `window.NeoSCADEditor`.</summary>
public static class EditorScript
{
    /// <summary>A JavaScript literal for <paramref name="value"/>.</summary>
    public static string Literal(object? value) => JsonSerializer.Serialize(value);

    public static string Call(string function, params object?[] args) =>
        $"NeoSCADEditor.{function}({string.Join(", ", args.Select(Literal))})";

    public static string Load(string text, string? uri, bool readOnly) => Call("load", text, uri, readOnly);
    public static string Text() => Call("text");
    public static string SetFontSize(double px) => Call("setFontSize", px);
    public static string LspReceive(string message) => Call("lspReceive", message);
    public static string Reveal(int line, int character) => Call("reveal", line, character);
    public static string Focus() => Call("focus");
    public static string LspSync() => Call("lspSync");
    public static string SetUri(string? uri) => Call("setURI", uri);

    // The Edit menu, chosen with the mouse (keys reach CodeMirror's
    // keymap directly; Shortcuts.cs).
    public static string Undo() => Call("undo");
    public static string Redo() => Call("redo");
    public static string SelectAll() => Call("selectAll");
    public static string OpenSearch() => Call("openSearch");
}

/// <summary>What the page's functions return, as ExecuteScriptAsync hands it back (JSON).</summary>
public static class EditorReply
{
    /// <summary>`load`'s history state: the version the text is now at.</summary>
    public static long? Version(string json) => Parse(json, out var v, out _) ? v : null;

    /// <summary>`text()`: the page's version and whole text.</summary>
    public static (long Version, string Text)? Text(string json) =>
        Parse(json, out var v, out var t) && v is { } version && t is { } text ? (version, text) : null;

    static bool Parse(string json, out long? version, out string? text)
    {
        version = null;
        text = null;
        try
        {
            using var doc = JsonDocument.Parse(json);
            var m = doc.RootElement;
            if (m.ValueKind != JsonValueKind.Object) return false;
            if (m.TryGetProperty("version", out var v) && v.TryGetInt64(out var i)) version = i;
            if (m.TryGetProperty("text", out var t) && t.ValueKind == JsonValueKind.String) text = t.GetString();
            return true;
        }
        catch (JsonException)
        {
            return false;
        }
    }
}
