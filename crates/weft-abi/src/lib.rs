//! Shared contract between the `weft` orchestrator (CLI) and `weft-shim`
//! (the `LD_PRELOAD` library loaded into target processes).
//!
//! Everything here must stay dependency-free and allocation-free: the shim
//! links this crate into arbitrary user processes.

#![no_std]

/// Environment variable carrying the run seed (decimal or `0x`-prefixed hex
/// `u64`). Its presence is what activates the shim; without it every hook is
/// a transparent passthrough to the real libc.
pub const ENV_SEED: &str = "WEFT_SEED";

/// Environment variable enabling per-call tracing to stderr (`"1"` enables).
pub const ENV_TRACE: &str = "WEFT_TRACE";

/// Environment variable overriding the path to the shim shared object,
/// checked by `weft run` before its built-in search.
pub const ENV_SHIM: &str = "WEFT_SHIM";

/// Environment variable selecting the scheduler interleaving strategy:
/// `"random"` (default) or `"rr"` (round-robin with perturbation). See
/// [`Strategy`].
pub const ENV_STRATEGY: &str = "WEFT_STRATEGY";

/// Environment variable that disables deterministic thread scheduling when set
/// to `"0"` or `"off"`. Time and randomness stay deterministic; thread
/// interleaving is left to the OS. Useful for isolating time/RNG behavior, and
/// for programs where only those need to be reproducible.
pub const ENV_SCHED: &str = "WEFT_SCHED";

/// Environment variable (`"1"`) making the shim print scheduler statistics
/// (thread count, scheduling decisions, distinct yield-point sites) to stderr
/// at process exit.
pub const ENV_SCHED_STATS: &str = "WEFT_SCHED_STATS";

/// Environment variable holding the path to the broker's Unix-domain socket.
/// Its presence activates network interception in the shim; without it,
/// socket calls pass through to the real kernel network stack.
pub const ENV_BROKER: &str = "WEFT_BROKER";

/// Environment variable holding this process's node index in the simulated
/// cluster (decimal `u32`). Used to label traffic and evaluate partitions.
pub const ENV_NODE_ID: &str = "WEFT_NODE_ID";

/// Environment variable holding the network-condition spec (see
/// `weft_net::config`); consumed by the broker, not the shim.
pub const ENV_NET: &str = "WEFT_NET";

/// Interleaving-selection strategy for the deterministic scheduler.
///
/// Both are fully deterministic functions of the run seed; they differ in
/// *how* they pick the next thread among those currently able to run:
///
/// - [`Strategy::Random`] draws uniformly from the enabled set at every
///   scheduling point. It explores the interleaving space most aggressively,
///   which finds concurrency bugs faster — the right default for fuzzing.
/// - [`Strategy::RoundRobin`] rotates through threads in a fixed order,
///   applying only occasional seed-driven perturbations. Runs are far easier
///   for a human to follow when debugging a specific failure, at the cost of
///   exploring fewer distinct interleavings per seed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Strategy {
    /// Uniform random selection among enabled threads (default).
    #[default]
    Random,
    /// Round-robin rotation with occasional seed-driven perturbation.
    RoundRobin,
}

impl Strategy {
    /// Parse a strategy name (`"random"` or `"rr"`/`"round-robin"`),
    /// case-insensitively.
    ///
    /// # Errors
    ///
    /// Returns `Err` with a static description for any unrecognized name.
    pub fn parse(s: &str) -> Result<Self, &'static str> {
        match s.trim() {
            "random" | "rand" => Ok(Self::Random),
            "rr" | "round-robin" | "roundrobin" => Ok(Self::RoundRobin),
            _ => Err("unknown strategy (expected 'random' or 'rr')"),
        }
    }

    /// The canonical name, as accepted by [`Strategy::parse`].
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Random => "random",
            Self::RoundRobin => "rr",
        }
    }
}

/// ChaCha8 sub-stream identifiers, one per interception domain.
///
/// Each domain draws from an independent PRNG stream so that, e.g., adding a
/// `getrandom` call to a program does not shift the values its `rand()` calls
/// see. Stream separation is native to ChaCha (64-bit stream counter), so
/// this is not ad-hoc key mangling.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u64)]
pub enum Domain {
    /// `rand`/`random`/`drand48` families.
    LibcRand = 0,
    /// `getrandom(2)` and `getentropy(3)`.
    GetRandom = 1,
    /// Reads from `/dev/urandom` and `/dev/random`.
    DevRandom = 2,
    /// `getauxval(AT_RANDOM)` — the 16 bytes the kernel hands every process.
    AuxRandom = 3,
    /// Seed-derived offset applied to the virtual realtime clock base.
    ClockOffset = 4,
    /// Deterministic scheduler's interleaving-selection stream.
    Scheduler = 5,
}

/// One step of SplitMix64 (Steele, Lea, Flood 2014). Used only to expand the
/// user's 64-bit seed into ChaCha key material and to mix user-supplied
/// reseeds (`srand`, `srand48`) with the run seed — never as an output PRNG.
#[inline]
#[must_use]
pub fn splitmix64(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// Expand a 64-bit seed into a 32-byte ChaCha8 key via four SplitMix64 steps.
#[must_use]
pub fn expand_seed(seed: u64) -> [u8; 32] {
    let mut state = seed;
    let mut key = [0u8; 32];
    for chunk in key.chunks_exact_mut(8) {
        chunk.copy_from_slice(&splitmix64(&mut state).to_le_bytes());
    }
    key
}

/// Parse the `WEFT_SEED` value: decimal, or hex with a `0x`/`0X` prefix.
///
/// # Errors
///
/// Returns `Err` with a static description if the string is empty or not a
/// valid `u64` in either form.
pub fn parse_seed(s: &str) -> Result<u64, &'static str> {
    let s = s.trim();
    if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        u64::from_str_radix(hex, 16).map_err(|_| "invalid hex seed")
    } else if s.is_empty() {
        Err("empty seed")
    } else {
        s.parse().map_err(|_| "invalid decimal seed")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seed_parsing() {
        assert_eq!(parse_seed("42"), Ok(42));
        assert_eq!(parse_seed("0xff"), Ok(255));
        assert_eq!(parse_seed("0XFF"), Ok(255));
        assert_eq!(parse_seed(" 7 "), Ok(7));
        assert!(parse_seed("").is_err());
        assert!(parse_seed("nope").is_err());
        assert!(parse_seed("0x").is_err());
    }

    #[test]
    fn expand_seed_is_deterministic_and_seed_sensitive() {
        assert_eq!(expand_seed(1), expand_seed(1));
        assert_ne!(expand_seed(1), expand_seed(2));
        assert_ne!(expand_seed(0), [0u8; 32]);
    }

    #[test]
    fn splitmix_reference_vector() {
        // Reference value for seed 0 from the published SplitMix64 algorithm.
        let mut s = 0u64;
        assert_eq!(splitmix64(&mut s), 0xE220_A839_7B1D_CDAF);
    }
}
