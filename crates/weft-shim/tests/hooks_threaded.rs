//! In-process concurrency tests for the hook layer, designed to run under
//! AddressSanitizer and ThreadSanitizer in CI: many threads calling the
//! actual `extern "C"` hook entry points at once.
//!
//! This test binary links the shim as an rlib, so its `#[no_mangle]` symbols
//! also interpose libc *for this binary* — which is exactly what we want to
//! stress. `WEFT_SEED` is set through a `Once` before any hook runs.

#![cfg(target_os = "linux")]

use std::mem::MaybeUninit;
use std::sync::Once;

/// Process constructor: guarantees `WEFT_SEED`/`WEFT_SCHED` are in the
/// environment before *anything* in this process reads it.
///
/// An `.init_array` constructor is not early enough on every glibc: measured
/// directly (ordered stderr writes from inside `state::init()` and from this
/// ctor), some libc-internal bootstrap step — before `.init_array` is
/// processed at all — already calls our interposed `clock_gettime` (observed
/// as the very first hook entry of the process, ahead of even
/// `pthread_create`). That first call drives the shim's lazy `state::init()`;
/// finding no `WEFT_SEED`, it caches the shim as *inactive* permanently (a
/// `OnceLock`, single-shot by design — matching production, where the seed is
/// fixed at `exec()` and never changes mid-run). Every hook then falls
/// through to the real monotonic clock, whose rapid consecutive reads can be
/// equal and break the strict-increase assertion below.
///
/// A ctor running inside this process is therefore too late in general. The
/// only point guaranteed to precede *any* code — including libc's own
/// bootstrap — is before `execve()`. So if the seed isn't visible yet, this
/// ctor re-execs the binary with it added to the environment; the freshly
/// exec'd process sees `WEFT_SEED` from its very first instruction, exactly
/// like a real `weft run` target. This is also why plain
/// `std::env::args_os()`/`current_exe()` aren't used to rebuild the exec:
/// Rust's own argv capture is *itself* a same-priority `.init_array` entry
/// that may not have run yet either, so argv/exe path are read directly from
/// procfs.
#[used]
#[cfg_attr(target_os = "linux", link_section = ".init_array.00000")]
static SEED_ENV_CTOR: extern "C" fn() = {
    extern "C" fn ctor() {
        use std::os::unix::process::CommandExt;

        // std::env::var/set_var read/write the process environ directly (no
        // Rust-side lazy cache), so — unlike argv — this is safe this early;
        // verified empirically (see the doc comment above).
        if std::env::var(weft_abi::ENV_SEED).is_ok() {
            return; // already seeded: this is the re-exec'd instance.
        }
        let exe_path = std::fs::read_link("/proc/self/exe").expect("read /proc/self/exe");
        let cmdline = std::fs::read("/proc/self/cmdline").expect("read /proc/self/cmdline");
        // NUL-separated argv, dropping the trailing empty split and arg0
        // (Command::new below supplies its own).
        let args: Vec<&std::ffi::OsStr> = cmdline
            .split(|&b| b == 0)
            .skip(1)
            .filter(|a| !a.is_empty())
            .map(std::os::unix::ffi::OsStrExt::from_bytes)
            .collect();
        // SAFETY: single-threaded (pre-.init_array/pre-main); Command::exec
        // replaces this process image, so nothing here observes a
        // partially-mutated environment.
        let err = std::process::Command::new(&exe_path)
            .args(&args)
            .env(weft_abi::ENV_SEED, "42")
            .env(weft_abi::ENV_SCHED, "0")
            .exec();
        panic!("re-exec with WEFT_SEED set failed: {err}");
    }
    ctor
};

fn ensure_seeded() {
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        // Safe here: runs before any other thread can be inside a hook
        // (every test path goes through this Once first).
        std::env::set_var(weft_abi::ENV_SEED, "42");
        // This suite validates the *engine's* thread-safety under genuine
        // concurrency, so it disables deterministic scheduling — otherwise the
        // scheduler would serialize these threads (and, since it is a
        // process-global singleton, collide with the binary's other parallel
        // tests). Scheduling itself is covered by tests/sched_harness.rs.
        std::env::set_var(weft_abi::ENV_SCHED, "0");
    });
}

fn now_mono_ns() -> u64 {
    let mut ts = MaybeUninit::<libc::timespec>::uninit();
    // SAFETY: valid out-pointer; this resolves to the shim's clock_gettime.
    let rc =
        unsafe { weft_shim::hooks::time::clock_gettime(libc::CLOCK_MONOTONIC, ts.as_mut_ptr()) };
    assert_eq!(rc, 0);
    // SAFETY: hook returned 0, so the timespec was written.
    let ts = unsafe { ts.assume_init() };
    u64::try_from(ts.tv_sec).unwrap() * 1_000_000_000 + u64::try_from(ts.tv_nsec).unwrap()
}

const THREADS: usize = 8;
const ITERS: usize = 20_000;

#[test]
fn hooks_survive_concurrent_hammering() {
    ensure_seeded();

    let handles: Vec<_> = (0..THREADS)
        .map(|_| {
            std::thread::spawn(|| {
                let mut last = 0;
                let mut buf = [0u8; 64];
                for i in 0..ITERS {
                    // Time: strictly increasing within a thread.
                    let now = now_mono_ns();
                    assert!(now > last, "clock went backwards: {last} -> {now}");
                    last = now;

                    // rand(): in range.
                    // SAFETY: no arguments; hook entry point.
                    let r = unsafe { weft_shim::hooks::rand::rand() };
                    assert!(r >= 0);

                    // getrandom: fills the buffer.
                    if i % 16 == 0 {
                        buf.fill(0);
                        // SAFETY: valid buffer pointer/length pair.
                        let n = unsafe {
                            weft_shim::hooks::rand::getrandom(buf.as_mut_ptr().cast(), buf.len(), 0)
                        };
                        assert_eq!(n, 64);
                        assert_ne!(buf, [0u8; 64], "getrandom produced all zeros");
                    }

                    // Virtual sleep: cheap and must be race-free.
                    if i % 64 == 0 {
                        let req = libc::timespec {
                            tv_sec: 0,
                            tv_nsec: 100,
                        };
                        // SAFETY: valid req pointer, null rem is allowed.
                        let rc = unsafe {
                            weft_shim::hooks::time::nanosleep(&raw const req, std::ptr::null_mut())
                        };
                        assert_eq!(rc, 0);
                    }
                }
            })
        })
        .collect();
    for h in handles {
        h.join().unwrap();
    }
}

#[test]
fn dev_urandom_open_read_close_concurrently() {
    ensure_seeded();
    let handles: Vec<_> = (0..4)
        .map(|_| {
            std::thread::spawn(|| {
                for _ in 0..500 {
                    // SAFETY: valid path literal; O_RDONLY needs no mode.
                    let fd = unsafe {
                        weft_shim::hooks::dev::open(c"/dev/urandom".as_ptr(), libc::O_RDONLY, 0)
                    };
                    assert!(fd >= 0);
                    let mut buf = [0u8; 32];
                    // SAFETY: valid fd and buffer.
                    let n = unsafe {
                        weft_shim::hooks::dev::read(fd, buf.as_mut_ptr().cast(), buf.len())
                    };
                    assert_eq!(n, 32);
                    // SAFETY: closing the fd we opened.
                    let rc = unsafe { weft_shim::hooks::dev::close(fd) };
                    assert_eq!(rc, 0);
                }
            })
        })
        .collect();
    for h in handles {
        h.join().unwrap();
    }
}

#[test]
fn seeded_draws_are_reproducible_at_engine_level() {
    ensure_seeded();
    // The hook-level global stream is shared across tests, so reproducibility
    // is asserted on fresh engine instances (what the seed fully determines).
    let a = weft_shim::rng::Domains::new(42);
    let b = weft_shim::rng::Domains::new(42);
    for _ in 0..1000 {
        assert_eq!(
            a.next_u64(weft_abi::Domain::GetRandom),
            b.next_u64(weft_abi::Domain::GetRandom)
        );
    }
}
