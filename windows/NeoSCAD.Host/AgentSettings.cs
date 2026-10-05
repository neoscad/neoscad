// What the app keeps between runs about AI agents
// (docs/audits/agent-connection-desktop.md, "Security and privacy"; the
// owner's decision 2: off until the user allows it), in
// %LOCALAPPDATA%\NeoSCAD\agents.json beside updates.json.
//
// The file is the one place every window's process reads: the app is one
// process per window, so turning agents on or off in one window has to
// reach the others (AgentConnection watches this file). A damaged or
// missing file reads as the defaults, which is "not allowed": a broken
// file must never be taken for consent.

using System.Text.Json;
using System.Text.Json.Serialization;

namespace NeoSCAD.Host;

public sealed record AgentSettings
{
    /// <summary>
    /// The user's consent: "Allow AI agents to work on open documents". Off
    /// until they turn it on; until then nothing listens.
    /// </summary>
    [JsonPropertyName("allowed")] public bool Allowed { get; init; }

    /// <summary>"Ask me before applying the agent's edits" (off, as on the web page).</summary>
    [JsonPropertyName("ask_before_edits")] public bool AskBeforeEdits { get; init; }

    /// <summary>
    /// The user turned agents off after having them on: the window's agent
    /// button then hides, and the Help menu is the way back (the audit's
    /// "off" state). Not set by "Not now" at the consent prompt, which
    /// keeps the button as the invitation.
    /// </summary>
    [JsonPropertyName("turned_off")] public bool TurnedOff { get; init; }

    /// <summary>
    /// The client last picked in the "Connect your AI agent" dialog, as
    /// <see cref="AgentSetup.ClientName"/> spells it; null until the user
    /// picks one, when the dialog starts on Claude Code. Kept here rather
    /// than per window so every window's dialog opens on the same client.
    /// </summary>
    [JsonPropertyName("setup_client")] public string? SetupClient { get; init; }

    /// <summary>
    /// "Using NeoSCAD with your agent" is open in the dialog: at first,
    /// and until the user folds it. A file from before the setting reads
    /// as open, since a missing key leaves the initialiser's value.
    /// </summary>
    [JsonPropertyName("usage_open")] public bool UsageOpen { get; init; } = true;

    /// <summary>Consent given (or taken back) from any of the app's switches.</summary>
    public AgentSettings WithAllowed(bool allowed) =>
        this with { Allowed = allowed, TurnedOff = !allowed && (Allowed || TurnedOff) };

    /// <summary>The file at <paramref name="path"/>, or the defaults when it is missing or damaged.</summary>
    public static AgentSettings Load(string path)
    {
        try
        {
            return JsonSerializer.Deserialize<AgentSettings>(File.ReadAllBytes(path)) ?? new AgentSettings();
        }
        catch (Exception e) when (e is IOException or UnauthorizedAccessException or JsonException)
        {
            return new AgentSettings();
        }
    }

    /// <summary>
    /// Written beside the target and moved over it, so a crash leaves the
    /// old file and another window's watcher never reads half of one. The
    /// temporary name is per process: two windows saving at once must not
    /// write into the same temporary file.
    /// </summary>
    public void Save(string path)
    {
        Directory.CreateDirectory(Path.GetDirectoryName(path)!);
        var tmp = $"{path}.{Environment.ProcessId}.tmp";
        File.WriteAllText(tmp, JsonSerializer.Serialize(this, new JsonSerializerOptions { WriteIndented = true }));
        File.Move(tmp, path, overwrite: true);
    }

    /// <summary>%LOCALAPPDATA%\NeoSCAD\agents.json for <paramref name="localAppData"/>.</summary>
    public static string PathIn(string localAppData) => Path.Combine(localAppData, "NeoSCAD", "agents.json");
}
