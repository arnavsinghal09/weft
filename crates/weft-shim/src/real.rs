//! Resolution of the *real* libc functions the hooks shadow, via
//! `dlsym(RTLD_NEXT, ..)`, cached per call site in an `AtomicPtr`.

use core::ffi::c_void;
use core::sync::atomic::{AtomicPtr, Ordering};

/// Resolve `name` (a NUL-terminated byte literal) to the next occurrence of
/// the symbol after this library, caching the result.
///
/// Aborts the process if the symbol cannot be resolved: every name we look up
/// is a libc symbol that must exist, and returning a null function pointer
/// would be immediate undefined behavior at the call site.
pub fn resolve(name: &'static [u8], cache: &AtomicPtr<c_void>) -> *mut c_void {
    debug_assert_eq!(name.last(), Some(&0), "symbol name must be NUL-terminated");
    let cached = cache.load(Ordering::Relaxed);
    if !cached.is_null() {
        return cached;
    }
    // `dlsym` may take libc-internal locks; fence so any interposed pthread
    // call it makes takes the passthrough path rather than re-entering the
    // scheduler.
    let _g = crate::sched::Reentrancy::enter();
    // SAFETY: `name` is a NUL-terminated C string literal.
    let sym = unsafe { libc::dlsym(libc::RTLD_NEXT, name.as_ptr().cast()) };
    if sym.is_null() {
        crate::trace::raw_stderr("[weft] FATAL: dlsym(RTLD_NEXT) failed for a libc symbol\n");
        // SAFETY: abort is always safe to call; unresolvable libc symbols
        // mean the process is unusable under interposition.
        unsafe { libc::abort() };
    }
    cache.store(sym, Ordering::Relaxed);
    sym
}

/// Expands to a callable `unsafe extern "C" fn` pointer for the real libc
/// `$name`, with per-call-site caching.
///
/// Safety contract (discharged by the caller's `unsafe` block, which always
/// wraps both this expansion and the call): the declared signature must match
/// the libc prototype exactly. Non-nullness is guaranteed by `resolve`, which
/// aborts rather than return null.
macro_rules! real {
    ($name:ident : fn($($arg:ty),* $(,)?) -> $ret:ty) => {{
        static CACHE: core::sync::atomic::AtomicPtr<core::ffi::c_void> =
            core::sync::atomic::AtomicPtr::new(core::ptr::null_mut());
        let sym = $crate::real::resolve(
            concat!(stringify!($name), "\0").as_bytes(),
            &CACHE,
        );
        core::mem::transmute::<
            *mut core::ffi::c_void,
            unsafe extern "C" fn($($arg),*) -> $ret,
        >(sym)
    }};
}
pub(crate) use real;
