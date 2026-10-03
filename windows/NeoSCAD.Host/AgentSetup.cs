// The "Connect your AI agent" dialog's client rows, without the UI
// (docs/mcp.md, "Setup from the apps"; docs/audits/agent-connection-desktop.md,
// Option A). What each client needs is the core's table
// (`agent_setup_rows`, crates/client/src/agent_setup.rs, shared with the
// macOS and Linux apps and the web page); this decides what the row's
// button says, runs its action off the UI thread, and turns the outcome
// into the sentence the row shows.
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
}

public sealed class NativeAgentSetup : IAgentSetupBackend
{
    public static readonly NativeAgentSetup Instance = new();
    public AgentSetupRow[] Rows(string cli) => NeoScad.AgentSetupRows(cli);
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

public sealed class AgentSetup
{
    /// <summary>"Things to ask", as the web page offers them (web/src/ui/agent.js, `IDEAS`).</summary>
    public static readonly string[] Ideas =
    [
        "Make the teeth smaller and show me the result.",
        "Why won't this print? Mark the problem spots in the view.",
        "Walk me through this model, pointing at each part in the 3D view.",
        "Turn the fixed sizes into customizer parameters.",
    ];

    readonly IAgentSetupBackend backend;
    readonly Func<string, Task<bool>> openUrl;

    /// <param name="cli">The app's `neoscad` (<see cref="AgentCli.Locate"/>); null when there is none.</param>
    /// <param name="openUrl">Opens a link with the system (`Launcher.LaunchUriAsync`): false when nothing handles it.</param>
    public AgentSetup(IAgentSetupBackend backend, string? cli, Func<string, Task<bool>> openUrl)
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
