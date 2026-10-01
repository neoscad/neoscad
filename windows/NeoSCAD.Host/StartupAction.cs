namespace NeoSCAD.Host;

/// <summary>What the first window shows, from the command line.</summary>
public abstract record StartupAction
{
    public sealed record Empty : StartupAction;
    public sealed record OpenFile(string Path) : StartupAction;
    public sealed record OpenExample(string Id) : StartupAction;

    /// <summary>
    /// `NeoSCAD.exe [FILE]` opens a file (as Explorer passes it);
    /// `--example ID` opens one of the core's examples (CI's screenshot
    /// uses it, so the picture shows a model rather than an empty window).
    /// `--log FILE` and `--panel NAME` are not actions (see <see
    /// cref="LogPath"/> and <see cref="PanelName"/>); their values are
    /// skipped, so the log file is never opened as a model.
    /// Other options are ignored.
    /// </summary>
    public static StartupAction Parse(IReadOnlyList<string> args)
    {
        for (var i = 0; i < args.Count; i++)
        {
            if (args[i] == "--example" && i + 1 < args.Count) return new OpenExample(args[i + 1]);
            if (args[i] == LogOption || args[i] == PanelOption)
            {
                i++;
                continue;
            }
            if (!args[i].StartsWith("--", StringComparison.Ordinal)) return new OpenFile(args[i]);
        }
        return new Empty();
    }

    const string LogOption = "--log";
    const string PanelOption = "--panel";

    /// <summary>The panels `--panel NAME` can open.</summary>
    public static readonly IReadOnlyList<string> Panels = ["customizer", "check", "measure"];

    /// <summary>
    /// The side panel `--panel NAME` opens at start (customizer, check or
    /// measure; CI's screenshot shows one this way), or null.
    /// </summary>
    public static string? PanelName(IReadOnlyList<string> args)
    {
        for (var i = 0; i + 1 < args.Count; i++)
        {
            if (args[i] == PanelOption && Panels.Contains(args[i + 1])) return args[i + 1];
        }
        return null;
    }

    /// <summary>The file `--log FILE` names (NeoSCAD.Host's AppLog), or null.</summary>
    public static string? LogPath(IReadOnlyList<string> args)
    {
        for (var i = 0; i + 1 < args.Count; i++)
        {
            if (args[i] == LogOption) return args[i + 1];
        }
        return null;
    }
}
