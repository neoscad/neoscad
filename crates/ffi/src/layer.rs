//! The window's drawable made into a wgpu surface: the macOS app's
//! `CAMetalLayer`, or the Windows app's `SwapChainPanel` (at the end of
//! this file). The only `unsafe` in the core, and the only raw pointers
//! that cross the bridge.
//!
//! # Why the `CAMetalLayer` path is sound
//!
//! `wgpu::Instance::create_surface_unsafe` with
//! `SurfaceTargetUnsafe::CoreAnimationLayer` needs "a valid object to
//! create a surface upon" (`wgpu-30.0.1/src/api/surface.rs:430-436`).
//! What it does with the pointer: wgpu-hal's `Surface::from_layer`
//! asserts the object is a `CAMetalLayer` and *retains* it
//! (`wgpu-hal-30.0.1/src/metal/surface.rs:46-49`); nothing keeps the raw
//! pointer. So the obligations are:
//!
//! 1. **The pointer is a live `CAMetalLayer` for the duration of the
//!    call.** The one caller is the app's `MetalView`
//!    (`apple/App/Viewport/MetalView.swift`), which passes its own backing
//!    layer as `Unmanaged.passUnretained(layer).toOpaque()` inside
//!    `withExtendedLifetime(layer)`, so the layer cannot be freed while
//!    Rust runs. A null pointer is refused here; an object of another
//!    class trips wgpu-hal's assertion, which [`crate::guarded`] turns into
//!    an error rather than undefined behaviour. What cannot be checked is
//!    a dangling pointer, which is why nothing but that view calls this.
//! 2. **The layer outlives the surface.** wgpu retains the layer, so the
//!    surface keeps it alive however long Rust holds it; the view's
//!    release of its own reference cannot free it under the surface.
//! 3. **The surface does not outlive its view's use of it.** The view
//!    calls `Viewport::detach` when it leaves its window and when it is
//!    deallocated, which drops the surface and with it the retain, so a
//!    closed window's layer is not kept alive (or drawn into) by the core.
//! 4. **Threads.** The layer is created, attached, resized and drawn on
//!    the main thread (the view is `@MainActor`, and its display link
//!    fires on the main run loop). That matters beyond this call:
//!    acquiring a drawable reads the hosting window's `occlusionState`
//!    (`wgpu-hal-30.0.1/src/metal/surface.rs:353-369`), an AppKit property
//!    that belongs to the main thread.

#![allow(unsafe_code)]

use crate::CoreError;

/// A surface on the `CAMetalLayer` at address `layer` (see the module
/// documentation for what the caller must guarantee).
#[cfg(target_vendor = "apple")]
pub(crate) fn surface_from_layer(
    instance: &wgpu::Instance,
    layer: u64,
) -> Result<wgpu::Surface<'static>, CoreError> {
    let addr = usize::try_from(layer)
        .ok()
        .filter(|a| *a != 0)
        .ok_or_else(|| CoreError::InvalidArgument {
            message: format!("not a layer pointer: {layer:#x}"),
        })?;
    let ptr = std::ptr::with_exposed_provenance_mut::<core::ffi::c_void>(addr);
    // SAFETY: `ptr` is non-null and, by the caller's contract (point 1
    // above), a live `CAMetalLayer` for this call; wgpu retains it before
    // returning (point 2), so the surface does not depend on the pointer
    // staying valid afterwards.
    let surface = unsafe {
        instance.create_surface_unsafe(wgpu::SurfaceTargetUnsafe::CoreAnimationLayer(ptr))
    };
    surface.map_err(|e| CoreError::Failed {
        message: format!("cannot draw into the view's layer: {e}"),
    })
}

/// Only Apple platforms have Core Animation.
#[cfg(not(target_vendor = "apple"))]
pub(crate) fn surface_from_layer(
    _instance: &wgpu::Instance,
    _layer: u64,
) -> Result<wgpu::Surface<'static>, CoreError> {
    Err(CoreError::Failed {
        message: "a CAMetalLayer viewport needs macOS".into(),
    })
}

/// A surface on the WinUI 3 `SwapChainPanel` whose `ISwapChainPanelNative`
/// interface is at address `panel`.
///
/// # Why it is sound
///
/// `SurfaceTargetUnsafe::SwapChainPanel` needs "a valid SwapChainPanel to
/// create a surface upon", whose refcount wgpu increments and keeps while
/// the surface lives (`wgpu-30.0.1/src/api/surface.rs:457-463`). What
/// wgpu-hal does with the pointer: it borrows it as an
/// `ISwapChainPanelNative` (`from_raw_borrowed`, no `QueryInterface`) and
/// takes a reference of its own (`to_owned`, an `AddRef`;
/// `wgpu-hal-30.0.1/src/dx12/mod.rs:584-598`). It later calls the
/// interface's one method, `SetSwapChain`, when the surface is configured
/// (`:1627-1633`). So the obligations are:
///
/// 1. **The pointer is the panel's `ISwapChainPanelNative`, alive for the
///    call.** Not the panel's `IInspectable`: wgpu-hal does not query for
///    the interface, it calls through the pointer's vtable, so any other
///    interface would call the wrong method. The one caller is the
///    Windows app's `ViewportPanel`
///    (`windows/NeoSCAD.App/Viewport/ViewportPanel.cs`), which queries
///    the panel for that interface (IID
///    `63aad0b8-7c24-40ff-85a8-640d944cc325`, the WinUI 3 one wgpu-hal
///    declares, `wgpu-hal-30.0.1/src/dx12/types.rs:9`), holds its
///    reference across this call and releases it afterwards. Null is
///    refused here.
/// 2. **The panel outlives the surface.** wgpu's own reference keeps the
///    interface alive however long Rust holds the surface.
/// 3. **The surface does not outlive the view's use of it.** The panel
///    calls `Viewport::detach` when it is unloaded, which drops the
///    surface and its reference, so a closed window's panel is not kept.
/// 4. **Threads.** Attaching, resizing (which configures the swap chain
///    and so calls `SetSwapChain`) and drawing happen on the window's UI
///    thread, where XAML objects belong.
#[cfg(windows)]
pub(crate) fn surface_from_swap_chain_panel(
    instance: &wgpu::Instance,
    panel: u64,
) -> Result<wgpu::Surface<'static>, CoreError> {
    let addr = usize::try_from(panel)
        .ok()
        .filter(|a| *a != 0)
        .ok_or_else(|| CoreError::InvalidArgument {
            message: format!("not a SwapChainPanel pointer: {panel:#x}"),
        })?;
    let ptr = std::ptr::with_exposed_provenance_mut::<core::ffi::c_void>(addr);
    // SAFETY: `ptr` is non-null and, by the caller's contract (point 1
    // above), the panel's live `ISwapChainPanelNative` for this call;
    // wgpu takes its own reference before returning (point 2).
    let surface =
        unsafe { instance.create_surface_unsafe(wgpu::SurfaceTargetUnsafe::SwapChainPanel(ptr)) };
    surface.map_err(|e| CoreError::Failed {
        message: format!("cannot draw into the view's panel: {e}"),
    })
}

/// Show the swap chain behind `surface` scaled by `x` and `y`
/// (`IDXGISwapChain2::SetMatrixTransform`), so that a buffer sized in
/// physical pixels covers its `SwapChainPanel` exactly rather than
/// `1 / x` times it.
///
/// # Why it is sound
///
/// 1. **`as_hal`.** Its contract (`wgpu-30.0.1/src/api/surface.rs:224-231`)
///    is about the hal resource: it must not be destroyed while wgpu
///    still uses it, and wgpu-hal's own rules hold. Nothing here destroys
///    anything: `dx12::Surface::swap_chain` returns a clone of the COM
///    pointer, an `AddRef` (`wgpu-hal-30.0.1/src/dx12/mod.rs:650-652`),
///    released when `chain` drops at the end of the call, and the guard
///    is dropped with it. `None` (not a DX12 surface, or not configured
///    yet) is a no-op.
/// 2. **`SetMatrixTransform`.** It reads one `DXGI_MATRIX_3X2_F` through
///    the pointer, which is a local that outlives the call. On a swap
///    chain that was not made for composition it fails with
///    `DXGI_ERROR_INVALID_CALL`, an error returned rather than undefined
///    behaviour; wgpu-hal makes a panel's with
///    `CreateSwapChainForComposition` (`:1542-1552`).
/// 3. **Threads.** Called from the configure that `Viewport::resize` or
///    `Viewport::draw` makes, on the panel's UI thread (point 4 of
///    [`surface_from_swap_chain_panel`]), while the viewport's lock is
///    held, so no present on this swap chain runs at the same time.
#[cfg(windows)]
pub(crate) fn set_swap_chain_scale(
    surface: &wgpu::Surface<'_>,
    x: f32,
    y: f32,
) -> Result<(), String> {
    use windows::Win32::Graphics::Dxgi::DXGI_MATRIX_3X2_F;
    // SAFETY: point 1 above: only the swap chain's COM pointer is cloned
    // out of the guard, and nothing is destroyed.
    let hal = unsafe { surface.as_hal::<wgpu::hal::api::Dx12>() }
        .ok_or_else(|| "not a Direct3D 12 surface".to_string())?;
    let chain = hal
        .swap_chain()
        .ok_or_else(|| "the surface has no swap chain yet".to_string())?;
    let matrix = DXGI_MATRIX_3X2_F {
        _11: x,
        _22: y,
        ..Default::default()
    };
    // SAFETY: point 2 above: `matrix` is live for the call, which only
    // reads it.
    unsafe { chain.SetMatrixTransform(&matrix) }.map_err(|e| format!("SetMatrixTransform: {e}"))
}

/// Only Windows has DXGI swap chains.
#[cfg(not(windows))]
pub(crate) fn set_swap_chain_scale(
    _surface: &wgpu::Surface<'_>,
    _x: f32,
    _y: f32,
) -> Result<(), String> {
    Err("a swap chain transform needs Windows".into())
}

/// Only Windows has XAML's `SwapChainPanel`.
#[cfg(not(windows))]
pub(crate) fn surface_from_swap_chain_panel(
    _instance: &wgpu::Instance,
    _panel: u64,
) -> Result<wgpu::Surface<'static>, CoreError> {
    Err(CoreError::Failed {
        message: "a SwapChainPanel viewport needs Windows".into(),
    })
}
