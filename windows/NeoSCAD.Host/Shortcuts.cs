// The app's keyboard shortcuts while the editor has the focus.
//
// A WinUI KeyboardAccelerator fires only for keys that reach XAML. Keys
// pressed in WebView2 go to the browser's own window first, and with the
// browser's accelerator keys off (EditorHost) a Ctrl+S typed in the editor
// never reaches the menu. WinUI 3's WebView2 does not expose the
// controller's AcceleratorKeyPressed event that Win32 hosts use for this.
// So the page forwards them, as it already does for F5 and F6 (the
// bundle's own `appKeys`, apple/Editor/web/src/editor.js): a script the
// app adds before the bundle (EditorPage.KeyScript) catches the chords in
// this table in the capture phase, before CodeMirror's keymap, stops them
// there and posts `{type: "command", name}`, the editor protocol's
// existing message, which the window performs as if the menu item had
// been chosen. On macOS the menu sees Command keys before the page does,
// so the effect is the same there: the menu wins over the editor's own
// binding of a chord (Ctrl+Shift+K deletes a line in CodeMirror; here it
// checks the model, as Command-Shift-K does on macOS).
//
// The same chords are the menu items' KeyboardAccelerators in
// MainWindow.xaml, for when the focus is elsewhere; a test checks the two
// agree. Undo, Redo, Select All and Find are not here: CodeMirror's own
// keys handle them in the editor, and the Edit menu calls the page for a
// mouse click (EditorScript.Undo and friends), as the macOS menu does.

using System.Text.Json;

namespace NeoSCAD.Host;

/// <summary>One chord: a key name (a lowercase letter, or "F5") with its modifiers.</summary>
public sealed record Shortcut(string Command, string Key, bool Ctrl = false, bool Shift = false, bool Alt = false)
{
    /// <summary>"Ctrl+Shift+K": how the page names the chord, and how a menu shows it.</summary>
    public string Chord =>
        (Ctrl ? "Ctrl+" : "") + (Alt ? "Alt+" : "") + (Shift ? "Shift+" : "") +
        (Key.Length == 1 ? Key.ToUpperInvariant() : Key);
}

public static class Shortcuts
{
    // Command names, as the editor protocol's `command` message carries them.
    public const string New = "new";
    public const string Open = "open";
    public const string Save = "save";
    public const string SaveAs = "saveAs";
    public const string Export = "export";
    public const string Check = "check";
    public const string Measure = "measure";
    public const string Customizer = "customizer";
    public const string Preview = "preview";
    public const string Render = "render";

    /// <summary>The chords the page forwards (F5 and F6 are the bundle's own).</summary>
    public static readonly IReadOnlyList<Shortcut> Forwarded =
    [
        new(New, "n", Ctrl: true),
        new(Open, "o", Ctrl: true),
        new(Save, "s", Ctrl: true),
        new(SaveAs, "s", Ctrl: true, Shift: true),
        new(Export, "e", Ctrl: true, Shift: true),
        new(Check, "k", Ctrl: true, Shift: true),
        new(Measure, "m", Ctrl: true, Shift: true),
        new(Customizer, "p", Ctrl: true, Shift: true),
    ];

    /// <summary>The command for a chord the page posted, or null.</summary>
    public static string? CommandFor(string chord) =>
        Forwarded.FirstOrDefault(s => s.Chord == chord)?.Command;
}

public static partial class EditorPage
{
    /// <summary>
    /// The page script that forwards <see cref="Shortcuts.Forwarded"/>
    /// (see Shortcuts.cs). It names a chord from `KeyboardEvent.key`, so
    /// Ctrl+S is the key that types "s" on the user's layout, as Windows'
    /// own accelerators go by the key's character; on a layout without
    /// Latin letters it falls back to the physical key (`code`). A held
    /// key's repeats are not forwarded: one press, one command.
    /// </summary>
    public static string KeyScript(IEnumerable<Shortcut> shortcuts)
    {
        var table = JsonSerializer.Serialize(shortcuts.ToDictionary(s => s.Chord, s => s.Command));
        return """
            (() => {
              if (!window.chrome || !window.chrome.webview) return;
              const view = window.chrome.webview;
              const table = TABLE;
              const keyName = (e) => {
                if (/^F\d+$/.test(e.key)) return e.key;
                if (/^[a-z]$/i.test(e.key)) return e.key.toUpperCase();
                const m = /^Key([A-Z])$/.exec(e.code);
                return m ? m[1] : null;
              };
              addEventListener("keydown", (e) => {
                const key = keyName(e);
                if (!key) return;
                const chord = (e.ctrlKey ? "Ctrl+" : "") + (e.altKey ? "Alt+" : "") +
                  (e.shiftKey ? "Shift+" : "") + key;
                const name = table[chord];
                if (!name) return;
                e.preventDefault();
                e.stopPropagation();
                if (!e.repeat) view.postMessage({ type: "command", name });
              }, true);
            })();
            """.Replace("TABLE", table);
    }
}
