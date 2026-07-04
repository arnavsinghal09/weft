//! Process-global shim state: parsed once on the first intercepted call.

use std::sync::OnceLock;

use crate::rng::Domains;
use crate::sched::Scheduler;
use crate::vclock::VClock;

pub struct Shim {
    pub seed: u64,
    pub trace: bool,
    /// Whether deterministic thread scheduling is engaged. When false, pthread
    /// operations pass through to the OS (time/randomness stay deterministic).
    pub sched_enabled: bool,
    pub clock: VClock,
    pub rngs: Domains,
    pub sched: Scheduler,
}

static SHIM: OnceLock<Option<Shim>> = OnceLock::new();

/// The active shim, or `None` when `WEFT_SEED` is absent/invalid (every hook
/// then passes through to the real libc — the do-no-harm rule).
///
/// Initialized lazily on first use rather than in an ELF constructor: ctor
/// ordering across preloaded libraries is unspecified, while by the time any
/// interposed libc function is called, libc and our runtime are ready.
pub fn shim() -> Option<&'static Shim> {
    SHIM.get_or_init(init).as_ref()
}

fn init() -> Option<Shim> {
    // Constructing the scheduler allocates; without this fence a `malloc` here
    // would re-enter an interposed pthread hook, call `shim()`, and deadlock
    // this very `OnceLock` initialization.
    let _g = crate::sched::Reentrancy::enter();

    let seed_str = std::env::var(weft_abi::ENV_SEED).ok()?;
    let seed = match weft_abi::parse_seed(&seed_str) {
        Ok(s) => s,
        Err(e) => {
            // A malformed seed is a hard configuration error: silently
            // running nondeterministically would defeat the whole tool.
            // Report on stderr and stay inactive (passthrough).
            crate::trace::raw_stderr(&format!(
                "[weft] ERROR: {} {seed_str:?}: {e}; shim inactive\n",
                weft_abi::ENV_SEED
            ));
            return None;
        }
    };
    let trace = std::env::var(weft_abi::ENV_TRACE).is_ok_and(|v| v == "1");
    let strategy = match std::env::var(weft_abi::ENV_STRATEGY) {
        Ok(v) => match weft_abi::Strategy::parse(&v) {
            Ok(s) => s,
            Err(e) => {
                crate::trace::raw_stderr(&format!(
                    "[weft] WARNING: {} {v:?}: {e}; using default\n",
                    weft_abi::ENV_STRATEGY
                ));
                weft_abi::Strategy::default()
            }
        },
        Err(_) => weft_abi::Strategy::default(),
    };
    let stats = std::env::var(weft_abi::ENV_SCHED_STATS).is_ok_and(|v| v == "1");
    let sched_enabled = match std::env::var(weft_abi::ENV_SCHED) {
        Ok(v) => v != "0" && !v.eq_ignore_ascii_case("off"),
        Err(_) => true,
    };

    let clock = VClock::new(Domains::clock_offset_secs(seed));
    let rngs = Domains::new(seed);
    let sched = Scheduler::new(seed, strategy, stats);

    if trace {
        crate::trace::raw_stderr(&format!(
            "[weft] shim active, seed={seed}, strategy={}, scheduling={}\n",
            strategy.name(),
            if sched_enabled { "on" } else { "off" }
        ));
    }
    if stats {
        // SAFETY: registering an extern "C" handler with no captured state.
        unsafe { libc::atexit(stats_atexit) };
    }

    Some(Shim {
        seed,
        trace,
        sched_enabled,
        clock,
        rngs,
        sched,
    })
}

extern "C" fn stats_atexit() {
    if let Some(s) = shim() {
        s.sched.print_stats();
    }
}
