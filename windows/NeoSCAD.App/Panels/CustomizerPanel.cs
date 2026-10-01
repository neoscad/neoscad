// The customizer: the document's annotated parameters (OpenSCAD's
// customizer comments, read by the core from the text), each with the
// Fluent control for the control OpenSCAD's customizer gives it, grouped
// as the file groups them. The counterpart of
// apple/App/Panels/CustomizerView.swift.
//
// Editing a value never touches the text: every edit goes through the
// core's `edit_parameter` (snapped to the slider's step, clamped, cut to
// length) into the document loop's values, and the document previews
// again with them (DocumentSession.Panels.cs). Reset returns every value
// to the text's. Parameter sets are OpenSCAD's JSON file beside the model.
//
// The controls are rebuilt only when the parameters themselves change (a
// new text); a value change only updates them, so a field being typed in
// keeps the focus and the caret.

using Microsoft.UI.Text;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using NeoSCAD.Host;
using NeoSCAD.Native;
using Windows.Globalization.NumberFormatting;

namespace NeoSCAD.App.Panels;

public sealed class CustomizerPanel : UserControl
{
    readonly DocumentSession document;
    readonly ComboBox sets = new() { MinWidth = 160, HorizontalAlignment = HorizontalAlignment.Stretch };
    readonly Button save = new() { Content = "Save…" };
    readonly Button reset = new() { Content = "Reset" };
    readonly TextBlock message = PanelText.Caption("");
    readonly StackPanel body = new() { Spacing = 8, Padding = new Thickness(8) };
    /// <summary>Puts each control's value (and its name's weight) in step with the document.</summary>
    readonly List<Action> refreshers = [];
    /// <summary>Set while the panel writes to its own controls, whose change events then do nothing.</summary>
    bool updating;

    public CustomizerPanel(DocumentSession document)
    {
        this.document = document;
        ToolTipService.SetToolTip(save, "Save the current values as a parameter set");
        ToolTipService.SetToolTip(reset, "Return every value to the one in the text");
        save.Click += async (_, _) => await SaveSetAsync();
        reset.Click += (_, _) => document.ResetParameters();
        sets.SelectionChanged += (_, _) => ChooseSet();

        var toolbar = new Grid { ColumnSpacing = 6, Padding = new Thickness(8, 8, 8, 0) };
        toolbar.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        toolbar.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        toolbar.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        toolbar.Children.Add(sets);
        Grid.SetColumn(save, 1);
        toolbar.Children.Add(save);
        Grid.SetColumn(reset, 2);
        toolbar.Children.Add(reset);

        var root = new Grid();
        root.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });
        root.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });
        root.RowDefinitions.Add(new RowDefinition { Height = new GridLength(1, GridUnitType.Star) });
        root.Children.Add(toolbar);
        message.Margin = new Thickness(12, 4, 12, 0);
        Grid.SetRow(message, 1);
        root.Children.Add(message);
        var scroll = new ScrollViewer { Content = body };
        Grid.SetRow(scroll, 2);
        root.Children.Add(scroll);
        Content = root;

        document.ParametersChanged += Rebuild;
        document.ParameterValuesChanged += Refresh;
        Rebuild();
    }

    // --- Building ---------------------------------------------------------------------

    void Rebuild()
    {
        body.Children.Clear();
        refreshers.Clear();
        if (document.ParameterGroups.Count == 0)
        {
            body.Children.Add(PanelText.Body("No parameters. Top-level assignments before the first module or " +
                                      "function, with customizer comments, appear here."));
        }
        foreach (var group in document.ParameterGroups)
        {
            var rows = new StackPanel { Spacing = 12 };
            foreach (var p in group.Parameters) rows.Children.Add(Row(p));
            body.Children.Add(new Expander
            {
                Header = group.Name,
                IsExpanded = true,
                HorizontalAlignment = HorizontalAlignment.Stretch,
                HorizontalContentAlignment = HorizontalAlignment.Stretch,
                Content = rows,
            });
        }
        Refresh();
    }

    FrameworkElement Row(Parameter p)
    {
        var name = new TextBlock { Text = p.Name, VerticalAlignment = VerticalAlignment.Center };
        var revert = new Button
        {
            Content = new FontIcon { Glyph = "", FontSize = 12 },
            Padding = new Thickness(4),
            Background = null,
            BorderThickness = new Thickness(0),
        };
        ToolTipService.SetToolTip(revert, "Back to the text's value");
        revert.Click += (_, _) => document.SetParameter(p.Name, null);
        var header = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 4 };
        header.Children.Add(name);
        header.Children.Add(revert);
        refreshers.Add(() =>
        {
            var edited = document.ParameterValues.ContainsKey(p.Name);
            name.FontWeight = edited ? FontWeights.SemiBold : FontWeights.Normal;
            revert.Visibility = edited ? Visibility.Visible : Visibility.Collapsed;
        });

        var row = new StackPanel { Spacing = 4 };
        row.Children.Add(header);
        if (p.Description.Length > 0) row.Children.Add(PanelText.Caption(p.Description));
        row.Children.Add(Control(p));
        return row;
    }

    void Edit(Parameter p, ParameterEdit edit)
    {
        if (!updating) document.EditParameter(p, edit);
    }

    double Number(Parameter p) => document.ValueOf(p) is ParameterValue.Number n ? n.Value : 0;

    FrameworkElement Control(Parameter p)
    {
        switch (p.Control)
        {
            case ParameterControl.Slider s:
            {
                var max = Math.Max(s.Min, s.Max);
                var slider = new Slider
                {
                    Minimum = s.Min,
                    Maximum = max,
                    // A slider without a step moves in hundredths of a
                    // small range, in ones of a larger one: the core keeps
                    // the value as it lands (`edit_parameter`'s Slide).
                    StepFrequency = s.Step ?? (max - s.Min >= 10 ? 1 : Math.Max((max - s.Min) / 100, 1e-9)),
                    VerticalAlignment = VerticalAlignment.Center,
                };
                var box = NumberField(s.Step);
                slider.ValueChanged += (_, e) => Edit(p, new ParameterEdit.Slide(e.NewValue));
                box.ValueChanged += (_, e) =>
                {
                    if (!double.IsNaN(e.NewValue)) Edit(p, new ParameterEdit.Type(e.NewValue));
                };
                refreshers.Add(() =>
                {
                    var v = Number(p);
                    slider.Value = v;
                    box.Value = v;
                });
                var grid = new Grid { ColumnSpacing = 8 };
                grid.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
                grid.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(96) });
                grid.Children.Add(slider);
                Grid.SetColumn(box, 1);
                grid.Children.Add(box);
                return grid;
            }
            case ParameterControl.SpinBox s:
            {
                var box = NumberField(s.Step);
                box.SpinButtonPlacementMode = NumberBoxSpinButtonPlacementMode.Inline;
                box.HorizontalAlignment = HorizontalAlignment.Left;
                box.MinWidth = 140;
                box.ValueChanged += (_, e) =>
                {
                    if (!double.IsNaN(e.NewValue)) Edit(p, new ParameterEdit.Type(e.NewValue));
                };
                refreshers.Add(() => box.Value = Number(p));
                return box;
            }
            case ParameterControl.Text t:
            {
                var box = new TextBox();
                if (t.MaxLength is { } n) box.MaxLength = (int)Math.Min(n, int.MaxValue);
                box.TextChanged += (_, _) => Edit(p, new ParameterEdit.Set(new ParameterValue.Text(box.Text)));
                refreshers.Add(() =>
                {
                    var v = document.ValueOf(p) is ParameterValue.Text s ? s.Value : "";
                    // Only when it differs: setting the same text would put
                    // the caret back at the start while the user types.
                    if (box.Text != v) box.Text = v;
                });
                return box;
            }
            case ParameterControl.Vector v:
            {
                var count = p.DefaultValue is ParameterValue.Vector d ? d.Value.Length : 0;
                var grid = new Grid { ColumnSpacing = 4 };
                var boxes = new NumberBox[count];
                for (var i = 0; i < count; i++)
                {
                    grid.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
                    var box = NumberField(v.Step);
                    var index = (uint)i;
                    box.ValueChanged += (_, e) =>
                    {
                        if (!double.IsNaN(e.NewValue)) Edit(p, new ParameterEdit.Item(index, e.NewValue));
                    };
                    Grid.SetColumn(box, i);
                    grid.Children.Add(box);
                    boxes[i] = box;
                }
                refreshers.Add(() =>
                {
                    var items = document.ValueOf(p) is ParameterValue.Vector x ? x.Value : [];
                    for (var i = 0; i < boxes.Length; i++) boxes[i].Value = i < items.Length ? items[i] : double.NaN;
                });
                return grid;
            }
            case ParameterControl.Dropdown d:
            {
                var combo = new ComboBox { MinWidth = 160 };
                foreach (var o in d.Options) combo.Items.Add(o.Label);
                combo.SelectionChanged += (_, _) =>
                {
                    var i = combo.SelectedIndex;
                    if (i >= 0 && i < d.Options.Length) Edit(p, new ParameterEdit.Set(d.Options[i].Value));
                };
                refreshers.Add(() =>
                {
                    var value = ParameterShapes.Value(document.ValueOf(p));
                    combo.SelectedIndex = Array.FindIndex(d.Options, o => ParameterShapes.Value(o.Value) == value);
                });
                return combo;
            }
            default:
            {
                var toggle = new ToggleSwitch { OnContent = "", OffContent = "", MinWidth = 0 };
                toggle.Toggled += (_, _) => Edit(p, new ParameterEdit.Set(new ParameterValue.Bool(toggle.IsOn)));
                refreshers.Add(() => toggle.IsOn = document.ValueOf(p) is ParameterValue.Bool { Value: true });
                return toggle;
            }
        }
    }

    /// <summary>
    /// A number field: committed on Enter or when it loses the focus (a
    /// half-typed "1." does not run the model), shown with as many
    /// decimals as the value has.
    /// </summary>
    static NumberBox NumberField(double? step) => new()
    {
        SmallChange = step ?? 1,
        LargeChange = (step ?? 1) * 10,
        ValidationMode = NumberBoxValidationMode.InvalidInputOverwritten,
        NumberFormatter = new DecimalFormatter
        {
            IntegerDigits = 1,
            FractionDigits = 0,
            NumberRounder = new SignificantDigitsNumberRounder { SignificantDigits = 12 },
        },
    };

    // --- Values and sets ------------------------------------------------------------

    void Refresh()
    {
        updating = true;
        try
        {
            foreach (var r in refreshers) r();
            sets.Items.Clear();
            sets.Items.Add("Design default values");
            foreach (var s in document.ParameterSets) sets.Items.Add(s);
            var selected = document.SelectedParameterSet is { } name
                ? document.ParameterSets.ToList().IndexOf(name) + 1
                : 0;
            sets.SelectedIndex = Math.Max(selected, 0);
            var available = document.ParameterSetPath is not null;
            sets.IsEnabled = available;
            save.IsEnabled = available && document.ParameterGroups.Count > 0;
            reset.IsEnabled = document.ParameterValues.Count > 0;
            ToolTipService.SetToolTip(sets, available
                ? "Parameter sets from the JSON file beside the model"
                : "Save the document to keep parameter sets beside it");
        }
        finally
        {
            updating = false;
        }
    }

    void ChooseSet()
    {
        if (updating) return;
        var i = sets.SelectedIndex;
        if (i <= 0)
        {
            document.ResetParameters();
            message.Text = "";
        }
        else if (i - 1 < document.ParameterSets.Count)
        {
            message.Text = document.ApplyParameterSet(document.ParameterSets[i - 1]) ?? "";
        }
    }

    async Task SaveSetAsync()
    {
        if (document.ParameterSetPath is not { } json || XamlRoot is null) return;
        var field = new TextBox
        {
            Text = document.SelectedParameterSet ?? $"Set {document.ParameterSets.Count + 1}",
            Header = $"The set is saved in {Path.GetFileName(json)}, next to the model.",
        };
        var dialog = new ContentDialog
        {
            XamlRoot = XamlRoot,
            Title = "Save Parameter Set",
            Content = field,
            PrimaryButtonText = "Save",
            CloseButtonText = "Cancel",
            DefaultButton = ContentDialogButton.Primary,
        };
        if (await dialog.ShowAsync() != ContentDialogResult.Primary) return;
        message.Text = document.SaveParameterSet(field.Text) ?? "";
    }
}
