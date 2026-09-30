using System.Runtime.InteropServices;

namespace NeoSCAD.Host;

/// <summary>
/// The WebView2 runtime the editor pane needs. It is part of Windows 11 and
/// of current Windows 10, but the installer does not bundle it, so a
/// machine without it (an old or stripped Windows 10) would otherwise get
/// an editor pane that stays blank, or a COM error. The app asks for the
/// runtime's version before starting the editor, and when there is none it
/// says so plainly and offers Microsoft's download page.
/// </summary>
public static class WebViewRuntime
{
    /// <summary>Microsoft's WebView2 page, whose "Evergreen Bootstrapper" installs the runtime.</summary>
    public const string DownloadUrl = "https://developer.microsoft.com/microsoft-edge/webview2/";

    public const string MissingTitle = "The editor needs Microsoft Edge WebView2";

    public const string MissingMessage =
        "NeoSCAD's editor runs in the Microsoft Edge WebView2 runtime, which is not installed on this PC. " +
        "Install the Evergreen runtime from " + DownloadUrl + " and start NeoSCAD again. " +
        "Until then you can open, preview, render and export models, but not edit them.";

    /// <summary>
    /// The installed runtime's version from <paramref name="probe"/>
    /// (CoreWebView2Environment.GetAvailableBrowserVersionString), or null
    /// when none is installed. The WinRT API reports a missing runtime by
    /// throwing (HRESULT_FROM_WIN32(ERROR_FILE_NOT_FOUND), a COMException),
    /// the .NET one by a FileNotFoundException subclass, and either may
    /// return an empty string; all three mean "not installed". Anything
    /// else is a different failure and is left to the caller, so it is not
    /// misreported as a missing runtime.
    /// </summary>
    public static string? Installed(Func<string?> probe)
    {
        try
        {
            var version = probe();
            return string.IsNullOrWhiteSpace(version) ? null : version;
        }
        catch (Exception e) when (e is COMException or FileNotFoundException)
        {
            return null;
        }
    }
}
