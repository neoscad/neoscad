// The window's side of AI agents (NeoSCAD.Host's AgentSettings,
// AgentDocumentHost, AgentConnection's indicator, AgentSetup, and the
// editor protocol's agent scripts): consent that stays off until given,
// the link's requests mapped onto a real DocumentSession with a fake
// editor, the revision counter, "ask before applying", and each setup
// row's flow with a fake backend.
//
// AgentSetupMachineTests and AgentEndToEndTests (below) use the real core
// on the machine: a stand-in `claude`, a scratch %APPDATA%, and the real
// `neoscad mcp` against the link.

using System.Text.Json;
using NeoSCAD.Host;
using NeoSCAD.Native;
using Xunit.Abstractions;

namespace NeoSCAD.Tests;

/// <summary>A page that does what the bundle's agentEdit does, to the session's copy.</summary>
sealed class FakeAgentEditor(DocumentSession session) : IAgentEditor
{
    public bool InStep { get; set; } = true;
    public AgentEditorOutcome Outcome { get; set; } = AgentEditorOutcome.Applied;
    public EditorSelection? Selection { get; set; }
    public List<string> Applied { get; } = [];
    public List<(EditorPosition, EditorPosition)> Revealed { get; } = [];

    public Task<AgentEditorOutcome> ApplyAsync(string editsJson)
    {
        Applied.Add(editsJson);
        if (Outcome == AgentEditorOutcome.Applied)
        {
            // The page applies the edits and reports the change, which the
            // session counts as one edit.
            var edits = JsonSerializer.Deserialize<JsonElement>(editsJson).EnumerateArray()
                .Select(e => new ReloadEdit(e.GetProperty("from")[0].GetUInt64(), e.GetProperty("from")[1].GetUInt64(),
                    e.GetProperty("to")[0].GetUInt64(), e.GetProperty("to")[1].GetUInt64(),
                    e.GetProperty("insert").GetString()!))
                .ToArray();
            session.EditorReplacedText(NeoScad.ApplyReloadEdits(session.Text, edits));
        }
        return Task.FromResult(Outcome);
    }

    public Task<EditorSelection?> SelectionAsync() => Task.FromResult(Selection);

    public Task<bool> RevealAsync(EditorPosition from, EditorPosition to)
    {
        Revealed.Add((from, to));
        return Task.FromResult(true);
    }
}

[Collection("Agents")]
public sealed class AgentTests : IDisposable
{
    static readonly Core core = new(new CoreConfig(null, false));
    static readonly TimeSpan Wait = TimeSpan.FromSeconds(20);

    readonly string dir = Directory.CreateTempSubdirectory("neoscad-agent-").FullName;
    readonly QueueDispatcher ui = new();
    readonly ManualTimer timer = new();
    readonly ManualClock clock = new();
    readonly DocumentSession doc;
    readonly FakeAgentEditor editor;
    readonly AgentDocumentHost host;
    readonly ITestOutputHelper output;

    public AgentTests(ITestOutputHelper output)
    {
        this.output = output;
        doc = new DocumentSession(core, ui, timer, clock, dir);
        var model = Path.Combine(dir, "gear.scad");
        File.WriteAllText(model, "teeth = 12;\ncube(teeth);\n");
        doc.Open(model);
        editor = new FakeAgentEditor(doc);
        host = new AgentDocumentHost(doc, ui, editor, () => null);
    }

    public void Dispose()
    {
        doc.Dispose();
        Directory.Delete(dir, true);
    }

    /// <summary>Call the host as the link does, on a thread of its own, pumping the UI thread meanwhile.</summary>
    T OnLink<T>(Func<T> call)
    {
        var task = Task.Run(call);
        Assert.True(ui.PumpUntil(() => task.IsCompleted, Wait), "the host did not answer");
        return task.GetAwaiter().GetResult();
    }

    static AgentEditRequest Change(ulong version, uint line, uint from, uint to, string insert) =>
        new(version, [new AgentTextEdit(new EditorPosition(line, from), new EditorPosition(line, to), insert)],
            $"line {line + 1}", "Claude Code");

    static string Message(AgentHostException e) => Assert.IsType<AgentHostException.Refused>(e).message;

    // --- Settings ---------------------------------------------------------------------

    [Fact]
    public void AgentsAreOffUntilAllowedAndADamagedFileIsNoConsent()
    {
        var path = AgentSettings.PathIn(dir);
        Assert.False(AgentSettings.Load(path).Allowed);
        Directory.CreateDirectory(Path.GetDirectoryName(path)!);
        File.WriteAllText(path, "{\"allowed\": tru");
        Assert.False(AgentSettings.Load(path).Allowed);

        var on = new AgentSettings().WithAllowed(true);
        on.Save(path);
        Assert.True(AgentSettings.Load(path).Allowed);
        Assert.Contains("\"allowed\": true", File.ReadAllText(path));
        // Turning it off hides the control; "Not now" before ever
        // allowing does not.
        Assert.True(on.WithAllowed(false).TurnedOff);
        Assert.False(new AgentSettings().WithAllowed(false).TurnedOff);
        Assert.False(on.WithAllowed(false).WithAllowed(true).TurnedOff);
    }

    [Fact]
    public void TheIndicatorFollowsTheSpec()
    {
        var off = new AgentSettings();
        Assert.Equal(new AgentIndicator(AgentIndicatorState.Idle, "Connect your AI agent",
            "Let an AI agent such as Claude Code work on this model"), AgentIndicator.For(off, null));
        Assert.Equal(AgentIndicatorState.Hidden,
            AgentIndicator.For(off.WithAllowed(true).WithAllowed(false), null).State);

        var on = off.WithAllowed(true);
        AgentStatus Status(params AgentClient[] clients) => new(true, true, "pipe", clients, null);
        Assert.Equal(AgentIndicatorState.Idle, AgentIndicator.For(on, Status()).State);
        var one = AgentIndicator.For(on, Status(new AgentClient(1, "Claude Code", null, null)));
        Assert.Equal((AgentIndicatorState.Connected, "Claude Code connected"), (one.State, one.Text));
        var busy = AgentIndicator.For(on, Status(new AgentClient(1, "Claude Code", "is editing", 1)));
        Assert.Equal((AgentIndicatorState.Working, "Claude Code is editing"), (busy.State, busy.Text));
        var two = AgentIndicator.For(on, Status(new AgentClient(1, "Claude Code", null, null),
            new AgentClient(2, null, null, null)));
        Assert.Equal("2 agents connected", two.Text);
        Assert.Contains("An agent", two.Tooltip);
        Assert.Contains("couldn't listen", AgentIndicator.For(on, new AgentStatus(true, false, null, [], "no pipe")).Tooltip);
    }

    // --- The editor's scripts ------------------------------------------------------------

    [Fact]
    public void TheAgentsEditIsCheckedInThePageAndItsAnswersParse()
    {
        Assert.Equal("""NeoSCADEditor.agentEdit([{"from":[0,0],"to":[0,1],"insert":"x"}], 8000, 7)""",
            EditorScript.GuardedAgentEdit("""[{"from":[0,0],"to":[0,1],"insert":"x"}]""", 7));
        Assert.Equal(new AgentEditReply.Applied(8),
            EditorReply.AgentEdit("""{"version":8,"undoDepth":1,"redoDepth":0,"marks":1}"""));
        // The bundle's refusal: its state as it is, `stale`, nothing marked.
        Assert.Equal(new AgentEditReply.Moved(),
            EditorReply.AgentEdit("""{"version":9,"undoDepth":1,"redoDepth":0,"stale":true,"marks":0}"""));
        Assert.Null(EditorReply.AgentEdit("null"));
        Assert.Null(EditorReply.AgentEdit(null));
        Assert.Equal(new EditorSelection(new EditorPosition(1, 2), new EditorPosition(3, 4)),
            EditorReply.Selection("""{"anchor":[1,2],"head":[3,4]}"""));
        Assert.Null(EditorReply.Selection("""{"anchor":[1],"head":[3,4]}"""));
        Assert.Equal("NeoSCADEditor.revealRange(1, 2, 3, 4)", EditorScript.RevealRange(1, 2, 3, 4));
    }

    [Fact]
    public async Task AWaitForThePagesVersionEndsWhenTheChangeArrives()
    {
        var sync = new EditorSync { Apply = (_, _, _) => true };
        sync.Loaded(3);
        Assert.True(sync.WaitForAsync(3, Wait).IsCompleted);
        var wait = sync.WaitForAsync(4, Wait);
        Assert.False(wait.IsCompleted);
        sync.Changes(new EditorMessage.Changes(3, 4, [(0, 0, "x")], EditKind.Edit, 1, 1, 0));
        await wait.WaitAsync(Wait);
        // A change that never comes ends at the timeout, not never.
        var lost = sync.WaitForAsync(9, TimeSpan.FromMilliseconds(50));
        await lost.WaitAsync(Wait);
    }

    // --- The host -----------------------------------------------------------------------

    [Fact]
    public void ReadGivesTheWindowsTextRevisionAndSelection()
    {
        editor.Selection = new EditorSelection(new EditorPosition(1, 0), new EditorPosition(1, 4));
        var state = OnLink(() => host.Read(AgentDocumentHost.DocumentId));
        Assert.Equal("teeth = 12;\ncube(teeth);\n", state.Text);
        Assert.Equal(doc.Revision, state.Version);
        Assert.Equal(editor.Selection, state.Selection);
        Assert.Empty(state.Overrides);

        // Unsaved typing is what the agent reads, at a new revision.
        var before = doc.Revision;
        Assert.True(doc.EditorChanged([new Utf16Edit(8, 10, "16")], EditKind.Edit, 25));
        state = OnLink(() => host.Read(AgentDocumentHost.DocumentId));
        Assert.Equal("teeth = 16;\ncube(teeth);\n", state.Text);
        Assert.Equal(before + 1, state.Version);

        var e = Assert.ThrowsAny<AgentHostException>(() => OnLink(() => host.Read(2)));
        Assert.Equal("the document was closed", Message(e));
    }

    [Fact]
    public void AnEditIsOneStepThroughTheEditorAtTheVersionTheAgentRead()
    {
        var v = doc.Revision;
        var r = OnLink(() => host.Edit(1, Change(v, 0, 8, 10, "16")));
        Assert.Equal(new AgentEditOutcome.Applied(v + 1), r);
        Assert.Equal("""[{"from":[0,8],"insert":"16","to":[0,10]}]""", Assert.Single(editor.Applied));
        Assert.Equal("teeth = 16;\ncube(teeth);\n", doc.Text);
        Assert.Equal(v + 1, doc.Revision);
        Assert.True(doc.IsDirty);

        // The old version is stale now, and nothing reaches the editor.
        r = OnLink(() => host.Edit(1, Change(v, 0, 8, 10, "20")));
        Assert.Equal(new AgentEditOutcome.Stale(v + 1), r);
        Assert.Single(editor.Applied);

        // The page had a keystroke the window had not seen yet.
        editor.Outcome = AgentEditorOutcome.Moved;
        r = OnLink(() => host.Edit(1, Change(v + 1, 0, 8, 10, "20")));
        Assert.Equal(new AgentEditOutcome.Stale(v + 1), r);
        Assert.Equal("teeth = 16;\ncube(teeth);\n", doc.Text);

        editor.Outcome = AgentEditorOutcome.Failed;
        var e = Assert.ThrowsAny<AgentHostException>(() => OnLink(() => host.Edit(1, Change(v + 1, 0, 8, 10, "20"))));
        Assert.Contains("read the document and try again", Message(e));
    }

    [Fact]
    public void WithoutAnEditorTheCopyTakesTheEditAndTheEditorGetsItLater()
    {
        editor.InStep = false;
        string? loaded = null;
        doc.TextLoaded += t => loaded = t;
        var v = doc.Revision;
        var r = OnLink(() => host.Edit(1, Change(v, 1, 5, 10, "teeth * 2")));
        Assert.Equal(new AgentEditOutcome.Applied(v + 1), r);
        Assert.Equal("teeth = 12;\ncube(teeth * 2);\n", doc.Text);
        Assert.Equal(doc.Text, loaded);
        Assert.True(doc.IsDirty);
        Assert.Empty(editor.Applied);
        // A preview of the agent's text is due.
        Assert.NotNull(timer.Armed);
    }

    [Fact]
    public void AskingFirstAppliesOnlyWhatTheUserAccepts()
    {
        var asked = new List<string>();
        var answer = false;
        host.AskBeforeEdits = () => true;
        host.Approve = (edit, _) =>
        {
            asked.Add($"{edit.Client}: {edit.Summary}");
            return Task.FromResult(answer);
        };
        var v = doc.Revision;
        Assert.Equal(new AgentEditOutcome.Declined(), OnLink(() => host.Edit(1, Change(v, 0, 8, 10, "16"))));
        Assert.Equal("teeth = 12;\ncube(teeth);\n", doc.Text);
        answer = true;
        Assert.Equal(new AgentEditOutcome.Applied(v + 1), OnLink(() => host.Edit(1, Change(v, 0, 8, 10, "16"))));
        Assert.Equal(["Claude Code: line 1", "Claude Code: line 1"], asked);

        // The user typed while the question was up: stale, not applied.
        host.Approve = (_, _) =>
        {
            Assert.True(doc.EditorChanged([new Utf16Edit(0, 0, " ")], EditKind.Edit, 26));
            return Task.FromResult(true);
        };
        Assert.Equal(new AgentEditOutcome.Stale(v + 2), OnLink(() => host.Edit(1, Change(v + 1, 0, 8, 10, "20"))));
    }

    [Fact]
    public void AnUnansweredQuestionIsDeclinedBeforeTheAgentGivesUp()
    {
        var saved = AgentDocumentHost.ApprovalWait;
        AgentDocumentHost.ApprovalWait = TimeSpan.FromMilliseconds(200);
        try
        {
            host.AskBeforeEdits = () => true;
            host.Approve = (_, cancel) =>
            {
                var never = new TaskCompletionSource<bool>();
                cancel.Register(() => never.TrySetCanceled());
                return never.Task;
            };
            Assert.Equal(new AgentEditOutcome.Declined(),
                OnLink(() => host.Edit(1, Change(doc.Revision, 0, 8, 10, "16"))));
        }
        finally
        {
            AgentDocumentHost.ApprovalWait = saved;
        }
    }

    [Fact]
    public void RevealGoesToTheEditorAndTheViewNeedsAView()
    {
        OnLink(() =>
        {
            host.Reveal(1, new EditorPosition(1, 0), new EditorPosition(1, 4));
            return true;
        });
        Assert.Equal((new EditorPosition(1, 0), new EditorPosition(1, 4)), Assert.Single(editor.Revealed));
        editor.InStep = false;
        Assert.ThrowsAny<AgentHostException>(() => OnLink(() =>
        {
            host.Reveal(1, new EditorPosition(0, 0), new EditorPosition(0, 0));
            return true;
        }));
        // No view (no graphics adapter): said so, on the link's thread.
        var e = Assert.ThrowsAny<AgentHostException>(() =>
            host.Camera(1, new AgentCameraChange("top", false, null, null, null)));
        Assert.Contains("no 3D view", Message(e));
        e = Assert.ThrowsAny<AgentHostException>(() => host.Annotate(1, [], []));
        Assert.Contains("no 3D view", Message(e));
    }

    [Fact]
    public void TheViewsCallsReachTheViewport()
    {
        Viewport view;
        try
        {
            view = new Viewport("Cornfield");
        }
        catch (CoreException)
        {
            return; // No GPU here (the Linux container): the Rust tests cover these calls.
        }
        using (view)
        {
            var withView = new AgentDocumentHost(doc, ui, editor, () => view);
            var cam = withView.Camera(1, new AgentCameraChange(null, false, [1, 2, 3], null, 50));
            Assert.Equal([1.0, 2.0, 3.0], cam.Vpt);
            withView.Annotate(1, [], [new ViewMarker([0, 0, 0], "here", [1, 0, 0, 1])]);
            // Opening the file scheduled a preview, and a capture waits for
            // a preview that is due (AgentDocumentHost.RunWait, a minute)
            // so the agent sees its edit's result. The manual timer never
            // fires by itself, so let the pause pass and run it here;
            // without this the capture outwaited OnLink's 20 s (the first
            // Windows CI run, the first with a graphics adapter).
            Assert.True(doc.RunPending);
            clock.NowMs += 1_000;
            timer.Fire();
            Assert.True(ui.PumpUntil(() => !doc.RunPending, Wait), "the opening preview did not finish");
            var started = DateTime.UtcNow;
            var capture = OnLink(() => withView.Capture(1, 128));
            output.WriteLine($"capture: {capture.Width}x{capture.Height} on {capture.Backend}, " +
                $"{(DateTime.UtcNow - started).TotalMilliseconds:0} ms");
            Assert.Equal((128u, 128u), (capture.Width, capture.Height));
        }
    }

    [Fact]
    public void ABusyWindowAndAClosedOneAreSaidSo()
    {
        var saved = AgentDocumentHost.UiWait;
        AgentDocumentHost.UiWait = TimeSpan.FromMilliseconds(100);
        try
        {
            // Nothing pumps the UI thread: the agent hears why, the link's
            // thread is not held.
            var e = Assert.ThrowsAny<AgentHostException>(() => host.Read(1));
            Assert.Contains("did not answer in time", Message(e));
        }
        finally
        {
            AgentDocumentHost.UiWait = saved;
        }
        host.Close();
        Assert.Equal("the document was closed", Message(Assert.ThrowsAny<AgentHostException>(() => host.Read(1))));
    }

    [Fact]
    public void ACaptureWaitsForTheRunTheEditScheduled()
    {
        var saved = AgentDocumentHost.RunWait;
        AgentDocumentHost.RunWait = TimeSpan.FromMilliseconds(300);
        try
        {
            doc.SchedulePreview();
            Assert.True(doc.RunPending);
            var started = DateTime.UtcNow;
            // The manual timer never fires: the wait ends at RunWait, then
            // says there is no view.
            Assert.ThrowsAny<AgentHostException>(() => OnLink(() => host.Capture(1, 256)));
            Assert.True(DateTime.UtcNow - started >= TimeSpan.FromMilliseconds(250));
        }
        finally
        {
            AgentDocumentHost.RunWait = saved;
        }
    }

    // --- Setup rows -----------------------------------------------------------------------

    sealed class FakeSetup : IAgentSetupBackend
    {
        public string? Claude = "/bin/claude";
        public Queue<ClaudeCodeOutcome> Code = new();
        public ClaudeDesktopOutcome Desktop = new ClaudeDesktopOutcome.Unchanged("cfg");
        public List<string> Calls = [];

        public AgentSetupRow[] Rows(string cli) =>
        [
            new(AgentSetupClient.ClaudeCode, "Claude Code", new AgentSetupAction.RunClaude(), $"claude mcp add … {cli} mcp", ""),
            new(AgentSetupClient.ClaudeDesktop, "Claude Desktop", new AgentSetupAction.MergeConfig(@"C:\Users\me\AppData\Roaming\Claude\claude_desktop_config.json"), "{}", ""),
            new(AgentSetupClient.Cursor, "Cursor", new AgentSetupAction.OpenUrl("cursor://x"), "{}", ""),
            new(AgentSetupClient.Other, "Other", new AgentSetupAction.CopyOnly(), $"{cli} mcp", ""),
        ];

        public string? FindClaude() => Claude;

        public ClaudeCodeOutcome AddToClaudeCode(string claude, string cli, bool replace)
        {
            Calls.Add($"claude {claude} {cli} replace={replace}");
            return Code.Dequeue();
        }

        public ClaudeDesktopOutcome AddToClaudeDesktop(string cli)
        {
            Calls.Add($"desktop {cli}");
            return Desktop;
        }
    }

    [Fact]
    public async Task ClaudeCodeIsAddedAndAnExistingEntryReplacedOnlyWhenTheUserSaysSo()
    {
        var fake = new FakeSetup();
        fake.Code.Enqueue(new ClaudeCodeOutcome.AlreadyExists("MCP server neoscad already exists in user config"));
        fake.Code.Enqueue(new ClaudeCodeOutcome.Added("Added stdio MCP server neoscad"));
        var setup = new AgentSetup(fake, @"C:\NeoSCAD\bin\neoscad.exe", _ => Task.FromResult(true));
        var row = setup.Row(AgentSetupClient.ClaudeCode)!;
        await setup.DetectAsync();
        Assert.Equal((SetupStep.Ready, "Add"), (row.Step, row.ButtonText));
        await setup.RunAsync(row);
        Assert.Equal((SetupStep.Confirm, "Replace"), (row.Step, row.ButtonText));
        Assert.Contains("already has a server called neoscad", row.Status);
        await setup.RunAsync(row);
        Assert.Equal((SetupStep.Done, false), (row.Step, row.ButtonEnabled));
        Assert.Equal(
            [@"claude /bin/claude C:\NeoSCAD\bin\neoscad.exe replace=False", @"claude /bin/claude C:\NeoSCAD\bin\neoscad.exe replace=True"],
            fake.Calls);
        Assert.Contains("all your projects", row.Status);

        // No claude: nothing to run, the command to copy instead.
        var none = new AgentSetup(new FakeSetup { Claude = null }, "neoscad", _ => Task.FromResult(true));
        await none.DetectAsync();
        var r = none.Row(AgentSetupClient.ClaudeCode)!;
        Assert.Equal((SetupStep.Unavailable, (string?)null), (r.Step, r.ButtonText));

        // claude failed: its output, and the copy text is the way round.
        var failing = new FakeSetup();
        failing.Code.Enqueue(new ClaudeCodeOutcome.Failed("boom"));
        var f = new AgentSetup(failing, "neoscad", _ => Task.FromResult(true));
        await f.RunAsync(f.Row(AgentSetupClient.ClaudeCode)!);
        Assert.Equal((SetupStep.Failed, "boom"), (f.Row(AgentSetupClient.ClaudeCode)!.Step, f.Row(AgentSetupClient.ClaudeCode)!.Detail));
    }

    [Fact]
    public async Task ClaudeDesktopIsWrittenOnlyAfterTheUserAgrees()
    {
        var fake = new FakeSetup
        {
            Desktop = new ClaudeDesktopOutcome.Written("cfg", "cfg.neoscad-backup-20261002T120000Z", false),
        };
        var setup = new AgentSetup(fake, "neoscad.exe", _ => Task.FromResult(true));
        var row = setup.Row(AgentSetupClient.ClaudeDesktop)!;
        await setup.RunAsync(row);
        // The question names the file; nothing is written yet.
        Assert.Equal(SetupStep.Confirm, row.Step);
        Assert.Contains(@"Roaming\Claude\claude_desktop_config.json", row.Status);
        Assert.Empty(fake.Calls);
        setup.Cancel(row);
        Assert.Equal(SetupStep.Ready, row.Step);
        await setup.RunAsync(row);
        await setup.RunAsync(row);
        Assert.Equal(["desktop neoscad.exe"], fake.Calls);
        Assert.Equal(SetupStep.Done, row.Step);
        Assert.Contains("Quit Claude Desktop", row.Status);
        Assert.Contains("neoscad-backup", row.Detail);

        fake.Desktop = new ClaudeDesktopOutcome.Refused("cfg", "line 2: a comment");
        var refused = new AgentSetup(fake, "neoscad.exe", _ => Task.FromResult(true));
        var rr = refused.Row(AgentSetupClient.ClaudeDesktop)!;
        await refused.RunAsync(rr);
        await refused.RunAsync(rr);
        Assert.Equal(SetupStep.Failed, rr.Step);
        Assert.Contains("line 2: a comment", rr.Status);
    }

    [Fact]
    public async Task LinksOpenAndSayWhenNothingTookThem()
    {
        var opened = new List<string>();
        var ok = true;
        var setup = new AgentSetup(new FakeSetup(), "neoscad", url =>
        {
            opened.Add(url);
            return Task.FromResult(ok);
        });
        var row = setup.Row(AgentSetupClient.Cursor)!;
        Assert.Equal("Open Cursor", row.ButtonText);
        await setup.RunAsync(row);
        Assert.Equal(["cursor://x"], opened);
        Assert.Equal(SetupStep.Done, row.Step);
        Assert.True(row.ButtonEnabled); // again, if the user dismissed Cursor's prompt
        ok = false;
        await setup.RunAsync(row);
        Assert.Equal(SetupStep.Failed, row.Step);
        Assert.Contains("Is Cursor installed", row.Status);
        Assert.Null(setup.Row(AgentSetupClient.Other)!.ButtonText);
    }

    [Fact]
    public async Task WithoutTheAppsCommandLineToolEveryRowSaysSo()
    {
        var fake = new FakeSetup();
        var setup = new AgentSetup(fake, null, _ => Task.FromResult(true));
        Assert.All(setup.Rows, r => Assert.Equal(SetupStep.Unavailable, r.Step));
        Assert.Contains("bin\\neoscad.exe", setup.Rows[0].Status);
        // The copy text still has a command, for a CLI installed on its own.
        Assert.Contains("neoscad mcp", setup.Row(AgentSetupClient.Other)!.Row.CopyText);
        await setup.RunAsync(setup.Rows[0]);
        Assert.Empty(fake.Calls);
    }

    [Fact]
    public void TheAppsOwnCommandLineToolComesFirst()
    {
        var exe = OperatingSystem.IsWindows() ? "neoscad.exe" : "neoscad";
        var app = Path.Combine(dir, "app");
        var other = Path.Combine(dir, "other");
        Directory.CreateDirectory(Path.Combine(app, "bin"));
        Directory.CreateDirectory(other);
        var path = string.Join(Path.PathSeparator, "", other, "");
        Assert.Null(AgentCli.Locate(app, path, OperatingSystem.IsWindows()));
        File.WriteAllText(Path.Combine(other, exe), "");
        Assert.Equal(Path.Combine(other, exe), AgentCli.Locate(app, path, OperatingSystem.IsWindows()));
        File.WriteAllText(Path.Combine(app, "bin", exe), "");
        Assert.Equal(Path.Combine(app, "bin", exe), AgentCli.Locate(app, path, OperatingSystem.IsWindows()));
    }
}
