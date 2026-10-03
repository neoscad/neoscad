// The document's own file changed by another program: the window's half
// of NeoSCAD.Host's DocumentSession.Disk.cs. A clean document takes the
// change in the editor by itself; this shows the InfoBar for a conflict or
// a missing file, and asks before Save overwrites another program's write.

using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using NeoSCAD.Host;

namespace NeoSCAD.App;

public sealed partial class MainWindow
{
    void ShowDiskNotice()
    {
        switch (document.Notice)
        {
            case DiskNotice.Changed c:
                DiskBar.Title = "The file changed on disk";
                DiskBar.Message = c.Reloadable
                    ? "Another program changed it while you had unsaved changes. Reload takes its version (Undo brings yours back); close this bar to keep yours."
                    : "Another program changed it, and it is no longer UTF-8 text. Close this bar to keep yours.";
                DiskReloadButton.Visibility = c.Reloadable ? Visibility.Visible : Visibility.Collapsed;
                DiskBar.IsOpen = true;
                break;
            case DiskNotice.Missing:
                DiskBar.Title = "The file was deleted or moved";
                DiskBar.Message = "Saving will write it again.";
                DiskReloadButton.Visibility = Visibility.Collapsed;
                DiskBar.IsOpen = true;
                break;
            default:
                DiskBar.IsOpen = false;
                break;
        }
    }

    void OnDiskReload(object sender, RoutedEventArgs e)
    {
        if (!document.ReloadFromDisk()) Status.Text = "The file on disk could not be read as text.";
    }

    void OnDiskKeepMine(InfoBar sender, object args)
    {
        // Closing the bar is the choice; the session forgets the notice
        // (and NoticeChanged then finds it closed already).
        if (document.Notice is not null) document.KeepMine();
    }

    /// <summary>
    /// Save to the document's file; when another program changed it since
    /// it was read or saved here, ask first: Save Anyway, Reload (theirs,
    /// as an undoable step) or Cancel.
    /// </summary>
    async Task<bool> SaveToFileAsync()
    {
        SaveOutcome outcome = SaveOutcome.Saved;
        if (!Write(() => outcome = document.Save())) return false;
        if (outcome == SaveOutcome.Saved) return true;
        if (outcome == SaveOutcome.NeedsName) return await SaveAsAsync();
        if (dialogShowing) return false;
        var dialog = new ContentDialog
        {
            XamlRoot = Root.XamlRoot,
            Title = $"{document.DisplayName} changed on disk",
            Content = new TextBlock
            {
                Text = "Another program changed the file since it was opened or saved here. Saving replaces its changes with yours.",
                TextWrapping = TextWrapping.Wrap,
            },
            PrimaryButtonText = "Save Anyway",
            SecondaryButtonText = "Reload",
            CloseButtonText = "Cancel",
            DefaultButton = ContentDialogButton.Close,
        };
        dialogShowing = true;
        ContentDialogResult result;
        try
        {
            result = await dialog.ShowAsync();
        }
        finally
        {
            dialogShowing = false;
        }
        switch (result)
        {
            case ContentDialogResult.Primary:
                return Write(() => document.Save(overwrite: true));
            case ContentDialogResult.Secondary:
                document.ReloadFromDisk();
                return false;
            default:
                return false;
        }
    }
}
