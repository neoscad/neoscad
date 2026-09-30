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
