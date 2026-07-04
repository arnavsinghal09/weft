//! Interposed pthread surface: the yield points of the deterministic
//! scheduler ([`crate::sched`]).
//!
//! Managed synchronization operations are handled *entirely* by the scheduler
//! model — we deliberately do **not** call the real `pthread_mutex_*` /
//! `pthread_cond_*`, because cooperative execution plus the scheduler's own
//! barrier'd hand-off already provides both mutual exclusion and the required
//! happens-before ordering. Only `pthread_create`/`pthread_join` call through
//! to the real libc (to actually spawn and reap OS threads).
//!
//! Every hook falls back to the real function when the shim is inactive, when
//! re-entered from inside the scheduler, or when the calling thread isn't
//! managed (e.g. the main thread before the first `pthread_create`).

use core::ffi::c_void;

use libc::{c_int, pthread_attr_t, pthread_cond_t, pthread_mutex_t, pthread_t, timespec};

use crate::real::real;
use crate::sched::{current_tid, is_reentrant, Reentrancy, Tid};
use crate::state::{shim, Shim};

/// The shim, when deterministic scheduling is engaged and we're not
/// re-entering from within scheduler machinery. `None` also when scheduling is
/// disabled (`WEFT_SCHED=0`), in which case pthread operations pass through to
/// the OS while time/randomness stay deterministic.
fn active() -> Option<&'static Shim> {
    if is_reentrant() {
        return None;
    }
    let s = shim()?;
    if s.sched_enabled {
        Some(s)
    } else {
        None
    }
}

/// The shim, only if the calling thread is registered with the scheduler
/// (so its sync operations are yield points). Unregistered threads — most
/// importantly the main thread before it ever calls `pthread_create` — pass
/// straight through, which keeps single-threaded programs zero-overhead.
fn managed() -> Option<&'static Shim> {
    let s = active()?;
    if current_tid().is_some() {
        Some(s)
    } else {
        None
    }
}

type StartRoutine = extern "C" fn(*mut c_void) -> *mut c_void;

struct TrampArg {
    start: StartRoutine,
    arg: *mut c_void,
    tid: Tid,
}

/// Wraps a target thread's start routine: adopt the reserved tid, park until
/// scheduled, run the real body (whose sync calls are yield points), then
/// mark finished and hand the token on.
extern "C" fn trampoline(raw: *mut c_void) -> *mut c_void {
    // SAFETY: `raw` is the `Box<TrampArg>` leaked in `pthread_create`.
    let a = unsafe { Box::from_raw(raw.cast::<TrampArg>()) };
    let Some(s) = shim() else {
        return (a.start)(a.arg);
    };
    s.sched.child_started(a.tid);
    let ret = (a.start)(a.arg);
    s.sched.thread_finished(a.tid);
    ret
}

/// # Safety
///
/// Arguments per the libc `pthread_create(3)` contract.
#[no_mangle]
pub unsafe extern "C" fn pthread_create(
    thread: *mut pthread_t,
    attr: *const pthread_attr_t,
    start: StartRoutine,
    arg: *mut c_void,
) -> c_int {
    let Some(s) = active() else {
        // SAFETY: forwarding the caller's arguments unchanged.
        return unsafe {
            real!(pthread_create: fn(*mut pthread_t, *const pthread_attr_t, StartRoutine, *mut c_void) -> c_int)(
                thread, attr, start, arg,
            )
        };
    };
    s.sched.ensure_main_registered();
    let tid = s.sched.register_child();
    let boxed = Box::into_raw(Box::new(TrampArg { start, arg, tid }));

    let rc = {
        // Fence libc's own internals during thread creation so any pthread
        // calls it makes take the passthrough path.
        let _g = Reentrancy::enter();
        // SAFETY: same signature as the real pthread_create; `trampoline` and
        // `boxed` stand in for the caller's start routine and argument.
        unsafe {
            real!(pthread_create: fn(*mut pthread_t, *const pthread_attr_t, StartRoutine, *mut c_void) -> c_int)(
                thread,
                attr,
                trampoline,
                boxed.cast::<c_void>(),
            )
        }
    };

    if rc != 0 {
        // Creation failed: reclaim the box and retire the reserved tid.
        // SAFETY: `boxed` came from Box::into_raw just above and the child
        // never started, so we own it.
        drop(unsafe { Box::from_raw(boxed) });
        s.sched.abandon_child(tid);
        return rc;
    }

    #[allow(clippy::cast_possible_truncation)] // pthread_t is pointer-sized on Linux
    // SAFETY: on success the real pthread_create wrote the handle to *thread.
    let handle = unsafe { *thread } as usize;
    s.sched.record_handle(handle, tid);
    // Yield so the scheduler may choose to run the new child now.
    s.sched.yield_now("pthread_create");
    rc
}

/// # Safety
///
/// Arguments per the libc `pthread_join(3)` contract.
#[no_mangle]
pub unsafe extern "C" fn pthread_join(thread: pthread_t, retval: *mut *mut c_void) -> c_int {
    if let Some(s) = managed() {
        #[allow(clippy::cast_possible_truncation)] // pthread_t is pointer-sized on Linux
        if let Some(tid) = s.sched.tid_for_handle(thread as usize) {
            s.sched.join(tid);
        }
    }
    // Reap the OS thread (also the path for unmanaged/unknown handles).
    let _g = Reentrancy::enter();
    // SAFETY: forwarding the caller's arguments unchanged.
    unsafe { real!(pthread_join: fn(pthread_t, *mut *mut c_void) -> c_int)(thread, retval) }
}

/// # Safety
///
/// Terminates the calling thread; does not return.
#[no_mangle]
pub unsafe extern "C" fn pthread_exit(retval: *mut c_void) -> ! {
    if let Some(s) = managed() {
        if let Some(tid) = current_tid() {
            s.sched.thread_finished(tid);
        }
    }
    let _g = Reentrancy::enter();
    // SAFETY: forwarding the caller's argument to the real, noreturn function.
    unsafe { real!(pthread_exit: fn(*mut c_void) -> ())(retval) };
    unreachable!("pthread_exit does not return")
}

// Reentrancy no-op rationale (applies to every sync hook below).
//
// When a sync hook is entered while `is_reentrant` is set, we are inside our
// own bootstrap or scheduler machinery — resolving a symbol via `dlsym`, or
// allocating inside a scheduler method — and libc has taken one of *its*
// internal locks (e.g. the malloc arena lock) through the interposed symbol.
// Calling the real function here would recurse (`dlsym` → `malloc` →
// `pthread_mutex_lock` → `dlsym` …). We instead return success without
// locking, which is sound because a reentrant context is always effectively
// single-threaded: during startup only one thread exists, and during a
// scheduler operation exactly one logical thread is running by construction.

/// # Safety
///
/// Arguments per the libc `pthread_mutex_lock(3)` contract.
#[no_mangle]
pub unsafe extern "C" fn pthread_mutex_lock(mutex: *mut pthread_mutex_t) -> c_int {
    if is_reentrant() {
        return 0;
    }
    if let Some(s) = managed() {
        s.sched.mutex_lock(mutex as usize);
        return 0;
    }
    // SAFETY: forwarding the caller's argument unchanged.
    unsafe { real!(pthread_mutex_lock: fn(*mut pthread_mutex_t) -> c_int)(mutex) }
}

/// # Safety
///
/// Arguments per the libc `pthread_mutex_trylock(3)` contract.
#[no_mangle]
pub unsafe extern "C" fn pthread_mutex_trylock(mutex: *mut pthread_mutex_t) -> c_int {
    if is_reentrant() {
        return 0; // acquired (no-op, single-threaded context)
    }
    if let Some(s) = managed() {
        return if s.sched.mutex_trylock(mutex as usize) {
            0
        } else {
            libc::EBUSY
        };
    }
    // SAFETY: forwarding the caller's argument unchanged.
    unsafe { real!(pthread_mutex_trylock: fn(*mut pthread_mutex_t) -> c_int)(mutex) }
}

/// # Safety
///
/// Arguments per the libc `pthread_mutex_unlock(3)` contract.
#[no_mangle]
pub unsafe extern "C" fn pthread_mutex_unlock(mutex: *mut pthread_mutex_t) -> c_int {
    if is_reentrant() {
        return 0;
    }
    if let Some(s) = managed() {
        s.sched.mutex_unlock(mutex as usize);
        return 0;
    }
    // SAFETY: forwarding the caller's argument unchanged.
    unsafe { real!(pthread_mutex_unlock: fn(*mut pthread_mutex_t) -> c_int)(mutex) }
}

/// # Safety
///
/// Arguments per the libc `pthread_cond_wait(3)` contract.
#[no_mangle]
pub unsafe extern "C" fn pthread_cond_wait(
    cond: *mut pthread_cond_t,
    mutex: *mut pthread_mutex_t,
) -> c_int {
    if is_reentrant() {
        return 0;
    }
    if let Some(s) = managed() {
        s.sched.cond_wait(cond as usize, mutex as usize);
        return 0;
    }
    // SAFETY: forwarding the caller's arguments unchanged.
    unsafe {
        real!(pthread_cond_wait: fn(*mut pthread_cond_t, *mut pthread_mutex_t) -> c_int)(cond, mutex)
    }
}

/// `pthread_cond_timedwait`: modeled as an untimed wait (the deadline is not
/// honored — see `docs/scheduling-model.md`). Signal-driven code is
/// unaffected; code relying solely on the timeout to wake is not supported.
///
/// # Safety
///
/// Arguments per the libc `pthread_cond_timedwait(3)` contract.
#[no_mangle]
pub unsafe extern "C" fn pthread_cond_timedwait(
    cond: *mut pthread_cond_t,
    mutex: *mut pthread_mutex_t,
    abstime: *const timespec,
) -> c_int {
    if is_reentrant() {
        return 0;
    }
    if let Some(s) = managed() {
        s.sched.cond_wait(cond as usize, mutex as usize);
        return 0;
    }
    // SAFETY: forwarding the caller's arguments unchanged.
    unsafe {
        real!(pthread_cond_timedwait: fn(*mut pthread_cond_t, *mut pthread_mutex_t, *const timespec) -> c_int)(
            cond, mutex, abstime,
        )
    }
}

/// # Safety
///
/// Arguments per the libc `pthread_cond_signal(3)` contract.
#[no_mangle]
pub unsafe extern "C" fn pthread_cond_signal(cond: *mut pthread_cond_t) -> c_int {
    if is_reentrant() {
        return 0;
    }
    if let Some(s) = managed() {
        s.sched.cond_signal(cond as usize);
        return 0;
    }
    // SAFETY: forwarding the caller's argument unchanged.
    unsafe { real!(pthread_cond_signal: fn(*mut pthread_cond_t) -> c_int)(cond) }
}

/// # Safety
///
/// Arguments per the libc `pthread_cond_broadcast(3)` contract.
#[no_mangle]
pub unsafe extern "C" fn pthread_cond_broadcast(cond: *mut pthread_cond_t) -> c_int {
    if is_reentrant() {
        return 0;
    }
    if let Some(s) = managed() {
        s.sched.cond_broadcast(cond as usize);
        return 0;
    }
    // SAFETY: forwarding the caller's argument unchanged.
    unsafe { real!(pthread_cond_broadcast: fn(*mut pthread_cond_t) -> c_int)(cond) }
}

/// # Safety
///
/// Always safe; declared unsafe only to match the interposed C ABI.
#[no_mangle]
pub unsafe extern "C" fn sched_yield() -> c_int {
    if let Some(s) = managed() {
        s.sched.yield_now("sched_yield");
        return 0;
    }
    // SAFETY: no arguments to forward.
    unsafe { real!(sched_yield: fn() -> c_int)() }
}
