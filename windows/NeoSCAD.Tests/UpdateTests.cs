// The update check, download and install helper (NeoSCAD.Host/Updates.cs)
// against the signed fixtures in crates/client/testdata/update (copied
// beside the tests; a throwaway minisign key, see make.sh there). The
// network is a fake handler; the check is the real core's where the test
// says so.

using System.Net;
using System.Runtime.InteropServices;
using System.Security.Cryptography;
using System.Text;
using NeoSCAD.Host;
using NeoSCAD.Native;

namespace NeoSCAD.Tests;

/// <summary>Serves fixed bodies by URL; anything else is a 404. Counts requests.</summary>
sealed class FakeHttp(Dictionary<string, byte[]> bodies) : HttpMessageHandler
{
    public List<HttpRequestMessage> Requests { get; } = [];

    protected override Task<HttpResponseMessage> SendAsync(HttpRequestMessage request, CancellationToken ct)
    {
        Requests.Add(request);
        var url = request.RequestUri!.ToString();
        return Task.FromResult(bodies.TryGetValue(url, out var body)
            ? new HttpResponseMessage(HttpStatusCode.OK) { Content = new ByteArrayContent(body) }
            : new HttpResponseMessage(HttpStatusCode.NotFound));
    }
}

public class UpdateTests
{
    const string Base = "http://127.0.0.1:8123/updates/v1/";
    const string StableUrl = Base + "stable.json";

    static byte[] Fixture(string name) => File.ReadAllBytes(Path.Combine(AppContext.BaseDirectory, "update", name));

    static FakeHttp Serve(string feed, string sig) => new(new()
    {
        [StableUrl] = Fixture(feed),
        [StableUrl + ".minisig"] = Fixture(sig),
    });

    /// <summary>
    /// The fixtures' key, when the core was built trusting it
    /// (NEOSCAD_UPDATE_TEST_PUBLIC_KEY at build time, which
    /// docker-test.sh passes through): only then can the real core accept
    /// them. A release build trusts only the release key.
    /// </summary>
    static bool CoreTrustsTestKey()
    {
        var key = Environment.GetEnvironmentVariable("NEOSCAD_UPDATE_TEST_PUBLIC_KEY");
        var test = File.ReadAllLines(Path.Combine(AppContext.BaseDirectory, "update", "test.pub"))[1];
        return key?.Trim() == test.Trim();
    }

    [Fact]
    public void DefaultsFollowTheOwnerDecisions()
    {
        var s = new UpdateSettings();
        Assert.True(s.Automatic);
        Assert.False(s.ReleaseCandidates);
        Assert.Equal(UpdateChannel.Stable, s.Channel);
        Assert.True(s.Due(1_000_000));
    }

    [Fact]
    public void ACheckIsDueOnceADay()
    {
        var s = new UpdateSettings { Checked = 1_000_000 };
        Assert.False(s.Due(1_000_000 + UpdateSettings.IntervalSeconds - 1));
        Assert.True(s.Due(1_000_000 + UpdateSettings.IntervalSeconds));
        Assert.True(s.Due(999_999)); // the clock went back
        Assert.False((s with { Automatic = false }).Due(5_000_000));
    }

    [Fact]
    public void SettingsSurviveARoundTripAndDamage()
    {
        var dir = Path.Combine(Path.GetTempPath(), "neoscad-upd-" + Guid.NewGuid().ToString("N"));
        try
        {
            var path = UpdateSettings.PathIn(dir);
            Assert.Equal(new UpdateSettings(), UpdateSettings.Load(path));
            var s = new UpdateSettings { ReleaseCandidates = true, Checked = 7, RcSerial = 3, Dismissed = "0.3.0" };
            s.Save(path);
            Assert.Equal(s, UpdateSettings.Load(path));
            File.WriteAllText(path, "{ not json");
            Assert.Equal(new UpdateSettings(), UpdateSettings.Load(path));
        }
        finally
        {
            Directory.Delete(dir, true);
        }
    }

    [Fact]
    public void SerialsAreKeptPerChannel()
    {
        var s = new UpdateSettings().WithSerial(UpdateChannel.Rc, 4).WithSerial(UpdateChannel.Stable, 2);
        Assert.Equal(4UL, s.Serial(UpdateChannel.Rc));
        Assert.Equal(2UL, s.Serial(UpdateChannel.Stable));
    }

    [Fact]
    public void TheFeedIsTheCoresOrHttpsOrLoopback()
    {
        Assert.Equal("https://neoscad.org/updates/v1/stable.json", UpdateClient.FeedUrl(UpdateChannel.Stable, null));
        Assert.Equal("https://neoscad.org/updates/v1/rc.json", UpdateClient.FeedUrl(UpdateChannel.Rc, ""));
        Assert.Equal(Base + "rc.json", UpdateClient.FeedUrl(UpdateChannel.Rc, Base.TrimEnd('/')));
        Assert.Throws<UpdateCheckException>(() => UpdateClient.FeedUrl(UpdateChannel.Stable, "http://example.org/"));
        Assert.Throws<UpdateCheckException>(() => UpdateClient.FeedUrl(UpdateChannel.Stable, "file:///c:/feed/"));
    }

    [Fact]
    public void EachArchitectureGetsItsMsi()
    {
        Assert.Equal(UpdatePlatform.WindowsX64, UpdateClient.PlatformFor(Architecture.X64));
        Assert.Equal(UpdatePlatform.WindowsArm64, UpdateClient.PlatformFor(Architecture.Arm64));
        Assert.Null(UpdateClient.PlatformFor(Architecture.X86));
    }

    [Fact]
    public void AutomaticChecksStayOffInCiAndWhenAsked()
    {
        Assert.False(UpdateClient.AutomaticDisabled(_ => null));
        Assert.True(UpdateClient.AutomaticDisabled(n => n == "CI" ? "true" : null));
        Assert.True(UpdateClient.AutomaticDisabled(n => n == UpdateClient.NoCheckEnv ? "1" : null));
    }

    [Fact]
    public async Task TheCheckPassesTheChannelPlatformAndSerialAndMapsTheOffer()
    {
        var http = Serve("stable-2.json", "stable-2.json.minisig");
        (UpdateChannel, UpdatePlatform?, ulong?)? seen = null;
        var msi = new UpdateArtifact("NeoSCAD-0.3.0-windows-x64.msi", "https://example.org/a.msi", "00", 1000);
        var client = new UpdateClient(http, (feed, sig, current, channel, platform, last) =>
        {
            Assert.Equal(Fixture("stable-2.json"), feed);
            Assert.Equal(Fixture("stable-2.json.minisig"), sig);
            Assert.Equal("0.2.1", current);
            seen = (channel, platform, last);
            return new UpdateCheck(2, "0.3.0", new UpdateOffer("0.3.0", "2026-10-01", "https://example.org/r", msi));
        });
        var settings = new UpdateSettings { StableSerial = 1 };
        var (next, notice, latest) = await client.CheckAsync(settings, "0.2.1", UpdatePlatform.WindowsX64, StableUrl);
        Assert.Equal((UpdateChannel.Stable, (UpdatePlatform?)UpdatePlatform.WindowsX64, (ulong?)1UL), seen);
        Assert.Equal(2UL, next.StableSerial);
        Assert.Equal("0.3.0", latest);
        Assert.NotNull(notice);
        Assert.Equal("NeoSCAD 0.3.0 is available", notice.Title);
        Assert.Equal(msi, notice.Msi);
        // A plain GET with a bare User-Agent: nothing identifying.
        Assert.All(http.Requests, r =>
        {
            Assert.Equal("neoscad", r.Headers.UserAgent.ToString());
            Assert.True(string.IsNullOrEmpty(r.RequestUri!.Query));
        });
    }

    [Fact]
    public async Task AFeedThatIsMissingIsAFailureNotAnOffer()
    {
        var client = new UpdateClient(new FakeHttp([]), (_, _, _, _, _, _) => throw new InvalidOperationException());
        await Assert.ThrowsAsync<UpdateCheckException>(() =>
            client.CheckAsync(new UpdateSettings(), "0.2.1", UpdatePlatform.WindowsX64, StableUrl));
    }

    [Fact]
    public async Task TheCoreRefusesAFeedSignedByAnotherKey()
    {
        // Signed by other.key, which no build trusts: refused whether or
        // not the core trusts the test key.
        var client = new UpdateClient(Serve("stable-2.json", "stable-2.json.other.minisig"));
        var e = await Assert.ThrowsAsync<UpdateCheckException>(() =>
            client.CheckAsync(new UpdateSettings(), "0.2.1", UpdatePlatform.WindowsX64, StableUrl));
        // "no update-feed key" from a core with no key at all (CI's).
        Assert.True(e.Message.Contains("not signed by a trusted key") || e.Message.Contains("no update-feed key"),
            e.Message);
    }

    [Fact]
    public async Task TheCoreRefusesATamperedFeed()
    {
        var tampered = Encoding.UTF8.GetBytes(Encoding.UTF8.GetString(Fixture("stable-2.json")).Replace("0.3.0", "9.9.9"));
        var http = new FakeHttp(new()
        {
            [StableUrl] = tampered,
            [StableUrl + ".minisig"] = Fixture("stable-2.json.minisig"),
        });
        var client = new UpdateClient(http);
        await Assert.ThrowsAsync<UpdateCheckException>(() =>
            client.CheckAsync(new UpdateSettings(), "0.2.1", UpdatePlatform.WindowsX64, StableUrl));
    }

    [Fact]
    public async Task TheCoreOffersTheMsiFromATestSignedFeed()
    {
        if (!CoreTrustsTestKey()) return; // a core built without the test key can verify nothing here
        var client = new UpdateClient(Serve("stable-2.json", "stable-2.json.minisig"));
        var (next, notice, _) = await client.CheckAsync(new UpdateSettings(), "0.2.1", UpdatePlatform.WindowsArm64, StableUrl);
        Assert.Equal(2UL, next.StableSerial);
        Assert.NotNull(notice);
        Assert.Equal("NeoSCAD-0.3.0-windows-arm64.msi", notice.Msi.Name);
        // The same feed again is fine; an older one is a replay.
        var old = new UpdateClient(Serve("stable-1.json", "stable-1.json.minisig"));
        await Assert.ThrowsAsync<UpdateCheckException>(() =>
            old.CheckAsync(next, "0.2.1", UpdatePlatform.WindowsArm64, StableUrl));
    }

    static UpdateArtifact Artifact(byte[] body, string? sha = null) => new(
        "NeoSCAD-9.9.9-windows-x64.msi", "https://example.org/NeoSCAD-9.9.9-windows-x64.msi",
        sha ?? Convert.ToHexString(SHA256.HashData(body)).ToLowerInvariant(), (ulong)body.Length);

    static string TempRoot()
    {
        var d = Path.Combine(Path.GetTempPath(), "neoscad-dl-" + Guid.NewGuid().ToString("N"));
        Directory.CreateDirectory(d);
        return d;
    }

    [Fact]
    public async Task ADownloadIsKeptOnlyWhenItsChecksumMatches()
    {
        var body = Encoding.ASCII.GetBytes("not really an msi");
        var root = TempRoot();
        try
        {
            var good = Artifact(body);
            var client = new UpdateClient(new FakeHttp(new() { [good.Url] = body }));
            var path = await client.DownloadAsync(good, root);
            Assert.Equal(body, File.ReadAllBytes(path));
            Assert.Equal(good.Name, Path.GetFileName(path));

            var bad = Artifact(body, sha: new string('0', 64));
            await Assert.ThrowsAsync<UpdateCheckException>(() => client.DownloadAsync(bad, root));
            // Only the good download's folder is left.
            Assert.Single(Directory.GetDirectories(root));
        }
        finally
        {
            Directory.Delete(root, true);
        }
    }

    [Fact]
    public async Task ADownloadLargerOrSmallerThanTheFeedSaysIsRefused()
    {
        var body = Encoding.ASCII.GetBytes("0123456789");
        var root = TempRoot();
        try
        {
            var a = Artifact(body);
            var client = new UpdateClient(new FakeHttp(new() { [a.Url] = body }));
            await Assert.ThrowsAsync<UpdateCheckException>(() => client.DownloadAsync(a with { Size = 4 }, root));
            await Assert.ThrowsAsync<UpdateCheckException>(() => client.DownloadAsync(a with { Size = 20 }, root));
            await Assert.ThrowsAsync<UpdateCheckException>(() =>
                client.DownloadAsync(a with { Url = "http://example.org/x.msi" }, root));
            await Assert.ThrowsAsync<UpdateCheckException>(() =>
                client.DownloadAsync(a with { Name = "..\\evil.msi" }, root));
            Assert.Empty(Directory.GetDirectories(root));
        }
        finally
        {
            Directory.Delete(root, true);
        }
    }

    [Fact]
    public void TheInstallerScriptWaitsChecksInstallsSilentlyAndRestarts()
    {
        var script = UpdateInstaller.Script(4242, @"C:\Users\o'neil\AppData\Local\Temp\u\NeoSCAD.msi", "abcdef",
            @"C:\Program Files\NeoSCAD\NeoSCAD.exe", @"C:\Temp\install.log");
        Assert.Contains("Get-Process -Id 4242", script);
        Assert.Contains("'C:\\Users\\o''neil\\AppData\\Local\\Temp\\u\\NeoSCAD.msi'", script);
        Assert.Contains("-ne 'ABCDEF'", script);
        Assert.Contains("/qn /norestart", script);
        Assert.Contains("-Verb RunAs -Wait -PassThru", script);
        Assert.EndsWith("Start-Process -FilePath $exe\nexit $code", script);

        var info = UpdateInstaller.StartInfo(script);
        Assert.Equal("powershell.exe", info.FileName);
        Assert.False(info.UseShellExecute); // the helper itself is not elevated
        var encoded = info.ArgumentList[info.ArgumentList.IndexOf("-EncodedCommand") + 1];
        Assert.Equal(script, Encoding.Unicode.GetString(Convert.FromBase64String(encoded)));
    }

    [Fact]
    public void TheInstallerScriptParsesInWindowsPowerShell()
    {
        // Only Windows has powershell.exe (CI's windows-app job runs this).
        // It starts the parser the way the app starts the helper, through
        // StartInfo's -EncodedCommand, and parses the helper's script
        // without running it.
        if (!OperatingSystem.IsWindows()) return;
        var script = UpdateInstaller.Script(4242, @"C:\Temp\o'neil\NeoSCAD.msi", "abcdef",
            @"C:\Program Files\NeoSCAD\NeoSCAD.exe", @"C:\Temp\install.log");
        var b64 = Convert.ToBase64String(Encoding.Unicode.GetBytes(script));
        var check = "$errors = $null; " +
                    "$text = [Text.Encoding]::Unicode.GetString([Convert]::FromBase64String('" + b64 + "')); " +
                    "[void][System.Management.Automation.Language.Parser]::ParseInput($text, [ref]$null, [ref]$errors); " +
                    "if ($errors.Count) { $errors | ForEach-Object { $_.Message }; exit 1 }; 'parsed'";
        var info = UpdateInstaller.StartInfo(check);
        info.RedirectStandardOutput = true;
        using var p = System.Diagnostics.Process.Start(info)!;
        var output = p.StandardOutput.ReadToEnd();
        p.WaitForExit();
        Assert.True(p.ExitCode == 0 && output.Contains("parsed"), output);
    }

    [Fact]
    public void OnlyTheMsisCopyIsOfferedTheInstall()
    {
        var pf = Path.Combine(Path.GetTempPath(), "Program Files");
        Assert.True(UpdateInstaller.IsInstalledCopy(Path.Combine(pf, "NeoSCAD", "NeoSCAD.exe"), pf));
        Assert.True(UpdateInstaller.IsInstalledCopy(Path.Combine(pf, "neoscad", "NeoSCAD.exe"), pf + Path.DirectorySeparatorChar));
        Assert.False(UpdateInstaller.IsInstalledCopy(Path.Combine(pf, "NeoSCAD", "bin", "NeoSCAD.exe"), pf));
        Assert.False(UpdateInstaller.IsInstalledCopy(Path.Combine(Path.GetTempPath(), "build", "NeoSCAD.exe"), pf));
    }
}
