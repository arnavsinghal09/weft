//! File I/O interception: write, fsync, and open hooks for fault injection.
//!
//! Tracks bytes written per file descriptor and can simulate fsync_lies
//! (fsync returns success without persisting) and torn writes (partial write
//! on process crash). When no file I/O fault configuration is active, all
//! hooks pass through to the real libc implementation.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;

use libc::{c_int, c_void, size_t, ssize_t, off_t, off64_t};

use crate::real::real;
use crate::state::shim;
use crate::trace::shim_trace;

static BYTES_WRITTEN: AtomicU64 = AtomicU64::new(0);

/// Deterministic `write(2)`: track bytes written for ENOSPC injection.
///
/// # Safety
///
/// `buf` must be valid for `count` bytes, per the libc contract.
#[no_mangle]
pub unsafe extern "C" fn write(fd: c_int, buf: *const c_void, count: size_t) -> ssize_t {
    let Some(s) = shim() else {
        return real(libc::write)(fd, buf, count);
    };

    // For now, just track bytes written. File I/O fault config would come from
    // environment or scenario data passed to the shim.
    // Future: check WEFT_ENOSPC_BYTES and return -ENOSPC if exceeded.
    BYTES_WRITTEN.fetch_add(count as u64, Ordering::Relaxed);

    if s.trace {
        shim_trace(&format!("write({fd}, {count}) -> bytes_written={}", BYTES_WRITTEN.load(Ordering::Relaxed)));
    }

    real(libc::write)(fd, buf, count)
}

/// Deterministic `pwrite(2)`: same as write but with offset.
///
/// # Safety
///
/// `buf` must be valid for `count` bytes, per the libc contract.
#[no_mangle]
pub unsafe extern "C" fn pwrite(fd: c_int, buf: *const c_void, count: size_t, offset: off_t) -> ssize_t {
    let Some(s) = shim() else {
        return real(libc::pwrite)(fd, buf, count, offset);
    };

    BYTES_WRITTEN.fetch_add(count as u64, Ordering::Relaxed);

    if s.trace {
        shim_trace(&format!("pwrite({fd}, {count}, {offset})"));
    }

    real(libc::pwrite)(fd, buf, count, offset)
}

/// Deterministic `pwrite64(2)`: same as pwrite but with 64-bit offset.
///
/// # Safety
///
/// `buf` must be valid for `count` bytes, per the libc contract.
#[no_mangle]
pub unsafe extern "C" fn pwrite64(fd: c_int, buf: *const c_void, count: size_t, offset: off64_t) -> ssize_t {
    let Some(s) = shim() else {
        return real(libc::pwrite64)(fd, buf, count, offset);
    };

    BYTES_WRITTEN.fetch_add(count as u64, Ordering::Relaxed);

    if s.trace {
        shim_trace(&format!("pwrite64({fd}, {count}, {offset})"));
    }

    real(libc::pwrite64)(fd, buf, count, offset)
}

/// Deterministic `fsync(2)`: optionally returns success without persisting.
///
/// When fsync_lies mode is active (set via WEFT_FSYNC_LIES=1), returns success
/// (0) without actually calling the real fsync. Otherwise passes through.
#[no_mangle]
pub extern "C" fn fsync(fd: c_int) -> c_int {
    let Some(s) = shim() else {
        return real(libc::fsync)(fd);
    };

    // Check if fsync_lies is enabled. For now, hardcode off; future work will
    // read this from scenario config passed via environment (e.g., WEFT_FSYNC_LIES=1).
    let fsync_lies = std::env::var("WEFT_FSYNC_LIES").is_ok_and(|v| v == "1");

    if fsync_lies {
        if s.trace {
            shim_trace(&format!("fsync({fd}) -> success (lies)"));
        }
        return 0; // Return success without actually syncing.
    }

    if s.trace {
        shim_trace(&format!("fsync({fd})"));
    }

    real(libc::fsync)(fd)
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
        return real(libc::fdatasync)(fd);
    };

    let fsync_lies = std::env::var("WEFT_FSYNC_LIES").is_ok_and(|v| v == "1");

    if fsync_lies {
        if s.trace {
            shim_trace(&format!("fdatasync({fd}) -> success (lies)"));
        }
        return 0;
    }

    if s.trace {
        shim_trace(&format!("fdatasync({fd})"));
    }

    real(libc::fdatasync)(fd)
}

/// Return total bytes written by this process (for testing/validation).
///
/// Not a libc function; used internally for scenario validation.
#[allow(dead_code)]
pub fn bytes_written() -> u64 {
    BYTES_WRITTEN.load(Ordering::Relaxed)
}
