// Design > NeoSCAD Extensions: constrained sketches (`--enable sketch`)
// and geometry queries (`--enable query`), NeoSCAD's extensions to the
// OpenSCAD language, as the macOS app's Settings > Language has them. Off
// by default; kept in %LOCALAPPDATA%\NeoSCAD\language.json
// (NeoSCAD.Host/LanguageSettings.cs) for the windows opened later, and
// watched (NeoSCAD.Host/LanguageSettingsWatch.cs) so that a toggle in
// another window, another process, reaches this one too. A toggle runs
// this window's document again with the new names
// (DocumentSession.SetEnable); this file is controls only.

using Microsoft.UI.Xaml;
using NeoSCAD.Host;

namespace NeoSCAD.App;

public sealed partial class MainWindow
{
    static readonly string LanguageSettingsPath =
        LanguageSettings.PathIn(Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData));

    LanguageSettingsWatch? languageWatch;

    /// <summary>The saved settings, onto the menu and the document (at start-up), then other windows' changes.</summary>
    void StartLanguageSettings()
    {
        var s = LanguageSettings.Load(LanguageSettingsPath);
        ApplyLanguageSettings(s);
        languageWatch = new LanguageSettingsWatch(LanguageSettingsPath, s, new WinUiDispatcher(DispatcherQueue),
            ApplyLanguageSettings);
        Closed += (_, _) => languageWatch?.Dispose();
    }

    /// <summary>Settings onto the menu (which raises no Click) and the document.</summary>
    void ApplyLanguageSettings(LanguageSettings s)
    {
        SketchItem.IsChecked = s.Sketch;
        QueryItem.IsChecked = s.Query;
        document.SetEnable(s.Names());
    }

    void OnSketchToggle(object sender, RoutedEventArgs e) => ChangeLanguageSettings();

    void OnQueryToggle(object sender, RoutedEventArgs e) => ChangeLanguageSettings();

    void ChangeLanguageSettings()
    {
        var s = new LanguageSettings { Sketch = SketchItem.IsChecked, Query = QueryItem.IsChecked };
        languageWatch?.Saved(s);
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
