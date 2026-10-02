// The Windows app's update check, download and install, apart from the
// window (docs/windows-app.md, "Updates"; docs/audits/auto-update.md has
// the design and the owner's decisions of 2026-09-30):
//
// - Check. About once a day, and from Help > Check for Updates, the app
//   GETs the signed feed (`update_feed_url`, plus `.minisig`) and hands
//   both to the core's `check_for_update`, the code the CLI and the Linux
//   app share: it verifies the minisign signature against the keys
//   compiled into the core, the channel, that the serial is not older than
//   the last one accepted (a replayed old feed), and offers only a newer
//   version that has this architecture's MSI.
// - Download. The MSI goes to a fresh folder under %TEMP%, no larger than
//   the signed feed says, and is kept only if its SHA-256 matches the
//   feed's. The signature covers the hash, so a file swapped on GitHub or
//   in transit is refused.
// - Install. Silent after one UAC prompt (owner decision): the app starts
//   a hidden, unelevated PowerShell that waits for the app to exit (an
//   installer can't replace files a running app holds), checks the hash
//   again, runs `msiexec /i … /qn` elevated, and starts the app again,
//   the new version when the install worked and the old one otherwise.
//   The helper stays unelevated so the restarted app does not run as
//   administrator.
//
// Settings ("Check for updates automatically", on; "Receive release
// candidates", off) and the per-channel serials live in
// %LOCALAPPDATA%\NeoSCAD\updates.json.

using System.Diagnostics;
using System.Runtime.InteropServices;
using System.Security.Cryptography;
using System.Text;
using System.Text.Json;
using System.Text.Json.Serialization;
using NeoSCAD.Native;

namespace NeoSCAD.Host;

/// <summary>What the app keeps between runs about updates.</summary>
public sealed record UpdateSettings
{
    /// <summary>Check about once a day (owner decision: on by default).</summary>
    [JsonPropertyName("automatic")] public bool Automatic { get; init; } = true;

    /// <summary>Follow the rc feed (owner decision: opt-in).</summary>
    [JsonPropertyName("rc")] public bool ReleaseCandidates { get; init; }

    /// <summary>
    /// When the last automatic check started (Unix seconds). Recorded
    /// before the request, so a failed check (offline) waits a day too.
    /// </summary>
    [JsonPropertyName("checked")] public long Checked { get; init; }

    [JsonPropertyName("stable_serial")] public ulong? StableSerial { get; init; }
    [JsonPropertyName("rc_serial")] public ulong? RcSerial { get; init; }

    /// <summary>A version the user chose "Later" for; automatic checks don't show it again.</summary>
    [JsonPropertyName("dismissed")] public string? Dismissed { get; init; }

    public const long IntervalSeconds = 24 * 60 * 60;

    public UpdateChannel Channel => ReleaseCandidates ? UpdateChannel.Rc : UpdateChannel.Stable;

    public ulong? Serial(UpdateChannel channel) => channel == UpdateChannel.Rc ? RcSerial : StableSerial;

    public UpdateSettings WithSerial(UpdateChannel channel, ulong serial) =>
        channel == UpdateChannel.Rc ? this with { RcSerial = serial } : this with { StableSerial = serial };

    /// <summary>
    /// Whether an automatic check is due at <paramref name="now"/> (Unix
    /// seconds). A last check in the future means the clock went back.
    /// </summary>
    public bool Due(long now) => Automatic && (now < Checked || now - Checked >= IntervalSeconds);

    /// <summary>The file at <paramref name="path"/>, or the defaults when it is missing or damaged.</summary>
    public static UpdateSettings Load(string path)
    {
        try
        {
            return JsonSerializer.Deserialize<UpdateSettings>(File.ReadAllBytes(path)) ?? new UpdateSettings();
        }
        catch (Exception e) when (e is IOException or UnauthorizedAccessException or JsonException)
        {
            return new UpdateSettings();
        }
    }

    /// <summary>Written beside the target and moved over it, so a crash leaves the old file.</summary>
    public void Save(string path)
    {
        Directory.CreateDirectory(Path.GetDirectoryName(path)!);
        var tmp = path + ".tmp";
        File.WriteAllText(tmp, JsonSerializer.Serialize(this, new JsonSerializerOptions { WriteIndented = true }));
        File.Move(tmp, path, overwrite: true);
    }

    /// <summary>%LOCALAPPDATA%\NeoSCAD\updates.json for <paramref name="localAppData"/>.</summary>
    public static string PathIn(string localAppData) => Path.Combine(localAppData, "NeoSCAD", "updates.json");
}

/// <summary>A newer release this PC can install.</summary>
public sealed record UpdateNotice(string Version, string Current, string Date, string ReleaseUrl, UpdateArtifact Msi)
{
    public string Title => $"NeoSCAD {Version} is available";

    public string Message =>
        $"You have {Current}. Install downloads {Msi.Name} ({Msi.Size / (1024.0 * 1024.0):0.#} MB), checks it, " +
        "closes NeoSCAD and asks Windows for permission to install it. NeoSCAD then starts again.";
}

/// <summary>Why a check found nothing to offer, or failed.</summary>
public sealed class UpdateCheckException(string message, Exception? inner = null) : Exception(message, inner);

/// <summary>The check and the download, with the network and the core's check as seams for the tests.</summary>
public sealed class UpdateClient
{
    /// <summary>Another feed directory, ending in `/`, for testing (as for the CLI and the Linux app).</summary>
    public const string FeedUrlEnv = "NEOSCAD_UPDATE_FEED_URL";

    /// <summary>Set to anything to stop automatic checks; CI (`CI`) never checks either.</summary>
    public const string NoCheckEnv = "NEOSCAD_NO_UPDATE_CHECK";

    /// <summary>A feed or signature larger than this is not one.</summary>
    public const int MaxFeedBytes = 64 * 1024;

    public delegate UpdateCheck Checker(byte[] feed, byte[] signature, string current, UpdateChannel channel,
        UpdatePlatform? platform, ulong? lastSerial);

    readonly HttpClient http;
    readonly Checker checker;

    /// <param name="handler">The HTTP stack; the tests pass a fake.</param>
    /// <param name="checker">The core's <c>check_for_update</c> unless a test passes another.</param>
    public UpdateClient(HttpMessageHandler? handler = null, Checker? checker = null)
    {
        http = new HttpClient(handler ?? new SocketsHttpHandler { UseCookies = false }, disposeHandler: true)
        {
            Timeout = TimeSpan.FromMinutes(10),
        };
        // Generic: no version, no OS (docs/privacy.md).
        http.DefaultRequestHeaders.UserAgent.ParseAdd("neoscad");
        this.checker = checker ?? NeoScad.CheckForUpdate;
    }

    /// <summary>Automatic checks are off for this process.</summary>
    public static bool AutomaticDisabled(Func<string, string?> env) =>
        env(NoCheckEnv) is not null || env("CI") is not null;

    /// <summary>
    /// The feed's URL for <paramref name="channel"/>: the core's, or one
    /// under <paramref name="custom"/> (<see cref="FeedUrlEnv"/>) when
    /// that is https or plain http to the loopback address.
    /// </summary>
    public static string FeedUrl(UpdateChannel channel, string? custom)
    {
        if (string.IsNullOrWhiteSpace(custom)) return NeoScad.UpdateFeedUrl(channel);
        var b = custom.Trim();
        if (!IsAllowedUrl(b)) throw new UpdateCheckException($"{FeedUrlEnv} must be https, or http to the loopback address");
        if (!b.EndsWith('/')) b += "/";
        return b + (channel == UpdateChannel.Rc ? "rc" : "stable") + ".json";
    }

    /// <summary>https, or http to this machine (a test feed).</summary>
    public static bool IsAllowedUrl(string url) =>
        Uri.TryCreate(url, UriKind.Absolute, out var u) &&
        (u.Scheme == Uri.UriSchemeHttps || (u.Scheme == Uri.UriSchemeHttp && u.IsLoopback));

    /// <summary>The MSI this process needs, by its architecture.</summary>
    public static UpdatePlatform? PlatformFor(Architecture arch) => arch switch
    {
        Architecture.X64 => UpdatePlatform.WindowsX64,
        Architecture.Arm64 => UpdatePlatform.WindowsArm64,
        _ => null,
    };

    /// <summary>
    /// Fetch and check the feed. Returns the settings with the accepted
    /// serial and the notice, if a newer release is offered; throws
    /// <see cref="UpdateCheckException"/> when the feed can't be fetched
    /// or is refused (bad signature, wrong channel, replayed).
    /// </summary>
    public async Task<(UpdateSettings Settings, UpdateNotice? Notice, string Latest)> CheckAsync(
        UpdateSettings settings, string current, UpdatePlatform platform, string feedUrl, CancellationToken ct = default)
    {
        var channel = settings.Channel;
        var feed = await GetSmallAsync(feedUrl, ct);
        var sig = await GetSmallAsync(feedUrl + ".minisig", ct);
        UpdateCheck check;
        try
        {
            check = checker(feed, sig, current, channel, platform, settings.Serial(channel));
        }
        catch (CoreException e)
        {
            throw new UpdateCheckException(CoreErrors.Describe(e), e);
        }
        var next = settings.WithSerial(channel, check.Serial);
        var notice = check.Offer is { Artifact: { } msi } offer
            ? new UpdateNotice(offer.Version, current, offer.Date, offer.Url, msi)
            : null;
        return (next, notice, check.Version);
    }

    async Task<byte[]> GetSmallAsync(string url, CancellationToken ct)
    {
        try
        {
            using var timeout = CancellationTokenSource.CreateLinkedTokenSource(ct);
            timeout.CancelAfter(TimeSpan.FromSeconds(15));
            using var response = await http.GetAsync(url, HttpCompletionOption.ResponseHeadersRead, timeout.Token);
            if (!response.IsSuccessStatusCode)
                throw new UpdateCheckException($"{url}: HTTP {(int)response.StatusCode}");
            await using var body = await response.Content.ReadAsStreamAsync(timeout.Token);
            return await ReadLimitedAsync(body, MaxFeedBytes, timeout.Token)
                   ?? throw new UpdateCheckException($"{url}: larger than a feed");
        }
        catch (Exception e) when (e is HttpRequestException or TaskCanceledException or IOException)
        {
            throw new UpdateCheckException($"{url}: {e.Message}", e);
        }
    }

    /// <summary>The stream's bytes, or null when there are more than <paramref name="limit"/>.</summary>
    static async Task<byte[]?> ReadLimitedAsync(Stream s, long limit, CancellationToken ct)
    {
        using var buffer = new MemoryStream();
        var chunk = new byte[16 * 1024];
        int n;
        while ((n = await s.ReadAsync(chunk, ct)) > 0)
        {
            buffer.Write(chunk, 0, n);
            if (buffer.Length > limit) return null;
        }
        return buffer.ToArray();
    }

    /// <summary>
    /// Download the MSI into a new folder under <paramref name="tempRoot"/>
    /// and check its size and SHA-256 against the signed feed. The path of
    /// the checked file; a file that fails is deleted.
    /// </summary>
    public async Task<string> DownloadAsync(UpdateArtifact msi, string tempRoot, IProgress<double>? progress = null,
        CancellationToken ct = default)
    {
        if (!IsAllowedUrl(msi.Url)) throw new UpdateCheckException($"not an https download: {msi.Url}");
        // A bare file name, whatever the OS calls a separator: it is
        // joined to a folder of ours, and must not climb out of it.
        var name = msi.Name;
        if (name.IndexOfAny(['/', '\\', ':']) >= 0 || name.StartsWith('.') ||
            !name.EndsWith(".msi", StringComparison.OrdinalIgnoreCase))
            throw new UpdateCheckException($"not an installer's name: {msi.Name}");
        var dir = Path.Combine(tempRoot, "NeoSCAD-update-" + Guid.NewGuid().ToString("N")[..12]);
        Directory.CreateDirectory(dir);
        var path = Path.Combine(dir, name);
        try
        {
            using var response = await http.GetAsync(msi.Url, HttpCompletionOption.ResponseHeadersRead, ct);
            if (!response.IsSuccessStatusCode)
                throw new UpdateCheckException($"{msi.Url}: HTTP {(int)response.StatusCode}");
            await using (var body = await response.Content.ReadAsStreamAsync(ct))
            await using (var file = File.Create(path))
            {
                var chunk = new byte[256 * 1024];
                long total = 0;
                int n;
                while ((n = await body.ReadAsync(chunk, ct)) > 0)
                {
                    total += n;
                    // The size is signed too: stop at once rather than
                    // fill the disk for a file that can't match.
                    if (total > (long)msi.Size) throw new UpdateCheckException("the download is larger than the release's");
                    await file.WriteAsync(chunk.AsMemory(0, n), ct);
                    progress?.Report(msi.Size == 0 ? 1 : (double)total / msi.Size);
                }
                if (total != (long)msi.Size) throw new UpdateCheckException("the download is incomplete");
            }
            if (!HashMatches(path, msi.Sha256))
                throw new UpdateCheckException("the download does not match the release's checksum");
            return path;
        }
        catch (Exception e)
        {
            try
            {
                Directory.Delete(dir, recursive: true);
            }
            catch (IOException)
            {
            }
            if (e is UpdateCheckException) throw;
            if (e is HttpRequestException or IOException or TaskCanceledException)
                throw new UpdateCheckException($"{msi.Url}: {e.Message}", e);
            throw;
        }
    }

    /// <summary>Whether the file's SHA-256 is <paramref name="sha256"/> (hex, any case).</summary>
    public static bool HashMatches(string path, string sha256)
    {
        using var file = File.OpenRead(path);
        var hash = Convert.ToHexString(SHA256.HashData(file));
        return string.Equals(hash, sha256.Trim(), StringComparison.OrdinalIgnoreCase);
    }
}

/// <summary>The silent install after one UAC prompt (see the top of this file).</summary>
public static class UpdateInstaller
{
    /// <summary>
    /// Whether this copy is the MSI's: per-machine, in
    /// <c>%ProgramFiles%\NeoSCAD</c> (NeoSCAD.wxs's INSTALLFOLDER; the
    /// installer has no folder choice). A copy anywhere else (a build
    /// folder) is not offered the install, since it would not be the one
    /// replaced; it gets the release page.
    /// </summary>
    public static bool IsInstalledCopy(string exePath, string programFiles) =>
        string.Equals(Path.GetDirectoryName(Path.GetFullPath(exePath)),
            Path.GetFullPath(Path.Combine(programFiles, "NeoSCAD")), StringComparison.OrdinalIgnoreCase);

    /// <summary>
    /// The helper's PowerShell: wait for <paramref name="pid"/> to exit,
    /// check the MSI's hash again (it sat in %TEMP% meanwhile), run msiexec
    /// silently and elevated, log to <paramref name="logPath"/>, and start
    /// <paramref name="exePath"/> again whatever happened (declining UAC
    /// throws, which leaves the old version to start).
    /// </summary>
    public static string Script(int pid, string msiPath, string sha256, string exePath, string logPath)
    {
        static string Quote(string s) => "'" + s.Replace("'", "''") + "'";
        return string.Join('\n',
            "$ErrorActionPreference = 'Stop'",
            $"$msi = {Quote(msiPath)}",
            $"$exe = {Quote(exePath)}",
            $"$log = {Quote(logPath)}",
            // The app may be gone already; one that hasn't closed in five
            // minutes is left alone rather than installed over.
            $"$app = Get-Process -Id {pid} -ErrorAction SilentlyContinue",
            "if ($app -and -not $app.WaitForExit(300000)) { exit 1 }",
            "$code = -1",
            "try {",
            $"  if ((Get-FileHash -Algorithm SHA256 -LiteralPath $msi).Hash -ne {Quote(sha256.Trim().ToUpperInvariant())}) {{ throw 'checksum' }}",
            // msiexec parses its own command line: the paths are quoted
            // for it, inside one argument string.
            "  $arguments = '/i \"' + $msi + '\" /qn /norestart /l*v \"' + $log + '\"'",
            "  $p = Start-Process -FilePath msiexec.exe -ArgumentList $arguments -Verb RunAs -Wait -PassThru",
            "  $code = $p.ExitCode",
            "} catch { }",
            "Start-Process -FilePath $exe",
            "exit $code");
    }

    /// <summary>
    /// How to start the helper: Windows PowerShell (present on every
    /// Windows 10 and 11), hidden, with the script as -EncodedCommand,
    /// which the execution policy does not apply to (it governs script
    /// files) and which needs no quoting.
    /// </summary>
    public static ProcessStartInfo StartInfo(string script)
    {
        var encoded = Convert.ToBase64String(Encoding.Unicode.GetBytes(script));
        var info = new ProcessStartInfo
        {
            FileName = "powershell.exe",
            UseShellExecute = false,
            CreateNoWindow = true,
        };
        foreach (var a in new[] { "-NoProfile", "-NonInteractive", "-WindowStyle", "Hidden", "-EncodedCommand", encoded })
            info.ArgumentList.Add(a);
        return info;
    }
}
