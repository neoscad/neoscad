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
    /// Other options are ignored.
    /// </summary>
    public static StartupAction Parse(IReadOnlyList<string> args)
    {
        for (var i = 0; i < args.Count; i++)
        {
            if (args[i] == "--example" && i + 1 < args.Count) return new OpenExample(args[i + 1]);
            if (!args[i].StartsWith("--", StringComparison.Ordinal)) return new OpenFile(args[i]);
        }
        return new Empty();
    }
}
