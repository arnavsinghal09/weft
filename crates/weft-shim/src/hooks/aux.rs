//! `getauxval(3)` interposition: `AT_RANDOM` — the 16 random bytes the kernel
//! hands every process, used by some runtimes for hash seeding and stack
//! canaries consumed before `main` — is answered with seed-derived bytes.

use libc::c_ulong;

use crate::real::real;
use crate::state::shim;

/// # Safety
///
/// Always safe; declared unsafe only to match the interposed C ABI. The
/// returned `AT_RANDOM` pointer stays valid for the process lifetime (it
/// points into shim-owned static state).
#[no_mangle]
pub unsafe extern "C" fn getauxval(type_: c_ulong) -> c_ulong {
    if let Some(s) = shim() {
        if type_ == libc::AT_RANDOM {
            return s.rngs.aux_random().as_ptr() as c_ulong;
        }
    }
    // SAFETY: forwarding the caller's argument unchanged.
    unsafe { real!(getauxval: fn(c_ulong) -> c_ulong)(type_) }
}
