// The document's own file, changed by another program (an AI agent
// through `neoscad mcp`, another editor), without the UI. Before this the
// window watched only the document's includes and Save replaced the file
// without looking, so such a change never showed and the next Save lost it
// (docs/audits/agent-connection-desktop.md, finding 1).
//
// What a change means is the core's (`DocumentFile`, crates/ffi/src/disk.rs
// over crates/client/src/disk.rs, shared with the macOS and Linux apps):
//
// - a clean document takes the change in place, as edits the editor's
//   `agentEdit` applies as one undoable, highlighted step (a load would
//   clear the undo history, so the user could neither see nor take back
//   what changed);
// - a document with unsaved changes gets a notice (the window's InfoBar:
//   Reload / Keep mine), and Save asks before overwriting;
// - a file deleted or moved away is said so; saving writes it again.

using System.Text;
using NeoSCAD.Native;

namespace NeoSCAD.Host;

/// <summary>What the window's bar says about the file on disk.</summary>
public abstract record DiskNotice
{
    /// <summary>Changed under unsaved edits; Reload is offered only when it is UTF-8 text.</summary>
    public sealed record Changed(bool Reloadable) : DiskNotice;

    /// <summary>Deleted or moved away.</summary>
    public sealed record Missing : DiskNotice;
}

/// <summary>What Save did.</summary>
public enum SaveOutcome
{
    Saved,
    /// <summary>Untitled: the window asks for a name (Save As).</summary>
    NeedsName,
    /// <summary>Another program changed the file since it was read or saved here: the window asks first.</summary>
    ChangedOnDisk,
}

public sealed partial class DocumentSession
{
    readonly DocumentFile disk = new();
    /// <summary>The last run's files, watched with the document's own.</summary>
    string[] runFiles = [];

    /// <summary>The bar's state; null when the file and the window agree.</summary>
    public DiskNotice? Notice { get; private set; }

    public event Action? NoticeChanged;

    /// <summary>
    /// Apply a reload in the editor: the argument is `agentEdit`'s edits
    /// as JSON (<see cref="EditorScript.AgentEdit"/>). Raised only while
    /// <see cref="EditorInStep"/> holds; otherwise the copy changes here and
    /// the editor is sent the whole text (<see cref="TextLoaded"/>).
    /// </summary>
    public event Action<string>? ReloadRequested;

    /// <summary>Whether the editor is up and holds this copy's version (set by the window's editor).</summary>
    public Func<bool> EditorInStep { get; set; } = () => false;

    /// <summary>A file's text as File.ReadAllText reads it (UTF-8, a byte-order mark honoured).</summary>
    static string Decode(byte[] bytes)
    {
        using var reader = new StreamReader(new MemoryStream(bytes), Encoding.UTF8, true);
        return reader.ReadToEnd();
    }

    /// <summary>Watch the last run's files and the document's own.</summary>
    void Rewatch() => watch.Watch(runFiles, FilePath);

    void SetNotice(DiskNotice? notice)
    {
        if (Notice == notice) return;
        Notice = notice;
        NoticeChanged?.Invoke();
    }

    /// <summary>The text is the file's again: no unsaved changes.</summary>
    void MarkSaved()
    {
        if (changeCount == 0) return;
        changeCount = 0;
        TitleChanged?.Invoke();
    }

    /// <summary>
    /// The watcher saw the document's file change (or the window's own save
    /// come back, which the core recognises): take it in, or report it.
    /// </summary>
    public void DocumentFileChanged()
    {
        if (closed || FilePath is not { } path) return;
        DiskAction action;
        try
        {
            action = disk.Check(path, storage, IsDirty);
        }
        catch (CoreException)
        {
            return;
        }
        if (action is not DiskAction.None) AppLog.Write($"the document's file changed: {action.GetType().Name}");
        switch (action)
        {
            case DiskAction.ReadAgain r:
                _ = Task.Delay(TimeSpan.FromMilliseconds(r.Ms))
                    .ContinueWith(_ => ui.Post(DocumentFileChanged), TaskScheduler.Default);
                break;
            case DiskAction.Saved:
                MarkSaved();
                SetNotice(null);
                break;
            case DiskAction.Resolved:
                SetNotice(null);
                break;
            case DiskAction.Reload r:
                ApplyReload(r.Edits);
                break;
            case DiskAction.Conflict c:
                SetNotice(new DiskNotice.Changed(c.Reloadable));
                break;
            case DiskAction.Missing:
                SetNotice(new DiskNotice.Missing());
                break;
        }
    }

    /// <summary>The bar's Reload: the file's text replaces the window's, as one undoable step.</summary>
    public bool ReloadFromDisk()
    {
        if (FilePath is not { } path) return false;
        ReloadEdit[]? edits;
        try
        {
            edits = disk.Reload(path, storage)?.ToArray();
        }
        catch (CoreException)
        {
            return false;
        }
        if (edits is null) return false;
        ApplyReload(edits);
        return true;
    }

    /// <summary>The bar's Keep mine: the notice goes; Save still asks before overwriting.</summary>
    public void KeepMine()
    {
        try
        {
            disk.KeepMine();
        }
        catch (CoreException)
        {
        }
        SetNotice(null);
    }

    void ApplyReload(IEnumerable<ReloadEdit> edits)
    {
        var list = edits.ToArray();
        SetNotice(null);
        if (EditorInStep() && ReloadRequested is { } request)
        {
            // The editor reports the edit back as a change (EditorChanged),
            // which then finds the document clean.
            request(NeoScad.ReloadEditsJson(list));
            return;
        }
        storage.Replace(NeoScad.ApplyReloadEdits(storage.Text(), list));
        TextRevised();
        ReloadLanded();
        loop.TextReplaced();
        TextLoaded?.Invoke(storage.Text());
        SchedulePreview();
    }

    /// <summary>After the copy changed: a reload landing makes the document clean again.</summary>
    void ReloadLanded()
    {
        try
        {
            if (disk.EditorChanged(storage)) MarkSaved();
        }
        catch (CoreException)
        {
        }
    }

    /// <summary>
    /// Save to the document's file. Unless <paramref name="overwrite"/>,
    /// a file another program changed since it was read or saved here is
    /// left alone and the window asks (<see cref="SaveOutcome.ChangedOnDisk"/>).
    /// </summary>
    public SaveOutcome Save(bool overwrite = false)
    {
        if (FilePath is null) return SaveOutcome.NeedsName;
        if (!overwrite)
        {
            try
            {
                if (disk.SaveCheck(FilePath) == SaveCheck.Changed) return SaveOutcome.ChangedOnDisk;
            }
            catch (CoreException)
            {
            }
        }
        SaveAs(FilePath);
        return SaveOutcome.Saved;
    }
}
