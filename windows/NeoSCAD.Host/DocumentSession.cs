// One document window's state and loop: the port of the macOS app's
// SCADDocument + DocumentLoop.swift, without the UI.
//
// After each pause in typing the document runs once (Core.RunDocument), and
// that one run feeds the window: the evaluation's diagnostics go to the
// editor's language server (the markers), the model to the 3D view, and
// the console's lines and the status line come back with the result. When
// to run, which run is current and whether the core has the text is the
// core's `DocumentController` (crates/client/src/document_loop.rs, shared
// with the macOS and Linux apps); this class keeps the one timer, the text
// and the dirty state, and makes the core calls the controller's plans ask
// for.
//
// Threads: every member is called on the UI thread; runs go to the thread
// pool and their results come back through the dispatcher.

using System.Text;
using NeoSCAD.Native;

namespace NeoSCAD.Host;

/// <summary>What the status line shows about the last run.</summary>
public abstract record RunReport
{
    public sealed record Idle : RunReport;
    public sealed record Running(RenderMode Mode) : RunReport;
    public sealed record Rendered(RenderResult Result, RenderMode Mode, string Summary) : RunReport;
    public sealed record Failed(string Message) : RunReport;
}

public sealed partial class DocumentSession : IDisposable
{
    public const string AppName = "NeoSCAD";

    readonly IUiDispatcher ui;
    readonly IUiTimer timer;
    readonly IMonotonicClock clock;
    readonly Core? core;
    readonly DocumentController loop;
    readonly EditorText storage = new("");
    int changeCount;
    string? untitledPath;
    CancellationTokenSource? runCancel;
    readonly FileWatch watch;
    bool closed;

    /// <summary>Untitled paths in use by open windows (unique per process).</summary>
    static readonly HashSet<string> untitledPaths = [];

    /// <param name="core">Null when the core did not start (<see cref="CoreService.Error"/>).</param>
    /// <param name="documentsDirectory">Where an untitled document lives for its includes.</param>
    public DocumentSession(Core? core, IUiDispatcher ui, IUiTimer timer, IMonotonicClock clock,
        string documentsDirectory)
    {
        this.core = core;
        this.ui = ui;
        this.timer = timer;
        this.clock = clock;
        DocumentsDirectory = documentsDirectory;
        loop = new DocumentController(null);
        watch = new FileWatch(ui, FilesChanged, DocumentFileChanged);
    }

    // --- What the window shows ------------------------------------------------

    /// <summary>The file on disk; null while untitled.</summary>
    public string? FilePath { get; private set; }

    /// <summary>The name shown for an untitled document ("Untitled", an example's).</summary>
    public string UntitledName { get; private set; } = "Untitled";

    public string DocumentsDirectory { get; set; }

    /// <summary>Run as soon as text is loaded (off for examples marked not to autorun).</summary>
    public bool RunsWhenTextIsReplaced { get; set; } = true;

    public string DisplayName => FilePath is { } p ? Path.GetFileName(p) : UntitledName;

    /// <summary>Unsaved changes: edits minus undos since the last save or load.</summary>
    public bool IsDirty => changeCount != 0;

    /// <summary>The window title, Windows style: "*model.scad - NeoSCAD" when dirty.</summary>
    public string Title => Titles.Window(DisplayName, IsDirty);

    public RunReport Report { get; private set; } = new RunReport.Idle();
    public IReadOnlyList<ConsoleLine> Console { get; private set; } = [];

    /// <summary>The document's text (the copy the editor keeps in step).</summary>
    public string Text => storage.Text();

    /// <summary>The 3D view the runs put their model into; set by the window.</summary>
    public Viewport? Viewport { get; set; }

    /// <summary>The editor's language server bridge; set by the window.</summary>
    public LanguageBridge? Language { get; set; }

    public event Action? TitleChanged;
    public event Action? ReportChanged;
    public event Action? ConsoleChanged;
    /// <summary>The whole text changed (open, new, example): the editor must load it.</summary>
    public event Action<string>? TextLoaded;
    /// <summary>A run is about to read the text: the page's language client should send its version now.</summary>
    public event Action? LanguageSyncRequested;
    /// <summary>A run put a model into the view.</summary>
    public event Action? ModelShown;

    /// <summary>The path the core knows the document by: the file, or a unique untitled path.</summary>
    public string CorePath => FilePath ?? UntitledPath;

    string UntitledPath
    {
        get
        {
            if (untitledPath is { } p) return p;
            string path;
            lock (untitledPaths)
            {
                try
                {
                    path = core?.UntitledPath(DocumentsDirectory, UntitledName, [.. untitledPaths])
                        ?? Path.Combine(DocumentsDirectory, UntitledName + ".scad");
                }
                catch (CoreException)
                {
                    path = Path.Combine(DocumentsDirectory, UntitledName + ".scad");
                }
                untitledPaths.Add(path);
            }
            untitledPath = path;
            return path;
        }
    }

    /// <summary>The URI the language server knows the document by.</summary>
    public string LanguageUri => new Uri(CorePath).AbsoluteUri;

    // --- Loading and saving ---------------------------------------------------

    /// <summary>Show <paramref name="text"/> as an untitled document called <paramref name="name"/>.</summary>
    public void LoadUntitled(string text, string name = "Untitled", bool autorun = true)
    {
        ReleaseUntitledPath();
        FilePath = null;
        UntitledName = name;
        RunsWhenTextIsReplaced = autorun;
        disk.Forget();
        runFiles = [];
        Rewatch();
        SetNotice(null);
        Replace(text);
        RefreshParameterSets();
    }

    /// <summary>An example from the core (File > Examples): untitled, named after its file.</summary>
    public void LoadExample(Example example)
    {
        var text = NeoScad.ExampleSource(example.Id);
        loop.SetParts(example.Parts);
        LoadUntitled(text, Path.GetFileNameWithoutExtension(example.FileName), example.Autorun);
    }

    /// <summary>Open a file from disk.</summary>
    public void Open(string path)
    {
        // Read as bytes, so the file is known by exactly what it held: the
        // watcher's next event is then told from another program's write
        // (DocumentSession.Disk.cs).
        var bytes = File.ReadAllBytes(path);
        var text = Decode(bytes);
        ReleaseUntitledPath();
        var old = FilePath;
        FilePath = Path.GetFullPath(path);
        RunsWhenTextIsReplaced = true;
        if (old != FilePath) CloseInCore(loop.SetPath(FilePath));
        disk.Loaded(bytes);
        runFiles = [];
        Rewatch();
        SetNotice(null);
        Replace(text);
        RefreshParameterSets();
    }

    /// <summary>Write the text to <paramref name="path"/> (Save As when it differs from the current file).</summary>
    public void SaveAs(string path)
    {
        path = Path.GetFullPath(path);
        var bytes = new UTF8Encoding(false).GetBytes(storage.Text());
        AtomicWrite(path, bytes);
        // What the file holds now: the watcher's report of this write is
        // recognised as the window's own.
        disk.Saved(bytes);
        SetNotice(null);
        if (path != FilePath)
        {
            ReleaseUntitledPath();
            FilePath = path;
            // The core knew the text under the old path; the controller
            // sends it again under the new one and closes the old buffer,
            // which would otherwise shadow the file still at that path.
            CloseInCore(loop.SetPath(path));
            // Parameter sets live beside the file (`name.json`), so a new
            // name means a new list, and an untitled document gets its
            // first one.
            RefreshParameterSets();
            Rewatch();
        }
        changeCount = 0;
        TitleChanged?.Invoke();
    }

    static void AtomicWrite(string path, byte[] bytes)
    {
        // A crash or a full disk mid-write must not leave a half file where
        // the model was: write beside it, then replace it in one step.
        var tmp = path + ".neoscad-tmp";
        File.WriteAllBytes(tmp, bytes);
        File.Move(tmp, path, overwrite: true);
    }

    void Replace(string text)
    {
        storage.Replace(text);
        TextRevised();
        changeCount = 0;
        loop.TextReplaced();
        TextLoaded?.Invoke(text);
        TitleChanged?.Invoke();
        // The customizer shows the new text's parameters now, not after a
        // first run that an example marked not to autorun may never get.
        RefreshParameters();
        if (RunsWhenTextIsReplaced) SchedulePreview();
    }

    void ReleaseUntitledPath()
    {
        if (untitledPath is null) return;
        lock (untitledPaths) untitledPaths.Remove(untitledPath);
        untitledPath = null;
    }

    void CloseInCore(string? path)
    {
        if (path is null || core is null) return;
        try
        {
            core.Close(path);
        }
        catch (CoreException)
        {
        }
    }

    // --- The editor's changes -------------------------------------------------

    /// <summary>
    /// One editor transaction (UTF-16 offsets): apply it to the copy,
    /// count it for the dirty state and forward it to the core. False if it
    /// does not fit the copy, which the editor's text then replaces.
    /// </summary>
    public bool EditorChanged(Utf16Edit[] edits, EditKind kind, ulong length)
    {
        TextEdit[] coreEdits;
        try
        {
            coreEdits = storage.Apply(edits);
        }
        catch (CoreException)
        {
            return false;
        }
        // Counted before the length check: the copy has changed either
        // way, and a mismatch is followed by the editor's whole text.
        TextRevised();
        if (storage.Utf16Length() != length) return false;
        var wasDirty = IsDirty;
        changeCount += kind == EditKind.Undo ? -1 : 1;
        if (IsDirty != wasDirty) TitleChanged?.Invoke();
        ReloadLanded();
        var state = loop.State();
        if (state.InSync && state.Path is { } path && core is not null)
        {
            try
            {
                var info = core.Edit(path, coreEdits);
                // The byte count is a cheap check that both copies agree.
                if (info.Length != storage.ByteLength()) loop.TextReplaced();
            }
            catch (CoreException)
            {
                loop.TextReplaced();
            }
        }
        SchedulePreview();
        return true;
    }

    /// <summary>
    /// The copies disagreed, so the editor's whole text replaced the
    /// document's. The editor had changes this copy missed, so it counts
    /// as one: marking it edited when it is not costs a needless save,
    /// while missing a change would lose it.
    /// </summary>
    public void EditorReplacedText(string text)
    {
        storage.Replace(text);
        TextRevised();
        var wasDirty = IsDirty;
        changeCount = changeCount == 0 ? 1 : changeCount;
        if (IsDirty != wasDirty) TitleChanged?.Invoke();
        ReloadLanded();
        loop.TextReplaced();
        SchedulePreview();
    }

    // --- The loop ---------------------------------------------------------------

    /// <summary>A preview once typing pauses; each call restarts the wait.</summary>
    public void SchedulePreview()
    {
        if (closed) return;
        loop.Schedule(clock.NowMs);
        ArmTimer();
    }

    /// <summary>The one timer: fire at the loop's next due run.</summary>
    void ArmTimer()
    {
        timer.Stop();
        if (loop.NextDueMs() is not { } due) return;
        var now = clock.NowMs;
        var wait = due > now ? due - now : 0;
        timer.Start(TimeSpan.FromMilliseconds(wait), () =>
        {
            if (closed) return;
            if (loop.Due(clock.NowMs) is { } mode) Run(mode);
            else ArmTimer();
        });
    }

    /// <summary>Run the document once in <paramref name="mode"/> (F5 preview, F6 render).</summary>
    public void Run(RenderMode mode)
    {
        if (closed) return;
        timer.Stop();
        if (core is null)
        {
            SetReport(new RunReport.Failed($"The core did not start: {CoreService.Error}"));
            return;
        }
        var path = CorePath;
        if (loop.BeginRun(mode, path) is not { } plan) return;
        CloseInCore(plan.Close);
        if (plan.SendText)
        {
            try
            {
                core.Update(path, storage.Text());
                loop.TextSent();
            }
            catch (CoreException e)
            {
                SetReport(new RunReport.Failed(CoreErrors.Describe(e)));
                return;
            }
        }
        LanguageSyncRequested?.Invoke();
        SetReport(new RunReport.Running(mode));
        runCancel?.Cancel();
        var cancel = runCancel = new CancellationTokenSource();
        var viewport = Viewport;
        var language = Language;
        var listener = language is null ? null : new EarlyMarkers(ui, language);
        // `#[uniffi(default = [])]` fields come out of uniffi-bindgen-cs as
        // `= null`, which its converters would dereference; the plan's own
        // request is built by Rust and has real arrays, but spell them out
        // in case a host builds one.
        var request = plan.Request with
        {
            Overrides = plan.Request.Overrides ?? [],
            Enable = plan.Request.Enable ?? [],
        };
        CoreService.Run(() => core.RunDocument(path, request, viewport, language?.Server, listener))
            .ContinueWith(t => ui.Post(() => Finished(t, plan.Generation, mode, cancel.Token)),
                TaskScheduler.Default);
    }

    void Finished(Task<DocumentResult> t, ulong generation, RenderMode mode, CancellationToken cancel)
    {
        if (closed || cancel.IsCancellationRequested || !loop.IsCurrent(generation)) return;
        if (t.Exception?.InnerException is { } e)
        {
            // A newer run took over; it reports instead.
            if (e is CoreException.Cancelled) return;
            SetReport(new RunReport.Failed(CoreErrors.Describe(e)));
            return;
        }
        var r = t.Result;
        if (r.Shown) ModelShown?.Invoke();
        Language?.DeliverPublications(r.Language);
        Console = r.Console;
        ConsoleChanged?.Invoke();
        // The files this run read (includes, uses, imports) are watched
        // from now on, with the document's own file (whose changes are a
        // reload or a notice, not a re-run: DocumentSession.Disk.cs), and
        // the customizer reads the parameters of the text that ran.
        runFiles = r.Files ?? [];
        Rewatch();
        RefreshParameters();
        string summary;
        try
        {
            summary = NeoScad.DescribeRender(r.Render, mode);
        }
        catch (CoreException)
        {
            summary = "";
        }
        SetReport(new RunReport.Rendered(r.Render, mode, summary));
        // Auto check follows renders, not previews, as on macOS: a check
        // renders the model itself, so after every pause in typing it
        // would double the work.
        if (mode == RenderMode.Render && CheckAfterRender) _ = RunCheckAsync();
    }

    /// <summary>
    /// A file the last run read changed on disk (an include saved by
    /// another editor): the last render runs again now, or a preview after
    /// the usual pause.
    /// </summary>
    public void FilesChanged()
    {
        if (closed) return;
        AppLog.Write("a watched file changed");
        try
        {
            if (loop.FilesChanged(clock.NowMs) is { } mode) Run(mode);
            else ArmTimer();
        }
        catch (CoreException)
        {
        }
    }

    /// <summary>The files watched: the last run's and the document's own.</summary>
    public IReadOnlyCollection<string> WatchedFiles => watch.Files;

    void SetReport(RunReport report)
    {
        Report = report;
        ReportChanged?.Invoke();
    }

    /// <summary>The loop's state (tests, diagnostics).</summary>
    public DocumentState LoopState() => loop.State();

    public void Dispose()
    {
        if (closed) return;
        closed = true;
        timer.Stop();
        runCancel?.Cancel();
        watch.Dispose();
        CancelPanels();
        Language?.Stop();
        try
        {
            loop.Close();
            if (loop.State().Path is { } p) CloseInCore(p);
        }
        catch (CoreException)
        {
        }
        ReleaseUntitledPath();
        storage.Dispose();
        disk.Dispose();
        loop.Dispose();
    }

    /// <summary>The evaluation's markers, before the geometry is built.</summary>
    sealed class EarlyMarkers(IUiDispatcher ui, LanguageBridge language) : DocumentListener
    {
        public void Language(string[] messages) => ui.Post(() => language.DeliverPublications(messages));
    }
}

public static class Titles
{
    /// <summary>
    /// A window title as Windows apps write it (Notepad, Paint): the
    /// document, then the app, with an asterisk before an edited document.
    /// </summary>
    public static string Window(string document, bool dirty) =>
        $"{(dirty ? "*" : "")}{document} - {DocumentSession.AppName}";
}
