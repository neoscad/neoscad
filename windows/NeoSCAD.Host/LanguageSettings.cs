// Design > NeoSCAD Extensions: which of NeoSCAD's language extensions the
// window runs its document with (docs/language-extensions.md, section 2),
// the Windows port of apple/App/Editor/LanguageSettings.swift. Constrained
// sketches (`--enable sketch`), geometry queries (`--enable query`),
// STEP export with exact surfaces (`--enable exact`, which adds STEP to
// File > Export As) and edge fillets and chamfers (`--enable fillet`),
// all off by default as on the command line: off, a
// file means exactly what it means in OpenSCAD. Kept in %LOCALAPPDATA%\NeoSCAD\language.json
// beside agents.json, so every window opened later starts with them.
//
// The names go to the window's document loop (every run, check, measure
// and export) and to its language server (the sketch vocabulary in
// completion, hover and navigation): DocumentSession.SetEnable.

using System.Text.Json;
using System.Text.Json.Serialization;

namespace NeoSCAD.Host;

public sealed record LanguageSettings
{
    /// <summary>Constrained sketches: <c>sketch() { ... }</c>.</summary>
    [JsonPropertyName("sketch")] public bool Sketch { get; init; }

    /// <summary>
    /// Geometry queries: <c>anchor()</c>, <c>child_anchors()</c>,
    /// <c>child_bounds()</c>, <c>child_measure()</c> and <c>child_distance()</c>.
    /// </summary>
    [JsonPropertyName("query")] public bool Query { get; init; }

    /// <summary>STEP export with exact surfaces (File > Export As > STEP).</summary>
    [JsonPropertyName("exact")] public bool Exact { get; init; }

    /// <summary>Edge fillets and chamfers: <c>fillet_edges()</c> and <c>chamfer_edges()</c>.</summary>
    [JsonPropertyName("fillet")] public bool Fillet { get; init; }

    /// <summary>The <c>--enable</c> names runs and the language server take.</summary>
    public string[] Names()
    {
        var names = new List<string>();
        if (Sketch) names.Add("sketch");
        if (Query) names.Add("query");
        if (Exact) names.Add("exact");
        if (Fillet) names.Add("fillet");
        return names.ToArray();
    }

    /// <summary>The file at <paramref name="path"/>, or everything off when it is missing or damaged.</summary>
    public static LanguageSettings Load(string path)
    {
        try
        {
            return JsonSerializer.Deserialize<LanguageSettings>(File.ReadAllBytes(path)) ?? new LanguageSettings();
        }
        catch (Exception e) when (e is IOException or UnauthorizedAccessException or JsonException)
        {
            return new LanguageSettings();
        }
    }

    /// <summary>Written beside the target and moved over it, so a crash leaves the old file.</summary>
    public void Save(string path)
    {
        Directory.CreateDirectory(Path.GetDirectoryName(path)!);
        var tmp = $"{path}.{Environment.ProcessId}.tmp";
        File.WriteAllText(tmp, JsonSerializer.Serialize(this, new JsonSerializerOptions { WriteIndented = true }));
        File.Move(tmp, path, overwrite: true);
    }

    /// <summary>%LOCALAPPDATA%\NeoSCAD\language.json for <paramref name="localAppData"/>.</summary>
    public static string PathIn(string localAppData) => Path.Combine(localAppData, "NeoSCAD", "language.json");
}
