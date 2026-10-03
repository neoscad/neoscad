// The document as an AI agent sees it (docs/agent-bridge.md, "Desktop
// apps"): a revision that counts every change of the text, the state an
// agent's `read` gets, and an agent's edit for a window whose editor is
// not up. AgentDocumentHost.cs maps the link's requests onto these.

using NeoSCAD.Native;

namespace NeoSCAD.Host;

public sealed partial class DocumentSession
{
    /// <summary>
    /// Counts every change of the window's text, the user's, a reload's and
    /// an agent's (the protocol's `version`). It only grows while the
    /// window is open, so a version never names two different texts: an
    /// agent's edit made against an old one is refused instead of landing
    /// on text it did not read. Starts at 1, so 0 is never current.
    /// </summary>
    public ulong Revision { get; private set; } = 1;

    /// <summary>The text changed (any cause): after <see cref="Revision"/> moved.</summary>
    public event Action? Revised;

    void TextRevised()
    {
        Revision++;
        Revised?.Invoke();
    }

    /// <summary>What an agent's `read` gets: the window's text, unsaved changes included.</summary>
    public AgentDocumentState AgentState(EditorSelection? selection)
    {
        var state = loop.State();
        var (mode, summary, running) = Report switch
        {
            RunReport.Running r => ((RenderMode?)r.Mode, r.Mode == RenderMode.Render ? "Rendering…" : "Previewing…", true),
            RunReport.Rendered r => (r.Mode, r.Summary, false),
            RunReport.Failed f => (state.LastMode, f.Message, false),
            _ => (state.LastMode, "", false),
        };
        ParameterOverride[] overrides;
        try
        {
            overrides = loop.Overrides() ?? [];
        }
        catch (CoreException)
        {
            overrides = [];
        }
        return new AgentDocumentState(Revision, Text, selection, overrides, state.Parts,
            new AgentRunStatus(mode, summary, running), [.. Console]);
    }

    /// <summary>
    /// A run is under way, or due after the pause in typing (an agent's
    /// edit schedules one): the agent's capture waits for it, so it sees
    /// the model its edit made.
    /// </summary>
    public bool RunPending => !closed && (Report is RunReport.Running || loop.State().DueMs is not null);

    /// <summary>
    /// An agent's edit with no editor to show it (WebView2 missing, or the
    /// page not loaded yet): the copy takes it, the document becomes
    /// edited, and the editor gets the whole text when it comes up. With an
    /// editor, edits go through its `agentEdit` instead, so they are one
    /// undoable, highlighted step (AgentDocumentHost).
    /// </summary>
    public void ApplyAgentEditsWithoutEditor(ReloadEdit[] edits)
    {
        storage.Replace(NeoScad.ApplyReloadEdits(storage.Text(), edits));
        TextRevised();
        var wasDirty = IsDirty;
        // One more edit; a count below zero (undone past the save) is
        // already dirty, and adding one there could make it read clean.
        if (changeCount >= 0) changeCount++;
        if (IsDirty != wasDirty) TitleChanged?.Invoke();
        ReloadLanded();
        loop.TextReplaced();
        TextLoaded?.Invoke(storage.Text());
        SchedulePreview();
    }
}
