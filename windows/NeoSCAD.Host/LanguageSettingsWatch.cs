// Design > NeoSCAD Extensions across windows. The app is one process per
// window, so a toggle in one window reaches the others only through
// language.json (LanguageSettings.cs): each window watches the file, as
// AgentConnection watches agents.json, and takes a change another window
// saved. Without this a window opened earlier kept running its document
// with the old names until toggled there, so two windows showed one file
// differently (an unknown `sketch` module in one, a profile in the other).
//
// The watcher is an idle OS notification; every member's callback runs on
// the UI thread (posted). A read that fails while another process is
// writing, or a file left damaged, changes nothing: only settings that
// load cleanly, and differ from the window's own, are passed on, so this
// window's own save comes back as a no-op.

using System.Text.Json;

namespace NeoSCAD.Host;

public sealed class LanguageSettingsWatch : IDisposable
{
    readonly string path;
    readonly IUiDispatcher ui;
    readonly Action<LanguageSettings> changed;
    readonly FileSystemWatcher? watch;
    LanguageSettings current;
    bool disposed;

    /// <param name="path">language.json (<see cref="LanguageSettings.PathIn"/>).</param>
    /// <param name="current">The settings the window runs with now.</param>
    /// <param name="changed">Called on the UI thread with settings another window saved.</param>
    public LanguageSettingsWatch(string path, LanguageSettings current, IUiDispatcher ui, Action<LanguageSettings> changed)
    {
        this.path = path;
        this.current = current;
        this.ui = ui;
        this.changed = changed;
        watch = Watch();
    }

    /// <summary>This window saved <paramref name="settings"/>; its own change is not news.</summary>
    public void Saved(LanguageSettings settings) => current = settings;

    FileSystemWatcher? Watch()
    {
        var dir = Path.GetDirectoryName(path)!;
        try
        {
            Directory.CreateDirectory(dir);
            var w = new FileSystemWatcher(dir, Path.GetFileName(path))
            {
                // A save is a write beside it renamed over it.
                NotifyFilter = NotifyFilters.LastWrite | NotifyFilters.FileName | NotifyFilters.Size,
            };
            w.Changed += (_, _) => ui.Post(Reload);
            w.Created += (_, _) => ui.Post(Reload);
            w.Renamed += (_, _) => ui.Post(Reload);
            w.Deleted += (_, _) => ui.Post(Reload);
            w.EnableRaisingEvents = true;
            return w;
        }
        catch (Exception e) when (e is IOException or UnauthorizedAccessException or ArgumentException)
        {
            AppLog.Write($"language settings: not watching: {e.Message}");
            return null;
        }
    }

    void Reload()
    {
        if (disposed) return;
        LanguageSettings s;
        try
        {
            // A missing file is everything off, as at start-up; a file
            // being written or damaged is no change.
            s = File.Exists(path)
                ? JsonSerializer.Deserialize<LanguageSettings>(File.ReadAllBytes(path)) ?? new LanguageSettings()
                : new LanguageSettings();
        }
        catch (Exception e) when (e is IOException or UnauthorizedAccessException or JsonException)
        {
            return;
        }
        if (s == current) return;
        current = s;
        changed(s);
    }

    public void Dispose()
    {
        if (disposed) return;
        disposed = true;
        watch?.Dispose();
    }
}
