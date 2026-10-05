// The "Connect your AI agent" dialog's client rows, without the UI
// (docs/mcp.md, "Setup from the apps"; docs/audits/agent-connection-desktop.md,
// Option A). What each client needs is the core's table
// (`agent_setup_rows`, crates/client/src/agent_setup.rs, shared with the
// macOS and Linux apps and the web page); this decides what the row's
// button says, runs its action off the UI thread, and turns the outcome
// into the sentence the row shows.
//
// The dialog shows one client at a time, picked in a selector bar: five
// cards at once made it a wall of buttons and JSON, most of it for clients
// the user doesn't have. The pick starts on Claude Code and is kept in
// agents.json. Under it, "Using NeoSCAD with your agent" is the core's
// text for this host and the picked client (`agent_setup_usage`,
// crates/client/src/agent_setup/usage.rs), shared with the other apps
// word for word, so nothing here writes guidance of its own.
//
// Every action goes through the client's own install path where it has
// one, so the client asks the user and owns its config: Claude Code's
// `claude mcp add`, Cursor's and VS Code's install links. Claude Desktop's
// config file is the one this app writes, only after the user agreed in
// the row (the owner's decision, 2026-10-02), and with a backup the core
// makes. Each client's config names the app's own bin\neoscad.exe by its
// absolute path (AgentCli), so nothing depends on PATH.

using NeoSCAD.Native;

namespace NeoSCAD.Host;

/// <summary>Where the app's own `neoscad` is.</summary>
public static class AgentCli
{
    /// <summary>
    /// bin\neoscad.exe beside the app (the MSI's, scripts/windows/build-msi.ps1),
    /// else a `neoscad` on <paramref name="path"/> (the CLI's own MSI or
    /// scoop, for a build run from the source tree), else null: then the
    /// dialog says there is nothing to set up with.
    /// </summary>
    public static string? Locate(string appDirectory, string? path, bool windows)
    {
        var exe = windows ? "neoscad.exe" : "neoscad";
        var bundled = Path.Combine(appDirectory, "bin", exe);
        if (File.Exists(bundled)) return Path.GetFullPath(bundled);
        foreach (var dir in (path ?? "").Split(Path.PathSeparator, StringSplitOptions.RemoveEmptyEntries))
        {
            try
            {
                var candidate = Path.Combine(dir.Trim('"'), exe);
                if (File.Exists(candidate)) return Path.GetFullPath(candidate);
            }
            catch (ArgumentException)
            {
                // A PATH entry with characters no path may hold.
            }
        }
        return null;
    }
}

/// <summary>The core's setup functions (crates/ffi/src/agent_setup.rs); a fake in the tests.</summary>
public interface IAgentSetupBackend
{
    AgentSetupRow[] Rows(string cli);
    /// <summary>Blocks for a moment (it looks on disk); called off the UI thread.</summary>
    string? FindClaude();
    /// <summary>Blocks while `claude` runs (up to 60 s); called off the UI thread.</summary>
    ClaudeCodeOutcome AddToClaudeCode(string claude, string cli, bool replace);
    ClaudeDesktopOutcome AddToClaudeDesktop(string cli);
    /// <summary>The picker's label: "Other" for "Other MCP clients".</summary>
    string ShortLabel(AgentSetupClient client);
    /// <summary>"Using NeoSCAD with your agent" for this host and <paramref name="client"/>.</summary>
    AgentUsage Usage(AgentSetupClient client);
}

public sealed class NativeAgentSetup : IAgentSetupBackend
{
    public static readonly NativeAgentSetup Instance = new();
    public AgentSetupRow[] Rows(string cli) => NeoScad.AgentSetupRows(cli);
    public string ShortLabel(AgentSetupClient client) => NeoScad.AgentSetupShortLabel(client);
    public AgentUsage Usage(AgentSetupClient client) => NeoScad.AgentSetupUsage(NeoScad.AgentSetupHost(), client);
    public string? FindClaude() => NeoScad.AgentSetupFindClaude();
    public ClaudeCodeOutcome AddToClaudeCode(string claude, string cli, bool replace) =>
        NeoScad.AgentSetupAddToClaudeCode(claude, cli, replace);
    public ClaudeDesktopOutcome AddToClaudeDesktop(string cli) => NeoScad.AgentSetupAddToClaudeDesktop(cli);
}

/// <summary>Where a row stands.</summary>
public enum SetupStep
{
    /// <summary>The button does the row's action.</summary>
    Ready,
    /// <summary>The action is running; the button is off.</summary>
    Working,
    /// <summary>The button asks for a yes (replace Claude Code's entry, write Claude Desktop's file).</summary>
    Confirm,
    /// <summary>Done; the status says what is left to do, if anything.</summary>
    Done,
    /// <summary>Nothing to run here (no `claude`, Claude Desktop not installed): copy instead.</summary>
    Unavailable,
    /// <summary>It failed; the status says why, and the copy text is the way round.</summary>
    Failed,
}

/// <summary>One client's row as the dialog shows it.</summary>
public sealed class AgentSetupRowModel(AgentSetupRow row)
{
    public AgentSetupRow Row { get; } = row;
    public AgentSetupClient Client => Row.Client;
    public SetupStep Step { get; internal set; } = SetupStep.Ready;

    /// <summary>The sentence under the row's name; empty for none.</summary>
    public string Status { get; internal set; } = "";

    /// <summary>The program's output, or a backup's path: shown smaller, selectable.</summary>
    public string? Detail { get; internal set; }

    /// <summary>`claude`, once found.</summary>
    internal string? Claude { get; set; }

    /// <summary>The main button's text; null for no button (only Copy).</summary>
    public string? ButtonText => Row.Action switch
    {
        AgentSetupAction.CopyOnly => null,
        _ when Step == SetupStep.Unavailable => null,
        AgentSetupAction.RunClaude => Step switch
        {
            SetupStep.Confirm => "Replace",
            SetupStep.Done => "Added",
            SetupStep.Working => "Adding…",
            _ => "Add",
        },
        AgentSetupAction.OpenUrl => $"Open {Row.Label}",
        AgentSetupAction.MergeConfig => Step switch
        {
            SetupStep.Confirm => "Add and keep a backup",
            SetupStep.Done => "Added",
            SetupStep.Working => "Adding…",
            _ => "Add…",
        },
        _ => null,
    };

    public bool ButtonEnabled => Step is SetupStep.Ready or SetupStep.Confirm or SetupStep.Failed
                                 || (Step == SetupStep.Done && Row.Action is AgentSetupAction.OpenUrl);
}

/// <summary>
/// One headed item of "Using NeoSCAD with your agent", ready to lay out:
/// the core's title and body, and under "Things to ask" its example
/// requests, already in quotation marks.
/// </summary>
public sealed record AgentUsageEntry(AgentUsageTopic Topic, string Title, string Body, IReadOnlyList<string> Examples);

public sealed class AgentSetup
{
    readonly IAgentSetupBackend backend;
    readonly Func<string, Task<bool>> openUrl;

    /// <param name="cli">The app's `neoscad` (<see cref="AgentCli.Locate"/>); null when there is none.</param>
    /// <param name="openUrl">Opens a link with the system (`Launcher.LaunchUriAsync`): false when nothing handles it.</param>
    /// <param name="keptClient">The client the user last picked (<see cref="AgentSettings.SetupClient"/>), if any.</param>
    public AgentSetup(IAgentSetupBackend backend, string? cli, Func<string, Task<bool>> openUrl,
        string? keptClient = null)
    {
        this.backend = backend;
        this.openUrl = openUrl;
        Cli = cli;
        AgentSetupRow[] rows;
        try
        {
            // Without the app's copy the rows still show what to copy, with
            // a bare `neoscad` for a CLI installed on its own.
            rows = backend.Rows(cli ?? "neoscad");
        }
        catch (CoreException)
        {
            rows = [];
        }
        Rows = rows.Select(r => new AgentSetupRowModel(r)).ToArray();
        // The kept client if this host still lists it, else Claude Code,
        // the one most users have and the only one that connects whatever
        // order things start in; a kept name the core no longer lists (an
        // older or newer app's file) must not leave the picker on nothing.
        if (ClientNamed(keptClient) is { } kept && Row(kept) is not null)
            Selected = kept;
        else if (Row(AgentSetupClient.ClaudeCode) is not null)
            Selected = AgentSetupClient.ClaudeCode;
        else
            Selected = Rows.FirstOrDefault()?.Client ?? AgentSetupClient.ClaudeCode;
        if (cli is null)
        {
            foreach (var r in Rows)
            {
                r.Step = SetupStep.Unavailable;
                r.Status = NoCli;
            }
        }
    }

    public const string NoCli =
        "This copy of NeoSCAD has no command-line tool (bin\\neoscad.exe). Install NeoSCAD with its installer, " +
        "or install the NeoSCAD command-line tool, then open this again.";

    public string? Cli { get; }

    public IReadOnlyList<AgentSetupRowModel> Rows { get; }

    /// <summary>A row changed (on the thread that awaited the action: the UI thread in the app).</summary>
    public event Action<AgentSetupRowModel>? Changed;

    public AgentSetupRowModel? Row(AgentSetupClient client) => Rows.FirstOrDefault(r => r.Client == client);

    /// <summary>The client whose setup the dialog shows (the picker's selection).</summary>
    public AgentSetupClient Selected { get; private set; }

    /// <summary>The chosen client's row; null only when the core listed none.</summary>
    public AgentSetupRowModel? SelectedRow => Row(Selected);

    /// <summary>
    /// The picker moved to <paramref name="client"/>: true when that
    /// changed the selection (the caller then keeps it in the settings),
    /// false for the same client or one this host doesn't list.
    /// </summary>
    public bool Select(AgentSetupClient client)
    {
        if (client == Selected || Row(client) is null) return false;
        Selected = client;
        return true;
    }

    /// <summary>The picker's label for <paramref name="client"/>: the core's short one, else the row's.</summary>
    public string ShortLabel(AgentSetupClient client)
    {
        try
        {
            return backend.ShortLabel(client);
        }
        catch (CoreException)
        {
            return Row(client)?.Row.Label ?? client.ToString();
        }
    }

    /// <summary>"Using NeoSCAD with your agent" for the chosen client.</summary>
    public IReadOnlyList<AgentUsageEntry> Usage() => Entries(backend.Usage(Selected));

    /// <summary>
    /// The core's section as the dialog lays it out: its items in order,
    /// and the examples under "Things to ask" (only there: the core sends
    /// them apart from the items so each app can place them), each in
    /// quotation marks, as the macOS sheet shows them.
    /// </summary>
    public static IReadOnlyList<AgentUsageEntry> Entries(AgentUsage usage) =>
        usage.Items.Select(i => new AgentUsageEntry(i.Topic, i.Title, i.Body,
                i.Topic == AgentUsageTopic.WhatToAsk ? usage.Examples.Select(e => $"“{e}”").ToArray() : []))
            .ToArray();

    /// <summary>
    /// The client's name in agents.json: the core's spelling
    /// (`kebab-case`), the same names the macOS app keeps, so the file
    /// doesn't depend on the C# enum's member names.
    /// </summary>
    public static string ClientName(AgentSetupClient client) => client switch
    {
        AgentSetupClient.ClaudeCode => "claude-code",
        AgentSetupClient.ClaudeDesktop => "claude-desktop",
        AgentSetupClient.Cursor => "cursor",
        AgentSetupClient.VsCode => "vs-code",
        AgentSetupClient.Other => "other",
        _ => client.ToString(),
    };

    /// <summary>The client <paramref name="name"/> names, or null for none or a name this app doesn't know.</summary>
    public static AgentSetupClient? ClientNamed(string? name)
    {
        foreach (var c in Enum.GetValues<AgentSetupClient>())
        {
            if (ClientName(c) == name) return c;
        }
        return null;
    }

    /// <summary>Look for `claude` off the UI thread, and say on its row whether one click will do.</summary>
    public async Task DetectAsync()
    {
        if (Cli is null || Row(AgentSetupClient.ClaudeCode) is not { } row) return;
        string? claude;
        try
        {
            claude = await Task.Run(backend.FindClaude);
        }
        catch (CoreException)
        {
            claude = null;
        }
        row.Claude = claude;
        if (row.Step is SetupStep.Ready or SetupStep.Unavailable)
        {
            if (claude is null)
            {
                Set(row, SetupStep.Unavailable,
                    "Claude Code wasn't found on this PC. Copy the command and run it in a terminal where `claude` works.");
            }
            else
            {
                Set(row, SetupStep.Ready, "Adds NeoSCAD to Claude Code for all your projects.");
            }
        }
    }

    /// <summary>The row's button.</summary>
    public async Task RunAsync(AgentSetupRowModel row)
    {
        if (Cli is not { } cli || !row.ButtonEnabled) return;
        switch (row.Row.Action)
        {
            case AgentSetupAction.RunClaude:
                await AddToClaudeCodeAsync(row, cli, replace: row.Step == SetupStep.Confirm);
                break;
            case AgentSetupAction.OpenUrl open:
                Set(row, SetupStep.Working, $"Opening {row.Row.Label}…");
                bool opened;
                try
                {
                    opened = await openUrl(open.Url);
                }
                catch (Exception e) when (e is UriFormatException or InvalidOperationException
                                              or System.Runtime.InteropServices.COMException)
                {
                    opened = false;
                }
                if (opened)
                    Set(row, SetupStep.Done, $"{row.Row.Label} should ask you to install NeoSCAD's server: confirm it there.");
                else
                    Set(row, SetupStep.Failed, $"Nothing opened the link. Is {row.Row.Label} installed? You can also copy the settings below.");
                break;
            case AgentSetupAction.MergeConfig merge:
                if (row.Step != SetupStep.Confirm)
                {
                    // The question, in the row: what will be written, and where.
                    Set(row, SetupStep.Confirm,
                        $"NeoSCAD will add a \"neoscad\" server to {merge.Path}, and save a copy of the file as it is now beside it.");
                    return;
                }
                await AddToClaudeDesktopAsync(row, cli);
                break;
        }
    }

    /// <summary>The Confirm step's "no": the row as it was.</summary>
    public void Cancel(AgentSetupRowModel row)
    {
        if (row.Step == SetupStep.Confirm) Set(row, SetupStep.Ready, "");
    }

    async Task AddToClaudeCodeAsync(AgentSetupRowModel row, string cli, bool replace)
    {
        if (row.Claude is null)
        {
            await DetectAsync();
            if (row.Claude is null) return;
        }
        var claude = row.Claude;
        Set(row, SetupStep.Working, replace ? "Replacing Claude Code's neoscad server…" : "Adding NeoSCAD to Claude Code…");
        ClaudeCodeOutcome outcome;
        try
        {
            outcome = await Task.Run(() => backend.AddToClaudeCode(claude, cli, replace));
        }
        catch (CoreException e)
        {
            outcome = new ClaudeCodeOutcome.Failed(CoreErrors.Describe(e));
        }
        switch (outcome)
        {
            case ClaudeCodeOutcome.Added a:
                Set(row, SetupStep.Done, "Added for all your projects. Start a new Claude Code session to use it.",
                    Trimmed(a.Output));
                break;
            case ClaudeCodeOutcome.AlreadyExists a:
                Set(row, SetupStep.Confirm,
                    "Claude Code already has a server called neoscad. Replace it with this NeoSCAD's?", Trimmed(a.Output));
                break;
            case ClaudeCodeOutcome.Failed f:
                Set(row, SetupStep.Failed, "Claude Code couldn't add it. Copy the command and run it in a terminal.",
                    Trimmed(f.Output));
                break;
        }
    }

    async Task AddToClaudeDesktopAsync(AgentSetupRowModel row, string cli)
    {
        Set(row, SetupStep.Working, "Adding NeoSCAD to Claude Desktop…");
        ClaudeDesktopOutcome outcome;
        try
        {
            outcome = await Task.Run(() => backend.AddToClaudeDesktop(cli));
        }
        catch (CoreException e)
        {
            Set(row, SetupStep.Failed, $"Couldn't change Claude Desktop's settings: {CoreErrors.Describe(e)}");
            return;
        }
        switch (outcome)
        {
            case ClaudeDesktopOutcome.Written w:
                Set(row, SetupStep.Done, "Added. Quit Claude Desktop and open it again to finish.",
                    w.Backup is { } b ? $"The old file was saved as {b}" : null);
                break;
            case ClaudeDesktopOutcome.Unchanged:
                Set(row, SetupStep.Done, "Already added. If Claude Desktop doesn't list NeoSCAD, quit it and open it again.");
                break;
            case ClaudeDesktopOutcome.NotInstalled:
                Set(row, SetupStep.Unavailable,
                    "Claude Desktop isn't installed for this user (or hasn't been opened yet). Open it once, then try again.");
                break;
            case ClaudeDesktopOutcome.Refused r:
                Set(row, SetupStep.Failed,
                    $"Left {r.Path} as it was: {r.Reason}. Copy the settings below and add them by hand.");
                break;
        }
    }

    /// <summary>`claude`'s output, kept to what a row can show.</summary>
    static string? Trimmed(string output)
    {
        var t = output.Trim();
        if (t.Length == 0) return null;
        return t.Length <= 600 ? t : t[..600] + "…";
    }

    void Set(AgentSetupRowModel row, SetupStep step, string status, string? detail = null)
    {
        row.Step = step;
        row.Status = status;
        row.Detail = detail;
        Changed?.Invoke(row);
    }
}
