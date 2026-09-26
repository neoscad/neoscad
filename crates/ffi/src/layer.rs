//! The app's `CAMetalLayer` made into a wgpu surface: the only `unsafe`
//! in the core, and the only raw pointer that crosses the bridge.
//!
//! # Why it is sound
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
