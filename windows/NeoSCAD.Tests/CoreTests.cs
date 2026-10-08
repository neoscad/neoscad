// The binding against the real core, and the document loop end to end
// without a window: these are what a mismatch between uniffi-bindgen-cs
// and the core's uniffi version would break first (checksums, records
// with defaults, foreign-implemented traits, objects).

using NeoSCAD.Host;
using NeoSCAD.Native;

namespace NeoSCAD.Tests;

public class BindingTests
{
    [Fact]
    public void TheCoreLoadsAndNamesItsVersion() => Assert.False(string.IsNullOrEmpty(NeoScad.CoreVersion()));

    [Fact]
    public void ExamplesComeFromTheCore()
    {
        var examples = NeoScad.Examples();
        Assert.NotEmpty(examples);
        Assert.All(examples, e => Assert.EndsWith(".scad", e.FileName));
        Assert.False(string.IsNullOrWhiteSpace(NeoScad.ExampleSource(examples[0].Id)));
    }

    [Fact]
    public void ExportFormatsIncludeStlAndAViewImage()
    {
        var formats = NeoScad.ExportFormats();
        Assert.Contains(formats, f => f.Id == "stl" && f.Kind == ExportKind.Geometry);
        Assert.Contains(formats, f => f.Kind == ExportKind.ViewImage);
    }

    [Fact]
    public void EditorOffsetsInUtf16BecomeTheCoresUtf8()
    {
        // "é" is one UTF-16 unit and two UTF-8 bytes; "😀" two and four.
        using var text = new EditorText("é😀x");
        var edits = text.Apply([new Utf16Edit(3, 4, "y")]);
        Assert.Equal([new TextEdit(6, 7, "y")], edits);
        Assert.Equal("é😀y", text.Text());
        Assert.Equal(4UL, text.Utf16Length());
        Assert.Equal(7UL, text.ByteLength());
    }

    [Fact]
    public void AnEditOutsideTheTextIsAnInvalidArgument()
    {
        using var text = new EditorText("ab");
        var e = Assert.ThrowsAny<CoreException>(() => text.Apply([new Utf16Edit(5, 6, "")]));
        Assert.IsType<CoreException.InvalidArgument>(e);
        Assert.False(string.IsNullOrEmpty(CoreErrors.Describe(e)));
    }

    [Fact]
    public void TheControllerSchedulesAfterItsDelay()
    {
        // The cast matters: uniffi-bindgen-cs also generates a
        // `DocumentController(ulong pointer)` constructor (wrapping a raw
        // Rust pointer), and a plain `150` binds to that one, not to
        // `(ulong? delayMs)`, and the first call through it crashes the
        // process (docs/windows-app.md, "Bindings").
        using var loop = new DocumentController((ulong?)150);
        loop.Schedule(1000);
        Assert.Equal(1150UL, loop.NextDueMs());
        Assert.Null(loop.Due(1100));
        Assert.Equal(RenderMode.Preview, loop.Due(1150));
    }

    [Fact]
    public void AnObserverImplementedInCSharpIsCalled()
    {
        using var loop = new DocumentController(null);
        var seen = new List<DocumentState>();
        loop.SetObserver(new Observer(seen));
        loop.Schedule(0);
        Assert.NotEmpty(seen);
        Assert.NotNull(seen[^1].DueMs);
    }

    sealed class Observer(List<DocumentState> seen) : DocumentObserver
    {
        public void StateChanged(DocumentState state) => seen.Add(state);
    }

    /// <summary>
    /// uniffi-bindgen-cs writes a Rust `#[uniffi(default = [])]` as a C#
    /// default of null (docs/windows-app.md, "Bindings"), so a request
    /// built in C# must spell out its lists.
    /// </summary>
    [Fact]
    public void EmptyListDefaultsComeOutAsNull()
    {
        var r = new DocumentRequest(RenderMode.Preview);
        Assert.Null(r.Overrides);
        Assert.Null(r.Enable);
    }
}

public class DocumentSessionTests
{
    static readonly Core core = new(new CoreConfig(null, false));

    static readonly ManualClock sharedClock = new();

    static (DocumentSession, QueueDispatcher, ManualTimer, string) Session(ManualClock? clock = null)
    {
        var dir = Directory.CreateTempSubdirectory("neoscad-doc-").FullName;
        var ui = new QueueDispatcher();
        var timer = new ManualTimer();
        return (new DocumentSession(core, ui, timer, clock ?? sharedClock, dir), ui, timer, dir);
    }

    [Fact]
    public void APauseRunsThePreviewAndFeedsConsoleAndReport()
    {
        var clock = new ManualClock();
        var (doc, ui, timer, dir) = Session(clock);
        try
        {
            doc.LoadUntitled("echo(\"hello\"); cube(10);");
            Assert.Equal(TimeSpan.FromMilliseconds(150), timer.Armed);
            // The loop's "now" has not moved; its due time is 150 ms on.
            // Firing early only re-arms, as a timer that woke too soon would.
            timer.Fire();
            Assert.NotNull(timer.Armed);
            Assert.True(doc.Report is RunReport.Idle);
            clock.NowMs += 150;
            timer.Fire();
            Assert.IsType<RunReport.Running>(doc.Report);
            Assert.True(ui.PumpUntil(() => doc.Report is not RunReport.Running, TimeSpan.FromSeconds(60)));
            var rendered = Assert.IsType<RunReport.Rendered>(doc.Report);
            Assert.Equal(RenderMode.Preview, rendered.Mode);
            Assert.Contains(doc.Console, l => l.Kind == ConsoleKind.Echo && l.Text.Contains("\"hello\""));
            Assert.True(doc.LoopState().InSync);
        }
        finally
        {
            doc.Dispose();
            Directory.Delete(dir, true);
        }
    }

    [Fact]
    public void AnErrorInTheModelReachesTheConsole()
    {
        var (doc, ui, _, dir) = Session();
        try
        {
            doc.LoadUntitled("cube(", autorun: false);
            doc.Run(RenderMode.Preview);
            Assert.True(ui.PumpUntil(() => doc.Report is not RunReport.Running, TimeSpan.FromSeconds(60)));
            Assert.Contains(doc.Console, l => l.Kind == ConsoleKind.Error);
        }
        finally
        {
            doc.Dispose();
            Directory.Delete(dir, true);
        }
    }

    /// <summary>
    /// Design > NeoSCAD Extensions: with geometry queries off,
    /// child_bounds() is OpenSCAD's unknown function; turned on, the
    /// document runs again and echoes the child's box.
    /// </summary>
    [Fact]
    public void TheQuerySettingRunsTheDocumentAgainWithQueries()
    {
        var (doc, ui, _, dir) = Session();
        try
        {
            doc.LoadUntitled("module m() { echo(child_bounds(0)); children(0); }\nm() cube([1, 2, 3]);\n", autorun: false);
            doc.Run(RenderMode.Preview);
            Assert.True(ui.PumpUntil(() => doc.Report is not RunReport.Running, TimeSpan.FromSeconds(60)));
            Assert.Contains(doc.Console, l => l.Kind == ConsoleKind.Warning && l.Text.Contains("unknown function 'child_bounds'"));
            Assert.True(doc.SetEnable(new LanguageSettings { Query = true }.Names()));
            Assert.False(doc.SetEnable(["query"]));
            Assert.True(ui.PumpUntil(() => doc.Report is not RunReport.Running
                && doc.Console.Any(l => l.Kind == ConsoleKind.Echo), TimeSpan.FromSeconds(60)));
            Assert.Contains(doc.Console, l => l.Kind == ConsoleKind.Echo && l.Text == "ECHO: [[0, 0, 0], [1, 2, 3]]");
            Assert.DoesNotContain(doc.Console, l => l.Kind == ConsoleKind.Warning);
        }
        finally
        {
            doc.Dispose();
            Directory.Delete(dir, true);
        }
    }

    [Fact]
    public void LanguageSettingsRoundTripAndDefaultToOff()
    {
        var dir = Directory.CreateTempSubdirectory("neoscad-lang-").FullName;
        try
        {
            var path = LanguageSettings.PathIn(dir);
            Assert.Empty(LanguageSettings.Load(path).Names());
            new LanguageSettings { Sketch = true, Query = true, Exact = true, Fillet = true }.Save(path);
            Assert.Equal(["sketch", "query", "exact", "fillet"], LanguageSettings.Load(path).Names());
            File.WriteAllText(path, "{\"sketch\": tru");
            Assert.Empty(LanguageSettings.Load(path).Names());
        }
        finally
        {
            Directory.Delete(dir, true);
        }
    }

    /// <summary>
    /// Another window (another process) saving language.json reaches this
    /// one through the watch; this window's own save, a damaged file and
    /// an unchanged one do not.
    /// </summary>
    [Fact]
    public void LanguageSettingsSavedElsewhereReachTheWatchingWindow()
    {
        var dir = Directory.CreateTempSubdirectory("neoscad-lang-").FullName;
        var ui = new QueueDispatcher();
        var seen = new List<LanguageSettings>();
        var path = LanguageSettings.PathIn(dir);
        var wait = TimeSpan.FromSeconds(10);
        var quiet = TimeSpan.FromMilliseconds(600);
        var watch = new LanguageSettingsWatch(path, new LanguageSettings(), ui, seen.Add);
        try
        {
            // Another window turns queries on.
            new LanguageSettings { Query = true }.Save(path);
            Assert.True(ui.PumpUntil(() => seen.Count == 1, wait));
            Assert.Equal(["query"], seen[0].Names());
            // This window turns sketches on: its own change is not news.
            var mine = new LanguageSettings { Sketch = true, Query = true };
            watch.Saved(mine);
            mine.Save(path);
            Assert.False(ui.PumpUntil(() => seen.Count > 1, quiet));
            // A half-written or damaged file changes nothing.
            File.WriteAllText(path, "{\"sketch\": tru");
            Assert.False(ui.PumpUntil(() => seen.Count > 1, quiet));
            // Another window turns everything off.
            new LanguageSettings().Save(path);
            Assert.True(ui.PumpUntil(() => seen.Count == 2, wait));
            Assert.Empty(seen[1].Names());
        }
        finally
        {
            watch.Dispose();
            Directory.Delete(dir, true);
        }
    }

    [Fact]
    public void EditsMarkTheDocumentDirtyAndUndoingCleansIt()
    {
        var (doc, _, _, dir) = Session();
        try
        {
            doc.LoadUntitled("cube(1);", autorun: false);
            var titles = 0;
            doc.TitleChanged += () => titles++;
            Assert.False(doc.IsDirty);
            Assert.True(doc.EditorChanged([new Utf16Edit(5, 6, "2")], EditKind.Edit, 8));
            Assert.Equal("cube(2);", doc.Text);
            Assert.True(doc.IsDirty);
            Assert.Equal("*Untitled - NeoSCAD", doc.Title);
            Assert.True(doc.EditorChanged([new Utf16Edit(5, 6, "1")], EditKind.Undo, 8));
            Assert.False(doc.IsDirty);
            Assert.Equal(2, titles);
            // A transaction whose length disagrees is refused (the editor
            // then sends its whole text).
            Assert.False(doc.EditorChanged([new Utf16Edit(0, 0, "x")], EditKind.Edit, 100));
        }
        finally
        {
            doc.Dispose();
            Directory.Delete(dir, true);
        }
    }

    [Fact]
    public void SaveAsWritesTheTextAndNamesTheWindow()
    {
        var (doc, _, _, dir) = Session();
        try
        {
            doc.LoadUntitled("cube(1);", autorun: false);
            doc.EditorChanged([new Utf16Edit(8, 8, "\n")], EditKind.Edit, 9);
            var path = Path.Combine(dir, "part.scad");
            doc.SaveAs(path);
            Assert.Equal("cube(1);\n", File.ReadAllText(path));
            Assert.False(doc.IsDirty);
            Assert.Equal("part.scad - NeoSCAD", doc.Title);
            Assert.Equal(path, doc.CorePath);
            Assert.False(File.Exists(path + ".neoscad-tmp"));
        }
        finally
        {
            doc.Dispose();
            Directory.Delete(dir, true);
        }
    }

    [Fact]
    public async Task ExportWritesAnStl()
    {
        var (doc, _, _, dir) = Session();
        try
        {
            doc.LoadUntitled("cube(10);", autorun: false);
            var output = Path.Combine(dir, "cube.stl");
            Assert.Null(await doc.ExportAsync(output, "stl"));
            Assert.StartsWith("solid", File.ReadAllText(output));
        }
        finally
        {
            doc.Dispose();
            Directory.Delete(dir, true);
        }
    }

    /// <summary>
    /// STEP is in File > Export As only with <c>exact</c>, is refused
    /// without it, and with it is written with its report.
    /// </summary>
    [Fact]
    public async Task StepFollowsTheExactSettingAndReportsItsShare()
    {
        var (doc, _, _, dir) = Session();
        try
        {
            Assert.DoesNotContain(NeoScad.ExportFormatsWith([]), f => f.Id == "step");
            Assert.Contains(NeoScad.ExportFormatsWith(["exact"]), f => f.Id == "step");
            doc.LoadUntitled("cube(10);", autorun: false);
            var output = Path.Combine(dir, "cube.step");
            Assert.Contains("--enable exact", await doc.ExportAsync(output, "step"));
            Assert.False(File.Exists(output));
            doc.SetEnable(new LanguageSettings { Exact = true }.Names());
            Assert.Null(await doc.ExportAsync(output, "step"));
            Assert.StartsWith("ISO-10303-21;", File.ReadAllText(output));
            Assert.StartsWith("STEP: 6 of 6 faces exact (100%).", doc.LastStepReport);
        }
        finally
        {
            doc.Dispose();
            Directory.Delete(dir, true);
        }
    }

    [Fact]
    public void AnExampleOpensUntitledUnderItsFileName()
    {
        var (doc, _, _, dir) = Session();
        try
        {
            var example = NeoScad.Examples()[0];
            string? loaded = null;
            doc.TextLoaded += t => loaded = t;
            doc.LoadExample(example);
            Assert.Equal(NeoScad.ExampleSource(example.Id), loaded);
            Assert.Equal(Path.GetFileNameWithoutExtension(example.FileName), doc.DisplayName);
            Assert.False(doc.IsDirty);
        }
        finally
        {
            doc.Dispose();
            Directory.Delete(dir, true);
        }
    }
}
