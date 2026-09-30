// A document window: wires the document's loop (NeoSCAD.Host's
// DocumentSession) to the editor pane, the 3D view, the console and the
// menus. Everything that is not a control lives in NeoSCAD.Host and is
// tested there; this file is the glue.

using Microsoft.UI.Windowing;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;
using NeoSCAD.App.Editor;
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

    public MainWindow(StartupAction startup)
    {
        InitializeComponent();
        AppWindow.Resize(new Windows.Graphics.SizeInt32(1400, 900));

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
        editor.Command += name =>
        {
            if (name == "preview") document.Run(RenderMode.Preview);
            else if (name == "render") document.Run(RenderMode.Render);
        };

        document.TitleChanged += () => Title = document.Title;
        document.ReportChanged += ShowReport;
        document.ConsoleChanged += ShowConsole;
        AppWindow.Closing += OnClosing;
        Closed += (_, _) => document.Dispose();

        BuildExamplesMenu();
        if (core is null) Status.Text = $"The core did not start: {CoreService.Error}";

        _ = StartAsync(startup);
    }

    async Task StartAsync(StartupAction startup)
    {
        await editor.StartAsync();
        if (editor.Error is { } e) ShowError(EditorError, e);
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

    RenderMode LastMode() => document.LoopState().LastMode ?? RenderMode.Preview;

    static void ShowError(TextBlock block, string message)
    {
        block.Text = message;
        block.Visibility = Visibility.Visible;
    }

    // --- Status and console -------------------------------------------------------

    void ShowReport()
    {
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
        return Write(() => document.Save());
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

    async void OnExportStl(object sender, RoutedEventArgs e)
    {
        var picker = new FileSavePicker
        {
            SuggestedStartLocation = PickerLocationId.DocumentsLibrary,
            SuggestedFileName = Path.GetFileNameWithoutExtension(document.DisplayName),
        };
        picker.FileTypeChoices.Add("STL mesh", [".stl"]);
        InitializeWithWindow(picker);
        if (await picker.PickSaveFileAsync() is not { } file) return;
        Status.Text = "Exporting…";
        var failure = await document.ExportAsync(file.Path, "stl");
        Status.Text = failure ?? $"Exported {file.Name}";
    }

    async void OnExportImage(object sender, RoutedEventArgs e)
    {
        var picker = new FileSavePicker
        {
            SuggestedStartLocation = PickerLocationId.PicturesLibrary,
            SuggestedFileName = Path.GetFileNameWithoutExtension(document.DisplayName),
        };
        picker.FileTypeChoices.Add("PNG image", [".png"]);
        InitializeWithWindow(picker);
        if (await picker.PickSaveFileAsync() is not { } file) return;
        var width = (uint)Math.Max(64, ViewPanel.ActualWidth * ViewPanel.CompositionScaleX);
        var height = (uint)Math.Max(64, ViewPanel.ActualHeight * ViewPanel.CompositionScaleY);
        var failure = await document.ExportImageAsync(file.Path, width, height);
        Status.Text = failure ?? $"Exported {file.Name}";
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
