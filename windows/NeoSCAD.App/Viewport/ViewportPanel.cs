// The 3D view: the core's wgpu renderer drawing with Direct3D 12 into the
// window's WinUI 3 SwapChainPanel (crates/ffi/src/layer.rs,
// "SwapChainPanel"). The Windows counterpart of
// apple/App/Viewport/MetalView.swift.
//
// The camera lives in Rust (render::camera): this class sends pointer
// deltas in device-independent pixels and never holds a camera of its
// own, so the view cannot drift from exports and snapshots.
//
//   left drag           orbit
//   right/middle drag   pan (the model follows the pointer)
//   wheel               zoom, a tenth of the distance a notch, as OpenSCAD
//
// Frames. CompositionTarget.Rendering fires once per display refresh on
// the UI thread; a frame is drawn only when the core says one is due (a
// camera move, a new model), so an idle view costs a flag check.
//
// Scale. The swap chain is sized in device-independent pixels, one buffer
// pixel per DIP, which the panel shows at its natural size; at 150%
// display scaling the compositor stretches it. Sizing it in physical
// pixels needs the inverse scale set on the swap chain
// (IDXGISwapChain2::SetMatrixTransform), which wgpu does not do:
// milestone 2 (docs/windows-app.md).

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

    public ViewportPanel(SwapChainPanel panel)
    {
        this.panel = panel;
        panel.Loaded += (_, _) => Attach();
        panel.Unloaded += (_, _) => Detach();
        panel.SizeChanged += (_, _) => Resize();
        panel.PointerPressed += OnPressed;
        panel.PointerMoved += OnMoved;
        panel.PointerReleased += OnReleased;
        panel.PointerCaptureLost += (_, _) => last = null;
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
                v.AttachSwapChainPanel((ulong)native.ToInt64(), panel.ActualWidth, panel.ActualHeight, 1.0, false);
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

    void Resize()
    {
        if (!attached)
        {
            Attach();
            return;
        }
        try
        {
            viewport?.Resize(panel.ActualWidth, panel.ActualHeight, 1.0);
        }
        catch (CoreException)
        {
        }
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
        panel.CapturePointer(e.Pointer);
        e.Handled = true;
    }

    void OnMoved(object sender, PointerRoutedEventArgs e)
    {
        if (last is not { } from) return;
        var to = e.GetCurrentPoint(panel).Position;
        double dx = to.X - from.X, dy = to.Y - from.Y;
        last = to;
        if (panning) Perform(v => v.Pan(dx, dy));
        else Perform(v => v.Orbit(dx, dy));
        e.Handled = true;
    }

    void OnReleased(object sender, PointerRoutedEventArgs e)
    {
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
