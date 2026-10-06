// How many marks an agent has drawn in this window's 3D view, for the chip
// over the view that counts and clears them ("2 agent marks", the macOS
// app's AgentMarksChip; MainWindow.Agents.cs shows it).
//
// Threads. An agent's marks are drawn on the link's thread
// (AgentDocumentHost.Annotate) and cleared on the UI thread (the chip's
// button), and two agents' requests can run at once on threads of their
// own. Each draw and the count it leaves are made under one lock, so the
// count always describes the view's last draw: a count posted with each
// draw instead could arrive after a later clear, and the chip would then
// offer to clear marks the view no longer shows. Changed is raised on the
// UI thread, which reads Count afresh.
//
// The marks outlive the link (they are the view's, not the agent's): a
// window whose user turns agents off keeps them, and the chip with them,
// until the user clears them.

namespace NeoSCAD.Host;

public sealed class AgentMarks(IUiDispatcher ui)
{
    readonly object gate = new();
    int count;

    /// <summary>The lines and markers the view last drew for an agent.</summary>
    public int Count
    {
        get
        {
            lock (gate) return count;
        }
    }

    /// <summary>The count may have changed (raised on the UI thread).</summary>
    public event Action? Changed;

    /// <summary>
    /// Draw with <paramref name="draw"/> (the view's SetAgentAnnotations,
    /// any thread), which leaves <paramref name="drawn"/> marks. A draw
    /// that throws leaves the count as it was, as it leaves the view.
    /// </summary>
    public void Set(Action draw, int drawn)
    {
        lock (gate)
        {
            draw();
            count = drawn;
        }
        ui.Post(() => Changed?.Invoke());
    }

    /// <summary>
    /// The chip's text, "1 agent mark" or "2 agent marks"; null for none,
    /// when the chip hides. The macOS chip's words.
    /// </summary>
    public static string? Label(int count) => count switch
    {
        <= 0 => null,
        1 => "1 agent mark",
        _ => $"{count} agent marks",
    };

    /// <summary>The chip button's tooltip and name for screen readers.</summary>
    public const string ClearText = "Clear the agent’s marks";
}
