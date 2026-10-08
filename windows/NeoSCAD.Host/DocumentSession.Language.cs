// The window's NeoSCAD language extensions (Design > NeoSCAD Extensions,
// LanguageSettings.cs): the `--enable` names the document loop passes to
// every run, check, measure and export, and the language server's, as the
// macOS app's `SCADDocument.applyLanguageSettings` hands them on.

using NeoSCAD.Native;

namespace NeoSCAD.Host;

public sealed partial class DocumentSession
{
    string[] enable = [];

    /// <summary>The <c>--enable</c> names runs take (Design > NeoSCAD Extensions).</summary>
    public IReadOnlyList<string> Enable => enable;

    /// <summary>
    /// Run with <paramref name="names"/> from now on, and give them to the
    /// language server; the document runs again in its last mode when they
    /// changed, so a model that uses an extension shows it at once.
    /// Whether they changed.
    /// </summary>
    public bool SetEnable(string[] names)
    {
        if (enable.SequenceEqual(names)) return false;
        enable = [.. names];
        try
        {
            loop.SetEnable(enable);
            Language?.Server?.SetEnable(enable);
        }
        catch (CoreException e)
        {
            AppLog.Write($"language settings: {e.Message}");
        }
        if (loop.State().LastMode is { } mode) Run(mode);
        return true;
    }
}
