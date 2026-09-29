//! The Win32 calls behind the named pipe's access control (see
//! `transport.rs`): this user's security identifier, which names the
//! default pipe and is the only one its security descriptor admits, and
//! the owner of a pipe a client opened, which must be that same user.
//!
//! The one module of this crate that allows `unsafe` (see `Cargo.toml`'s
//! lints). interprocess and std wrap the pipe itself; neither reads a
//! token or an object's owner. Every pointer here is either filled in by
//! the call that returns it and released with the matching free (a
//! token handle with `CloseHandle`, `LocalAlloc`ed memory with
//! `LocalFree`), or borrowed from a buffer that outlives its use.

#![allow(unsafe_code)]

use std::io;
use std::os::windows::io::AsRawHandle;
use std::ptr::null_mut;

use windows_sys::Win32::Foundation::{CloseHandle, ERROR_SUCCESS, HANDLE, LocalFree};
use windows_sys::Win32::Security::Authorization::{
    ConvertSidToStringSidW, GetSecurityInfo, SE_KERNEL_OBJECT,
};
use windows_sys::Win32::Security::{
    EqualSid, GetTokenInformation, OWNER_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID,
    TOKEN_QUERY, TOKEN_USER, TokenUser,
};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

/// This process's user, as `S-1-5-21-...`.
pub fn current_user_sid() -> io::Result<String> {
    with_user_sid(|sid| {
        let mut wide: *mut u16 = null_mut();
        // SAFETY: `sid` is valid for this closure; on success `wide` is a
        // NUL-terminated string that we own and free below.
        if unsafe { ConvertSidToStringSidW(sid, &mut wide) } == 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `wide` is NUL-terminated (ConvertSidToStringSidW's
        // contract), so every index up to the terminator is in bounds.
        let text = unsafe {
            let mut len = 0;
            while *wide.add(len) != 0 {
                len += 1;
            }
            String::from_utf16_lossy(std::slice::from_raw_parts(wide, len))
        };
        // SAFETY: allocated by ConvertSidToStringSidW with LocalAlloc.
        unsafe { LocalFree(wide.cast()) };
        Ok(text)
    })
}

/// Whether the kernel object behind `handle` (an open pipe) is owned by
/// this process's user.
pub fn owned_by_current_user(handle: &impl AsRawHandle) -> io::Result<bool> {
    let mut owner: PSID = null_mut();
    let mut descriptor: PSECURITY_DESCRIPTOR = null_mut();
    // SAFETY: the handle is open for the duration (borrowed from its
    // owner); on success `owner` points into `descriptor`, which we own.
    // Reading the owner needs READ_CONTROL, which a handle opened for
    // reading has.
    let rc = unsafe {
        GetSecurityInfo(
            handle.as_raw_handle() as HANDLE,
            SE_KERNEL_OBJECT,
            OWNER_SECURITY_INFORMATION,
            &mut owner,
            null_mut(),
            null_mut(),
            null_mut(),
            &mut descriptor,
        )
    };
    if rc != ERROR_SUCCESS {
        return Err(io::Error::from_raw_os_error(rc as i32));
    }
    // SAFETY: both SIDs are valid while `descriptor` and the token buffer
    // are alive, which they are for the comparison.
    let same = with_user_sid(|me| Ok(unsafe { EqualSid(owner, me) } != 0));
    // SAFETY: allocated by GetSecurityInfo with LocalAlloc; `owner` is not
    // used after this.
    unsafe { LocalFree(descriptor) };
    same
}

/// Run `f` with this process's user SID, which is valid only during `f`.
fn with_user_sid<R>(f: impl FnOnce(PSID) -> io::Result<R>) -> io::Result<R> {
    struct Token(HANDLE);
    impl Drop for Token {
        fn drop(&mut self) {
            // SAFETY: opened by OpenProcessToken below and closed once.
            unsafe { CloseHandle(self.0) };
        }
    }
    let mut raw: HANDLE = null_mut();
    // SAFETY: GetCurrentProcess is a pseudo-handle that needs no closing;
    // on success `raw` is a token handle we own (closed by `Token`).
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut raw) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let token = Token(raw);
    // First call: the size only (it fails with ERROR_INSUFFICIENT_BUFFER).
    let mut len = 0u32;
    // SAFETY: a null buffer of length 0 is allowed for the size query.
    unsafe { GetTokenInformation(token.0, TokenUser, null_mut(), 0, &mut len) };
    if len == 0 {
        return Err(io::Error::last_os_error());
    }
    // u64s, so the buffer is aligned for TOKEN_USER (pointer-aligned).
    let mut buf = vec![0u64; (len as usize).div_ceil(8)];
    // SAFETY: `buf` holds at least `len` bytes.
    if unsafe { GetTokenInformation(token.0, TokenUser, buf.as_mut_ptr().cast(), len, &mut len) }
        == 0
    {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the call filled `buf` with a TOKEN_USER, whose SID pointer
    // points into `buf` itself, which outlives `f`.
    let user = unsafe { &*buf.as_ptr().cast::<TOKEN_USER>() };
    f(user.User.Sid)
}
