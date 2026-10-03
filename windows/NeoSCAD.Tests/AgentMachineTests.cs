// The agent setup and link against the real core on this machine:
//
// - AgentSetupMachineTests: a stand-in `claude` found on PATH and run
//   through `claude mcp add` (a shell script here, a .cmd on Windows), and
//   Claude Desktop's config under a scratch %APPDATA% (Windows only: the
//   core has no Claude Desktop on Linux, and says so). They change this
//   process's environment, so they run alone.
// - AgentEndToEndTests: the window's AgentConnection listening (a Unix
//   socket here, a named pipe on Windows) and the real `neoscad mcp`
//   driving it over MCP stdio, as an agent would. Runs when
//   NEOSCAD_TEST_CLI names a built `neoscad` (CI's Windows job builds one;
//   scripts/windows/docker-test.sh --with-cli does in Docker), and passes
//   vacuously otherwise, saying so.

using System.Collections.Concurrent;
using System.Diagnostics;
using System.Runtime.InteropServices;
using System.Text;
using System.Text.Json;
using System.Text.Json.Nodes;
using NeoSCAD.Host;
using NeoSCAD.Native;
using Xunit.Abstractions;

namespace NeoSCAD.Tests;

/// <summary>
/// The process environment as native code sees it. On Windows .NET's
/// variables are the process's; on Linux .NET keeps its own copy, which
/// the core (Rust's std::env) never reads, so libc's is set too.
/// </summary>
static class NativeEnv
{
    [DllImport("libc", EntryPoint = "setenv")]
    static extern int SetEnv(string name, string value, int overwrite);

    [DllImport("libc", EntryPoint = "unsetenv")]
    static extern int UnsetEnv(string name);

    public static void Set(string name, string? value)
    {
        Environment.SetEnvironmentVariable(name, value);
        if (OperatingSystem.IsWindows()) return;
        if (value is null) UnsetEnv(name);
        else SetEnv(name, value, 1);
    }
}

/// <summary>
/// The agent tests run one at a time: some change this process's
/// environment, and some shorten AgentDocumentHost's waits, which are
/// static.
/// </summary>
[CollectionDefinition("Agents", DisableParallelization = true)]
public sealed class AgentsCollection;

[Collection("Agents")]
public sealed class AgentSetupMachineTests : IDisposable
{
    readonly string dir = Directory.CreateTempSubdirectory("neoscad-setup-").FullName;
    readonly Dictionary<string, string?> saved = [];

    void SetEnv(string name, string? value)
    {
        if (!saved.ContainsKey(name)) saved[name] = Environment.GetEnvironmentVariable(name);
        NativeEnv.Set(name, value);
    }

    public void Dispose()
    {
        foreach (var (name, value) in saved) NativeEnv.Set(name, value);
        Directory.Delete(dir, true);
    }

    /// <summary>
    /// A `claude` that keeps its one entry in a file and answers as Claude
    /// Code does (crates/ffi/src/agent_setup.rs has the same stand-in).
    /// </summary>
    string FakeClaude()
    {
        var bin = Path.Combine(dir, "claude-bin");
        Directory.CreateDirectory(bin);
        if (OperatingSystem.IsWindows())
        {
            var cmd = Path.Combine(bin, "claude.cmd");
            File.WriteAllText(cmd, string.Join("\r\n",
                "@echo off",
                "echo %*>>\"%~dp0log\"",
                "if \"%2\"==\"add\" goto add",
                "if \"%2\"==\"remove\" goto remove",
                "exit /b 2",
                ":add",
                "if exist \"%~dp0state\" (",
                "  echo MCP server neoscad already exists in user config",
                "  exit /b 1",
                ")",
                "type nul > \"%~dp0state\"",
                "echo Added stdio MCP server neoscad",
                "exit /b 0",
                ":remove",
                "del \"%~dp0state\"",
                "echo Removed MCP server neoscad",
                "exit /b 0",
                ""));
            return cmd;
        }
        var sh = Path.Combine(bin, "claude");
        File.WriteAllText(sh,
            "#!/bin/sh\n" +
            $"echo \"$@\" >> '{bin}/log'\n" +
            "case \"$2\" in\n" +
            $"add) if [ -f '{bin}/state' ]; then echo \"MCP server $5 already exists in user config\"; exit 1; fi; touch '{bin}/state'; echo \"Added stdio MCP server $5\";;\n" +
            $"remove) rm -f '{bin}/state'; echo \"Removed MCP server $5\";;\n" +
            "esac\n");
        File.SetUnixFileMode(sh, UnixFileMode.UserRead | UnixFileMode.UserWrite | UnixFileMode.UserExecute);
        return sh;
    }

    [Fact]
    public async Task ClaudeCodeIsFoundOnPathAndAddedThroughItsOwnCommand()
    {
        var claude = FakeClaude();
        SetEnv("PATH", Path.GetDirectoryName(claude) + Path.PathSeparator + Environment.GetEnvironmentVariable("PATH"));
        Assert.Equal(claude, NeoScad.AgentSetupFindClaude(), ignoreCase: OperatingSystem.IsWindows());

        var cli = Path.Combine(dir, "NeoSCAD", "bin", OperatingSystem.IsWindows() ? "neoscad.exe" : "neoscad");
        // The dialog's row, on the real core: found, added, then (asked
        // again) found existing and replaced on the user's say-so.
        var setup = new AgentSetup(NativeAgentSetup.Instance, cli, _ => Task.FromResult(true));
        var row = setup.Row(AgentSetupClient.ClaudeCode)!;
        Assert.IsType<AgentSetupAction.RunClaude>(row.Row.Action);
        Assert.Contains(cli, row.Row.CopyText);
        await setup.DetectAsync();
        Assert.Equal(SetupStep.Ready, row.Step);
        await setup.RunAsync(row);
        Assert.Equal(SetupStep.Done, row.Step);
        Assert.Contains("Added stdio MCP server neoscad", row.Detail);

        var again = new AgentSetup(NativeAgentSetup.Instance, cli, _ => Task.FromResult(true));
        var row2 = again.Row(AgentSetupClient.ClaudeCode)!;
        await again.RunAsync(row2);
        Assert.Equal(SetupStep.Confirm, row2.Step);
        await again.RunAsync(row2);
        Assert.Equal(SetupStep.Done, row2.Step);

        var log = File.ReadAllLines(Path.Combine(Path.GetDirectoryName(claude)!, "log"))
            .Select(l => l.Trim().Replace("\"", "")).ToArray();
        Assert.Equal(4, log.Length);
        Assert.Equal($"mcp add --scope user neoscad -- {cli} mcp", log[0]);
        Assert.Equal("mcp remove --scope user neoscad", log[2]);
        Assert.Equal(log[0], log[3]);
    }

    [Fact]
    public void ClaudeDesktopsConfigIsMergedWithABackup()
    {
        var appdata = Path.Combine(dir, "Roaming");
        Directory.CreateDirectory(appdata);
        SetEnv("APPDATA", appdata);
        var cli = Path.Combine(dir, "NeoSCAD", "bin", "neoscad.exe");
        if (!OperatingSystem.IsWindows())
        {
            // Claude Desktop runs on macOS and Windows only; this core
            // says so rather than write anywhere.
            Assert.Throws<CoreException.InvalidArgument>(() => NeoScad.AgentSetupAddToClaudeDesktop(cli));
            Assert.DoesNotContain(NeoScad.AgentSetupRows(cli), r => r.Action is AgentSetupAction.MergeConfig);
            return;
        }
        var config = Path.Combine(appdata, "Claude", "claude_desktop_config.json");
        var row = Assert.Single(NeoScad.AgentSetupRows(cli), r => r.Client == AgentSetupClient.ClaudeDesktop);
        Assert.Equal(config, Assert.IsType<AgentSetupAction.MergeConfig>(row.Action).Path, ignoreCase: true);

        // Not installed (no Claude folder): nothing is created.
        Assert.IsType<ClaudeDesktopOutcome.NotInstalled>(NeoScad.AgentSetupAddToClaudeDesktop(cli));
        Assert.False(Directory.Exists(Path.GetDirectoryName(config)));

        Directory.CreateDirectory(Path.GetDirectoryName(config)!);
        var old = "{\"mcpServers\": {\"other\": {\"command\": \"x\"}}, \"theme\": \"dark\"}";
        File.WriteAllText(config, old);
        var written = Assert.IsType<ClaudeDesktopOutcome.Written>(NeoScad.AgentSetupAddToClaudeDesktop(cli));
        Assert.Equal(old, File.ReadAllText(written.Backup!));
        var now = JsonNode.Parse(File.ReadAllText(config))!;
        Assert.Equal(cli, (string?)now["mcpServers"]!["neoscad"]!["command"]);
        Assert.Equal("mcp", (string?)now["mcpServers"]!["neoscad"]!["args"]![0]);
        Assert.Equal("x", (string?)now["mcpServers"]!["other"]!["command"]);
        Assert.Equal("dark", (string?)now["theme"]);
        Assert.IsType<ClaudeDesktopOutcome.Unchanged>(NeoScad.AgentSetupAddToClaudeDesktop(cli));
    }
}

[Collection("Agents")]
public sealed class AgentEndToEndTests(ITestOutputHelper output) : IDisposable
{
    static readonly Core core = new(new CoreConfig(null, false));
    static readonly TimeSpan Wait = TimeSpan.FromSeconds(30);

    // Short: on Unix the rendezvous holds a socket, whose path is limited
    // to about 100 bytes.
    readonly string dir = Directory.CreateTempSubdirectory("nsa").FullName;
    readonly QueueDispatcher ui = new();

    public void Dispose()
    {
        try
        {
            Directory.Delete(dir, true);
        }
        catch (IOException)
        {
            // A pipe or socket the link is still closing; the temp folder's.
        }
    }

    readonly Stopwatch elapsed = Stopwatch.StartNew();
    // What the failure output shows: the command line's stderr, and the
    // link's view of its clients. Windows CI is the only place the pipes
    // run, and a hang there otherwise says only "no answer in time".
    McpClient? mcp;
    AgentConnection? agents;

    void Note(string what) => output.WriteLine($"[{elapsed.ElapsedMilliseconds,6} ms] {what}");

    T Pumped<T>(string step, Func<T> work)
    {
        Note($"{step}: sent");
        var task = Task.Run(work);
        if (!ui.PumpUntil(() => task.IsCompleted, Wait))
        {
            Note($"{step}: no answer after {Wait.TotalSeconds:0} s");
            Diagnose();
            Assert.Fail($"no answer in time to {step}");
        }
        Note($"{step}: answered");
        return task.GetAwaiter().GetResult();
    }

    void Diagnose()
    {
        Note($"the window's link: {agents?.Address ?? "not listening"}; clients: " +
            string.Join(", ", agents?.Status?.Clients.Select(c => $"{c.Id} {c.Name ?? "(no name yet)"}") ?? []));
        if (OperatingSystem.IsWindows())
        {
            // What `neoscad mcp`'s discovery lists (crates/agent-link/src/discovery.rs).
            try
            {
                var pipes = Directory.GetFiles(@"\\.\pipe\").Where(p => p.Contains("neoscad", StringComparison.OrdinalIgnoreCase));
                Note($"neoscad pipes: {string.Join(", ", pipes)}");
            }
            catch (Exception e)
            {
                Note($"cannot list the pipes: {e.Message}");
            }
        }
        if (mcp is not null)
        {
            Note($"neoscad mcp {(mcp.Exited ? "has exited" : "is running")}; its stderr:");
            foreach (var line in mcp.Stderr) output.WriteLine($"  {line}");
        }
    }

    [Fact]
    public void TheRealCommandLineReadsAndEditsTheWindowsDocument()
    {
        var cli = Environment.GetEnvironmentVariable("NEOSCAD_TEST_CLI");
        if (string.IsNullOrEmpty(cli))
        {
            output.WriteLine("skipped: set NEOSCAD_TEST_CLI to a built neoscad to run this");
            return;
        }
        Assert.True(File.Exists(cli), $"NEOSCAD_TEST_CLI={cli} does not exist");
        // The fake timer never runs the preview an edit schedules, which a
        // capture would otherwise wait the full minute for.
        var savedRunWait = AgentDocumentHost.RunWait;
        AgentDocumentHost.RunWait = TimeSpan.FromSeconds(1);
        try
        {
            Run(cli);
        }
        finally
        {
            AgentDocumentHost.RunWait = savedRunWait;
            if (mcp is not null)
            {
                // Shown when the test fails anywhere (xunit prints a passing
                // test's output only when asked).
                output.WriteLine("neoscad mcp's stderr:");
                foreach (var line in mcp.Stderr) output.WriteLine($"  {line}");
            }
        }
    }

    void Run(string cli)
    {
        Directory.CreateDirectory(Path.Combine(dir, "work"));
        Directory.CreateDirectory(Path.Combine(dir, "models"));
        var model = Path.Combine(dir, "models", "gear.scad");
        File.WriteAllText(model, "cube(10);\n");
        using var doc = new DocumentSession(core, ui, new ManualTimer(), new ManualClock(), dir);
        doc.Open(model);
        var editor = new FakeAgentEditor(doc);
        var settings = Path.Combine(dir, "agents.json");
        new AgentSettings().WithAllowed(true).Save(settings);
        // On Windows the folder's name tags the pipe names (crates/agent-link/src/discovery.rs).
        var rendezvous = Path.Combine(dir, $"rv{Environment.ProcessId}");
        using var agents = new AgentConnection(doc, ui, editor, () => null, "test", settings, rendezvous,
            watchSettings: false);
        this.agents = agents;
        agents.Activated(true);
        Assert.True(ui.PumpUntil(() => agents.Address is not null, Wait), "the link did not listen");
        Note($"listening at {agents.Address}");

        using var mcp = McpClient.Start(cli, Path.Combine(dir, "work"), rendezvous);
        this.mcp = mcp;
        Note("neoscad mcp started");
        var init = Pumped("initialize", () => mcp.Call("initialize", new
        {
            protocolVersion = "2025-11-25",
            capabilities = new { },
            clientInfo = new { name = "test-client", title = "Test Client", version = "1" },
        }));
        Assert.Contains("NeoSCAD app open", init.GetProperty("result").GetProperty("instructions").GetString());
        if (!ui.PumpUntil(() => agents.Status?.Clients.FirstOrDefault()?.Name == "Test Client", Wait))
        {
            Diagnose();
            Assert.Fail($"the window did not see the agent: {agents.Status}");
        }
        Note("the window sees the agent");
        Assert.Equal("Test Client connected", agents.Indicator.Text);

        var started = Stopwatch.StartNew();
        var read = Pumped("editor_read", () => mcp.Tool("editor_read", new { }));
        output.WriteLine($"editor_read through neoscad mcp and the C# host: {started.Elapsed.TotalMilliseconds:0.0} ms");
        var text = McpClient.Text(read);
        Assert.Contains("gear.scad in NeoSCAD (document 1)", text);
        Assert.Contains($"version {doc.Revision}", text);
        Assert.Contains("cube(10);", text);

        var v = doc.Revision;
        var edit = Pumped("editor_edit", () => mcp.Tool("editor_edit", new
        {
            version = v,
            edits = new[] { new { old = "cube(10);", @new = "cube(20);" } },
        }));
        Assert.Contains($"now version {v + 1}", McpClient.Text(edit));
        Assert.Equal("cube(20);\n", doc.Text);
        Assert.True(doc.IsDirty);
        Assert.Single(editor.Applied);

        // An error in the window reaches the agent as a sentence.
        var capture = Pumped("view_capture", () => mcp.Tool("view_capture", new { }));
        Assert.True(capture.GetProperty("isError").GetBoolean());
        Assert.Contains("no 3D view", McpClient.Text(capture));

        // Disconnect, then the consent switch off: the link goes, and on
        // Unix its socket with it.
        var client = agents.Status!.Clients.Single().Id;
        agents.Disconnect(client);
        if (!ui.PumpUntil(() => agents.Status?.Clients.Length == 0, Wait))
        {
            Diagnose();
            Assert.Fail("the agent was still connected after the window disconnected it");
        }
        Note("disconnected");
        agents.SetAllowed(false);
        Assert.Null(agents.Status);
        Assert.Equal(AgentIndicatorState.Hidden, agents.Indicator.State);
        if (!OperatingSystem.IsWindows())
            Assert.Empty(Directory.GetFiles(rendezvous, "*.sock"));
    }
}

/// <summary>`neoscad mcp` over stdio, as an MCP client drives it.</summary>
sealed class McpClient : IDisposable
{
    readonly Process process;
    readonly Stopwatch since = Stopwatch.StartNew();
    readonly ConcurrentQueue<string> stderr = new();
    int next = 1;

    McpClient(Process process) => this.process = process;

    /// <summary>What it has written to stderr so far, each line with its time since the start.</summary>
    public IEnumerable<string> Stderr => stderr;

    public bool Exited
    {
        get
        {
            try
            {
                return process.HasExited;
            }
            catch (InvalidOperationException)
            {
                return true;
            }
        }
    }

    public static McpClient Start(string cli, string cwd, string rendezvous)
    {
        var info = new ProcessStartInfo(cli, "mcp")
        {
            WorkingDirectory = cwd,
            RedirectStandardInput = true,
            RedirectStandardOutput = true,
            RedirectStandardError = true,
            UseShellExecute = false,
            StandardOutputEncoding = Encoding.UTF8,
        };
        info.Environment["NEOSCAD_AGENT_DIR"] = rendezvous;
        info.Environment.Remove("OPENSCADPATH");
        var p = Process.Start(info)!;
        var client = new McpClient(p);
        p.ErrorDataReceived += (_, e) =>
        {
            if (e.Data is not null) client.stderr.Enqueue($"[{client.since.ElapsedMilliseconds,6} ms] {e.Data}");
        };
        p.BeginErrorReadLine();
        return client;
    }

    public JsonElement Call(string method, object parameters)
    {
        var id = next++;
        var message = JsonSerializer.Serialize(new { jsonrpc = "2.0", id, method, @params = parameters });
        process.StandardInput.WriteLine(message);
        process.StandardInput.Flush();
        while (true)
        {
            var line = process.StandardOutput.ReadLine() ?? throw new InvalidOperationException("the server closed");
            var r = JsonSerializer.Deserialize<JsonElement>(line);
            if (r.TryGetProperty("id", out var rid) && rid.ValueKind == JsonValueKind.Number && rid.GetInt32() == id)
                return r;
            // Notifications (tools/list_changed) and anything else: skipped.
        }
    }

    public JsonElement Tool(string name, object arguments) =>
        Call("tools/call", new { name, arguments }).GetProperty("result");

    public static string Text(JsonElement result) =>
        result.GetProperty("content")[0].GetProperty("text").GetString() ?? "";

    public void Dispose()
    {
        try
        {
            process.Kill();
            process.WaitForExit(5000);
        }
        catch (InvalidOperationException)
        {
        }
        process.Dispose();
    }
}
