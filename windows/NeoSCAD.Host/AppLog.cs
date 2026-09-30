// A diagnostic log for runs nobody watches: `NeoSCAD.exe --log FILE`
// appends one timestamped line per event (WebView2 start-up and
// navigation, the editor's ready message, the first render, unhandled
// exceptions). CI passes it and uploads the file beside the screenshot,
// because a screenshot alone showed a blank editor pane and nothing about
// why (docs/windows-app.md, "Diagnostics").
//
// Off unless opened: every Write is then a no-op, so call sites need no
// guard. Lines are flushed as they are written, so a crash or a killed
// process (CI stops the app with Stop-Process) still leaves every line
// written before it.

using System.Diagnostics;
using System.Globalization;
using System.Text;

namespace NeoSCAD.Host;

public static class AppLog
{
    static readonly object Gate = new();
    static readonly Stopwatch Clock = Stopwatch.StartNew();
    static StreamWriter? writer;

    /// <summary>Whether a log file is open.</summary>
    public static bool Enabled
    {
        get
        {
            lock (Gate) return writer is not null;
        }
    }

    /// <summary>
    /// Append to <paramref name="path"/> from now on (its directory is
    /// made if missing). A file that cannot be opened leaves the log off:
    /// diagnostics must never stop the app from starting.
    /// </summary>
    public static bool Open(string path)
    {
        try
        {
            var full = Path.GetFullPath(path);
            if (Path.GetDirectoryName(full) is { Length: > 0 } dir) Directory.CreateDirectory(dir);
            var stream = new FileStream(full, FileMode.Append, FileAccess.Write, FileShare.ReadWrite);
            lock (Gate)
            {
                writer?.Dispose();
                writer = new StreamWriter(stream, new UTF8Encoding(false)) { AutoFlush = true };
            }
            return true;
        }
        catch (Exception e) when (e is IOException or UnauthorizedAccessException or ArgumentException
                                      or NotSupportedException)
        {
            return false;
        }
    }

    /// <summary>Stop logging and close the file.</summary>
    public static void Close()
    {
        lock (Gate)
        {
            writer?.Dispose();
            writer = null;
        }
    }

    /// <summary>
    /// One line: UTC time, seconds since start-up, the managed thread, and
    /// <paramref name="message"/> with its line breaks folded, so each event
    /// stays one line of the file.
    /// </summary>
    public static void Write(string message)
    {
        lock (Gate)
        {
            if (writer is null) return;
            try
            {
                writer.WriteLine(Format(DateTime.UtcNow, Clock.Elapsed, Environment.CurrentManagedThreadId, message));
            }
            catch (IOException)
            {
                // A full disk or a vanished file: drop the line, keep the app.
            }
        }
    }

    /// <summary>An exception as one line: its type, message and HRESULT, then its inner ones.</summary>
    public static void Write(string context, Exception e) => Write($"{context}: {Describe(e)}");

    public static string Describe(Exception e)
    {
        var text = new StringBuilder();
        for (Exception? x = e; x is not null; x = x.InnerException)
        {
            if (text.Length > 0) text.Append(" <- ");
            text.Append(x.GetType().FullName).Append(" (0x")
                .Append(x.HResult.ToString("X8", CultureInfo.InvariantCulture)).Append("): ").Append(x.Message);
        }
        if (e.StackTrace is { } stack) text.Append(" | at ").Append(stack.Trim());
        return text.ToString();
    }

    public static string Format(DateTime utc, TimeSpan sinceStart, int thread, string message) =>
        string.Create(CultureInfo.InvariantCulture,
            $"{utc:yyyy-MM-ddTHH:mm:ss.fffZ} +{sinceStart.TotalSeconds:0.000}s [{thread}] {Fold(message)}");

    static string Fold(string message) => message.Replace("\r\n", "\n").Replace('\r', '\n').Replace("\n", " | ");
}
