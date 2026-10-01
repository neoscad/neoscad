// Small pieces the panels share: text in the Fluent type ramp, the theme's
// brushes and styles looked up without failing (a missing key in code is
// an exception, where XAML would fail to load), and numbers formatted by
// the core, so a value reads the same in every app.

using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;
using NeoSCAD.Native;

namespace NeoSCAD.App.Panels;

static class PanelText
{
    public static TextBlock Body(string text) => Styled(text, "BodyTextBlockStyle");

    public static TextBlock Caption(string text)
    {
        var t = Styled(text, "CaptionTextBlockStyle");
        if (Brush("TextFillColorSecondaryBrush") is { } b) t.Foreground = b;
        return t;
    }

    public static TextBlock Strong(string text) => Styled(text, "BodyStrongTextBlockStyle");

    static TextBlock Styled(string text, string style)
    {
        var t = new TextBlock { Text = text, TextWrapping = TextWrapping.Wrap, IsTextSelectionEnabled = true };
        if (Application.Current.Resources.TryGetValue(style, out var s) && s is Style st) t.Style = st;
        return t;
    }

    /// <summary>A theme brush by key, or null when the theme lacks it.</summary>
    public static Brush? Brush(string key) =>
        Application.Current.Resources.TryGetValue(key, out var b) ? b as Brush : null;

    /// <summary>The accent ("primary action") style for a button, when the theme has it.</summary>
    public static void Accent(Button button)
    {
        if (Application.Current.Resources.TryGetValue("AccentButtonStyle", out var s) && s is Style st) button.Style = st;
    }

    /// <summary>A number as the core writes it (no trailing zeros, no float noise).</summary>
    public static string Number(double x)
    {
        try
        {
            return NeoScad.FormatNumber(x);
        }
        catch (Exception e) when (e is CoreException or DllNotFoundException or TypeInitializationException)
        {
            return x.ToString("G6", System.Globalization.CultureInfo.CurrentCulture);
        }
    }

    public static string Point(double[] p) => "(" + string.Join(", ", p.Select(Number)) + ")";
}
