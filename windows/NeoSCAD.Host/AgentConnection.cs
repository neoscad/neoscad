// This window's agent link (docs/agent-bridge.md, "Desktop apps"): the
// core's AgentLink, run while the user allows agents, with this window's
// document registered on it and AgentDocumentHost answering for it.
//
// Consent and cost. Until the user allows agents nothing here runs: no
// AgentLink exists, so there is no pipe and no thread (the audit's "nothing
// at all until the user has enabled agents"). The one standing cost is a
// FileSystemWatcher on agents.json, an idle OS notification, which is how
// a choice made in one window reaches the others: the app is one process
// per window, each with its own link. Turning agents off in any window
// stops every window's link at once, since the setting is what each
// watches; a window that kept listening after the user said no would be
// the wrong failure.
//
// Threads. Every member is for the UI thread. The link's status comes on
// its threads; the observer only posts it (it must not wait: the UI
// thread may be calling into the link at that moment), and the newest
// sequence wins, since statuses from different threads can arrive out of
// order (crates/ffi/src/agent.rs).

using NeoSCAD.Native;

namespace NeoSCAD.Host;

public sealed class AgentConnection : IDisposable
{
    readonly DocumentSession session;
    readonly IUiDispatcher ui;
    readonly IAgentEditor editor;
    readonly Func<Viewport?> viewport;
    readonly string appVersion;
    readonly string settingsPath;
    readonly string? rendezvous;
    readonly FileSystemWatcher? watch;
    AgentLink? link;
    AgentDocumentHost? host;
    ulong sequence;
    bool focused;
    bool disposed;

    /// <param name="settingsPath">agents.json (<see cref="AgentSettings.PathIn"/>).</param>
    /// <param name="rendezvous">A test directory to listen in (`AgentLink.WithDir`); null for this user's place.</param>
    /// <param name="watchSettings">Follow other windows' changes to the settings file.</param>
    public AgentConnection(DocumentSession session, IUiDispatcher ui, IAgentEditor editor, Func<Viewport?> viewport,
        string appVersion, string settingsPath, string? rendezvous = null, bool watchSettings = true)
    {
        this.session = session;
        this.ui = ui;
        this.editor = editor;
        this.viewport = viewport;
        this.appVersion = appVersion;
        this.settingsPath = settingsPath;
        this.rendezvous = rendezvous;
        Settings = AgentSettings.Load(settingsPath);
        session.TitleChanged += Register;
        if (watchSettings) watch = WatchSettings();
        Apply();
    }

    public AgentSettings Settings { get; private set; }

    /// <summary>The link's last status; null while agents are not allowed.</summary>
    public AgentStatus? Status { get; private set; }

    /// <summary>The link's address once listening (a pipe name), for the log.</summary>
    public string? Address { get; private set; }

    /// <summary>The settings or the status changed.</summary>
    public event Action? Changed;

    /// <summary>
    /// Ask the user about an agent's edit when they chose "Ask before
    /// applying" (the window's InfoBar): true to apply.
    /// </summary>
    public Func<AgentEditRequest, CancellationToken, Task<bool>>? Approve { get; set; }

    /// <summary>The window's control: its text, state and tooltip.</summary>
    public AgentIndicator Indicator => AgentIndicator.For(Settings, Status);

    /// <summary>The user's consent, from any of the window's switches (saved for every window).</summary>
    public void SetAllowed(bool allowed) => Save(Settings.WithAllowed(allowed));

    public void SetAskBeforeEdits(bool ask) => Save(Settings with { AskBeforeEdits = ask });

    /// <summary>"Disconnect": end this agent's connection to this window.</summary>
    public void Disconnect(ulong client)
    {
        try
        {
            link?.Disconnect(client);
        }
        catch (CoreException e)
        {
            AppLog.Write($"agents: disconnect failed: {CoreErrors.Describe(e)}");
        }
    }

    /// <summary>The window became the active one: requests that name no document act on it.</summary>
    public void Activated(bool active)
    {
        focused = active;
        if (!active || link is null) return;
        try
        {
            link.DocumentFocused(AgentDocumentHost.DocumentId);
        }
        catch (CoreException)
        {
        }
    }

    /// <summary>Read the settings file again (another window changed it).</summary>
    public void Reload()
    {
        if (disposed) return;
        var now = AgentSettings.Load(settingsPath);
        if (now == Settings) return;
        Settings = now;
        Apply();
    }

    void Save(AgentSettings next)
    {
        if (next == Settings) return;
        Settings = next;
        try
        {
            next.Save(settingsPath);
        }
        catch (Exception e) when (e is IOException or UnauthorizedAccessException)
        {
            // This window still does as asked; the others keep their state.
            AppLog.Write($"agents: could not save the settings: {e.Message}");
        }
        Apply();
    }

    /// <summary>Start or stop the link to match the settings.</summary>
    void Apply()
    {
        if (disposed) return;
        if (Settings.Allowed) Start();
        else Stop();
        Changed?.Invoke();
    }

    void Start()
    {
        if (link is not null) return;
        host = new AgentDocumentHost(session, ui, editor, viewport)
        {
            AskBeforeEdits = () => Settings.AskBeforeEdits,
            Approve = (edit, cancel) => this.Approve?.Invoke(edit, cancel) ?? Task.FromResult(true),
        };
        try
        {
            link = rendezvous is null
                ? new AgentLink(host, appVersion)
                : AgentLink.WithDir(host, appVersion, rendezvous);
            link.SetObserver(new Observer(this, link));
            Register();
            if (focused) link.DocumentFocused(AgentDocumentHost.DocumentId);
            link.SetAllowed(true);
        }
        catch (CoreException e)
        {
            AppLog.Write($"agents: the link did not start: {CoreErrors.Describe(e)}");
            Stop();
            return;
        }
        // Creating the pipe (and clearing a crashed run's) is quick, but it
        // is the system's work: off the UI thread.
        var started = link;
        Task.Run(() =>
        {
            try
            {
                var address = started.Start();
                ui.Post(() =>
                {
                    if (link != started) return;
                    Address = address;
                    AppLog.Write($"agents: listening at {address}");
                });
            }
            catch (CoreException e)
            {
                // The status carries the error too, for the control.
                AppLog.Write($"agents: could not listen: {CoreErrors.Describe(e)}");
            }
        });
    }

    void Stop()
    {
        host?.Close();
        host = null;
        var old = link;
        link = null;
        sequence = 0;
        Status = null;
        Address = null;
        if (old is null) return;
        try
        {
            // Tells each agent why, then the pipe goes; releasing the link
            // frees what is left.
            old.SetAllowed(false);
            old.SetObserver(null);
        }
        catch (CoreException)
        {
        }
        old.Dispose();
        AppLog.Write("agents: stopped");
    }

    /// <summary>The document as the link lists it: its name as the title shows it, and its file once saved.</summary>
    void Register()
    {
        if (link is null) return;
        try
        {
            link.DocumentOpened(AgentDocumentHost.DocumentId, session.DisplayName, session.FilePath);
        }
        catch (CoreException)
        {
        }
    }

    void StatusArrived(AgentLink from, AgentStatus status, ulong seq)
    {
        // A stopped link's last words, or an older status overtaken by a
        // newer one on another thread.
        if (link != from || seq <= sequence) return;
        sequence = seq;
        Status = status;
        Changed?.Invoke();
    }

    FileSystemWatcher? WatchSettings()
    {
        var dir = Path.GetDirectoryName(settingsPath)!;
        try
        {
            Directory.CreateDirectory(dir);
            var w = new FileSystemWatcher(dir, Path.GetFileName(settingsPath))
            {
                // A save is a write beside it renamed over it.
                NotifyFilter = NotifyFilters.LastWrite | NotifyFilters.FileName | NotifyFilters.Size,
            };
            w.Changed += (_, _) => ui.Post(Reload);
            w.Created += (_, _) => ui.Post(Reload);
            w.Renamed += (_, _) => ui.Post(Reload);
            w.Deleted += (_, _) => ui.Post(Reload);
            w.EnableRaisingEvents = true;
            return w;
        }
        catch (Exception e) when (e is IOException or UnauthorizedAccessException or ArgumentException)
        {
            AppLog.Write($"agents: not watching the settings: {e.Message}");
            return null;
        }
    }

    public void Dispose()
    {
        if (disposed) return;
        disposed = true;
        watch?.Dispose();
        session.TitleChanged -= Register;
        try
        {
            link?.DocumentClosed(AgentDocumentHost.DocumentId);
        }
        catch (CoreException)
        {
        }
        Stop();
    }

    sealed class Observer(AgentConnection owner, AgentLink from) : AgentObserver
    {
        public void StatusChanged(AgentStatus status, ulong sequence) =>
            owner.ui.Post(() => owner.StatusArrived(from, status, sequence));
    }
}

/// <summary>The window's agent control (the audit's "UI spec").</summary>
public enum AgentIndicatorState
{
    /// <summary>The user turned agents off: no control, the Help menu is the way back.</summary>
    Hidden,
    /// <summary>"Connect your AI agent": not allowed yet, or allowed with no agent connected.</summary>
    Idle,
    /// <summary>"Claude Code connected", "2 agents connected".</summary>
    Connected,
    /// <summary>"Claude Code is editing": an agent is doing something now.</summary>
    Working,
}

public sealed record AgentIndicator(AgentIndicatorState State, string Text, string Tooltip)
{
    public static AgentIndicator For(AgentSettings settings, AgentStatus? status)
    {
        if (!settings.Allowed)
        {
            return settings.TurnedOff
                ? new(AgentIndicatorState.Hidden, "", "")
                : new(AgentIndicatorState.Idle, "Connect your AI agent",
                    "Let an AI agent such as Claude Code work on this model");
        }
        if (status is null || status.Clients.Length == 0)
        {
            var tip = status?.Error is { } error
                ? $"NeoSCAD couldn't listen for agents: {error}"
                : "AI agents are allowed. Set one up, then ask it about this model.";
            return new(AgentIndicatorState.Idle, "Connect your AI agent", tip);
        }
        string line;
        try
        {
            line = NeoScad.AgentStatusLine(status);
        }
        catch (CoreException)
        {
            line = $"{status.Clients.Length} connected";
        }
        var working = status.Clients.Any(c => c.Activity is not null);
        // Names are what each agent says it is: a label, not a verified identity.
        var names = string.Join(", ", status.Clients.Select(c => c.Name ?? "An agent"));
        return new(working ? AgentIndicatorState.Working : AgentIndicatorState.Connected, line,
            $"{line}. Connected: {names}.");
    }
}
