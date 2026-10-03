using System.Collections.Concurrent;
using NeoSCAD.Host;

namespace NeoSCAD.Tests;

/// <summary>A UI thread stand-in: posted work waits until the test pumps it.</summary>
sealed class QueueDispatcher : IUiDispatcher
{
    static readonly TimeSpan Slice = TimeSpan.FromMilliseconds(10);
    readonly BlockingCollection<Action> queue = [];

    public void Post(Action action) => queue.Add(action);

    /// <summary>Run posted work until <paramref name="done"/> holds; false on timeout.</summary>
    public bool PumpUntil(Func<bool> done, TimeSpan timeout)
    {
        var deadline = DateTime.UtcNow + timeout;
        while (!done())
        {
            var left = deadline - DateTime.UtcNow;
            if (left <= TimeSpan.Zero) return false;
            // In slices, so a condition another thread makes true (an
            // agent's request answered) is seen without work being posted.
            if (queue.TryTake(out var a, left < Slice ? left : Slice)) a();
        }
        return true;
    }
}

sealed class ManualClock : IMonotonicClock
{
    public ulong NowMs { get; set; } = 1_000;
}

/// <summary>A timer the test fires by hand.</summary>
sealed class ManualTimer : IUiTimer
{
    public TimeSpan? Armed { get; private set; }
    Action? fire;

    public void Start(TimeSpan after, Action fire)
    {
        Armed = after;
        this.fire = fire;
    }

    public void Stop()
    {
        Armed = null;
        fire = null;
    }

    public void Fire()
    {
        var f = fire ?? throw new InvalidOperationException("the timer is not armed");
        Armed = null;
        fire = null;
        f();
    }
}
