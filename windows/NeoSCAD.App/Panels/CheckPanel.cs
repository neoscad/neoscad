// The check panel: is the model printable on an FDM printer? The core's
// `check` (thin walls, overhangs, floating parts, the bed) on the
// document's text with its customizer values, against the printer set
// here. Each finding shows its severity, message and fix; selecting one
// picks it out in the 3D view's overlay (its numbered marker with its box)
// and turns the view to it. The counterpart of
// apple/App/Panels/CheckPanel.swift; what it does to the document is
// DocumentSession.Panels.cs ("Check").

using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using NeoSCAD.Host;
using NeoSCAD.Native;

namespace NeoSCAD.App.Panels;

public sealed class CheckPanel : UserControl
{
    readonly DocumentSession document;
    readonly PrinterPreset[] presets;
    readonly ComboBox printer = new() { Header = "Printer", HorizontalAlignment = HorizontalAlignment.Stretch };
    readonly NumberBox nozzle = Field("Nozzle (mm)", 0.05);
    readonly NumberBox minWall = Field("Thinnest wall (mm)", 0.1);
    readonly NumberBox overhang = Field("Steepest overhang (°)", 5);
    readonly CheckBox afterRender = new() { Content = "Check after each render" };
    readonly Button run = new() { Content = "Check" };
    readonly ProgressRing ring = new() { IsActive = false, Width = 16, Height = 16 };
    readonly TextBlock summary = PanelText.Strong("");
    readonly TextBlock error = PanelText.Body("");
    readonly ListView findings = new() { SelectionMode = ListViewSelectionMode.Single };
    CheckReport? shown;
    bool updating;

    public CheckPanel(DocumentSession document)
    {
        this.document = document;
        presets = Safe(NeoScad.PrinterPresets, []);
        foreach (var p in presets) printer.Items.Add(p.Name);
        printer.Items.Add("Custom");
        printer.SelectionChanged += (_, _) => ChoosePrinter();
        nozzle.ValueChanged += (_, e) => Edit(s => s with { Nozzle = e.NewValue });
        minWall.ValueChanged += (_, e) => Edit(s => s with { MinWall = e.NewValue });
        overhang.ValueChanged += (_, e) => Edit(s => s with { MaxOverhang = e.NewValue });
        afterRender.Checked += (_, _) => document.CheckAfterRender = true;
        afterRender.Unchecked += (_, _) => document.CheckAfterRender = false;
        PanelText.Accent(run);
        run.Click += (_, _) => _ = document.RunCheckAsync();
        if (PanelText.Brush("SystemFillColorCriticalBrush") is { } critical) error.Foreground = critical;
        findings.SelectionChanged += (_, _) =>
        {
            if (!updating) document.SelectFinding((findings.SelectedItem as FrameworkElement)?.Tag as uint?);
        };

        var settings = new Grid { ColumnSpacing = 8, RowSpacing = 8 };
        settings.ColumnDefinitions.Add(new ColumnDefinition());
        settings.ColumnDefinitions.Add(new ColumnDefinition());
        for (var i = 0; i < 3; i++) settings.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });
        Place(settings, printer, 0, 0, 2);
        Place(settings, nozzle, 1, 0);
        Place(settings, minWall, 1, 1);
        Place(settings, overhang, 2, 0);

        var action = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 8 };
        action.Children.Add(run);
        action.Children.Add(ring);
        action.Children.Add(afterRender);

        var top = new StackPanel { Spacing = 8, Padding = new Thickness(12, 8, 12, 4) };
        top.Children.Add(settings);
        top.Children.Add(action);
        top.Children.Add(summary);
        top.Children.Add(error);

        var root = new Grid();
        root.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });
        root.RowDefinitions.Add(new RowDefinition { Height = new GridLength(1, GridUnitType.Star) });
        root.Children.Add(top);
        Grid.SetRow(findings, 1);
        root.Children.Add(findings);
        Content = root;

        document.CheckChanged += Refresh;
        Refresh();
    }

    static NumberBox Field(string header, double step) => new()
    {
        Header = header,
        SmallChange = step,
        SpinButtonPlacementMode = NumberBoxSpinButtonPlacementMode.Compact,
        ValidationMode = NumberBoxValidationMode.InvalidInputOverwritten,
    };

    static void Place(Grid grid, FrameworkElement e, int row, int column, int span = 1)
    {
        Grid.SetRow(e, row);
        Grid.SetColumn(e, column);
        Grid.SetColumnSpan(e, span);
        grid.Children.Add(e);
    }

    static T Safe<T>(Func<T> call, T fallback)
    {
        try
        {
            return call();
        }
        catch (Exception e) when (e is CoreException or DllNotFoundException or TypeInitializationException)
        {
            return fallback;
        }
    }

    /// <summary>A field changed: the settings, checked by the core, become "Custom" unless they match a preset.</summary>
    void Edit(Func<PrinterSettings, PrinterSettings> change)
    {
        if (updating) return;
        var s = change(document.CheckSettings);
        if (double.IsNaN(s.Nozzle) || double.IsNaN(s.MinWall) || double.IsNaN(s.MaxOverhang)) return;
        document.CheckSettings = Safe(() => NeoScad.ValidatedPrinterSettings(s with { Preset = "" }), s);
        Refresh();
    }

    void ChoosePrinter()
    {
        if (updating) return;
        var i = printer.SelectedIndex;
        if (i < 0 || i >= presets.Length) return;
        document.CheckSettings = Safe(() => NeoScad.ApplyPrinterPreset(document.CheckSettings, presets[i].Id),
            document.CheckSettings);
        Refresh();
    }

    void Refresh()
    {
        updating = true;
        try
        {
            var s = document.CheckSettings;
            var preset = Array.FindIndex(presets, p => p.Id == s.Preset);
            printer.SelectedIndex = preset >= 0 ? preset : presets.Length;
            nozzle.Value = s.Nozzle;
            minWall.Value = s.MinWall;
            overhang.Value = s.MaxOverhang;
            afterRender.IsChecked = document.CheckAfterRender;
            run.IsEnabled = !document.CheckRunning;
            ring.IsActive = document.CheckRunning;
            summary.Text = document.CheckRunning ? "Checking…" : document.CheckSummary();
            error.Text = document.CheckError ?? "";
            error.Visibility = document.CheckError is null ? Visibility.Collapsed : Visibility.Visible;
            if (!ReferenceEquals(shown, document.CheckReport))
            {
                shown = document.CheckReport;
                findings.Items.Clear();
                foreach (var f in shown?.Findings ?? []) findings.Items.Add(Item(f));
            }
            findings.SelectedItem = findings.Items.OfType<FrameworkElement>()
                .FirstOrDefault(i => i.Tag is uint id && id == document.SelectedFinding);
        }
        finally
        {
            updating = false;
        }
    }

    static FrameworkElement Item(CheckFinding f)
    {
        var (glyph, brush) = f.Severity switch
        {
            FindingSeverity.Error => ("", "SystemFillColorCriticalBrush"),
            FindingSeverity.Warning => ("", "SystemFillColorCautionBrush"),
            _ => ("", "SystemFillColorAttentionBrush"),
        };
        var icon = new FontIcon { Glyph = glyph, FontSize = 16, VerticalAlignment = VerticalAlignment.Top };
        if (PanelText.Brush(brush) is { } b) icon.Foreground = b;
        AutomationName(icon, f.Severity.ToString());

        var text = new StackPanel { Spacing = 2 };
        var title = PanelText.Strong($"{f.Id}. {f.Message}");
        title.IsTextSelectionEnabled = false;
        text.Children.Add(title);
        if (f.Fix.Length > 0) text.Children.Add(PanelText.Caption(f.Fix));
        if (f.Part is { } part) text.Children.Add(PanelText.Caption($"Part: {part}"));

        var row = new Grid { ColumnSpacing = 10, Padding = new Thickness(0, 6, 0, 6), Tag = f.Id };
        row.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        row.ColumnDefinitions.Add(new ColumnDefinition());
        row.Children.Add(icon);
        Grid.SetColumn(text, 1);
        row.Children.Add(text);
        return row;
    }

    static void AutomationName(UIElement e, string name) =>
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(e, name);
}
