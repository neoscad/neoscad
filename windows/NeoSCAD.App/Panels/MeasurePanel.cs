// The measure panel: the model's volume, area and size (`neoscad
// measure`, with the customizer's values), and point-to-point distance:
// with picking on, two clicks on the model give the distance between the
// points, both marked in the 3D view. The counterpart of
// apple/App/Panels/MeasurePanel.swift (its sections and part-to-part
// distances are not here yet; docs/followups.md). What it does to the
// document is DocumentSession.Panels.cs ("Measure").

using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using NeoSCAD.Host;
using NeoSCAD.Native;

namespace NeoSCAD.App.Panels;

public sealed class MeasurePanel : UserControl
{
    readonly DocumentSession document;
    readonly Button run = new() { Content = "Measure" };
    readonly ProgressRing ring = new() { IsActive = false, Width = 16, Height = 16 };
    readonly TextBlock error = PanelText.Body("");
    readonly TextBlock stats = PanelText.Body("");
    readonly ToggleSwitch picking = new() { Header = "Pick points in the view", OnContent = "On", OffContent = "Off" };
    readonly TextBlock picks = PanelText.Body("");
    readonly TextBlock distance = PanelText.Strong("");
    readonly Button clear = new() { Content = "Clear Points" };
    bool updating;

    public MeasurePanel(DocumentSession document)
    {
        this.document = document;
        PanelText.Accent(run);
        run.Click += (_, _) => _ = document.RunMeasureAsync();
        clear.Click += (_, _) => document.ClearPicks();
        picking.Toggled += (_, _) =>
        {
            if (updating) return;
            document.Picking = picking.IsOn;
            // Picking needs a measurement to cut rays against.
            if (picking.IsOn && document.Measurement is null && !document.MeasureRunning) _ = document.RunMeasureAsync();
            Refresh();
        };
        if (PanelText.Brush("SystemFillColorCriticalBrush") is { } critical) error.Foreground = critical;

        var action = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 8 };
        action.Children.Add(run);
        action.Children.Add(ring);

        var body = new StackPanel { Spacing = 10, Padding = new Thickness(12, 8, 12, 12) };
        body.Children.Add(action);
        body.Children.Add(error);
        body.Children.Add(stats);
        body.Children.Add(picking);
        body.Children.Add(PanelText.Caption("Click the model twice: the distance between the two points. A third click starts again."));
        body.Children.Add(picks);
        body.Children.Add(distance);
        body.Children.Add(clear);
        Content = new ScrollViewer { Content = body };

        document.MeasureChanged += Refresh;
        Refresh();
    }

    void Refresh()
    {
        updating = true;
        try
        {
            run.IsEnabled = !document.MeasureRunning;
            ring.IsActive = document.MeasureRunning;
            error.Text = document.MeasureError ?? "";
            error.Visibility = document.MeasureError is null ? Visibility.Collapsed : Visibility.Visible;
            stats.Text = Describe(document.Measurement);
            picking.IsOn = document.Picking;
            picks.Text = string.Join("\n", document.Picks.Select((p, i) => $"{(char)('A' + i)}: {PanelText.Point(p)}"));
            distance.Text = document.PickedDistance is { } d ? $"Distance: {PanelText.Number(d)} mm" : "";
            clear.IsEnabled = document.Picks.Count > 0;
        }
        finally
        {
            updating = false;
        }
    }

    static string Describe(MeasureResult? r)
    {
        if (r is null) return "Measure the model for its volume, area and size.";
        if (r.Model is not { } m) return "The model has no solid to measure (2D, or empty).";
        var size = m.BboxMax.Zip(m.BboxMin, (hi, lo) => PanelText.Number(hi - lo));
        var lines = new List<string>
        {
            $"Volume: {PanelText.Number(m.Volume)} mm³",
            $"Surface: {PanelText.Number(m.Area)} mm²",
            $"Size: {string.Join(" × ", size)} mm",
            $"Centre of mass: {PanelText.Point(m.Centroid)}",
        };
        if (r.Components is { } c) lines.Add($"Pieces: {c}");
        if (r.Manifold == false) lines.Add("Not a closed solid (not manifold).");
        if (r.Parts.Length > 0) lines.Add($"Parts: {string.Join(", ", r.Parts.Select(p => p.Name))}");
        return string.Join("\n", lines);
    }
}
