//! Randomness interposition: the `rand`/`random` families, the `*48`
//! (drand48) family, `getrandom(2)`, and `getentropy(3)`.
//!
//! Everything draws from the ChaCha8 domain streams in [`crate::rng`]; the
//! caller-state variants (`rand_r`, `erand48`/`nrand48`/`jrand48`) advance
//! the caller's own state deterministically, mixed with the run seed so a
//! different `--seed` still changes their output.

use core::ffi::c_void;
use core::mem::MaybeUninit;

use libc::{c_char, c_int, c_long, c_uint, c_ushort, size_t, ssize_t};

use crate::real::real;
use crate::state::shim;
use crate::trace::shim_trace;
use weft_abi::{splitmix64, Domain};

const RAND_31_MASK: u64 = 0x7FFF_FFFF;

/// Deterministic `rand(3)`: uniform in `[0, RAND_MAX]`.
///
/// # Safety
///
/// Always safe; declared unsafe only to match the interposed C ABI.
#[no_mangle]
pub unsafe extern "C" fn rand() -> c_int {
    let Some(s) = shim() else {
        // SAFETY: no arguments to forward.
        return unsafe { real!(rand: fn() -> c_int)() };
    };
    #[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)] // 31-bit mask
    let v = (s.rngs.next_u64(Domain::LibcRand) & RAND_31_MASK) as c_int;
    shim_trace!(s, "rand() -> {v}");
    v
}

/// `srand(3)`: reseeds the libc-rand stream, mixed with the run seed.
///
/// # Safety
///
/// Always safe; declared unsafe only to match the interposed C ABI.
#[no_mangle]
pub unsafe extern "C" fn srand(seed: c_uint) {
    let Some(s) = shim() else {
        // SAFETY: forwarding the caller's argument unchanged.
        return unsafe { real!(srand: fn(c_uint) -> ())(seed) };
    };
    shim_trace!(s, "srand({seed})");
    s.rngs.reseed_libc_rand(u64::from(seed));
}

/// Deterministic `random(3)`: uniform in `[0, 2^31)`.
///
/// # Safety
///
/// Always safe; declared unsafe only to match the interposed C ABI.
#[no_mangle]
pub unsafe extern "C" fn random() -> c_long {
    let Some(s) = shim() else {
        // SAFETY: no arguments to forward.
        return unsafe { real!(random: fn() -> c_long)() };
    };
    #[allow(clippy::cast_possible_wrap)] // 31-bit mask fits any c_long
    let v = (s.rngs.next_u64(Domain::LibcRand) & RAND_31_MASK) as c_long;
    v
}

/// `srandom(3)`: same reseed as [`srand`].
///
/// # Safety
///
/// Always safe; declared unsafe only to match the interposed C ABI.
#[no_mangle]
pub unsafe extern "C" fn srandom(seed: c_uint) {
    let Some(s) = shim() else {
        // SAFETY: forwarding the caller's argument unchanged.
        return unsafe { real!(srandom: fn(c_uint) -> ())(seed) };
    };
    s.rngs.reseed_libc_rand(u64::from(seed));
}

/// `rand_r(3)`: advances the caller's state word deterministically.
///
/// # Safety
///
/// `seedp` must be a valid pointer, per the libc contract.
#[no_mangle]
pub unsafe extern "C" fn rand_r(seedp: *mut c_uint) -> c_int {
    let Some(s) = shim() else {
        // SAFETY: forwarding the caller's argument unchanged.
        return unsafe { real!(rand_r: fn(*mut c_uint) -> c_int)(seedp) };
    };
    // SAFETY: seedp is valid per the libc contract (rand_r has no null case).
    let state = unsafe { *seedp };
    let (next, value) = crate::rng::rand_r_step(state, s.seed);
    // SAFETY: as above.
    unsafe { *seedp = next };
    value
}

/// glibc `initstate(3)`: treated as a reseed; the caller's state buffer is
/// accepted but unused (our generator state lives in the shim).
///
/// # Safety
///
/// Arguments per the libc contract; `statebuf` is returned, never written.
#[no_mangle]
pub unsafe extern "C" fn initstate(
    seed: c_uint,
    statebuf: *mut c_char,
    size: size_t,
) -> *mut c_char {
    let Some(s) = shim() else {
        // SAFETY: forwarding the caller's arguments unchanged.
        return unsafe {
            real!(initstate: fn(c_uint, *mut c_char, size_t) -> *mut c_char)(seed, statebuf, size)
        };
    };
    s.rngs.reseed_libc_rand(u64::from(seed));
    statebuf
}

/// glibc `setstate(3)`: a no-op under the shim (state is seed-derived).
///
/// # Safety
///
/// Arguments per the libc contract; `statebuf` is returned, never written.
#[no_mangle]
pub unsafe extern "C" fn setstate(statebuf: *mut c_char) -> *mut c_char {
    if shim().is_none() {
        // SAFETY: forwarding the caller's argument unchanged.
        return unsafe { real!(setstate: fn(*mut c_char) -> *mut c_char)(statebuf) };
    }
    statebuf
}

#[allow(clippy::cast_precision_loss)] // top 53 bits and 2^53 are exact in f64
fn next_f64(bits: u64) -> f64 {
    let mantissa = (bits >> 11) as f64;
    mantissa * (1.0 / (1u64 << 53) as f64)
}

/// `drand48(3)`: uniform double in `[0, 1)`.
///
/// # Safety
///
/// Always safe; declared unsafe only to match the interposed C ABI.
#[no_mangle]
pub unsafe extern "C" fn drand48() -> f64 {
    let Some(s) = shim() else {
        // SAFETY: no arguments to forward.
        return unsafe { real!(drand48: fn() -> f64)() };
    };
    next_f64(s.rngs.next_u64(Domain::LibcRand))
}

/// `lrand48(3)`: uniform in `[0, 2^31)`.
///
/// # Safety
///
/// Always safe; declared unsafe only to match the interposed C ABI.
#[no_mangle]
pub unsafe extern "C" fn lrand48() -> c_long {
    let Some(s) = shim() else {
        // SAFETY: no arguments to forward.
        return unsafe { real!(lrand48: fn() -> c_long)() };
    };
    #[allow(clippy::cast_possible_wrap)] // 31-bit mask fits any c_long
    let v = (s.rngs.next_u64(Domain::LibcRand) & RAND_31_MASK) as c_long;
    v
}

/// `mrand48(3)`: uniform signed 32-bit.
///
/// # Safety
///
/// Always safe; declared unsafe only to match the interposed C ABI.
#[no_mangle]
pub unsafe extern "C" fn mrand48() -> c_long {
    let Some(s) = shim() else {
        // SAFETY: no arguments to forward.
        return unsafe { real!(mrand48: fn() -> c_long)() };
    };
    #[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)] // want i32 range
    let v = c_long::from(s.rngs.next_u64(Domain::LibcRand) as u32 as i32);
    v
}

/// `srand48(3)`: reseeds the libc-rand stream, mixed with the run seed.
///
/// # Safety
///
/// Always safe; declared unsafe only to match the interposed C ABI.
#[no_mangle]
pub unsafe extern "C" fn srand48(seedval: c_long) {
    let Some(s) = shim() else {
        // SAFETY: forwarding the caller's argument unchanged.
        return unsafe { real!(srand48: fn(c_long) -> ())(seedval) };
    };
    #[allow(clippy::cast_sign_loss)] // reinterpreting seed bits is intended
    s.rngs.reseed_libc_rand(seedval as u64);
}

/// Advance a caller-owned 48-bit `xsubi` state, mixed with the run seed;
/// returns the 64-bit value backing this step's output.
fn step_xsubi(xsubi: *mut c_ushort, run_seed: u64) -> u64 {
    // SAFETY: xsubi points to an unsigned short[3] per the libc contract.
    let parts = unsafe { core::slice::from_raw_parts_mut(xsubi, 3) };
    let state = u64::from(parts[0]) | (u64::from(parts[1]) << 16) | (u64::from(parts[2]) << 32);
    let mut mix = state ^ run_seed.rotate_left(23);
    let out = splitmix64(&mut mix);
    #[allow(clippy::cast_possible_truncation)] // deliberate 16-bit slicing
    {
        parts[0] = out as c_ushort;
        parts[1] = (out >> 16) as c_ushort;
        parts[2] = (out >> 32) as c_ushort;
    }
    out
}

/// `erand48(3)`: uniform double from caller-owned state.
///
/// # Safety
///
/// `xsubi` must point to `unsigned short[3]`, per the libc contract.
#[no_mangle]
pub unsafe extern "C" fn erand48(xsubi: *mut c_ushort) -> f64 {
    let Some(s) = shim() else {
        // SAFETY: forwarding the caller's argument unchanged.
        return unsafe { real!(erand48: fn(*mut c_ushort) -> f64)(xsubi) };
    };
    next_f64(step_xsubi(xsubi, s.seed))
}

/// `nrand48(3)`: uniform `[0, 2^31)` from caller-owned state.
///
/// # Safety
///
/// `xsubi` must point to `unsigned short[3]`, per the libc contract.
#[no_mangle]
pub unsafe extern "C" fn nrand48(xsubi: *mut c_ushort) -> c_long {
    let Some(s) = shim() else {
        // SAFETY: forwarding the caller's argument unchanged.
        return unsafe { real!(nrand48: fn(*mut c_ushort) -> c_long)(xsubi) };
    };
    #[allow(clippy::cast_possible_wrap)] // 31-bit mask fits any c_long
    let v = (step_xsubi(xsubi, s.seed) & RAND_31_MASK) as c_long;
    v
}

/// `jrand48(3)`: uniform signed 32-bit from caller-owned state.
///
/// # Safety
///
/// `xsubi` must point to `unsigned short[3]`, per the libc contract.
#[no_mangle]
pub unsafe extern "C" fn jrand48(xsubi: *mut c_ushort) -> c_long {
    let Some(s) = shim() else {
        // SAFETY: forwarding the caller's argument unchanged.
        return unsafe { real!(jrand48: fn(*mut c_ushort) -> c_long)(xsubi) };
    };
    #[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)] // want i32 range
    let v = c_long::from(step_xsubi(xsubi, s.seed) as u32 as i32);
    v
}

/// `getrandom(2)`: fills the buffer from the `GetRandom` stream. Never
/// blocks, never returns short.
///
/// # Safety
///
/// `buf` must be valid for writes of `buflen` bytes, per the syscall contract.
#[no_mangle]
pub unsafe extern "C" fn getrandom(buf: *mut c_void, buflen: size_t, flags: c_uint) -> ssize_t {
    // Reentrant path: our own machinery (e.g. a `HashMap` seeding its keys, or
    // Rust std initializing a thread-local) must get real entropy and must not
    // re-enter `shim()` while it is initializing.
    if crate::sched::is_reentrant() {
        // SAFETY: forwarding the caller's arguments unchanged.
        return unsafe {
            real!(getrandom: fn(*mut c_void, size_t, c_uint) -> ssize_t)(buf, buflen, flags)
        };
    }
    let Some(s) = shim() else {
        // SAFETY: forwarding the caller's arguments unchanged.
        return unsafe {
            real!(getrandom: fn(*mut c_void, size_t, c_uint) -> ssize_t)(buf, buflen, flags)
        };
    };
    if buf.is_null() {
        // SAFETY: setting errno through libc's thread-local errno location.
        unsafe { *libc::__errno_location() = libc::EFAULT };
        return -1;
    }
    #[allow(clippy::cast_sign_loss)] // isize::MAX is positive
    let len = buflen.min(isize::MAX as usize);
    // SAFETY: buf is non-null and valid for `len` writes per the contract.
    let slice = unsafe { core::slice::from_raw_parts_mut(buf.cast::<MaybeUninit<u8>>(), len) };
    s.rngs.fill_uninit(Domain::GetRandom, slice);
    shim_trace!(s, "getrandom(len={len}) -> deterministic");
    #[allow(clippy::cast_possible_wrap)] // capped to isize::MAX above
    let out = len as ssize_t;
    out
}

/// `getentropy(3)`: like the real one, rejects requests over 256 bytes.
///
/// # Safety
///
/// `buf` must be valid for writes of `length` bytes, per the libc contract.
#[no_mangle]
pub unsafe extern "C" fn getentropy(buf: *mut c_void, length: size_t) -> c_int {
    let Some(s) = shim() else {
        // SAFETY: forwarding the caller's arguments unchanged.
        return unsafe { real!(getentropy: fn(*mut c_void, size_t) -> c_int)(buf, length) };
    };
    if length > 256 || buf.is_null() {
        // SAFETY: setting errno through libc's thread-local errno location.
        unsafe {
            *libc::__errno_location() = if buf.is_null() {
                libc::EFAULT
            } else {
                libc::EIO
            }
        };
        return -1;
    }
    // SAFETY: buf is non-null and valid for `length` writes per the contract.
    let slice = unsafe { core::slice::from_raw_parts_mut(buf.cast::<MaybeUninit<u8>>(), length) };
    s.rngs.fill_uninit(Domain::GetRandom, slice);
    0
}
