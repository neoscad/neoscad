// What an AI agent's requests do in this window (docs/agent-bridge.md,
// "Desktop apps", "The app's API"): the core's `AgentHost`, implemented on
// the window's DocumentSession, its editor (through IAgentEditor, the
// WebView2 page in the app, a fake in the tests) and its 3D view.
//
// Threads. The link calls each method on a thread of its own, never the UI
// thread, with nothing locked (crates/ffi/src/agent.rs, "Threads"). What
// touches the document or the editor hops to the UI thread through the
// dispatcher, and the link's thread waits for the answer; the UI thread
// itself never waits on anything here. The view's calls (camera, capture,
// marks) are the core's own, which lock the view in Rust, so they run on
// the link's thread: a capture's readback then never holds up the
// window's frames or typing.
//
// One window is one process, so a document is always number 1 here; the
// command line tells the windows of several processes apart by their
// addresses (crates/cli/src/mcp/app.rs).

using NeoSCAD.Native;

namespace NeoSCAD.Host;

/// <summary>What became of an agent's edit in the editor.</summary>
public enum AgentEditorOutcome
{
    /// <summary>Applied, and the document's copy has taken it (its revision moved by one).</summary>
    Applied,
    /// <summary>The page had changed since the copy; nothing applied.</summary>
    Moved,
    /// <summary>The page did not answer, or the script failed.</summary>
    Failed,
}

/// <summary>The editor's part in an agent's requests (the app's EditorHost).</summary>
public interface IAgentEditor
{
    /// <summary>The page is up and the document's copy is at its version.</summary>
    bool InStep { get; }

    /// <summary>
    /// `agentEdit` with <paramref name="editsJson"/> (one undoable,
    /// highlighted step) if the page is still at the copy's version,
    /// completing once the change has come back to the copy.
    /// </summary>
    Task<AgentEditorOutcome> ApplyAsync(string editsJson);

    Task<EditorSelection?> SelectionAsync();

    /// <summary>Select the range and scroll to it (`revealRange`).</summary>
    Task<bool> RevealAsync(EditorPosition from, EditorPosition to);
}

public sealed class AgentDocumentHost : AgentHost
{
    /// <summary>This window's document, as the link knows it.</summary>
    public const ulong DocumentId = 1;

    /// <summary>
    /// How long a request waits for the UI thread: longer means a window
    /// stuck in a modal dialog; the agent is told so rather than hanging.
    /// </summary>
    public static TimeSpan UiWait { get; set; } = TimeSpan.FromSeconds(10);

    /// <summary>
    /// How long "Ask before applying" waits for the user: under the command
    /// line's 150 s for an edit (crates/cli/src/mcp/tools/browser.rs), as
    /// the web page's 140 s is (web/src/ui/agent.js), so the agent hears
    /// "declined" rather than a timeout.
    /// </summary>
    public static TimeSpan ApprovalWait { get; set; } = TimeSpan.FromSeconds(140);

    /// <summary>A capture waits this long for a preview that is running or due.</summary>
    public static TimeSpan RunWait { get; set; } = TimeSpan.FromSeconds(60);

    readonly DocumentSession session;
    readonly IUiDispatcher ui;
    readonly IAgentEditor editor;
    readonly Func<Viewport?> viewport;
    readonly AutoResetEvent reported = new(false);
    volatile bool closed;

    public AgentDocumentHost(DocumentSession session, IUiDispatcher ui, IAgentEditor editor, Func<Viewport?> viewport)
    {
        this.session = session;
        this.ui = ui;
        this.editor = editor;
        this.viewport = viewport;
        session.ReportChanged += Reported;
    }

    void Reported() => reported.Set();

    /// <summary>"Ask me before applying the agent's edits" (read on the UI thread).</summary>
    public Func<bool> AskBeforeEdits { get; set; } = () => false;

    /// <summary>
    /// Ask the user about an edit, on the UI thread: true to apply. The
    /// token is cancelled when the agent can wait no longer; the prompt
    /// should then go.
    /// </summary>
    public Func<AgentEditRequest, CancellationToken, Task<bool>>? Approve { get; set; }

    /// <summary>The window is closing: every request from now on is refused.</summary>
    public void Close()
    {
        closed = true;
        // Called on the UI thread, where the event is raised; a host made
        // again when the user turns agents back on subscribes afresh.
        session.ReportChanged -= Reported;
    }

    // --- AgentHost ---------------------------------------------------------------

    public AgentDocumentState Read(ulong document)
    {
        Check(document);
        return OnUi(async () =>
        {
            var selection = editor.InStep ? await editor.SelectionAsync() : null;
            return session.AgentState(selection);
        }, UiWait);
    }

    public AgentEditOutcome Edit(ulong document, AgentEditRequest edit)
    {
        Check(document);
        return OnUi(() => EditOnUi(edit), ApprovalWait + UiWait);
    }

    public void Reveal(ulong document, EditorPosition from, EditorPosition to)
    {
        Check(document);
        OnUi(async () =>
        {
            if (!editor.InStep || !await editor.RevealAsync(from, to))
                throw Refused("the editor is not showing in NeoSCAD's window");
            return true;
        }, UiWait);
    }

    public AgentCamera Camera(ulong document, AgentCameraChange change)
    {
        Check(document);
        return View(v => v.ApplyAgentCamera(change));
    }

    public AgentCapture Capture(ulong document, uint maxSide)
    {
        Check(document);
        WaitForRun();
        return View(v => v.CaptureAsShown(maxSide));
    }

    public void Annotate(ulong document, ViewLine[] lines, ViewMarker[] markers)
    {
        Check(document);
        View(v =>
        {
            v.SetAgentAnnotations(lines, markers);
            return true;
        });
    }

    // --- The work --------------------------------------------------------------------

    async Task<AgentEditOutcome> EditOnUi(AgentEditRequest edit)
    {
        if (session.Revision != edit.Version) return new AgentEditOutcome.Stale(session.Revision);
        if (AskBeforeEdits() && Approve is { } approve)
        {
            bool yes;
            using (var cancel = new CancellationTokenSource(ApprovalWait))
            {
                try
                {
                    yes = await approve(edit, cancel.Token);
                }
                catch (OperationCanceledException)
                {
                    yes = false;
                }
            }
            if (!yes) return new AgentEditOutcome.Declined();
            if (closed) throw Refused("the document was closed");
            // The user may have typed while deciding.
            if (session.Revision != edit.Version) return new AgentEditOutcome.Stale(session.Revision);
        }
        var edits = edit.Edits.Select(e => new ReloadEdit(e.From.Line, e.From.Character, e.To.Line,
            e.To.Character, e.Insert)).ToArray();
        // The edit is the next change of the text whichever way it lands
        // (the page's change, a resync after it, or the copy without an
        // editor), so the version it makes is known now. Taking the
        // revision after it arrived instead could count a keystroke made
        // since, which the agent never saw.
        var after = session.Revision + 1;
        if (!editor.InStep)
        {
            session.ApplyAgentEditsWithoutEditor(edits);
            return new AgentEditOutcome.Applied(after);
        }
        return await editor.ApplyAsync(NeoScad.ReloadEditsJson(edits)) switch
        {
            AgentEditorOutcome.Applied => new AgentEditOutcome.Applied(after),
            AgentEditorOutcome.Moved => new AgentEditOutcome.Stale(session.Revision),
            _ => throw Refused("the editor did not take the edit; read the document and try again"),
        };
    }

    /// <summary>
    /// Until no preview is running or due (the agent just edited, and the
    /// view should show the result), or <see cref="RunWait"/>.
    /// </summary>
    void WaitForRun()
    {
        var deadline = DateTime.UtcNow + RunWait;
        while (DateTime.UtcNow < deadline && OnUi(() => Task.FromResult(session.RunPending), UiWait))
        {
            // Woken by the run's report; the timeout covers a scheduled run
            // that the loop then skipped, which reports nothing.
            reported.WaitOne(TimeSpan.FromMilliseconds(250));
        }
    }

    T View<T>(Func<Viewport, T> call)
    {
        if (closed) throw Refused("the document was closed");
        if (viewport() is not { } v) throw Refused("NeoSCAD's window has no 3D view (no graphics adapter was found)");
        try
        {
            return call(v);
        }
        catch (CoreException e)
        {
            throw Refused(CoreErrors.Describe(e));
        }
        catch (ObjectDisposedException)
        {
            throw Refused("the document was closed");
        }
    }

    void Check(ulong document)
    {
        if (closed || document != DocumentId) throw Refused("the document was closed");
    }

    /// <summary>
    /// Run <paramref name="work"/> on the UI thread and wait for it here,
    /// on the link's thread, up to <paramref name="timeout"/>.
    /// </summary>
    T OnUi<T>(Func<Task<T>> work, TimeSpan timeout)
    {
        var done = new TaskCompletionSource<T>(TaskCreationOptions.RunContinuationsAsynchronously);
        ui.Post(async () =>
        {
            try
            {
                done.TrySetResult(await work());
            }
            catch (Exception e)
            {
                done.TrySetException(e);
            }
        });
        try
        {
            if (!done.Task.Wait(timeout))
                throw Refused("NeoSCAD's window did not answer in time (is it showing a dialog?)");
            return done.Task.Result;
        }
        catch (AggregateException e) when (e.InnerException is AgentHostException inner)
        {
            throw inner;
        }
        catch (AggregateException e) when (e.InnerException is CoreException core)
        {
            throw Refused(CoreErrors.Describe(core));
        }
        catch (AggregateException e)
        {
            throw Refused($"NeoSCAD failed: {e.InnerException?.Message ?? e.Message}");
        }
    }

    static AgentHostException Refused(string message) => new AgentHostException.Refused(message);
}
