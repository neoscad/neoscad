// Keeps the document's copy of the text in step with the editor's, as
// the macOS app's EditorController does (its "Versions" comment).
//
// CodeMirror owns editing (selection, undo). Every transaction comes to
// the app at once and is applied to the document's copy, which is what a
// save writes and what the core is sent: pulling the text from the page at
// save time would mean waiting on WebView2's asynchronous script calls
// inside a save. The page numbers its texts; a change is applied only if
// its `base` is the version this copy holds. After a load, or when a
// change cannot be applied (a message lost, an offset that does not fit),
// the version is unknown and changes are dropped until the page's answer
// arrives: the loaded version, or its whole text.

using NeoSCAD.Native;

namespace NeoSCAD.Host;

public sealed class EditorSync
{
    /// <summary>
    /// Apply one transaction's edits (UTF-16 offsets) to the document's
    /// copy, which then has <c>length</c> UTF-16 units. False if they do
    /// not fit: the copies disagree.
    /// </summary>
    public Func<Utf16Edit[], EditKind, ulong, bool> Apply { get; set; } = (_, _, _) => false;

    /// <summary>Ask the page for its whole text (the app then calls <see cref="Resynced"/>).</summary>
    public Action RequestText { get; set; } = () => { };

    /// <summary>The editor version the document's copy matches; null while a load or resync is on its way.</summary>
    public long? Version { get; private set; }
    public int UndoDepth { get; private set; }
    public int RedoDepth { get; private set; }
    public int Applied { get; private set; }
    public int Resyncs { get; private set; }

    /// <summary>A load was sent; its answer (the page's state) carries the new version.</summary>
    public void LoadSent() => Version = null;

    /// <summary>The page loaded the text: its version.</summary>
    public void Loaded(long version)
    {
        Version = version;
        // A load starts the page's count again, so nothing waited for can
        // still come: let every waiter go.
        Release(all: true);
    }

    /// <summary>The page's whole text arrived after a disagreement, at <paramref name="version"/>.</summary>
    public void Resynced(long version)
    {
        Version = version;
        Release(all: false);
    }

    readonly List<(long Version, TaskCompletionSource Done)> waiters = [];

    /// <summary>
    /// Completes once this copy has caught up with the page's
    /// <paramref name="version"/> (an agent's edit the page applied has come
    /// back as a change), or after <paramref name="timeout"/>. Call on the
    /// UI thread, like everything here.
    /// </summary>
    public Task WaitForAsync(long version, TimeSpan timeout)
    {
        if (Version is { } v && v >= version) return Task.CompletedTask;
        var done = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
        waiters.Add((version, done));
        return Task.WhenAny(done.Task, Task.Delay(timeout));
    }

    void Release(bool all)
    {
        for (var i = waiters.Count - 1; i >= 0; i--)
        {
            var (version, done) = waiters[i];
            if (!all && (Version is not { } v || v < version)) continue;
            waiters.RemoveAt(i);
            done.TrySetResult();
        }
    }

    public void Changes(EditorMessage.Changes m)
    {
        UndoDepth = m.UndoDepth ?? UndoDepth;
        RedoDepth = m.RedoDepth ?? RedoDepth;
        if (m.Base is not { } b || m.Version is not { } next) return;
        // A load or a resync is on its way and will replace these.
        if (Version is not { } v) return;
        if (b != v || m.Edits is null || m.Length is not { } length || length < 0)
        {
            Resync();
            return;
        }
        var edits = m.Edits.Select(e => new Utf16Edit(e.From, e.To, e.Insert)).ToArray();
        if (Apply(edits, m.Kind, (ulong)length))
        {
            Version = next;
            Applied++;
            if (waiters.Count > 0) Release(all: false);
        }
        else
        {
            Resync();
        }
    }

    void Resync()
    {
        Version = null;
        Resyncs++;
        RequestText();
    }
}
