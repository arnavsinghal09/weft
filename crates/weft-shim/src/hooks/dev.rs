//! `/dev/urandom` and `/dev/random` interposition.
//!
//! Strategy: when the target opens one of these paths, we open `/dev/null`
//! instead — reserving a *real* file descriptor so `close`/`dup`/`fstat`
//! semantics stay sane — record the fd in a small lock-free table, and answer
//! `read`/`pread` on recorded fds from the `DevRandom` ChaCha8 stream.
//! `fopen` can't go through this path because glibc's stdio calls `read`
//! through an internal non-interposable alias, so it gets a `fopencookie`
//! stream. Each such stream owns an *independent* seed-derived substream
//! (keyed by process-global open order), so glibc's read-ahead only draws
//! into that file's own sequence and the buffered bytes it discards at
//! `fclose` are deterministic — see [`crate::rng::DevFileRng`].

use core::ffi::c_void;
use core::mem::MaybeUninit;
use core::sync::atomic::{AtomicI32, AtomicU64, Ordering};

use libc::{c_char, c_int, mode_t, off_t, size_t, ssize_t, FILE};

use crate::real::real;
use crate::rng::DevFileRng;
use crate::state::{shim, Shim};
use crate::trace::shim_trace;
use weft_abi::Domain;

/// Tracked deterministic-random fds. Fixed-size: no allocation in hooks.
/// 64 simultaneously-open random fds is far beyond any sane program.
static TRACKED_FDS: [AtomicI32; 64] = {
    #[allow(clippy::declare_interior_mutable_const)] // array-init pattern
    const EMPTY: AtomicI32 = AtomicI32::new(-1);
    [EMPTY; 64]
};

fn track_fd(fd: c_int) -> bool {
    for slot in &TRACKED_FDS {
        if slot
            .compare_exchange(-1, fd, Ordering::AcqRel, Ordering::Relaxed)
            .is_ok()
        {
            return true;
        }
    }
    false // table full: caller falls back to the real device (nondeterministic but functional)
}

fn is_tracked(fd: c_int) -> bool {
    fd >= 0 && TRACKED_FDS.iter().any(|s| s.load(Ordering::Acquire) == fd)
}

fn untrack_fd(fd: c_int) {
    for slot in &TRACKED_FDS {
        let _ = slot.compare_exchange(fd, -1, Ordering::AcqRel, Ordering::Relaxed);
    }
}

/// Does `path` name one of the random devices? (`path` may be null.)
fn is_random_device(path: *const c_char) -> bool {
    if path.is_null() {
        return false;
    }
    // SAFETY: comparing the caller's NUL-terminated path against literals.
    unsafe {
        libc::strcmp(path, c"/dev/urandom".as_ptr()) == 0
            || libc::strcmp(path, c"/dev/random".as_ptr()) == 0
    }
}

/// Open `/dev/null` via the real `open` to reserve an fd for a fake device.
fn open_placeholder_fd(s: &Shim) -> c_int {
    // SAFETY: real open() with a valid path literal and no mode argument
    // needed for O_RDONLY.
    let fd = unsafe {
        real!(open: fn(*const c_char, c_int, mode_t) -> c_int)(
            c"/dev/null".as_ptr(),
            libc::O_RDONLY,
            0,
        )
    };
    if fd >= 0 && !track_fd(fd) {
        shim_trace!(s, "random-fd table full; giving out the real device");
        // SAFETY: closing the placeholder we just opened.
        unsafe { real!(close: fn(c_int) -> c_int)(fd) };
        return -2; // sentinel: fall back to the real device
    }
    shim_trace!(s, "open(/dev/*random) -> deterministic fd {fd}");
    fd
}

/// `open(2)`. Declared with an explicit `mode` argument instead of C
/// varargs: on every supported ABI (x86-64, aarch64 SysV) the third argument
/// is passed identically whether or not the prototype is variadic, and it is
/// only read when `O_CREAT`/`O_TMPFILE` is set — never the case for the
/// random devices we divert.
///
/// # Safety
///
/// Arguments per the libc `open(2)` contract.
#[no_mangle]
pub unsafe extern "C" fn open(path: *const c_char, flags: c_int, mode: mode_t) -> c_int {
    if let Some(s) = shim() {
        if is_random_device(path) {
            let fd = open_placeholder_fd(s);
            if fd != -2 {
                return fd;
            }
        }
    }
    // SAFETY: forwarding the caller's arguments unchanged.
    unsafe { real!(open: fn(*const c_char, c_int, mode_t) -> c_int)(path, flags, mode) }
}

/// glibc LFS alias of [`open`].
///
/// # Safety
///
/// Arguments per the libc `open64` contract.
#[no_mangle]
pub unsafe extern "C" fn open64(path: *const c_char, flags: c_int, mode: mode_t) -> c_int {
    if let Some(s) = shim() {
        if is_random_device(path) {
            let fd = open_placeholder_fd(s);
            if fd != -2 {
                return fd;
            }
        }
    }
    // SAFETY: forwarding the caller's arguments unchanged.
    unsafe { real!(open64: fn(*const c_char, c_int, mode_t) -> c_int)(path, flags, mode) }
}

/// `openat(2)`. Only absolute paths can name the random devices, so `dirfd`
/// never matters for the diverted case.
///
/// # Safety
///
/// Arguments per the libc `openat(2)` contract.
#[no_mangle]
pub unsafe extern "C" fn openat(dirfd: c_int, path: *const c_char, flags: c_int, mode: mode_t) -> c_int {
    if let Some(s) = shim() {
        if is_random_device(path) {
            let fd = open_placeholder_fd(s);
            if fd != -2 {
                return fd;
            }
        }
    }
    // SAFETY: forwarding the caller's arguments unchanged.
    unsafe {
        real!(openat: fn(c_int, *const c_char, c_int, mode_t) -> c_int)(dirfd, path, flags, mode)
    }
}

/// glibc LFS alias of [`openat`].
///
/// # Safety
///
/// Arguments per the libc `openat64` contract.
#[no_mangle]
pub unsafe extern "C" fn openat64(dirfd: c_int, path: *const c_char, flags: c_int, mode: mode_t) -> c_int {
    if let Some(s) = shim() {
        if is_random_device(path) {
            let fd = open_placeholder_fd(s);
            if fd != -2 {
                return fd;
            }
        }
    }
    // SAFETY: forwarding the caller's arguments unchanged.
    unsafe {
        real!(openat64: fn(c_int, *const c_char, c_int, mode_t) -> c_int)(dirfd, path, flags, mode)
    }
}

fn fill_deterministic(s: &Shim, buf: *mut c_void, count: size_t) -> ssize_t {
    #[allow(clippy::cast_sign_loss)] // isize::MAX is positive
    let len = count.min(isize::MAX as usize);
    // SAFETY: buf is valid for `count >= len` writes per the read contract.
    let slice = unsafe { core::slice::from_raw_parts_mut(buf.cast::<MaybeUninit<u8>>(), len) };
    s.rngs.fill_uninit(Domain::DevRandom, slice);
    #[allow(clippy::cast_possible_wrap)] // capped to isize::MAX above
    let out = len as ssize_t;
    out
}

/// `read(2)`: answered from the `DevRandom` stream for tracked fds.
///
/// # Safety
///
/// Arguments per the libc `read(2)` contract.
#[no_mangle]
pub unsafe extern "C" fn read(fd: c_int, buf: *mut c_void, count: size_t) -> ssize_t {
    if let Some(s) = shim() {
        if is_tracked(fd) && !buf.is_null() {
            let n = fill_deterministic(s, buf, count);
            shim_trace!(s, "read(random fd {fd}, {count}) -> deterministic");
            return n;
        }
    }
    // SAFETY: forwarding the caller's arguments unchanged.
    unsafe { real!(read: fn(c_int, *mut c_void, size_t) -> ssize_t)(fd, buf, count) }
}

/// `pread(2)` on a random device is position-independent randomness.
///
/// # Safety
///
/// Arguments per the libc `pread(2)` contract.
#[no_mangle]
pub unsafe extern "C" fn pread(fd: c_int, buf: *mut c_void, count: size_t, offset: off_t) -> ssize_t {
    if let Some(s) = shim() {
        if is_tracked(fd) && !buf.is_null() {
            return fill_deterministic(s, buf, count);
        }
    }
    // SAFETY: forwarding the caller's arguments unchanged.
    unsafe { real!(pread: fn(c_int, *mut c_void, size_t, off_t) -> ssize_t)(fd, buf, count, offset) }
}

/// glibc LFS alias of [`pread`].
///
/// # Safety
///
/// Arguments per the libc `pread64` contract.
#[no_mangle]
pub unsafe extern "C" fn pread64(fd: c_int, buf: *mut c_void, count: size_t, offset: off_t) -> ssize_t {
    if let Some(s) = shim() {
        if is_tracked(fd) && !buf.is_null() {
            return fill_deterministic(s, buf, count);
        }
    }
    // SAFETY: forwarding the caller's arguments unchanged.
    unsafe { real!(pread64: fn(c_int, *mut c_void, size_t, off_t) -> ssize_t)(fd, buf, count, offset) }
}

/// `close(2)`: untracks diverted fds, then closes the placeholder.
///
/// # Safety
///
/// Arguments per the libc `close(2)` contract.
#[no_mangle]
pub unsafe extern "C" fn close(fd: c_int) -> c_int {
    if shim().is_some() {
        untrack_fd(fd);
        crate::hooks::socket::untrack(fd);
    }
    // SAFETY: forwarding the caller's argument unchanged.
    unsafe { real!(close: fn(c_int) -> c_int)(fd) }
}

// ---------------------------------------------------------------------------
// fopen: glibc stdio reads via an internal alias that bypasses our `read`
// hook, so FILE* streams for the random devices are built with fopencookie.
// ---------------------------------------------------------------------------

/// glibc's `cookie_io_functions_t` and `fopencookie(3)`, declared here
/// because the `libc` crate does not expose them. musl provides the same
/// ABI. The function table is passed by value, matching the C prototype.
#[repr(C)]
struct CookieIoFunctions {
    read: Option<unsafe extern "C" fn(*mut c_void, *mut c_char, size_t) -> ssize_t>,
    write: Option<unsafe extern "C" fn(*mut c_void, *const c_char, size_t) -> ssize_t>,
    seek: Option<unsafe extern "C" fn(*mut c_void, *mut libc::off64_t, c_int) -> c_int>,
    close: Option<unsafe extern "C" fn(*mut c_void) -> c_int>,
}

extern "C" {
    fn fopencookie(
        cookie: *mut c_void,
        mode: *const c_char,
        io_funcs: CookieIoFunctions,
    ) -> *mut FILE;
}

/// Counts random-device `fopen`s process-wide; the value at open time is the
/// substream index handed to [`DevFileRng`], making each stream's identity a
/// deterministic function of open order rather than of thread scheduling.
static DEV_FILE_COUNTER: AtomicU64 = AtomicU64::new(0);

/// glibc stdio read callback: fills `buf` from this file's private substream,
/// which is carried as the fopencookie `cookie`.
unsafe extern "C" fn cookie_read(cookie: *mut c_void, buf: *mut c_char, size: size_t) -> ssize_t {
    // SAFETY: `cookie` is the `*mut DevFileRng` we leaked in `cookie_stream`;
    // it stays valid until `cookie_close` reclaims it, and glibc never calls
    // read after close. The shared reference is sound because `DevFileRng`
    // guards its state with an internal `Mutex`.
    let rng = unsafe { &*(cookie.cast::<DevFileRng>()) };
    #[allow(clippy::cast_sign_loss)] // isize::MAX is positive
    let len = size.min(isize::MAX as usize);
    // SAFETY: glibc guarantees `buf` is valid for `size >= len` writes.
    let slice = unsafe { core::slice::from_raw_parts_mut(buf.cast::<MaybeUninit<u8>>(), len) };
    rng.fill_uninit(slice);
    #[allow(clippy::cast_possible_wrap)] // capped to isize::MAX above
    let out = len as ssize_t;
    out
}

/// glibc stdio close callback: reclaims and drops the leaked substream.
unsafe extern "C" fn cookie_close(cookie: *mut c_void) -> c_int {
    if !cookie.is_null() {
        // SAFETY: reconstructs the `Box<DevFileRng>` leaked in `cookie_stream`.
        // Called exactly once per stream (glibc `fclose` contract), so there
        // is no double free.
        drop(unsafe { Box::from_raw(cookie.cast::<DevFileRng>()) });
    }
    0
}

fn mode_is_read_only(mode: *const c_char) -> bool {
    if mode.is_null() {
        return false;
    }
    // SAFETY: mode is a NUL-terminated string per the fopen contract.
    let first = unsafe { *mode };
    #[allow(clippy::cast_possible_wrap)] // 'r' is ASCII, fits any c_char
    let r = b'r' as c_char;
    first == r
}

fn cookie_stream(s: &Shim, mode: *const c_char) -> *mut FILE {
    // One private substream per open, indexed by process-global open order.
    // Buffering is left on: read-ahead only advances this file's own stream,
    // so the bytes glibc discards at fclose are a deterministic function of
    // the open index — no shared-stream interleaving to make it vary.
    let index = DEV_FILE_COUNTER.fetch_add(1, Ordering::Relaxed);
    let cookie = Box::into_raw(Box::new(s.rngs.dev_file_stream(index)));
    let funcs = CookieIoFunctions {
        read: Some(cookie_read),
        write: None,
        seek: None,
        close: Some(cookie_close),
    };
    shim_trace!(s, "fopen(/dev/*random) -> deterministic cookie stream #{index}");
    // SAFETY: `cookie` is a live `DevFileRng` leaked just above; the function
    // table is static; the mode string is the caller's, valid per the fopen
    // contract. On success glibc owns the cookie and returns it via
    // `cookie_close`.
    let file = unsafe { fopencookie(cookie.cast::<c_void>(), mode, funcs) };
    if file.is_null() {
        // fopencookie failed and will not call cookie_close, so reclaim the
        // Box ourselves to avoid leaking the substream.
        // SAFETY: `cookie` is the pointer we just leaked and glibc never took
        // ownership (null return), so this is the sole owner.
        drop(unsafe { Box::from_raw(cookie) });
    }
    file
}

/// `fopen(3)` for the random devices returns a `fopencookie` stream backed by
/// the `DevRandom` ChaCha8 stream. Write modes fall through to the real fopen.
///
/// # Safety
///
/// Arguments per the libc `fopen(3)` contract.
#[no_mangle]
pub unsafe extern "C" fn fopen(path: *const c_char, mode: *const c_char) -> *mut FILE {
    if let Some(s) = shim() {
        if is_random_device(path) && mode_is_read_only(mode) {
            return cookie_stream(s, mode);
        }
    }
    // SAFETY: forwarding the caller's arguments unchanged.
    unsafe { real!(fopen: fn(*const c_char, *const c_char) -> *mut FILE)(path, mode) }
}

/// glibc LFS alias of [`fopen`].
///
/// # Safety
///
/// Arguments per the libc `fopen64` contract.
#[no_mangle]
pub unsafe extern "C" fn fopen64(path: *const c_char, mode: *const c_char) -> *mut FILE {
    if let Some(s) = shim() {
        if is_random_device(path) && mode_is_read_only(mode) {
            return cookie_stream(s, mode);
        }
    }
    // SAFETY: forwarding the caller's arguments unchanged.
    unsafe { real!(fopen64: fn(*const c_char, *const c_char) -> *mut FILE)(path, mode) }
}
