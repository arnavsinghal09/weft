//! The interposed libc surface. Every function here is `#[no_mangle]
//! extern "C"` with a libc-identical ABI, follows the same shape —
//! *seed active? answer from the engine : tail-call the real function* —
//! and never allocates after first-call initialization.

pub mod aux;
pub mod dev;
pub mod file;
pub mod rand;
pub mod socket;
pub mod thread;
pub mod time;
