// NeoSCAD.Host's UI seams (Ui.cs) on WinUI's DispatcherQueue: posts go to
// the window's UI thread, and a DispatcherQueueTimer fires there too, so
// the document loop's timer callback and run results never race the UI.

using Microsoft.UI.Dispatching;
using NeoSCAD.Host;

namespace NeoSCAD.App;

sealed class WinUiDispatcher(DispatcherQueue queue) : IUiDispatcher
{
    public void Post(Action action) => queue.TryEnqueue(() => action());
}

sealed class WinUiTimer : IUiTimer
{
    readonly DispatcherQueueTimer timer;
    Action? fire;

    public WinUiTimer(DispatcherQueue queue)
    {
        timer = queue.CreateTimer();
        timer.IsRepeating = false;
        timer.Tick += (_, _) =>
        {
            var f = fire;
            fire = null;
            f?.Invoke();
        };
    }

    public void Start(TimeSpan after, Action fire)
    {
        timer.Stop();
        this.fire = fire;
        timer.Interval = after;
        timer.Start();
    }

    public void Stop()
    {
        timer.Stop();
        fire = null;
    }
}
