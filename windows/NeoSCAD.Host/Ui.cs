// What the host logic needs from a UI toolkit, and no more: a way back to
// the UI thread, a one-shot timer on it, and a monotonic clock. The app
// implements them with WinUI's DispatcherQueue (NeoSCAD.App/WinUiHost.cs);
// the tests with a manual clock and an inline dispatcher, so the document
// loop runs under `dotnet test` on any OS without a window.

namespace NeoSCAD.Host;

/// <summary>Runs work on the UI thread.</summary>
public interface IUiDispatcher
{
    /// <summary>Queue <paramref name="action"/> on the UI thread.</summary>
    void Post(Action action);
}

/// <summary>One timer on the UI thread; starting it again restarts it.</summary>
public interface IUiTimer
{
    void Start(TimeSpan after, Action fire);
    void Stop();
}

/// <summary>
/// Milliseconds on a monotonic clock of the host's own: the core's
/// `DocumentController` takes "now" from the host, since library code
/// never reads the clock (CLAUDE.md, "Rules").
/// </summary>
public interface IMonotonicClock
{
    ulong NowMs { get; }
}

/// <summary>The system's monotonic clock (<see cref="Environment.TickCount64"/>).</summary>
public sealed class SystemClock : IMonotonicClock
{
    public static readonly SystemClock Instance = new();
    public ulong NowMs => (ulong)Environment.TickCount64;
}
