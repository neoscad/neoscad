// The panels' host logic against the real core: the customizer, check,
// measure, file watching and export (NeoSCAD.Host/DocumentSession.Panels.cs,
// FileWatch.cs). No window and no GPU, so no viewport: the overlay is
// checked through the core's own `view_overlay`.

using System.Xml.Linq;
using NeoSCAD.Host;
using NeoSCAD.Native;

namespace NeoSCAD.Tests;

public sealed class PanelTests : IDisposable
{
    static readonly Core core = new(new CoreConfig(null, false));
    static readonly TimeSpan Wait = TimeSpan.FromSeconds(60);

    readonly string dir = Directory.CreateTempSubdirectory("neoscad-panel-").FullName;
    readonly QueueDispatcher ui = new();
    readonly ManualTimer timer = new();
    readonly ManualClock clock = new();
    readonly DocumentSession doc;

    public PanelTests()
    {
        doc = new DocumentSession(core, ui, timer, clock, dir);
    }

    public void Dispose()
    {
        doc.Dispose();
        Directory.Delete(dir, true);
    }

    void RunAndWait(RenderMode mode = RenderMode.Preview)
    {
        doc.Run(mode);
        Assert.True(ui.PumpUntil(() => doc.Report is not RunReport.Running, Wait));
    }

    void Await(Task task) => Assert.True(ui.PumpUntil(() => task.IsCompleted, Wait));

    /// <summary>Let the pause pass and fire the loop's timer: the preview an edit scheduled.</summary>
    void FirePause()
    {
        Assert.NotNull(timer.Armed);
        clock.NowMs += 1_000;
        timer.Fire();
        Assert.True(ui.PumpUntil(() => doc.Report is not RunReport.Running, Wait));
    }

    static Parameter Find(DocumentSession d, string name) =>
        d.ParameterGroups.SelectMany(g => g.Parameters).First(p => p.Name == name);

    // --- The customizer ---------------------------------------------------------------

    [Fact]
    public void ARunReadsTheParametersAndAnEditRunsWithoutTouchingTheText()
    {
        const string text = "width = 10; // [1:100]\necho(width = width);\ncube(width);\n";
        doc.LoadUntitled(text, autorun: false);
        var rebuilt = 0;
        doc.ParametersChanged += () => rebuilt++;
        RunAndWait();
        Assert.True(ui.PumpUntil(() => doc.ParameterGroups.Count > 0, Wait));
        Assert.Equal(1, rebuilt);
        var width = Find(doc, "width");
        Assert.IsType<ParameterControl.Slider>(width.Control);
        Assert.Equal(new ParameterValue.Number(10), doc.ValueOf(width));

        doc.EditParameter(width, new ParameterEdit.Set(new ParameterValue.Number(42)));
        Assert.Equal(new ParameterValue.Number(42), doc.ValueOf(width));
        Assert.Equal(text, doc.Text);
        Assert.False(doc.IsDirty);
        FirePause();
        Assert.Contains(doc.Console, l => l.Kind == ConsoleKind.Echo && l.Text.Contains("width = 42"));
        // Same parameters, so the panel keeps its controls.
        Assert.True(ui.PumpUntil(() => doc.ParameterValues.ContainsKey("width"), Wait));
        Assert.Equal(1, rebuilt);

        // The text's own value is no override.
        doc.EditParameter(width, new ParameterEdit.Type(10));
        Assert.Empty(doc.ParameterValues);
        // A slider's field takes a number as typed, as OpenSCAD's does.
        doc.EditParameter(width, new ParameterEdit.Type(500));
        Assert.Equal(new ParameterValue.Number(500), doc.ValueOf(width));
        doc.ResetParameters();
        Assert.Empty(doc.ParameterValues);
    }

    [Fact]
    public void ParameterSetsAreSavedBesideTheModelAndApplied()
    {
        doc.LoadUntitled("size = 5; // [1:20]\ncube(size);\n", autorun: false);
        Assert.Null(doc.ParameterSetPath);
        Assert.NotNull(doc.SaveParameterSet("big"));
        var path = Path.Combine(dir, "box.scad");
        doc.SaveAs(path);
        Assert.Equal(Path.Combine(dir, "box.json"), doc.ParameterSetPath);
        RunAndWait();
        Assert.True(ui.PumpUntil(() => doc.ParameterGroups.Count > 0, Wait));
        var size = Find(doc, "size");

        doc.EditParameter(size, new ParameterEdit.Set(new ParameterValue.Number(15)));
        Assert.Null(doc.SaveParameterSet("big"));
        Assert.True(File.Exists(Path.Combine(dir, "box.json")));
        Assert.Equal(["big"], doc.ParameterSets);
        Assert.Equal("big", doc.SelectedParameterSet);

        doc.ResetParameters();
        Assert.Equal(new ParameterValue.Number(5), doc.ValueOf(size));
        Assert.Null(doc.ApplyParameterSet("big"));
        Assert.Equal(new ParameterValue.Number(15), doc.ValueOf(size));
        Assert.Equal("big", doc.SelectedParameterSet);
        Assert.NotNull(doc.ApplyParameterSet("missing"));
    }

    [Fact]
    public void ParameterShapesCompareByContentNotByReference()
    {
        ParameterGroup[] Groups(double v) =>
        [
            new("Size", [new Parameter("v", "", new ParameterControl.Vector(null, null, null),
                new ParameterValue.Vector([v, 2]))]),
        ];
        Assert.True(ParameterShapes.Same(Groups(1), Groups(1)));
        Assert.False(ParameterShapes.Same(Groups(1), Groups(3)));
    }

    // --- Check ------------------------------------------------------------------------------

    [Fact]
    public void ACheckFindsAThinWallAndSelectingItPutsItInTheOverlay()
    {
        doc.LoadUntitled("cube([20, 20, 0.1]);", autorun: false);
        var check = doc.RunCheckAsync();
        Assert.True(doc.CheckRunning);
        Await(check);
        Assert.False(doc.CheckRunning);
        Assert.Null(doc.CheckError);
        var report = Assert.IsType<CheckReport>(doc.CheckReport);
        Assert.NotEmpty(report.Findings);
        Assert.NotEqual("", doc.CheckSummary());

        var first = report.Findings[0];
        doc.SelectFinding(first.Id);
        Assert.Equal(first.Id, doc.SelectedFinding);
        var overlay = doc.Overlay();
        Assert.Equal(first.Id, overlay.Selected);
        Assert.NotEmpty(NeoScad.ViewOverlay(overlay).Markers);
        doc.SelectFinding(null);
        Assert.Null(doc.Overlay().Selected);
    }

    [Fact]
    public void ACheckUsesTheCustomizersValues()
    {
        // Thick by default; the customizer makes it thin.
        doc.LoadUntitled("t = 5; // [0.1:10]\ncube([20, 20, t]);", autorun: false);
        RunAndWait();
        Assert.True(ui.PumpUntil(() => doc.ParameterGroups.Count > 0, Wait));
        var thick = doc.RunCheckAsync();
        Await(thick);
        var before = doc.CheckReport!.Findings.Length;
        doc.SetParameter("t", new ParameterValue.Number(0.1));
        var thin = doc.RunCheckAsync();
        Await(thin);
        Assert.True(doc.CheckReport!.Findings.Length > before);
    }

    // --- Measure ----------------------------------------------------------------------------

    [Fact]
    public void MeasureGivesTheVolumeAndTwoPicksADistance()
    {
        doc.LoadUntitled("cube(10);", autorun: false);
        Await(doc.RunMeasureAsync());
        Assert.Null(doc.MeasureError);
        var model = doc.Measurement?.Model;
        Assert.NotNull(model);
        Assert.Equal(1000, model.Volume, 3);

        // Picking is off: a click is the view's.
        Assert.False(doc.PickAlong([5, 5, 50], [0, 0, -1]));
        doc.Picking = true;
        Assert.True(doc.PickAlong([5, 5, 50], [0, 0, -1]));
        Assert.True(doc.PickAlong([5, 5, -50], [0, 0, 1]));
        Assert.Equal(2, doc.Picks.Count);
        Assert.Equal(10, doc.PickedDistance!.Value, 6);
        Assert.Equal(2, doc.Overlay().Picks.Length);
        // A miss is taken but adds nothing; a third pick starts a new pair.
        Assert.True(doc.PickAlong([50, 50, 50], [0, 0, -1]));
        Assert.Equal(2, doc.Picks.Count);
        Assert.True(doc.PickAlong([5, 5, 50], [0, 0, -1]));
        Assert.Single(doc.Picks);
        doc.ClearPicks();
        Assert.Empty(doc.Picks);
    }

    // --- File watching ----------------------------------------------------------------------

    [Fact]
    public void ChangingAnIncludedFileSchedulesAPreview()
    {
        var include = Path.Combine(dir, "part.scad");
        File.WriteAllText(include, "module part() cube(1);\n");
        var model = Path.Combine(dir, "model.scad");
        File.WriteAllText(model, "include <part.scad>\npart();\n");
        doc.Open(model);
        clock.NowMs += 1_000;
        timer.Fire();
        Assert.True(ui.PumpUntil(() => doc.Report is RunReport.Rendered, Wait));
        Assert.Contains(doc.WatchedFiles, f => Path.GetFileName(f) == "part.scad");
        // The document's own file is watched too, for another program's
        // change to it (DiskTests).
        Assert.Contains(doc.WatchedFiles, f => Path.GetFileName(f) == "model.scad");
        Assert.Null(timer.Armed);

        File.WriteAllText(include, "module part() cube(2);\n");
        Assert.True(ui.PumpUntil(() => timer.Armed is not null, TimeSpan.FromSeconds(20)));
    }

    [Fact]
    public void AWatchCoalescesABurstAndIgnoresOtherFiles()
    {
        var calls = 0;
        using var watch = new FileWatch(ui, () => calls++) { Latency = TimeSpan.FromMilliseconds(200) };
        var watched = Path.Combine(dir, "a.scad");
        File.WriteAllText(watched, "1");
        watch.Watch([watched, Path.Combine(dir, "missing", "b.scad")]);
        Assert.Equal(2, watch.Files.Count);

        File.WriteAllText(Path.Combine(dir, "other.scad"), "x");
        Assert.False(ui.PumpUntil(() => calls > 0, TimeSpan.FromMilliseconds(600)));

        // An editor's save: write a temporary file, then rename it over the original.
        File.WriteAllText(watched, "2");
        var tmp = Path.Combine(dir, "a.scad.tmp");
        File.WriteAllText(tmp, "3");
        File.Move(tmp, watched, overwrite: true);
        Assert.True(ui.PumpUntil(() => calls > 0, TimeSpan.FromSeconds(10)));
        ui.PumpUntil(() => false, TimeSpan.FromMilliseconds(500));
        Assert.Equal(1, calls);

        // Still watched after the rename replaced the file.
        File.WriteAllText(watched, "4");
        Assert.True(ui.PumpUntil(() => calls > 1, TimeSpan.FromSeconds(10)));
    }

    // --- Export -----------------------------------------------------------------------------

    [Fact]
    public async Task ExportWritesEachGeometryFormatWithItsStages()
    {
        doc.LoadUntitled("cube(10);", autorun: false);
        foreach (var (format, ext) in new[] { ("3mf", "3mf"), ("off", "off"), ("obj", "obj"), ("binstl", "stl") })
        {
            var stages = new List<string>();
            var output = Path.Combine(dir, $"cube-{format}.{ext}");
            Assert.Null(await doc.ExportAsync(output, format, s => { lock (stages) stages.Add(s); }));
            Assert.True(new FileInfo(output).Length > 0, format);
            lock (stages) Assert.Contains("geometry", stages);
        }
        Assert.StartsWith("OFF", File.ReadAllText(Path.Combine(dir, "cube-off.off")));
    }

    [Fact]
    public async Task ExportTakesTheCustomizersValues()
    {
        doc.LoadUntitled("w = 10; // [1:100]\ncube([w, 1, 1]);", autorun: false);
        RunAndWait();
        Assert.True(ui.PumpUntil(() => doc.ParameterGroups.Count > 0, Wait));
        doc.SetParameter("w", new ParameterValue.Number(77));
        var output = Path.Combine(dir, "bar.off");
        Assert.Null(await doc.ExportAsync(output, "off"));
        Assert.Contains("77", File.ReadAllText(output));
    }

    [Fact]
    public async Task A2DModelExportsAsSvgButNotAsStl()
    {
        doc.LoadUntitled("square(10);", autorun: false);
        var svg = Path.Combine(dir, "square.svg");
        Assert.Null(await doc.ExportAsync(svg, "svg"));
        Assert.Contains("<svg", File.ReadAllText(svg));
        Assert.NotNull(await doc.ExportAsync(Path.Combine(dir, "square.stl"), "binstl"));
    }

    [Fact]
    public async Task ACancelledExportSaysSoAndWritesNothing()
    {
        doc.LoadUntitled("cube(10);", autorun: false);
        using var cancel = new CancelToken();
        cancel.Cancel();
        var output = Path.Combine(dir, "never.stl");
        Assert.NotNull(await doc.ExportAsync(output, "binstl", cancel: cancel));
        Assert.False(File.Exists(output));
    }

    [Fact]
    public async Task ASnapshotSheetIsAPng()
    {
        doc.LoadUntitled("cube(10);", autorun: false);
        var output = Path.Combine(dir, "sheet.png");
        var failure = await doc.ExportAsync(output, "snapshot");
        // The sheet draws offscreen; a container without a GPU adapter
        // says so rather than writing a file.
        if (failure is null) Assert.Equal(0x89, File.ReadAllBytes(output)[0]);
        else Assert.False(File.Exists(output));
    }
}

public class ShortcutTests
{
    [Fact]
    public void PanelOpensAPanelAndIsNotAFile()
    {
        string[] args = ["--panel", "customizer", "--example", "csg"];
        Assert.Equal("customizer", StartupAction.PanelName(args));
        Assert.Equal(new StartupAction.OpenExample("csg"), StartupAction.Parse(args));
        Assert.IsType<StartupAction.Empty>(StartupAction.Parse(["--panel", "check"]));
        Assert.Null(StartupAction.PanelName(["--panel", "nonsense"]));
    }

    [Fact]
    public void ChordsNameTheirCommands()
    {
        Assert.Equal(Shortcuts.SaveAs, Shortcuts.CommandFor("Ctrl+Shift+S"));
        Assert.Equal(Shortcuts.Save, Shortcuts.CommandFor("Ctrl+S"));
        Assert.Null(Shortcuts.CommandFor("Ctrl+Z"));
        Assert.Equal(Shortcuts.Forwarded.Count, Shortcuts.Forwarded.Select(s => s.Chord).Distinct().Count());
    }

    [Fact]
    public void TheKeyScriptCarriesTheTable()
    {
        var script = EditorPage.KeyScript(Shortcuts.Forwarded);
        // The table is JSON, which escapes "+" (a valid JavaScript string either way).
        Assert.Contains(System.Text.Json.JsonSerializer.Serialize("Ctrl+Shift+K") + ":\"check\"", script);
        Assert.DoesNotContain("TABLE", script);
        Assert.Contains("addEventListener(\"keydown\"", script);
    }

    /// <summary>
    /// Every forwarded chord is also a menu item's KeyboardAccelerator, so
    /// the key does the same whether the editor has the focus or not.
    /// </summary>
    [Fact]
    public void EveryForwardedChordIsAMenuAccelerator()
    {
        var xaml = XDocument.Load(Path.Combine(Repo.Root, "windows", "NeoSCAD.App", "MainWindow.xaml"));
        var chords = xaml.Descendants().Where(e => e.Name.LocalName == "KeyboardAccelerator").Select(e =>
        {
            var mods = ((string?)e.Attribute("Modifiers") ?? "").Split(',', StringSplitOptions.TrimEntries);
            return new Shortcut("", ((string)e.Attribute("Key")!).ToLowerInvariant(),
                mods.Contains("Control"), mods.Contains("Shift"), mods.Contains("Menu")).Chord;
        }).ToHashSet();
        foreach (var s in Shortcuts.Forwarded) Assert.Contains(s.Chord, chords);
    }
}

public class ExportTextTests
{
    [Fact]
    public void EveryStageReadsAsASentence()
    {
        Assert.Equal("Building the geometry…", ExportText.Describe("geometry"));
        Assert.Equal("Starting…", ExportText.Describe(null));
        Assert.Equal("Writing…", ExportText.Describe("writing"));
    }
}

static class Repo
{
    /// <summary>The checkout, found from the test's own folder.</summary>
    public static string Root
    {
        get
        {
            for (var d = new DirectoryInfo(AppContext.BaseDirectory); d is not null; d = d.Parent)
            {
                if (File.Exists(Path.Combine(d.FullName, "windows", "NeoSCAD.sln"))) return d.FullName;
            }
            throw new InvalidOperationException("the checkout was not found");
        }
    }
}
