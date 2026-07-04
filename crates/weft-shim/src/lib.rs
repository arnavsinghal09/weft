//! `weft-shim`: the `LD_PRELOAD` library that makes time and randomness
//! deterministic in an unmodified target process.
//!
//! # Architecture
//!
//! Three layers, strictly separated:
//!
//! 1. **Engine** ([`vclock`], [`rng`], [`state`]) — pure, portable logic: a
//!    virtual clock and per-domain ChaCha8 streams derived from one 64-bit
//!    seed. Unit-testable on any OS, and what the sanitizer jobs hammer.
//! 2. **Hooks** ([`hooks`], Linux-only) — `#[no_mangle] extern "C"` functions
//!    with libc-identical signatures. Each one asks [`state::shim`] whether a
//!    seed is active: if yes, it answers from the engine; if no, it tail-calls
//!    the real libc function resolved via `dlsym(RTLD_NEXT, ..)` ([`real`]).
//!    This is the do-no-harm rule: without `WEFT_SEED`, a preloaded shim is
//!    behaviorally invisible.
//! 3. **Trace** ([`trace`]) — optional per-call logging straight to fd 2 with
//!    a stack buffer and a raw `write(2)`: no allocation, no stdio, safe to
//!    call from inside any hook.
//!
//! Seed flow: `weft run --seed N` sets `WEFT_SEED=N` and `LD_PRELOAD`. On the
//! first intercepted call, [`state`] parses the seed once (`OnceLock`),
//! expands it into a 32-byte ChaCha8 key ([`weft_abi::expand_seed`]), and
//! instantiates one ChaCha8 stream per [`weft_abi::Domain`] plus the virtual
//! clock (whose realtime base gets a seed-derived offset). Children inherit
//! both env vars across `fork`/`exec`, so process trees stay deterministic.

// Hook bodies are `unsafe fn`; require explicit unsafe blocks inside them so
// every unsafe operation carries its own SAFETY comment (workspace clippy
// denies undocumented blocks).
#![warn(unsafe_op_in_unsafe_fn)]

pub mod rng;
pub mod sched;
pub mod state;
pub mod trace;
pub mod vclock;

#[cfg(target_os = "linux")]
pub mod hooks;
#[cfg(target_os = "linux")]
pub mod real;
