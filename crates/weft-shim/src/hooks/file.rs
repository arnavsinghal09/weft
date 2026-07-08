//! File I/O interception: write, fsync, and open hooks for fault injection.
//!
//! Tracks bytes written per file descriptor and can simulate fsync_lies
//! (fsync returns success without persisting) and torn writes (partial write
//! on process crash). When no file I/O fault configuration is active, all
//! hooks pass through to the real libc implementation.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;

use libc::{c_int, c_void, off64_t, off_t, size_t, ssize_t};

use crate::real::real;
use crate::state::shim;
use crate::trace::shim_trace;

static BYTES_WRITTEN: AtomicU64 = AtomicU64::new(0);

/// Whether `fsync`/`fdatasync` should lie (`WEFT_FSYNC_LIES=1`), parsed once.
/// Hooks must not call `std::env::var` per call: it allocates.
fn fsync_lies() -> bool {
    static FSYNC_LIES: OnceLock<bool> = OnceLock::new();
    *FSYNC_LIES.get_or_init(|| std::env::var("WEFT_FSYNC_LIES").is_ok_and(|v| v == "1"))
}

/// Deterministic `write(2)`: track bytes written for ENOSPC injection.
///
/// No tracing here: [`crate::trace`] emits via `write(2)`, which resolves to
/// this very hook under interposition, so a trace line from inside `write`
/// would recurse without bound.
///
/// # Safety
///
/// `buf` must be valid for `count` bytes, per the libc contract.
#[no_mangle]
pub unsafe extern "C" fn write(fd: c_int, buf: *const c_void, count: size_t) -> ssize_t {
    if shim().is_some() {
        // For now, just track bytes written. File I/O fault config would come
        // from environment or scenario data passed to the shim.
        // Future: check WEFT_ENOSPC_BYTES and return -ENOSPC if exceeded.
        BYTES_WRITTEN.fetch_add(count as u64, Ordering::Relaxed);
    }

    // SAFETY: forwarding the caller's arguments unchanged to real write().
    unsafe { real!(write: fn(c_int, *const c_void, size_t) -> ssize_t)(fd, buf, count) }
}

/// Deterministic `pwrite(2)`: same as write but with offset.
///
/// # Safety
///
/// `buf` must be valid for `count` bytes, per the libc contract.
#[no_mangle]
pub unsafe extern "C" fn pwrite(
    fd: c_int,
    buf: *const c_void,
    count: size_t,
    offset: off_t,
) -> ssize_t {
    if let Some(s) = shim() {
        BYTES_WRITTEN.fetch_add(count as u64, Ordering::Relaxed);
        shim_trace!(s, "pwrite({fd}, {count}, {offset})");
    }

    // SAFETY: forwarding the caller's arguments unchanged to real pwrite().
    unsafe {
        real!(pwrite: fn(c_int, *const c_void, size_t, off_t) -> ssize_t)(fd, buf, count, offset)
    }
}

/// Deterministic `pwrite64(2)`: same as pwrite but with 64-bit offset.
///
/// # Safety
///
/// `buf` must be valid for `count` bytes, per the libc contract.
#[no_mangle]
pub unsafe extern "C" fn pwrite64(
    fd: c_int,
    buf: *const c_void,
    count: size_t,
    offset: off64_t,
) -> ssize_t {
    if let Some(s) = shim() {
        BYTES_WRITTEN.fetch_add(count as u64, Ordering::Relaxed);
        shim_trace!(s, "pwrite64({fd}, {count}, {offset})");
    }

    // SAFETY: forwarding the caller's arguments unchanged to real pwrite64().
    unsafe {
        real!(pwrite64: fn(c_int, *const c_void, size_t, off64_t) -> ssize_t)(
            fd, buf, count, offset,
        )
    }
}

/// Deterministic `fsync(2)`: optionally returns success without persisting.
///
/// When fsync_lies mode is active (set via WEFT_FSYNC_LIES=1), returns success
/// (0) without actually calling the real fsync. Otherwise passes through.
#[no_mangle]
pub extern "C" fn fsync(fd: c_int) -> c_int {
    let Some(s) = shim() else {
        // SAFETY: forwarding the caller's argument unchanged to real fsync().
        return unsafe { real!(fsync: fn(c_int) -> c_int)(fd) };
    };

    if fsync_lies() {
        shim_trace!(s, "fsync({fd}) -> success (lies)");
        return 0; // Return success without actually syncing.
    }

    shim_trace!(s, "fsync({fd})");

    // SAFETY: forwarding the caller's argument unchanged to real fsync().
    unsafe { real!(fsync: fn(c_int) -> c_int)(fd) }
}

/// Deterministic `fdatasync(2)`: similar to fsync_lies but for data only.
///
/// # Notes
///
/// When fsync_lies is active, also lies for fdatasync. Most applications
/// treat fdatasync and fsync equivalently for fault-tolerance purposes.
#[no_mangle]
pub extern "C" fn fdatasync(fd: c_int) -> c_int {
    let Some(s) = shim() else {
        // SAFETY: forwarding the caller's argument unchanged to real fdatasync().
        return unsafe { real!(fdatasync: fn(c_int) -> c_int)(fd) };
    };

    if fsync_lies() {
        shim_trace!(s, "fdatasync({fd}) -> success (lies)");
        return 0;
    }

    shim_trace!(s, "fdatasync({fd})");

    // SAFETY: forwarding the caller's argument unchanged to real fdatasync().
    unsafe { real!(fdatasync: fn(c_int) -> c_int)(fd) }
}

/// Return total bytes written by this process (for testing/validation).
///
/// Not a libc function; used internally for scenario validation.
#[allow(dead_code)]
pub fn bytes_written() -> u64 {
    BYTES_WRITTEN.load(Ordering::Relaxed)
}
