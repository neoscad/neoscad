// A document window: wires the document's loop (NeoSCAD.Host's
// DocumentSession) to the editor pane, the 3D view, the console, the side
// panels (Panels/) and the menus. Everything that is not a control lives
// in NeoSCAD.Host and is tested there; this file is the glue.

using Microsoft.UI.Windowing;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;
using NeoSCAD.App.Editor;
using NeoSCAD.App.Panels;
using NeoSCAD.App.Viewport;
using NeoSCAD.Host;
using NeoSCAD.Native;
using Windows.Storage.Pickers;

namespace NeoSCAD.App;

public sealed partial class MainWindow : Window
{
    readonly DocumentSession document;
    readonly EditorHost editor;
    readonly ViewportPanel view;
    bool closing;
    CustomizerPanel? customizer;
    CheckPanel? checkPanel;
    MeasurePanel? measurePanel;
    /// <summary>The format Ctrl+Shift+E exports to: the last one chosen.</summary>
    string lastExportFormat = "binstl";
    /// <summary>A ContentDialog is showing (WinUI allows one at a time per window).</summary>
    bool dialogShowing;

    /// <param name="panel">A side panel to open at start (`--panel`), or null.</param>
    public MainWindow(StartupAction startup, string? panel = null)
    {
        InitializeComponent();
        AppWindow.Resize(new Windows.Graphics.SizeInt32(1400, 900));
        // The title bar and taskbar icon. The exe's embedded icon
        // (ApplicationIcon) is what Explorer and the Start menu show; an
        // unpackaged WinUI window does not take it from there by itself.
        var icon = Path.Combine(AppContext.BaseDirectory, "Assets", "NeoSCAD.ico");
        if (File.Exists(icon)) AppWindow.SetIcon(icon);

        var queue = DispatcherQueue;
        var core = CoreService.Shared;
        document = new DocumentSession(core, new WinUiDispatcher(queue), new WinUiTimer(queue),
            SystemClock.Instance, Environment.GetFolderPath(Environment.SpecialFolder.MyDocuments));

        LanguageServer? server = null;
        try
        {
            server = core?.LanguageServer(true);
        }
        catch (CoreException)
        {
        }
        document.Language = new LanguageBridge(server, new WinUiDispatcher(queue), new WinUiTimer(queue));

        view = new ViewportPanel(ViewPanel);
        document.Viewport = view.Viewport;
        if (view.Error is { } viewError) ShowError(ViewError, $"No 3D view: {viewError}");
        view.SchemeChanged += () => document.Run(LastMode());

        editor = new EditorHost(EditorView, document);
        // F5 and F6 from the bundle's own keymap, and the menu's chords the
        // page forwards while the editor has the focus (Shortcuts.cs).
        editor.Command += Perform;
        view.Click += (x, y) => document.PickAt(x, y);

        document.TitleChanged += () => Title = document.Title;
        document.ReportChanged += ShowReport;
        document.ReportChanged += UpdateExportMenu;
        document.ConsoleChanged += ShowConsole;
        document.NoticeChanged += ShowDiskNotice; // MainWindow.Disk.cs
        AppWindow.Closing += OnClosing;
        // The agent link first: it stops answering before the document goes.
        Closed += (_, _) =>
        {
            StopAgents();
            document.Dispose();
        };

        BuildExamplesMenu();
        BuildExportMenu();
        StartLanguageSettings(); // MainWindow.Language.cs
        StartUpdates(); // MainWindow.Updates.cs
        StartAgents(); // MainWindow.Agents.cs
        if (panel is not null) ShowPanel(panel);
        if (core is null) Status.Text = $"The core did not start: {CoreService.Error}";
        AppLog.Write(core is null ? $"core did not start: {CoreService.Error}" : "core started");
        if (view.Error is { } noView) AppLog.Write($"no 3D view: {noView}");
        if (AppLog.Enabled) StartHeartbeat(queue);

        _ = StartAsync(startup);
    }

    /// <summary>
    /// For the log: a line from the UI thread every 5 s for the first 30 s,
    /// so a run whose window froze (a stuck WebView2 callback, say) shows
    /// where the lines stop.
    /// </summary>
    static void StartHeartbeat(Microsoft.UI.Dispatching.DispatcherQueue queue)
    {
        var beats = 0;
        var timer = queue.CreateTimer();
        timer.Interval = TimeSpan.FromSeconds(5);
        timer.Tick += (t, _) =>
        {
            AppLog.Write("UI thread alive");
            if (++beats == 6) t.Stop();
        };
        timer.Start();
    }

    async Task StartAsync(StartupAction startup)
    {
        try
        {
            await editor.StartAsync();
        }
        catch (Exception x)
        {
            // This task is discarded by the constructor, so an exception
            // here would be lost and the window left half started.
            AppLog.Write("editor start threw", x);
        }
        if (editor.Error is { } e) ShowError(EditorError, e);
        if (editor.RuntimeMissing) await OfferWebViewDownloadAsync();
        AppLog.Write($"startup: {startup}");
        switch (startup)
        {
            case StartupAction.OpenFile(var path):
                Open(path);
                break;
            case StartupAction.OpenExample(var id):
                if (Array.Find(SafeExamples(), x => x.Id == id) is { } example) document.LoadExample(example);
                break;
            default:
                document.LoadUntitled("");
                break;
        }
        Title = document.Title;
    }

    /// <summary>
    /// No WebView2 runtime: say so in a dialog with a button that opens
    /// Microsoft's download page, since the pane's text alone is easy to
    /// miss and a link in a TextBlock cannot be clicked.
    /// </summary>
    async Task OfferWebViewDownloadAsync()
    {
        try
        {
            // The window's content may not be loaded yet: this runs from the
            // constructor, and without a runtime nothing above awaited.
            if (Root.XamlRoot is null)
            {
                var loaded = new TaskCompletionSource();
                Root.Loaded += (_, _) => loaded.TrySetResult();
                await loaded.Task;
            }
            var dialog = new ContentDialog
            {
                XamlRoot = Root.XamlRoot,
                Title = WebViewRuntime.MissingTitle,
                Content = new TextBlock { Text = WebViewRuntime.MissingMessage, TextWrapping = TextWrapping.Wrap },
                PrimaryButtonText = "Download WebView2",
                CloseButtonText = "Not now",
                DefaultButton = ContentDialogButton.Primary,
            };
            if (await dialog.ShowAsync() == ContentDialogResult.Primary)
                await Windows.System.Launcher.LaunchUriAsync(new Uri(WebViewRuntime.DownloadUrl));
        }
        catch (Exception x)
        {
            AppLog.Write("webview2 download offer failed", x);
        }
    }

    RenderMode LastMode() => document.LoopState().LastMode ?? RenderMode.Preview;

    static void ShowError(TextBlock block, string message)
    {
        block.Text = message;
        block.Visibility = Visibility.Visible;
    }

    // --- Status and console -------------------------------------------------------

    bool reportedFirstResult;

    void ShowReport()
    {
        if (!reportedFirstResult && document.Report is RunReport.Rendered or RunReport.Failed)
        {
            reportedFirstResult = true;
            AppLog.Write(document.Report switch
            {
                RunReport.Rendered r => $"first result: {r.Mode}, {r.Summary}",
                RunReport.Failed f => $"first result: failed, {f.Message}",
                _ => "",
            });
        }
        Status.Text = document.Report switch
        {
            RunReport.Running r => r.Mode == RenderMode.Render ? "Rendering…" : "Previewing…",
            RunReport.Rendered r => r.Summary,
            RunReport.Failed f => f.Message,
            _ => "",
        };
    }

    void ShowConsole()
    {
        var items = ConsoleList.Items;
        items.Clear();
        foreach (var line in document.Console)
        {
            var block = new TextBlock
            {
                Text = line.Text,
                FontFamily = new FontFamily("Cascadia Mono, Consolas"),
                FontSize = 12,
                TextWrapping = TextWrapping.Wrap,
                IsTextSelectionEnabled = true,
            };
            var brush = line.Kind switch
            {
                ConsoleKind.Error => "SystemFillColorCriticalBrush",
                ConsoleKind.Warning or ConsoleKind.Deprecated => "SystemFillColorCautionBrush",
                _ => null,
            };
            if (brush is not null && Application.Current.Resources.TryGetValue(brush, out var b))
            {
                block.Foreground = (Brush)b;
            }
            items.Add(block);
        }
        if (items.Count > 0) ConsoleList.ScrollIntoView(items[^1]);
    }

    // --- File ---------------------------------------------------------------------

    void BuildExamplesMenu()
    {
        foreach (var example in SafeExamples())
        {
            var item = new MenuFlyoutItem { Text = example.Title, Tag = example.Id };
            item.Click += async (_, _) =>
            {
                if (await ConfirmDiscardAsync()) document.LoadExample(example);
            };
            ExamplesMenu.Items.Add(item);
        }
        ExamplesMenu.IsEnabled = ExamplesMenu.Items.Count > 0;
    }

    static Example[] SafeExamples()
    {
        try
        {
            return NeoScad.Examples();
        }
        catch (Exception e) when (e is CoreException or DllNotFoundException)
        {
            return [];
        }
    }

    async void OnNew(object sender, RoutedEventArgs e)
    {
        if (await ConfirmDiscardAsync()) document.LoadUntitled("");
    }

    async void OnOpen(object sender, RoutedEventArgs e)
    {
        if (!await ConfirmDiscardAsync()) return;
        var picker = new FileOpenPicker { SuggestedStartLocation = PickerLocationId.DocumentsLibrary };
        picker.FileTypeFilter.Add(".scad");
        picker.FileTypeFilter.Add("*");
        InitializeWithWindow(picker);
        if (await picker.PickSingleFileAsync() is { } file) Open(file.Path);
    }

    void Open(string path)
    {
        try
        {
            document.Open(path);
        }
        catch (Exception e) when (e is IOException or UnauthorizedAccessException)
        {
            Status.Text = $"Could not open {path}: {e.Message}";
        }
    }

    async void OnSave(object sender, RoutedEventArgs e) => await SaveAsync();

    async void OnSaveAs(object sender, RoutedEventArgs e) => await SaveAsAsync();

    async Task<bool> SaveAsync()
    {
        if (document.FilePath is null) return await SaveAsAsync();
        return await SaveToFileAsync(); // MainWindow.Disk.cs
    }

    async Task<bool> SaveAsAsync()
    {
        var picker = new FileSavePicker
        {
            SuggestedStartLocation = PickerLocationId.DocumentsLibrary,
            SuggestedFileName = Path.GetFileNameWithoutExtension(document.DisplayName),
        };
        picker.FileTypeChoices.Add("OpenSCAD model", [".scad"]);
        InitializeWithWindow(picker);
        if (await picker.PickSaveFileAsync() is not { } file) return false;
        return Write(() => document.SaveAs(file.Path));
    }

    bool Write(Action save)
    {
        try
        {
            save();
            return true;
        }
        catch (Exception e) when (e is IOException or UnauthorizedAccessException)
        {
            Status.Text = $"Could not save: {e.Message}";
            return false;
        }
    }

    // --- Export ---------------------------------------------------------------------

    ExportFormatInfo[] exportFormats = [];

    /// <summary>File > Export As: every format the core offers, in its order.</summary>
    void BuildExportMenu()
    {
        try
        {
            exportFormats = NeoScad.ExportFormats();
        }
        catch (Exception e) when (e is CoreException or DllNotFoundException)
        {
            exportFormats = [];
        }
        foreach (var format in exportFormats)
        {
            var item = new MenuFlyoutItem { Text = $"{format.Title}…", Tag = format.Id };
            item.Click += async (_, _) => await ExportAsync(format);
            ExportMenu.Items.Add(item);
        }
        ExportMenu.IsEnabled = exportFormats.Length > 0;
        UpdateExportMenu();
    }

    /// <summary>A 2D model's formats only for a 2D model, a 3D model's for a 3D one, once one rendered.</summary>
    void UpdateExportMenu()
    {
        var dims = document.LastDimensions;
        foreach (var item in ExportMenu.Items.OfType<MenuFlyoutItem>())
        {
            var format = Array.Find(exportFormats, f => f.Id == (string)item.Tag);
            item.IsEnabled = format?.Dimension is not { } d || dims is null || d == dims;
        }
        ExportAgainItem.Text = FormatFor(lastExportFormat) is { } f ? $"Export {f.Title}…" : "Export…";
    }

    ExportFormatInfo? FormatFor(string id) => Array.Find(exportFormats, f => f.Id == id);

    /// <summary>Ctrl+Shift+E: the last format again, or the one that suits the model's dimension.</summary>
    async void OnExportAgain(object sender, RoutedEventArgs e) => await ExportAgainAsync();

    async Task ExportAgainAsync()
    {
        string id;
        try
        {
            id = NeoScad.SuggestExportFormat(lastExportFormat, document.LastDimensions);
        }
        catch (CoreException)
        {
            id = lastExportFormat;
        }
        if (FormatFor(id) is { } format) await ExportAsync(format);
    }

    /// <summary>
    /// Ask where, then export: geometry renders in full (with the
    /// customizer's values) under a dialog that shows the stage and can
    /// cancel; the view's image is taken at once.
    /// </summary>
    async Task ExportAsync(ExportFormatInfo format)
    {
        if (dialogShowing) return;
        var picker = new FileSavePicker
        {
            SuggestedStartLocation = format.Kind == ExportKind.Geometry
                ? PickerLocationId.DocumentsLibrary
                : PickerLocationId.PicturesLibrary,
            SuggestedFileName = Path.GetFileNameWithoutExtension(document.DisplayName),
        };
        picker.FileTypeChoices.Add(format.Title, ["." + format.Extension]);
        InitializeWithWindow(picker);
        if (await picker.PickSaveFileAsync() is not { } file) return;
        lastExportFormat = format.Id;
        UpdateExportMenu();
        string? failure;
        if (format.Kind == ExportKind.ViewImage)
        {
            var width = (uint)Math.Clamp(ViewPanel.ActualWidth * ViewPanel.CompositionScaleX, 64, 8192);
            var height = (uint)Math.Clamp(ViewPanel.ActualHeight * ViewPanel.CompositionScaleY, 64, 8192);
            failure = await document.ExportImageAsync(file.Path, width, height);
        }
        else
        {
            failure = await ExportWithProgressAsync(file.Path, file.Name, format);
        }
        Status.Text = failure ?? $"Exported {file.Name}";
        if (failure is not null) AppLog.Write($"export {format.Id} failed: {failure}");
    }

    async Task<string?> ExportWithProgressAsync(string path, string name, ExportFormatInfo format)
    {
        CancelToken cancel;
        try
        {
            cancel = new CancelToken();
        }
        catch (CoreException e)
        {
            return CoreErrors.Describe(e);
        }
        using (cancel)
        {
            var stage = new TextBlock { Text = ExportText.Describe(null) };
            var content = new StackPanel { Spacing = 12, MinWidth = 320 };
            content.Children.Add(new ProgressBar { IsIndeterminate = true });
            content.Children.Add(stage);
            var dialog = new ContentDialog
            {
                XamlRoot = Root.XamlRoot,
                Title = $"Exporting {name}",
                Content = content,
                CloseButtonText = "Cancel",
            };
            var queue = DispatcherQueue;
            Status.Text = $"Exporting {name}…";
            var export = document.ExportAsync(path, format.Id,
                s => queue.TryEnqueue(() => stage.Text = ExportText.Describe(s)), cancel);
            // A quick export finishes before the dialog would flash up.
            if (await Task.WhenAny(export, Task.Delay(400)) != export)
            {
                dialogShowing = true;
                var shown = dialog.ShowAsync().AsTask();
                var first = await Task.WhenAny(export, shown);
                if (first == shown)
                {
                    // Cancel (or Esc): the core stops at its next check
                    // and the export says so; nothing is left behind.
                    try
                    {
                        cancel.Cancel();
                    }
                    catch (CoreException)
                    {
                    }
                }
                else
                {
                    dialog.Hide();
                }
                await shown;
                dialogShowing = false;
            }
            return await export;
        }
    }

    void OnExit(object sender, RoutedEventArgs e) => Close();

    void InitializeWithWindow(object picker) =>
        WinRT.Interop.InitializeWithWindow.Initialize(picker, WinRT.Interop.WindowNative.GetWindowHandle(this));

    /// <summary>Whether unsaved changes may go: asks to save them first.</summary>
    async Task<bool> ConfirmDiscardAsync()
    {
        if (!document.IsDirty) return true;
        var dialog = new ContentDialog
        {
            XamlRoot = Root.XamlRoot,
            Title = $"Save changes to {document.DisplayName}?",
            PrimaryButtonText = "Save",
            SecondaryButtonText = "Don't save",
            CloseButtonText = "Cancel",
            DefaultButton = ContentDialogButton.Primary,
        };
        return await dialog.ShowAsync() switch
        {
            ContentDialogResult.Primary => await SaveAsync(),
            ContentDialogResult.Secondary => true,
            _ => false,
        };
    }

    async void OnClosing(AppWindow sender, AppWindowClosingEventArgs e)
    {
        if (closing || !document.IsDirty) return;
        e.Cancel = true;
        if (await ConfirmDiscardAsync())
        {
            closing = true;
            Close();
        }
    }

    // --- Edit ---------------------------------------------------------------------------

    void OnUndo(object sender, RoutedEventArgs e) => editor.Perform(EditorScript.Undo());
    void OnRedo(object sender, RoutedEventArgs e) => editor.Perform(EditorScript.Redo());
    void OnSelectAll(object sender, RoutedEventArgs e) => editor.Perform(EditorScript.SelectAll());
    void OnFind(object sender, RoutedEventArgs e) => editor.Perform(EditorScript.OpenSearch());

    /// <summary>A command by name: the editor protocol's `command` message (Shortcuts.cs).</summary>
    async void Perform(string name)
    {
        AppLog.Write($"command from the editor: {name}");
        switch (name)
        {
            case Shortcuts.Preview: document.Run(RenderMode.Preview); break;
            case Shortcuts.Render: document.Run(RenderMode.Render); break;
            case Shortcuts.New: OnNew(this, new RoutedEventArgs()); break;
            case Shortcuts.Open: OnOpen(this, new RoutedEventArgs()); break;
            case Shortcuts.Save: await SaveAsync(); break;
            case Shortcuts.SaveAs: await SaveAsAsync(); break;
            case Shortcuts.Export: await ExportAgainAsync(); break;
            case Shortcuts.Check: Check(); break;
            case Shortcuts.Measure: Measure(); break;
            case Shortcuts.Customizer: TogglePanel("customizer"); break;
        }
    }

    // --- Panels ---------------------------------------------------------------------------

    /// <summary>The panel shown in the pane, by its tag ("customizer", "check", "measure").</summary>
    string? shownPanel;

    void ShowPanel(string name)
    {
        UserControl panel = name switch
        {
            "check" => checkPanel ??= new CheckPanel(document),
            "measure" => measurePanel ??= new MeasurePanel(document),
            _ => customizer ??= new CustomizerPanel(document),
        };
        shownPanel = name;
        PanelHost.Content = panel;
        SidePanes.IsPaneOpen = true;
        var tab = PanelTabs.Items.FirstOrDefault(i => (string)i.Tag == name);
        if (tab is not null && PanelTabs.SelectedItem != tab) PanelTabs.SelectedItem = tab;
        UpdatePanelItems();
    }

    void HidePanels()
    {
        SidePanes.IsPaneOpen = false;
        shownPanel = null;
        UpdatePanelItems();
    }

    /// <summary>The menu's toggle: show the panel, or hide the pane when it is the one showing.</summary>
    void TogglePanel(string name)
    {
        if (SidePanes.IsPaneOpen && shownPanel == name) HidePanels();
        else ShowPanel(name);
    }

    void UpdatePanelItems()
    {
        CustomizerItem.IsChecked = SidePanes.IsPaneOpen && shownPanel == "customizer";
        CheckItem.IsChecked = SidePanes.IsPaneOpen && shownPanel == "check";
        MeasureItem.IsChecked = SidePanes.IsPaneOpen && shownPanel == "measure";
    }

    void OnPanelItem(object sender, RoutedEventArgs e)
    {
        if (sender is FrameworkElement { Tag: string name }) TogglePanel(name);
    }

    void OnPanelTab(SelectorBar sender, SelectorBarSelectionChangedEventArgs e)
    {
        if (sender.SelectedItem is { Tag: string name } && name != shownPanel && SidePanes.IsPaneOpen) ShowPanel(name);
    }

    void OnClosePanels(object sender, RoutedEventArgs e) => HidePanels();

    /// <summary>Design > Check: the check panel, checking now.</summary>
    void Check()
    {
        ShowPanel("check");
        _ = document.RunCheckAsync();
    }

    /// <summary>Design > Measure: the measure panel, measuring now.</summary>
    void Measure()
    {
        ShowPanel("measure");
        _ = document.RunMeasureAsync();
    }

    void OnCheck(object sender, RoutedEventArgs e) => Check();
    void OnMeasure(object sender, RoutedEventArgs e) => Measure();

    // --- Design and View ----------------------------------------------------------

    void OnPreview(object sender, RoutedEventArgs e) => document.Run(RenderMode.Preview);
    void OnRender(object sender, RoutedEventArgs e) => document.Run(RenderMode.Render);
    void OnViewAll(object sender, RoutedEventArgs e) => view.Perform(v => v.ViewAll());
    void OnResetView(object sender, RoutedEventArgs e) => view.Perform(v => v.ResetView());

    void OnPreset(object sender, RoutedEventArgs e)
    {
        if (sender is FrameworkElement { Tag: string tag } && Enum.TryParse<ViewPreset>(tag, out var preset))
        {
            view.Perform(v => v.SetView(preset));
        }
    }
}
