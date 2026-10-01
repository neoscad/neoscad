// The 3D view: the core's wgpu renderer drawing with Direct3D 12 into the
// window's WinUI 3 SwapChainPanel (crates/ffi/src/layer.rs,
// "SwapChainPanel"). The Windows counterpart of
// apple/App/Viewport/MetalView.swift.
//
// The camera lives in Rust (render::camera): this class sends pointer
// deltas in device-independent pixels (PanelScale.PointerDelta) and never
// holds a camera of its own, so the view cannot drift from exports and
// snapshots.
//
//   left drag           orbit
//   right/middle drag   pan (the model follows the pointer)
//   wheel               zoom, a tenth of the distance a notch, as OpenSCAD
//
// Frames. CompositionTarget.Rendering fires once per display refresh on
// the UI thread; a frame is drawn only when the core says one is due (a
// camera move, a new model), so an idle view costs a flag check.
//
// Scale. The core sizes the swap chain in physical pixels (DIPs times the
// panel's CompositionScaleX/Y) and sets the inverse scale on it
// (IDXGISwapChain2::SetMatrixTransform, crates/ffi/src/layer.rs) after
// every configure, so frames are drawn at the display's resolution and
// shown at the panel's size. Without the transform a panel shows one
// buffer pixel per DIP: a DIP-sized buffer was stretched and blurred at
// 150%, a pixel-sized one would overflow the panel. The scale changes
// with the display, the system setting and zoom transforms above the
// panel, which CompositionScaleChanged reports, so both it and
// SizeChanged resize.

using System.Runtime.InteropServices;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Input;
using Microsoft.UI.Xaml.Media;
using NeoSCAD.Host;
using NeoSCAD.Native;
using WinRT;

namespace NeoSCAD.App.Viewport;

/// <summary>
/// Drives one SwapChainPanel (declared in MainWindow.xaml): attaches the
/// core's viewport to it, forwards the pointer and draws frames. A wrapper
/// rather than a subclass, so the panel stays a plain XAML element.
/// </summary>
public sealed class ViewportPanel
{
    /// <summary>The WinUI 3 ISwapChainPanelNative (microsoft.ui.xaml.media.dxinterop.h), as wgpu-hal declares it.</summary>
    static readonly Guid IidSwapChainPanelNative = new("63aad0b8-7c24-40ff-85a8-640d944cc325");

    /// <summary>OpenSCAD's default scheme for a light theme, and a dark one (the macOS app's pair).</summary>
    public const string LightScheme = "Cornfield";
    public const string DarkScheme = "Tomorrow Night";

    readonly SwapChainPanel panel;
    Native.Viewport? viewport;
    bool attached;
    Windows.Foundation.Point? last;
    bool panning;
    /// <summary>Where a left press started, while it has not moved far enough to be a drag.</summary>
    Windows.Foundation.Point? click;

    /// <summary>
    /// How far (DIPs) a left press may move and still be a click: a hand
    /// on a mouse wobbles a pixel or two, and that orbit is not a pick.
    /// </summary>
    const double ClickSlop = 4;

    /// <summary>
    /// A left click on the view (no drag), at its point in DIPs from the
    /// panel's top left, the coordinates the core's `ray_at` takes: the
    /// measure panel's pick. The orbit it began still happens.
    /// </summary>
    public event Action<double, double>? Click;

    public ViewportPanel(SwapChainPanel panel)
    {
        this.panel = panel;
        panel.Loaded += (_, _) => Attach();
        panel.Unloaded += (_, _) => Detach();
        panel.SizeChanged += (_, _) => Resize("size");
        panel.CompositionScaleChanged += (_, _) => Resize("scale");
        panel.PointerPressed += OnPressed;
        panel.PointerMoved += OnMoved;
        panel.PointerReleased += OnReleased;
        panel.PointerCaptureLost += (_, _) =>
        {
            last = null;
            click = null;
        };
        panel.PointerWheelChanged += OnWheel;
        panel.ActualThemeChanged += (_, _) => ApplyTheme();
    }

    /// <summary>The core's viewport, made on first use; null when there is no GPU (see <see cref="Error"/>).</summary>
    public Native.Viewport? Viewport
    {
        get
        {
            if (viewport is null && Error is null)
            {
                try
                {
                    viewport = new Native.Viewport(SchemeFor(panel.ActualTheme));
                }
                catch (CoreException e)
                {
                    Error = CoreErrors.Describe(e);
                }
                catch (DllNotFoundException e)
                {
                    Error = e.Message;
                }
            }
            return viewport;
        }
    }

    /// <summary>Why there is no view.</summary>
    public string? Error { get; private set; }

    /// <summary>
    /// Called after the colour scheme followed the theme: the model's
    /// colours come from the scheme it was built in, so the document runs
    /// again.
    /// </summary>
    public event Action? SchemeChanged;

    static string SchemeFor(ElementTheme theme) => theme == ElementTheme.Dark ? DarkScheme : LightScheme;

    void Attach()
    {
        if (attached || Viewport is not { } v || panel.ActualWidth <= 0 || panel.ActualHeight <= 0) return;
        // The native XAML object behind the projection: CsWinRT's
        // reference to it, not a COM wrapper around the managed object
        // (which is what Marshal.GetIUnknownForObject would make).
        var unknown = ((IWinRTObject)panel).NativeObject.ThisPtr;
        try
        {
            // wgpu-hal calls through this pointer as an ISwapChainPanelNative
            // without asking for the interface itself, so it must be that
            // interface's pointer, not the panel's IInspectable (layer.rs,
            // point 1). The reference taken here is held across the call
            // and released after: wgpu keeps one of its own.
            var iid = IidSwapChainPanelNative;
            Marshal.ThrowExceptionForHR(Marshal.QueryInterface(unknown, in iid, out var native));
            try
            {
                v.AttachSwapChainPanel((ulong)native.ToInt64(), panel.ActualWidth, panel.ActualHeight,
                    panel.CompositionScaleX, panel.CompositionScaleY, false);
                attached = true;
            }
            finally
            {
                Marshal.Release(native);
            }
        }
        catch (CoreException e)
        {
            Error = CoreErrors.Describe(e);
        }
        AppLog.Write(attached ? $"view: attached {Describe()}" : $"view: not attached: {Error}");
        if (attached) CompositionTarget.Rendering += OnRendering;
    }

    void Detach()
    {
        if (!attached) return;
        CompositionTarget.Rendering -= OnRendering;
        attached = false;
        try
        {
            viewport?.Detach();
        }
        catch (CoreException)
        {
        }
    }

    void Resize(string why)
    {
        if (!attached)
        {
            Attach();
            return;
        }
        try
        {
            viewport?.ResizeSwapChainPanel(panel.ActualWidth, panel.ActualHeight,
                panel.CompositionScaleX, panel.CompositionScaleY);
        }
        catch (CoreException e)
        {
            AppLog.Write($"view: resize ({why})", e);
            return;
        }
        // Scale changes are rare and worth a line; a window drag resizes
        // at every step and would flood the log.
        if (why == "scale") AppLog.Write($"view: rescaled {Describe()}");
    }

    /// <summary>The panel's size and scale, the swap chain's pixels and its transform, for the log.</summary>
    string Describe()
    {
        double w = panel.ActualWidth, h = panel.ActualHeight;
        double sx = panel.CompositionScaleX, sy = panel.CompositionScaleY;
        var (pw, ph) = PanelScale.Pixels(w, h, sx, sy);
        string transform;
        try
        {
            transform = viewport?.SwapChainTransform() ?? "none";
        }
        catch (CoreException e)
        {
            transform = CoreErrors.Describe(e);
        }
        return $"{w}x{h} DIPs, composition scale {sx} x {sy}, swap chain {pw}x{ph} px, transform {transform}";
    }

    void OnRendering(object? sender, object e)
    {
        if (viewport is not { } v) return;
        try
        {
            if (v.NeedsDraw()) v.Draw();
        }
        catch (CoreException)
        {
            // A lost device or a surface that could not take a frame: the
            // next refresh tries again.
        }
    }

    void ApplyTheme()
    {
        if (viewport is not { } v) return;
        try
        {
            var scheme = SchemeFor(panel.ActualTheme);
            if (v.ColorScheme() == scheme) return;
            v.SetColorScheme(scheme);
            SchemeChanged?.Invoke();
        }
        catch (CoreException)
        {
        }
    }

    /// <summary>Run a camera call, ignoring a core error (the view just does not move).</summary>
    public void Perform(Action<Native.Viewport> call)
    {
        if (viewport is not { } v) return;
        try
        {
            call(v);
        }
        catch (CoreException)
        {
        }
    }

    void OnPressed(object sender, PointerRoutedEventArgs e)
    {
        var p = e.GetCurrentPoint(panel);
        var props = p.Properties;
        panning = props.IsRightButtonPressed || props.IsMiddleButtonPressed;
        last = p.Position;
        click = props.IsLeftButtonPressed ? p.Position : null;
        panel.CapturePointer(e.Pointer);
        e.Handled = true;
    }

    void OnMoved(object sender, PointerRoutedEventArgs e)
    {
        if (last is not { } from) return;
        var to = e.GetCurrentPoint(panel).Position;
        if (click is { } c && Math.Abs(to.X - c.X) + Math.Abs(to.Y - c.Y) > ClickSlop) click = null;
        // Positions are in DIPs, whatever the display scale; the core wants
        // its viewport's points (PanelScale).
        var (dx, dy) = PanelScale.PointerDelta(to.X - from.X, to.Y - from.Y,
            panel.CompositionScaleX, panel.CompositionScaleY);
        last = to;
        if (panning) Perform(v => v.Pan(dx, dy));
        else Perform(v => v.Orbit(dx, dy));
        e.Handled = true;
    }

    void OnReleased(object sender, PointerRoutedEventArgs e)
    {
        if (click is { } c)
        {
            click = null;
            Click?.Invoke(c.X, c.Y);
        }
        last = null;
        panel.ReleasePointerCapture(e.Pointer);
        e.Handled = true;
    }

    void OnWheel(object sender, PointerRoutedEventArgs e)
    {
        // 120 is one notch of a wheel (WHEEL_DELTA); precision touchpads
        // send fractions of it.
        var notches = e.GetCurrentPoint(panel).Properties.MouseWheelDelta / 120.0;
        Perform(v => v.Zoom(notches));
        e.Handled = true;
    }
}
