// Design > NeoSCAD Extensions: constrained sketches (`--enable sketch`)
// and geometry queries (`--enable query`), NeoSCAD's extensions to the
// OpenSCAD language, as the macOS app's Settings > Language has them. Off
// by default; kept in %LOCALAPPDATA%\NeoSCAD\language.json
// (NeoSCAD.Host/LanguageSettings.cs) for the windows opened later. A
// toggle runs this window's document again with the new names
// (DocumentSession.SetEnable); this file is controls only.

using Microsoft.UI.Xaml;
using NeoSCAD.Host;

namespace NeoSCAD.App;

public sealed partial class MainWindow
{
    static readonly string LanguageSettingsPath =
        LanguageSettings.PathIn(Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData));

    /// <summary>The saved settings, onto the menu and the document (at start-up).</summary>
    void StartLanguageSettings()
    {
        var s = LanguageSettings.Load(LanguageSettingsPath);
        SketchItem.IsChecked = s.Sketch;
        QueryItem.IsChecked = s.Query;
        document.SetEnable(s.Names());
    }

    void OnSketchToggle(object sender, RoutedEventArgs e) => ChangeLanguageSettings();

    void OnQueryToggle(object sender, RoutedEventArgs e) => ChangeLanguageSettings();

    void ChangeLanguageSettings()
    {
        var s = new LanguageSettings { Sketch = SketchItem.IsChecked, Query = QueryItem.IsChecked };
        try
        {
            s.Save(LanguageSettingsPath);
        }
        catch (Exception ex) when (ex is IOException or UnauthorizedAccessException)
        {
            AppLog.Write($"language settings: cannot save: {ex.Message}");
        }
        document.SetEnable(s.Names());
    }
}
