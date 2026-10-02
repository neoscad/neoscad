// The window's side of updates (NeoSCAD.Host/Updates.cs has the check,
// the download and the install helper, and why each is done that way):
// the automatic check's timer, Help > Check for Updates and its two
// settings, and the "Update available" bar with Install.
//
// An automatic check runs ten seconds after start-up and then hourly,
// each time only when a day has passed since the last one, and is silent
// whatever happens (offline, a feed between releases, a refused
// signature): it is logged with --log, nothing more. The menu's check
// says what it found.

using System.Diagnostics;
using System.Runtime.InteropServices;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using NeoSCAD.Host;
using NeoSCAD.Native;

namespace NeoSCAD.App;

public sealed partial class MainWindow
{
    static readonly string UpdateSettingsPath =
        UpdateSettings.PathIn(Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData));

    readonly UpdateClient updates = new();
    UpdateSettings updateSettings = UpdateSettings.Load(UpdateSettingsPath);
    UpdateNotice? updateNotice;
    /// <summary>A check or a download is running.</summary>
    bool updateBusy;

    static long UnixNow => DateTimeOffset.UtcNow.ToUnixTimeSeconds();

    static bool AutomaticOff => UpdateClient.AutomaticDisabled(Environment.GetEnvironmentVariable);

    /// <summary>From the constructor: the menu's state and the automatic check's timer.</summary>
    void StartUpdates()
    {
        AutoUpdateItem.IsChecked = updateSettings.Automatic;
        RcUpdateItem.IsChecked = updateSettings.ReleaseCandidates;
        if (AutomaticOff)
        {
            AppLog.Write("update: automatic checks off for this process");
            return;
        }
        var timer = DispatcherQueue.CreateTimer();
        timer.Interval = TimeSpan.FromSeconds(10);
        timer.IsRepeating = true;
        timer.Tick += (t, _) =>
        {
            t.Interval = TimeSpan.FromHours(1);
            _ = AutomaticCheckAsync();
        };
        timer.Start();
        Closed += (_, _) => timer.Stop();
    }

    void SaveUpdateSettings(UpdateSettings s)
    {
        updateSettings = s;
        try
        {
            s.Save(UpdateSettingsPath);
        }
        catch (Exception e) when (e is IOException or UnauthorizedAccessException)
        {
            AppLog.Write($"update: could not save the settings: {e.Message}");
        }
    }

    async Task AutomaticCheckAsync()
    {
        if (updateBusy || AutomaticOff || !updateSettings.Due(UnixNow)) return;
        // No key in this build (before the release key exists): nothing
        // could verify, so don't ask the network at all.
        if (!SafeCheckAvailable()) return;
        SaveUpdateSettings(updateSettings with { Checked = UnixNow });
        await CheckForUpdatesAsync(manual: false);
    }

    static bool SafeCheckAvailable()
    {
        try
        {
            return NeoScad.UpdateCheckAvailable();
        }
        catch (Exception e)
        {
            AppLog.Write("update: the core is not available", e);
            return false;
        }
    }

    async void OnCheckForUpdates(object sender, RoutedEventArgs e)
    {
        if (!SafeCheckAvailable())
        {
            await ShowUpdateMessageAsync("Can't check for updates", "This build of NeoSCAD has no update key.");
            return;
        }
        await CheckForUpdatesAsync(manual: true);
    }

    async Task CheckForUpdatesAsync(bool manual)
    {
        if (updateBusy) return;
        if (UpdateClient.PlatformFor(RuntimeInformation.ProcessArchitecture) is not { } platform) return;
        updateBusy = true;
        string? failure = null;
        try
        {
            var current = NeoScad.CoreVersion();
            var url = UpdateClient.FeedUrl(updateSettings.Channel,
                Environment.GetEnvironmentVariable(UpdateClient.FeedUrlEnv));
            var (next, notice, latest) = await updates.CheckAsync(updateSettings, current, platform, url);
            // Only the serial: a setting may have changed while the
            // request was out.
            SaveUpdateSettings(updateSettings with { StableSerial = next.StableSerial, RcSerial = next.RcSerial });
            if (notice is null)
            {
                AppLog.Write($"update: up to date ({current}; the feed has {latest})");
                HideUpdate();
                if (manual) await ShowUpdateMessageAsync("NeoSCAD is up to date", $"You have NeoSCAD {current}.");
            }
            else
            {
                AppLog.Write($"update: {notice.Version} available");
                if (manual || updateSettings.Dismissed != notice.Version) ShowUpdate(notice);
            }
        }
        catch (Exception x)
        {
            // An update check must never take the app down: every failure
            // is logged, and told only to someone who asked.
            AppLog.Write($"update: refused or failed: {x.Message}");
            failure = x is UpdateCheckException or CoreException ? MessageOf(x) : x.Message;
        }
        finally
        {
            updateBusy = false;
        }
        if (manual && failure is not null) await ShowUpdateMessageAsync("Could not check for updates", failure);
    }

    static string MessageOf(Exception x) => x is CoreException ? CoreErrors.Describe(x) : x.Message;

    bool InstalledCopy =>
        Environment.ProcessPath is { } exe &&
        UpdateInstaller.IsInstalledCopy(exe, Environment.GetFolderPath(Environment.SpecialFolder.ProgramFiles));

    void ShowUpdate(UpdateNotice notice)
    {
        updateNotice = notice;
        UpdateBar.Title = notice.Title;
        // A copy outside Program Files (a build folder) is not the one the
        // MSI would replace: it gets the release page instead.
        UpdateBar.Message = InstalledCopy ? notice.Message : $"You have {notice.Current}.";
        UpdateButton.Content = InstalledCopy ? "Install" : "Release Page";
        UpdateButton.IsEnabled = true;
        UpdateBar.Severity = InfoBarSeverity.Informational;
        UpdateBar.IsOpen = true;
    }

    void HideUpdate()
    {
        updateNotice = null;
        UpdateBar.IsOpen = false;
    }

    /// <summary>The bar's close button: not now, and not this version again unless asked.</summary>
    void OnUpdateLater(InfoBar sender, object args)
    {
        if (updateNotice is { } n) SaveUpdateSettings(updateSettings with { Dismissed = n.Version });
        updateNotice = null;
    }

    async void OnUpdateInstall(object sender, RoutedEventArgs e)
    {
        if (updateNotice is not { } notice || updateBusy) return;
        if (!InstalledCopy || Environment.ProcessPath is not { } exe)
        {
            await Windows.System.Launcher.LaunchUriAsync(new Uri(notice.ReleaseUrl));
            return;
        }
        // The app closes to be replaced: unsaved changes first.
        if (!await ConfirmDiscardAsync()) return;
        updateBusy = true;
        UpdateButton.IsEnabled = false;
        try
        {
            UpdateBar.Message = "Downloading…";
            var progress = new Progress<double>(f => UpdateBar.Message = $"Downloading… {f:P0}");
            var msi = await updates.DownloadAsync(notice.Msi, Path.GetTempPath(), progress);
            AppLog.Write($"update: {msi} downloaded and checked; installing");
            var log = Path.Combine(Path.GetDirectoryName(msi)!, "install.log");
            var script = UpdateInstaller.Script(Environment.ProcessId, msi, notice.Msi.Sha256, exe, log);
            using (Process.Start(UpdateInstaller.StartInfo(script)))
            {
            }
            // Past the save prompt already: close without asking again.
            closing = true;
            Close();
        }
        catch (Exception x)
        {
            AppLog.Write("update: install failed", x);
            UpdateBar.Severity = InfoBarSeverity.Error;
            UpdateBar.Message = $"The update could not be installed: {MessageOf(x)}";
            UpdateButton.IsEnabled = true;
        }
        finally
        {
            updateBusy = false;
        }
    }

    void OnAutoUpdateToggle(object sender, RoutedEventArgs e)
    {
        SaveUpdateSettings(updateSettings with { Automatic = AutoUpdateItem.IsChecked });
        if (AutoUpdateItem.IsChecked) _ = AutomaticCheckAsync();
    }

    void OnRcUpdateToggle(object sender, RoutedEventArgs e)
    {
        // Another feed: what the old one offered no longer applies, and
        // its check is due now rather than tomorrow.
        SaveUpdateSettings(updateSettings with
        {
            ReleaseCandidates = RcUpdateItem.IsChecked,
            Checked = 0,
            Dismissed = null,
        });
        HideUpdate();
        _ = AutomaticCheckAsync();
    }

    async Task ShowUpdateMessageAsync(string title, string message)
    {
        if (dialogShowing || Root.XamlRoot is null) return;
        dialogShowing = true;
        try
        {
            await new ContentDialog
            {
                XamlRoot = Root.XamlRoot,
                Title = title,
                Content = new TextBlock { Text = message, TextWrapping = TextWrapping.Wrap },
                CloseButtonText = "OK",
            }.ShowAsync();
        }
        finally
        {
            dialogShowing = false;
        }
    }
}
