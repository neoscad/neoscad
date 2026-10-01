//! The process's memory in use, measured, for the memory limit
//! (`session::Config::memory_probe`, `eval::limits::Guard::with_probe`).
//!
//! The limit's estimate counts values, nodes and geometry results, but
//! not a geometry kernel's working memory or the caches: BOSL2's
//! fractal_tree peaks at about 2 GB real against under 512 MiB estimated.
//! A host can measure, so every native one does:
//!
//! - macOS: the physical footprint (`proc_pid_rusage`'s
//!   `ri_phys_footprint`), what Activity Monitor and `footprint` report:
//!   resident and compressed private memory, so a process the system is
//!   compressing still counts what it holds;
//! - Linux: the resident set (`/proc/self/statm`, pages times the page
//!   size); `smaps_rollup` would add swap, but costs a walk of every
//!   mapping on each read;
//! - Windows: the private bytes committed (`PROCESS_MEMORY_COUNTERS_EX`'s
//!   `PrivateUsage`), what Task Manager's "Commit size" shows.
//!
//! Elsewhere there is no probe and the estimate alone applies. A probe
//! only ever stops a request; under the limit nothing it measures reaches
//! the output, so runs stay byte-identical.
//!
//! This file is compiled into both `neoscad` and the app core
//! (`crates/ffi/src/lib.rs` includes it by path), so the command line and
//! the apps measure the same thing. It is each crate's one module besides
//! its platform glue that allows `unsafe`: every platform's measurement is
//! a C call with an out-parameter.

#![allow(unsafe_code)]

use std::sync::Arc;

/// The process's measured memory, or `None` on a platform without a
/// probe. Each call reads the system afresh; the session reads it at
/// most every 10 ms (`session::memory`).
pub fn probe() -> Option<session::MemoryProbe> {
    if cfg!(any(target_os = "macos", target_os = "linux", windows)) && in_use().is_some() {
        Some(Arc::new(|| in_use().unwrap_or(0)))
    } else {
        None
    }
}

/// Hand memory freed since the last call back to the system, so that the
/// probe sees cache eviction at once. mimalloc keeps freed pages for a
/// second before it returns them (`purge_delay`), which would hide what
/// eviction gave back from the reading right after it. Without mimalloc,
/// the system allocator's frees show at once on macOS and Windows, and on
/// Linux glibc keeps small freed blocks but unmaps large ones (meshes).
pub fn release() -> Option<session::MemoryRelease> {
    #[cfg(all(feature = "mimalloc", not(target_arch = "wasm32")))]
    {
        Some(Arc::new(|| {
            // mimalloc's own collect, from the C library the `mimalloc`
            // crate builds and links (it is not in that crate's safe API).
            unsafe extern "C" {
                fn mi_collect(force: bool);
            }
            // SAFETY: `mi_collect` takes no pointers and may be called on
            // any thread at any time; with `force` it also purges the
            // pages other threads abandoned.
            unsafe { mi_collect(true) }
        }))
    }
    #[cfg(not(all(feature = "mimalloc", not(target_arch = "wasm32"))))]
    {
        None
    }
}

/// The process's footprint in bytes (see the module documentation).
#[cfg(target_os = "macos")]
pub fn in_use() -> Option<u64> {
    let mut info = std::mem::MaybeUninit::<libc::rusage_info_v2>::zeroed();
    // SAFETY: `proc_pid_rusage` writes at most a `rusage_info_v2` (the
    // flavour asked for) through the pointer, which points at one.
    let r = unsafe {
        libc::proc_pid_rusage(
            std::process::id() as libc::c_int,
            libc::RUSAGE_INFO_V2,
            info.as_mut_ptr().cast(),
        )
    };
    // SAFETY: zeroed is a valid `rusage_info_v2` (plain integers), and on
    // success the call filled it in.
    (r == 0).then(|| unsafe { info.assume_init() }.ri_phys_footprint)
}

/// The process's resident set in bytes (see the module documentation).
#[cfg(target_os = "linux")]
pub fn in_use() -> Option<u64> {
    use std::sync::OnceLock;
    static PAGE: OnceLock<u64> = OnceLock::new();
    // SAFETY: `sysconf` reads a system constant; it takes no pointers.
    let page = *PAGE.get_or_init(|| {
        u64::try_from(unsafe { libc::sysconf(libc::_SC_PAGESIZE) }).unwrap_or(4096)
    });
    let statm = std::fs::read_to_string("/proc/self/statm").ok()?;
    let resident: u64 = statm.split_whitespace().nth(1)?.parse().ok()?;
    Some(resident * page)
}

/// The process's private committed bytes (see the module documentation).
#[cfg(windows)]
pub fn in_use() -> Option<u64> {
    use windows_sys::Win32::System::ProcessStatus::{
        K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX,
    };
    use windows_sys::Win32::System::Threading::GetCurrentProcess;
    let mut c = PROCESS_MEMORY_COUNTERS_EX {
        cb: std::mem::size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32,
        ..Default::default()
    };
    // SAFETY: the pseudo-handle of the current process needs no closing,
    // and the call writes at most `cb` bytes into `c`, which is that size
    // (the `_EX` layout begins with the plain one, as the API expects).
    let ok = unsafe {
        K32GetProcessMemoryInfo(
            GetCurrentProcess(),
            (&raw mut c).cast::<PROCESS_MEMORY_COUNTERS>(),
            c.cb,
        )
    };
    (ok != 0).then_some(c.PrivateUsage as u64)
}

/// No probe on this platform.
#[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
pub fn in_use() -> Option<u64> {
    None
}

#[cfg(test)]
mod tests {
    #[test]
    fn measures_where_it_can() {
        let Some(probe) = super::probe() else {
            // Only a platform that cannot measure has no probe.
            assert_eq!(super::in_use(), None);
            return;
        };
        let before = probe();
        // A process running tests holds at least a megabyte.
        assert!(before > 1 << 20, "{before}");
        // Touch 128 MiB, so it is resident (and private) however lazily
        // the system commits it, and see the measurement grow (by half of
        // it, in case another test frees memory meanwhile).
        let block = vec![1u8; 128 << 20];
        let after = probe();
        assert!(after >= before + (64 << 20), "{before} -> {after}");
        drop(std::hint::black_box(block));
        if let Some(release) = super::release() {
            release();
        }
    }
}
