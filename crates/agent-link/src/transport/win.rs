//! The Win32 calls behind the named pipe's access control (see
//! `transport.rs`): this user's security identifier, which names the
//! default pipe and is the only one its security descriptor admits, and
//! the owner of a pipe a client opened, which must be that same user.
//! Also the client's overlapped reads and writes ([`OverlappedPipe`]),
//! which std's `File` does not do, and the cancelling of a connection's
//! pending I/O from another thread ([`cancellable`]), which is what
//! `transport::Closer` does here.
//!
//! The one module of this crate that allows `unsafe` (see `Cargo.toml`'s
//! lints). interprocess and std wrap the pipe itself; neither reads a
//! token or an object's owner, and interprocess's overlapped I/O is only
//! for pipes it opens itself, without the identification-only flag the
//! client opens with. Every pointer here is either filled in by
//! the call that returns it and released with the matching free (a
//! token handle with `CloseHandle`, `LocalAlloc`ed memory with
//! `LocalFree`), or borrowed from a buffer that outlives its use.

#![allow(unsafe_code)]

use std::io;
use std::os::windows::io::AsRawHandle;
use std::ptr::null_mut;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_BROKEN_PIPE, ERROR_IO_PENDING, ERROR_PIPE_NOT_CONNECTED, ERROR_SUCCESS,
    HANDLE, LocalFree,
};
use windows_sys::Win32::Security::Authorization::{
    ConvertSidToStringSidW, GetSecurityInfo, SE_KERNEL_OBJECT,
};
use windows_sys::Win32::Security::{
    EqualSid, GetTokenInformation, OWNER_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID,
    TOKEN_QUERY, TOKEN_USER, TokenUser,
};
use windows_sys::Win32::Storage::FileSystem::{ReadFile, WriteFile};
use windows_sys::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};
use windows_sys::Win32::System::Threading::{CreateEventW, GetCurrentProcess, OpenProcessToken};

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

/// A pipe client opened for overlapped I/O (`FILE_FLAG_OVERLAPPED`), read
/// and written from different threads at once.
///
/// Why not a plain `File`: Windows serializes every I/O on a handle opened
/// without that flag ("If this flag is not specified, then I/O operations
/// are serialized", CreateFileW's documentation), and a duplicated handle
/// shares the one file object. `neoscad mcp` keeps a read pending on its
/// app connection all the time (the app may speak first), so its next
/// write, the `welcome` with the client's name that `initialize` sends,
/// waited behind that read for as long as the app said nothing: the first
/// Windows CI run's "no answer in time". Here each operation has its own
/// `OVERLAPPED` and event, so a read and a write proceed independently.
///
/// Each call waits for its own operation before returning, so the
/// `OVERLAPPED` and the buffer outlive the I/O. Reads and writes take
/// `&self`: the reading and the writing half share one `OverlappedPipe`
/// (through [`cancellable`]), and the handle closes when both are gone, so
/// it cannot close under a pending operation.
#[derive(Debug)]
pub struct OverlappedPipe(std::fs::File);

impl OverlappedPipe {
    /// `file` must have been opened with `FILE_FLAG_OVERLAPPED`.
    pub fn new(file: std::fs::File) -> OverlappedPipe {
        OverlappedPipe(file)
    }

    fn handle(&self) -> HANDLE {
        self.0.as_raw_handle() as HANDLE
    }

    /// Start one operation with `start` (given the `OVERLAPPED` to use)
    /// and wait for it: the bytes it moved.
    fn run(&self, start: impl FnOnce(*mut OVERLAPPED) -> i32) -> io::Result<usize> {
        struct Event(HANDLE);
        impl Drop for Event {
            fn drop(&mut self) {
                // SAFETY: created by CreateEventW below and closed once.
                unsafe { CloseHandle(self.0) };
            }
        }
        // Manual reset, as an event GetOverlappedResult waits on must be.
        // SAFETY: no attributes and no name; a null result is an error.
        let raw = unsafe { CreateEventW(std::ptr::null(), 1, 0, std::ptr::null()) };
        if raw.is_null() {
            return Err(io::Error::last_os_error());
        }
        let event = Event(raw);
        // SAFETY: OVERLAPPED is plain data; all zeroes is its initial state.
        let mut ov: OVERLAPPED = unsafe { std::mem::zeroed() };
        ov.hEvent = event.0;
        if start(&mut ov) == 0 {
            let e = io::Error::last_os_error();
            if e.raw_os_error() != Some(ERROR_IO_PENDING as i32) {
                // Never started, so nothing refers to `ov`.
                return Err(e);
            }
        }
        let mut moved = 0u32;
        // SAFETY: `ov` belongs to the operation just started on this
        // handle; with bWait set this returns only once it has completed,
        // after which neither `ov` nor the caller's buffer is touched.
        if unsafe { GetOverlappedResult(self.handle(), &ov, &mut moved, 1) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(moved as usize)
    }
}

impl AsRawHandle for OverlappedPipe {
    fn as_raw_handle(&self) -> std::os::windows::io::RawHandle {
        self.0.as_raw_handle()
    }
}

impl io::Read for &OverlappedPipe {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let len = u32::try_from(buf.len()).unwrap_or(u32::MAX);
        let (h, ptr) = (self.handle(), buf.as_mut_ptr());
        // SAFETY: `ptr` is valid for `len` bytes until `run` returns,
        // which is after the read has completed.
        match self.run(|ov| unsafe { ReadFile(h, ptr, len, null_mut(), ov) }) {
            // The app closed its end: the end of the stream, as a socket's
            // read of 0 is, rather than an error.
            Err(e)
                if e.raw_os_error() == Some(ERROR_BROKEN_PIPE as i32)
                    || e.raw_os_error() == Some(ERROR_PIPE_NOT_CONNECTED as i32) =>
            {
                Ok(0)
            }
            r => r,
        }
    }
}

impl io::Write for &OverlappedPipe {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let len = u32::try_from(buf.len()).unwrap_or(u32::MAX);
        let (h, ptr) = (self.handle(), buf.as_ptr());
        // SAFETY: `ptr` is valid for `len` bytes until `run` returns,
        // which is after the write has completed.
        self.run(|ov| unsafe { WriteFile(h, ptr, len, null_mut(), ov) })
    }

    /// A completed write is in the pipe already; nothing is held here.
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// What a connection's reader, writer and closer share: whether it has
/// been closed, and how many of its operations are under way.
#[derive(Default)]
struct Shutdown {
    closed: AtomicBool,
    busy: AtomicUsize,
}

impl Shutdown {
    /// Run one operation unless the connection is closed (`None` then).
    /// The count goes up before the flag is read, and the closer sets the
    /// flag before it reads the count, so either this operation sees the
    /// flag or the closer sees the operation and cancels until it is done.
    fn run<R>(&self, op: impl FnOnce() -> io::Result<R>) -> Option<io::Result<R>> {
        self.busy.fetch_add(1, Ordering::SeqCst);
        let out = if self.closed.load(Ordering::SeqCst) {
            None
        } else {
            Some(op())
        };
        self.busy.fetch_sub(1, Ordering::SeqCst);
        out
    }

    fn closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }
}

/// How long [`cancellable`]'s closer keeps cancelling an operation that is
/// still under way. Pipe reads and writes end at once when cancelled, so
/// this is only reached if a thread is starved between counting itself in
/// and starting its call; the closer then gives up, as it did before it
/// could cancel anything.
const CANCEL_FOR: Duration = Duration::from_secs(2);

/// One half of a [`cancellable`] connection.
struct Half<T> {
    io: Arc<T>,
    shutdown: Arc<Shutdown>,
}

impl<T> io::Read for Half<T>
where
    for<'a> &'a T: io::Read,
{
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let io = &self.io;
        match self.shutdown.run(|| (&**io).read(buf)) {
            // Closed: the end of the stream, as a read on a Unix socket
            // that was shut down gives, whether the closer came first or
            // cancelled this read (`ERROR_OPERATION_ABORTED`).
            None => Ok(0),
            Some(Err(_)) if self.shutdown.closed() => Ok(0),
            Some(r) => r,
        }
    }
}

impl<T> io::Write for Half<T>
where
    for<'a> &'a T: io::Write,
{
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let io = &self.io;
        match self.shutdown.run(|| (&**io).write(buf)) {
            None => Err(io::ErrorKind::BrokenPipe.into()),
            Some(r) => r,
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        let io = &self.io;
        match self.shutdown.run(|| (&**io).flush()) {
            None => Err(io::ErrorKind::BrokenPipe.into()),
            Some(r) => r,
        }
    }
}

/// The reader, the writer and the closer [`cancellable`] makes.
pub type Cancellable = (
    Box<dyn io::Read + Send>,
    Box<dyn io::Write + Send>,
    Box<dyn Fn() + Send + Sync>,
);

/// A pipe connection's reader and writer (the two halves of one handle,
/// overlapped) and a closer that ends both from any thread, as shutting a
/// Unix socket down does.
///
/// Without the closer a Windows connection ended only when its peer
/// closed: Disconnect, stop and a refused extra agent sent `bye` and then
/// waited, and an agent that never closed its end kept a reader thread
/// blocked in its read for as long as it ran. The closer marks the
/// connection closed, so no new operation starts, and cancels the handle's
/// pending ones with `CancelIoEx` (both halves', whichever thread started
/// them; that needs the handle to be overlapped, which the client's
/// [`OverlappedPipe`] and interprocess's server pipes are). It cancels
/// again until no operation is under way, because one that had counted
/// itself in but not yet started when the first cancel ran would otherwise
/// start afterwards and block. Data already written stays in the pipe for
/// the peer to read, as it would after a Unix shutdown.
///
/// The handle is not closed here: an operation on another thread may
/// still be returning from it, and a closed handle's value can be reused
/// by the next one opened, so a later cancel could hit another file. It
/// closes when both halves are dropped, which a closed connection's users
/// do once their read returns. The closer holds the halves only weakly, so
/// it never keeps the pipe open, and does nothing once both are gone.
pub fn cancellable<R, W>(reader: R, writer: W) -> Cancellable
where
    R: AsRawHandle + Send + Sync + 'static,
    W: AsRawHandle + Send + Sync + 'static,
    for<'a> &'a R: io::Read,
    for<'a> &'a W: io::Write,
{
    let shutdown = Arc::new(Shutdown::default());
    let (reader, writer) = (Arc::new(reader), Arc::new(writer));
    let (weak_r, weak_w) = (Arc::downgrade(&reader), Arc::downgrade(&writer));
    let state = shutdown.clone();
    let close = move || {
        state.closed.store(true, Ordering::SeqCst);
        let deadline = Instant::now() + CANCEL_FOR;
        loop {
            // Both halves, though they share one handle: either may be the
            // one still alive.
            let r = cancel_all(&weak_r);
            let w = cancel_all(&weak_w);
            if !r && !w {
                // Both halves are gone, and the handle with them.
                return;
            }
            if state.busy.load(Ordering::SeqCst) == 0 || Instant::now() >= deadline {
                return;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    };
    (
        Box::new(Half {
            io: reader,
            shutdown: shutdown.clone(),
        }),
        Box::new(Half {
            io: writer,
            shutdown,
        }),
        Box::new(close),
    )
}

/// Cancel every pending operation of this process on the handle behind
/// `half`, if it is still open: false when it is gone.
fn cancel_all<T: AsRawHandle>(half: &Weak<T>) -> bool {
    let Some(io) = half.upgrade() else {
        return false;
    };
    // SAFETY: the handle is open while `io` is held. A null OVERLAPPED
    // cancels all of this process's I/O on the handle; with nothing
    // pending the call fails with ERROR_NOT_FOUND, which is fine.
    unsafe { CancelIoEx(io.as_raw_handle() as HANDLE, std::ptr::null()) };
    true
}
