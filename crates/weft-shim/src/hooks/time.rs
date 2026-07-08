//! Time interposition: `time`, `gettimeofday`, `clock_gettime`,
//! `clock_getres`, `timespec_get`, and the sleep family.
//!
//! All virtual time comes from [`crate::vclock::VClock`]; sleeps advance it
//! and return immediately. When no seed is active every function tail-calls
//! the real libc implementation.

use core::ffi::c_void;

use libc::{c_int, c_uint, clockid_t, time_t, timespec, timeval, useconds_t};

use crate::real::real;
use crate::state::shim;
use crate::trace::shim_trace;
use crate::vclock::NANOS_PER_SEC;

#[allow(clippy::cast_possible_wrap)] // virtual times stay far below i64::MAX
fn write_timespec(ns: u64, tp: *mut timespec) {
    if tp.is_null() {
        return;
    }
    // SAFETY: caller (libc API contract) supplies a valid timespec pointer;
    // null was checked above.
    unsafe {
        (*tp).tv_sec = (ns / NANOS_PER_SEC) as time_t;
        (*tp).tv_nsec = (ns % NANOS_PER_SEC) as _;
    }
}

fn read_timespec_ns(tp: *const timespec) -> Option<u64> {
    if tp.is_null() {
        return None;
    }
    // SAFETY: non-null pointer supplied by the caller per the libc contract.
    let (sec, nsec) = unsafe { ((*tp).tv_sec, (*tp).tv_nsec) };
    if sec < 0 || !(0..1_000_000_000).contains(&nsec) {
        return None;
    }
    #[allow(clippy::cast_sign_loss)] // negativity checked above
    Some(sec as u64 * NANOS_PER_SEC + nsec as u64)
}

/// Deterministic `time(2)`.
///
/// # Safety
///
/// `tloc` must be null or valid for writes, per the libc contract.
#[no_mangle]
pub unsafe extern "C" fn time(tloc: *mut time_t) -> time_t {
    let Some(s) = shim() else {
        // SAFETY: forwarding the caller's arguments unchanged to real time().
        return unsafe { real!(time: fn(*mut time_t) -> time_t)(tloc) };
    };
    #[allow(clippy::cast_possible_wrap)] // virtual times stay far below i64::MAX
    let secs = (s.clock.now_real_ns() / NANOS_PER_SEC) as time_t;
    if !tloc.is_null() {
        // SAFETY: tloc checked non-null; valid per the libc contract.
        unsafe { *tloc = secs };
    }
    shim_trace!(s, "time() -> {secs}");
    secs
}

/// Deterministic `gettimeofday(2)`. The timezone argument, obsolete in POSIX,
/// is zeroed when non-null.
///
/// # Safety
///
/// `tv`/`tz` must each be null or valid for writes, per the libc contract.
#[no_mangle]
pub unsafe extern "C" fn gettimeofday(tv: *mut timeval, tz: *mut c_void) -> c_int {
    let Some(s) = shim() else {
        // SAFETY: forwarding the caller's arguments unchanged.
        return unsafe { real!(gettimeofday: fn(*mut timeval, *mut c_void) -> c_int)(tv, tz) };
    };
    let ns = s.clock.now_real_ns();
    if !tv.is_null() {
        // SAFETY: tv checked non-null; valid per the libc contract.
        #[allow(clippy::cast_possible_wrap)] // virtual times stay far below i64::MAX
        unsafe {
            (*tv).tv_sec = (ns / NANOS_PER_SEC) as time_t;
            (*tv).tv_usec = ((ns % NANOS_PER_SEC) / 1_000) as _;
        }
    }
    if !tz.is_null() {
        // SAFETY: tz checked non-null; struct timezone is two ints (8 bytes),
        // valid for writes per the libc contract.
        unsafe { core::ptr::write_bytes(tz.cast::<u8>(), 0, 8) };
    }
    shim_trace!(
        s,
        "gettimeofday() -> {}.{:06}",
        ns / NANOS_PER_SEC,
        (ns % NANOS_PER_SEC) / 1_000
    );
    0
}

/// Deterministic `clock_gettime(2)`. Realtime clocks get the seed-offset wall
/// clock; every other clock id (monotonic, boottime, CPU-time) maps to the
/// virtual-monotonic counter.
///
/// # Safety
///
/// `tp` must be null or valid for writes, per the libc contract.
#[no_mangle]
pub unsafe extern "C" fn clock_gettime(clk: clockid_t, tp: *mut timespec) -> c_int {
    let Some(s) = shim() else {
        // SAFETY: forwarding the caller's arguments unchanged.
        return unsafe { real!(clock_gettime: fn(clockid_t, *mut timespec) -> c_int)(clk, tp) };
    };
    let ns = match clk {
        libc::CLOCK_REALTIME | libc::CLOCK_REALTIME_COARSE | libc::CLOCK_TAI => {
            s.clock.now_real_ns()
        }
        _ => s.clock.now_mono_ns(),
    };
    write_timespec(ns, tp);
    shim_trace!(
        s,
        "clock_gettime({clk}) -> {}.{:09}",
        ns / NANOS_PER_SEC,
        ns % NANOS_PER_SEC
    );
    0
}

/// `clock_getres(2)`: virtual clocks have 1 ns resolution.
///
/// # Safety
///
/// `tp` must be null or valid for writes, per the libc contract.
#[no_mangle]
pub unsafe extern "C" fn clock_getres(clk: clockid_t, tp: *mut timespec) -> c_int {
    if shim().is_none() {
        // SAFETY: forwarding the caller's arguments unchanged.
        return unsafe { real!(clock_getres: fn(clockid_t, *mut timespec) -> c_int)(clk, tp) };
    }
    write_timespec(1, tp);
    0
}

/// C11 `timespec_get`. `base` other than `TIME_UTC` (1) fails per the spec.
///
/// # Safety
///
/// `tp` must be null or valid for writes, per the libc contract.
#[no_mangle]
pub unsafe extern "C" fn timespec_get(tp: *mut timespec, base: c_int) -> c_int {
    let Some(s) = shim() else {
        // SAFETY: forwarding the caller's arguments unchanged.
        return unsafe { real!(timespec_get: fn(*mut timespec, c_int) -> c_int)(tp, base) };
    };
    if base != 1 {
        return 0; // TIME_UTC is the only base C11 defines
    }
    write_timespec(s.clock.now_real_ns(), tp);
    base
}

/// `nanosleep(2)`: advances virtual time by the requested duration, never
/// blocks, never reports interruption.
///
/// # Safety
///
/// `req` must be a valid pointer; `rem` null or valid for writes.
#[no_mangle]
#[allow(clippy::similar_names)] // req/rem are the POSIX parameter names
pub unsafe extern "C" fn nanosleep(req: *const timespec, rem: *mut timespec) -> c_int {
    let Some(s) = shim() else {
        // SAFETY: forwarding the caller's arguments unchanged.
        return unsafe { real!(nanosleep: fn(*const timespec, *mut timespec) -> c_int)(req, rem) };
    };
    let Some(ns) = read_timespec_ns(req) else {
        // SAFETY: setting errno through libc's thread-local errno location.
        unsafe { *libc::__errno_location() = libc::EINVAL };
        return -1;
    };
    s.clock.advance_ns(ns);
    write_timespec(0, rem); // never interrupted: zero remaining time
    shim_trace!(
        s,
        "nanosleep({}.{:09}) -> instant",
        ns / NANOS_PER_SEC,
        ns % NANOS_PER_SEC
    );
    0
}

/// `clock_nanosleep(2)`, including `TIMER_ABSTIME` deadlines against the
/// virtual clocks. Returns 0 or an error number (not -1/errno).
///
/// # Safety
///
/// `req` must be a valid pointer; `rem` null or valid for writes.
#[no_mangle]
#[allow(clippy::similar_names)] // req/rem are the POSIX parameter names
pub unsafe extern "C" fn clock_nanosleep(
    clk: clockid_t,
    flags: c_int,
    req: *const timespec,
    rem: *mut timespec,
) -> c_int {
    let Some(s) = shim() else {
        // SAFETY: forwarding the caller's arguments unchanged.
        return unsafe {
            real!(clock_nanosleep: fn(clockid_t, c_int, *const timespec, *mut timespec) -> c_int)(
                clk, flags, req, rem,
            )
        };
    };
    let Some(ns) = read_timespec_ns(req) else {
        return libc::EINVAL;
    };
    if flags & libc::TIMER_ABSTIME != 0 {
        match clk {
            libc::CLOCK_REALTIME => s.clock.advance_real_to(ns),
            _ => s.clock.advance_mono_to(ns),
        }
    } else {
        s.clock.advance_ns(ns);
    }
    write_timespec(0, rem);
    0
}

/// `sleep(3)`: advances virtual time; reports zero seconds remaining.
///
/// # Safety
///
/// Always safe; declared unsafe only to match the interposed C ABI.
#[no_mangle]
pub unsafe extern "C" fn sleep(seconds: c_uint) -> c_uint {
    let Some(s) = shim() else {
        // SAFETY: forwarding the caller's argument unchanged.
        return unsafe { real!(sleep: fn(c_uint) -> c_uint)(seconds) };
    };
    s.clock.advance_ns(u64::from(seconds) * NANOS_PER_SEC);
    shim_trace!(s, "sleep({seconds}) -> instant");
    0
}

/// `usleep(3)`: advances virtual time.
///
/// # Safety
///
/// Always safe; declared unsafe only to match the interposed C ABI.
#[no_mangle]
pub unsafe extern "C" fn usleep(usec: useconds_t) -> c_int {
    let Some(s) = shim() else {
        // SAFETY: forwarding the caller's argument unchanged.
        return unsafe { real!(usleep: fn(useconds_t) -> c_int)(usec) };
    };
    s.clock.advance_ns(u64::from(usec) * 1_000);
    0
}
