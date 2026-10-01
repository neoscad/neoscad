//! The wasm32 build's global allocator: Rust's own (`System`), counting the
//! bytes alive so the memory limit can measure instead of estimate.
//!
//! The memory limit's estimate (`eval::limits`) counts values, nodes and
//! geometry results, but not a geometry kernel's working memory: a big
//! boolean grew the instance past the 1 GiB limit until an allocation
//! failed, and on wasm32 a failed allocation aborts (an `unreachable`
//! trap), so the page lost the worker and its state instead of getting a
//! `resource-limit` diagnostic. With the count's peak as the session's
//! memory probe, the guard sees that growth at the next geometry node or
//! primitive ring and stops the request there. One kernel operation still
//! runs to its end, so a single operation that needs the rest of the
//! address space on its own still traps.
//!
//! The probe reads the request's peak, not the bytes alive now: a kernel
//! operation frees its working copies before it returns, so by the next
//! check the live count is back down, yet the instance had to hold the
//! peak (and does hold it, as wasm memory never shrinks). The peak is
//! reset as each request starts ([`reset_peak`], from `wasm.rs`). It is
//! of bytes asked for, not of the wasm memory's size, which would carry
//! one heavy model's high-water mark over every later request.
//!
//! This is the crate's one module with `unsafe` of its own: implementing
//! `GlobalAlloc` is `unsafe` by definition. Every method forwards to
//! `System` with the same arguments, so the safety contract is `System`'s,
//! passed through unchanged.

#![allow(unsafe_code)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

/// Bytes allocated and not yet freed.
static LIVE: AtomicUsize = AtomicUsize::new(0);
/// The most [`LIVE`] has been since [`reset_peak`].
static PEAK: AtomicUsize = AtomicUsize::new(0);

/// Count `n` more bytes alive.
fn grow(n: usize) {
    let live = LIVE.fetch_add(n, Ordering::Relaxed) + n;
    PEAK.fetch_max(live, Ordering::Relaxed);
}

/// `System`, counting.
struct Counting;

#[global_allocator]
static GLOBAL: Counting = Counting;

// SAFETY: each method calls `System`'s with the caller's arguments, which
// satisfy `System`'s contract because they satisfy `GlobalAlloc`'s; the
// count is updated only after a successful allocation and before nothing
// that could observe the memory.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: as above.
        let p = unsafe { System.alloc(layout) };
        if !p.is_null() {
            grow(layout.size());
        }
        p
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        // SAFETY: as above.
        let p = unsafe { System.alloc_zeroed(layout) };
        if !p.is_null() {
            grow(layout.size());
        }
        p
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: as above.
        unsafe { System.dealloc(ptr, layout) };
        LIVE.fetch_sub(layout.size(), Ordering::Relaxed);
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // SAFETY: as above.
        let p = unsafe { System.realloc(ptr, layout, new_size) };
        if !p.is_null() {
            if new_size >= layout.size() {
                grow(new_size - layout.size());
            } else {
                LIVE.fetch_sub(layout.size() - new_size, Ordering::Relaxed);
            }
        }
        p
    }
}

/// Bytes the instance has allocated and not freed.
pub fn live() -> u64 {
    LIVE.load(Ordering::Relaxed) as u64
}

/// The most bytes alive at once since the last [`reset_peak`]: the
/// session's memory probe.
pub fn peak() -> u64 {
    PEAK.load(Ordering::Relaxed) as u64
}

/// Start a new peak from the bytes alive now (as a request starts).
pub fn reset_peak() {
    PEAK.store(LIVE.load(Ordering::Relaxed), Ordering::Relaxed);
}
