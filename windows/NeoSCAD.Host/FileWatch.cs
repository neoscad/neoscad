// Watches the files a document's last run read (its includes, used
// libraries, imports and fonts; `DocumentResult.files`) and says when one
// of them changes on disk, so the document runs again, as OpenSCAD's
// "Automatic Reload and Preview" does for the files a design depends on.
// The Windows counterpart of apple/App/Document/FileWatcher.swift.
//
// One FileSystemWatcher per directory, not per file: an editor that saves
// by writing a temporary file and renaming it over the original (most do)
// replaces the file, and a directory watcher still sees the new file under
// the watched name (as a Renamed or Created event), where a handle on the
// old file would go quiet after the first save. A burst of events (one
// save raises several) becomes one call after `Latency`.

namespace NeoSCAD.Host;

public sealed class FileWatch : IDisposable
{
    readonly IUiDispatcher ui;
    readonly Action changed;
    readonly List<FileSystemWatcher> watchers = [];
    string[] directories = [];
    // Read on the watchers' threads, replaced whole on the UI thread.
    volatile HashSet<string> files = new(Comparer);
    int pending;
    volatile bool disposed;

    /// <summary>Paths compare as the file system does: without case on Windows.</summary>
    static StringComparer Comparer => OperatingSystem.IsWindows() ? StringComparer.OrdinalIgnoreCase : StringComparer.Ordinal;

    /// <param name="changed">Called on the UI thread after a watched file changed.</param>
    public FileWatch(IUiDispatcher ui, Action changed)
    {
        this.ui = ui;
        this.changed = changed;
    }

    /// <summary>Events within this interval come as one call.</summary>
    public TimeSpan Latency { get; set; } = TimeSpan.FromMilliseconds(100);

    /// <summary>The files watched (full paths).</summary>
    public IReadOnlyCollection<string> Files => files;

    /// <summary>Watch exactly <paramref name="paths"/> from now on (those in missing folders are skipped).</summary>
    public void Watch(IEnumerable<string> paths)
    {
        if (disposed) return;
        var next = new HashSet<string>(paths.Select(Full).OfType<string>(), Comparer);
        if (next.SetEquals(files)) return;
        files = next;
        var dirs = next.Select(Path.GetDirectoryName).OfType<string>()
            .Where(Directory.Exists).Distinct(Comparer).Order(Comparer).ToArray();
        if (dirs.SequenceEqual(directories, Comparer)) return;
        directories = dirs;
        Restart();
    }

    static string? Full(string path)
    {
        try
        {
            return Path.GetFullPath(path);
        }
        catch (Exception e) when (e is ArgumentException or NotSupportedException or PathTooLongException)
        {
            return null;
        }
    }

    void Restart()
    {
        StopWatchers();
        foreach (var dir in directories)
        {
            try
            {
                var w = new FileSystemWatcher(dir)
                {
                    IncludeSubdirectories = false,
                    NotifyFilter = NotifyFilters.FileName | NotifyFilters.LastWrite | NotifyFilters.Size
                                   | NotifyFilters.CreationTime,
                };
                w.Changed += (_, e) => Hit(e.FullPath);
                w.Created += (_, e) => Hit(e.FullPath);
                w.Deleted += (_, e) => Hit(e.FullPath);
                w.Renamed += (_, e) => Hit(e.FullPath);
                // A lost buffer (too many changes at once) may have held one
                // of ours; running again is cheaper than missing it.
                w.Error += (_, _) => Fire();
                w.EnableRaisingEvents = true;
                watchers.Add(w);
            }
            catch (Exception e) when (e is IOException or ArgumentException or UnauthorizedAccessException
                                          or PlatformNotSupportedException)
            {
                AppLog.Write($"cannot watch {dir}: {e.Message}");
            }
        }
    }

    void Hit(string path)
    {
        if (files.Contains(path)) Fire();
    }

    void Fire()
    {
        if (disposed || Interlocked.Exchange(ref pending, 1) == 1) return;
        _ = Task.Delay(Latency).ContinueWith(_ => ui.Post(() =>
        {
            Interlocked.Exchange(ref pending, 0);
            if (!disposed) changed();
        }), TaskScheduler.Default);
    }

    void StopWatchers()
    {
        foreach (var w in watchers)
        {
            w.EnableRaisingEvents = false;
            w.Dispose();
        }
        watchers.Clear();
    }

    public void Dispose()
    {
        disposed = true;
        StopWatchers();
        files = new HashSet<string>(Comparer);
        directories = [];
    }
}
