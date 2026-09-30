// The display-scale arithmetic of the 3D view's SwapChainPanel
// (NeoSCAD.App/Viewport/ViewportPanel.cs), kept out of the WinUI project
// so it is tested on any OS.
//
// The core sizes the panel's swap chain in physical pixels (DIPs times the
// panel's CompositionScaleX and CompositionScaleY, truncated) and sets the
// inverse scale on it (crates/ffi/src/viewport.rs,
// attach_swap_chain_panel). Its viewport has one scale, CompositionScaleX,
// so its "points" are DIPs across but CompositionScaleY / CompositionScaleX
// DIPs down; a pointer delta sent unconverted would pan the model a little
// faster or slower than the pointer vertically whenever the two differ.

namespace NeoSCAD.Host;

public static class PanelScale
{
    /// <summary>
    /// A composition scale as XAML reports it, or 1 for one that cannot
    /// be (a panel not yet in a window reports 0). The core applies the
    /// same rule.
    /// </summary>
    public static double Sane(double scale) => double.IsFinite(scale) && scale > 0 ? scale : 1.0;

    /// <summary>
    /// The swap chain's size in physical pixels for a panel
    /// <paramref name="width"/> by <paramref name="height"/> DIPs, as the
    /// core computes it (truncated, 0 for an empty or invalid size): for
    /// the log.
    /// </summary>
    public static (uint Width, uint Height) Pixels(double width, double height, double scaleX, double scaleY) =>
        (Truncate(width * Sane(scaleX)), Truncate(height * Sane(scaleY)));

    static uint Truncate(double px) =>
        double.IsFinite(px) && px > 0 ? (uint)Math.Min(px, uint.MaxValue) : 0u;

    /// <summary>
    /// A pointer move of <paramref name="dx"/>, <paramref name="dy"/> DIPs
    /// as the viewport's points: unchanged across, scaled by
    /// scaleY / scaleX down.
    /// </summary>
    public static (double Dx, double Dy) PointerDelta(double dx, double dy, double scaleX, double scaleY) =>
        (dx, dy * Sane(scaleY) / Sane(scaleX));
}
