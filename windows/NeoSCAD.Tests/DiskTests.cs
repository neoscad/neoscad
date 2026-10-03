// Another program changing the open document's file
// (NeoSCAD.Host/DocumentSession.Disk.cs) against the real core and a real
// FileSystemWatcher: a clean document reloads in place, a dirty one gets a
// notice, Save asks before overwriting, a deleted file is reported, and the
// window's own save is not mistaken for anyone else's.

using NeoSCAD.Host;
using NeoSCAD.Native;

namespace NeoSCAD.Tests;

public sealed class DiskTests : IDisposable
{
    static readonly Core core = new(new CoreConfig(null, false));
    static readonly TimeSpan Wait = TimeSpan.FromSeconds(20);

    readonly string dir = Directory.CreateTempSubdirectory("neoscad-disk-").FullName;
    readonly QueueDispatcher ui = new();
    readonly ManualTimer timer = new();
    readonly ManualClock clock = new();
    readonly DocumentSession doc;
    readonly string model;

    public DiskTests()
    {
        doc = new DocumentSession(core, ui, timer, clock, dir);
        model = Path.Combine(dir, "model.scad");
        File.WriteAllText(model, "cube(1);\n");
        doc.Open(model);
    }

    public void Dispose()
    {
        doc.Dispose();
        Directory.Delete(dir, true);
    }

    /// <summary>Type at the end of the text, as the editor would report it.</summary>
    void Type(string s)
    {
        var length = (ulong)doc.Text.Length;
        Assert.True(doc.EditorChanged([new Utf16Edit(length, length, s)], EditKind.Edit,
            length + (ulong)s.Length));
    }

    /// <summary>Another program's save: a temporary file renamed over the model, as most editors do.</summary>
    void TheirSave(string text)
    {
        var tmp = Path.Combine(dir, ".model.scad.tmp");
        File.WriteAllText(tmp, text);
        File.Move(tmp, model, overwrite: true);
    }

    [Fact]
    public void ACleanDocumentTakesTheChangeInPlaceWithoutAnEditor()
    {
        string? loaded = null;
        doc.TextLoaded += t => loaded = t;
        TheirSave("cube(2);\n");
        Assert.True(ui.PumpUntil(() => doc.Text == "cube(2);\n", Wait));
        Assert.False(doc.IsDirty);
        Assert.Null(doc.Notice);
        // No editor in step: the copy changed and the editor is sent it.
        Assert.Equal("cube(2);\n", loaded);
        Assert.Equal(SaveOutcome.Saved, doc.Save());
    }

    [Fact]
    public void ACleanDocumentWithAnEditorGetsOneAgentEdit()
    {
        var requests = new List<string>();
        doc.EditorInStep = () => true;
        doc.ReloadRequested += requests.Add;
        TheirSave("cube(2);\n");
        Assert.True(ui.PumpUntil(() => requests.Count > 0, Wait));
        Assert.Equal("""[{"from":[0,5],"insert":"2","to":[0,6]}]""", Assert.Single(requests));
        Assert.Equal("cube(1);\n", doc.Text);
        // The page applies it and reports the change: the document is
        // clean, at the file's text, and Undo would make it edited again.
        Assert.True(doc.EditorChanged([new Utf16Edit(5, 6, "2")], EditKind.Edit, 9));
        Assert.Equal("cube(2);\n", doc.Text);
        Assert.False(doc.IsDirty);
        Assert.True(doc.EditorChanged([new Utf16Edit(5, 6, "1")], EditKind.Undo, 9));
        Assert.True(doc.IsDirty);
    }

    [Fact]
    public void ADirtyDocumentGetsANoticeAndSaveAsksBeforeOverwriting()
    {
        Type("sphere(1);\n");
        TheirSave("cube(3);\n");
        Assert.True(ui.PumpUntil(() => doc.Notice is not null, Wait));
        Assert.Equal(new DiskNotice.Changed(true), doc.Notice);
        Assert.Equal("cube(1);\nsphere(1);\n", doc.Text);
        // Save does not overwrite silently.
        Assert.Equal(SaveOutcome.ChangedOnDisk, doc.Save());
        Assert.Equal("cube(3);\n", File.ReadAllText(model));
        // Keep mine: the notice goes, and Save still asks.
        doc.KeepMine();
        Assert.Null(doc.Notice);
        Assert.Equal(SaveOutcome.ChangedOnDisk, doc.Save());
        // Save Anyway writes, and its own echo is no change.
        Assert.Equal(SaveOutcome.Saved, doc.Save(overwrite: true));
        Assert.Equal("cube(1);\nsphere(1);\n", File.ReadAllText(model));
        Assert.False(doc.IsDirty);
        Assert.False(ui.PumpUntil(() => doc.Notice is not null || doc.IsDirty, TimeSpan.FromMilliseconds(800)));
    }

    [Fact]
    public void ReloadFromTheNoticeTakesTheirText()
    {
        Type("sphere(1);\n");
        TheirSave("cube(4);\n");
        Assert.True(ui.PumpUntil(() => doc.Notice is not null, Wait));
        Assert.True(doc.ReloadFromDisk());
        Assert.Equal("cube(4);\n", doc.Text);
        Assert.False(doc.IsDirty);
        Assert.Null(doc.Notice);
        Assert.Equal(SaveOutcome.Saved, doc.Save());
    }

    [Fact]
    public void ADeletedFileIsReportedAndSavingWritesItAgain()
    {
        File.Delete(model);
        Assert.True(ui.PumpUntil(() => doc.Notice is not null, Wait));
        Assert.Equal(new DiskNotice.Missing(), doc.Notice);
        Assert.Equal(SaveOutcome.Saved, doc.Save());
        Assert.Equal("cube(1);\n", File.ReadAllText(model));
        Assert.Null(doc.Notice);
    }

    [Fact]
    public void TheWindowsOwnSaveIsNotAnotherProgramsChange()
    {
        var requests = 0;
        doc.EditorInStep = () => true;
        doc.ReloadRequested += _ => requests++;
        Type("sphere(2);\n");
        Assert.Equal(SaveOutcome.Saved, doc.Save());
        Assert.False(ui.PumpUntil(() => requests > 0 || doc.Notice is not null, TimeSpan.FromMilliseconds(800)));
        Assert.False(doc.IsDirty);
    }
}
