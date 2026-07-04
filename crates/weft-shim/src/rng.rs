//! Deterministic randomness: one ChaCha8 stream per interception domain.
//!
//! # Why ChaCha8 (and not something else)
//!
//! - **Named, published algorithm** (Bernstein's ChaCha, 8 rounds), used via
//!   `rand_chacha` — no hand-rolled generator anywhere.
//! - **Statistical quality**: indistinguishable-from-random by construction;
//!   passes PractRand/TestU01 with margin. No fuzzing campaign will trip over
//!   generator artifacts the way it could with an LCG or a weak xorshift.
//! - **Speed**: multiple GB/s in software. Every draw here already crosses a
//!   function-call interception boundary, so the generator is never the
//!   bottleneck (measured: see docs/architecture.md overhead numbers).
//! - **Sub-streams for free**: ChaCha keys a 64-bit *stream* counter
//!   independent of position, so each [`Domain`] gets a provably independent
//!   sequence from the same 32-byte key — no ad-hoc seed surgery, and adding
//!   draws in one domain never shifts values seen by another.
//!
//! xoshiro256++ was the runner-up (faster) but sub-stream construction via
//! `jump()` is clumsier and its quality margin is smaller; speed is not the
//! constraint here.

use core::mem::MaybeUninit;
use std::sync::Mutex;

use rand_core::{RngCore, SeedableRng};
use rand_chacha::ChaCha8Rng;
use weft_abi::{expand_seed, splitmix64, Domain};

fn stream_for(key: [u8; 32], domain: Domain) -> ChaCha8Rng {
    let mut rng = ChaCha8Rng::from_seed(key);
    rng.set_stream(domain as u64);
    rng
}

/// Stream-id base for per-FILE `/dev/urandom` substreams. Chosen far above
/// the fixed [`Domain`] ids (0..=4) so a substream can never collide with a
/// domain stream regardless of how many files a program opens.
const DEV_FILE_STREAM_BASE: u64 = 0x1000_0000;

/// Fill possibly-uninitialized memory from `rng` via a small stack chunk,
/// avoiding a zero-init pass over caller memory in the hot path. Shared by
/// [`Domains::fill_uninit`] and [`DevFileRng::fill_uninit`].
fn fill_uninit_from(rng: &mut ChaCha8Rng, buf: &mut [MaybeUninit<u8>]) {
    let mut chunk = [0u8; 512];
    let mut off = 0;
    while off < buf.len() {
        let n = (buf.len() - off).min(chunk.len());
        rng.fill_bytes(&mut chunk[..n]);
        for (dst, src) in buf[off..off + n].iter_mut().zip(&chunk[..n]) {
            dst.write(*src);
        }
        off += n;
    }
}

/// An independent `/dev/urandom` substream owned by a single `fopen`ed
/// `FILE`. Kept behind its own `Mutex` so concurrent `fread`s on the same
/// stream — and glibc stdio's internal read-ahead — are data-race free
/// without relying on stdio's own `FILE` lock for our state.
#[derive(Debug)]
pub struct DevFileRng(Mutex<ChaCha8Rng>);

impl DevFileRng {
    /// Fill possibly-uninitialized memory from this file's substream.
    ///
    /// # Panics
    ///
    /// Only if the lock is poisoned, which cannot happen: the critical
    /// section performs no panicking operations.
    pub fn fill_uninit(&self, buf: &mut [MaybeUninit<u8>]) {
        let mut rng = self.0.lock().unwrap();
        fill_uninit_from(&mut rng, buf);
    }
}

/// All PRNG state for one process. Each domain sits behind its own `Mutex` so
/// heavy `/dev/urandom` traffic never contends with `rand()` calls, and so a
/// draw is a single short critical section (no allocation inside).
#[derive(Debug)]
pub struct Domains {
    seed: u64,
    libc_rand: Mutex<ChaCha8Rng>,
    getrandom: Mutex<ChaCha8Rng>,
    dev_random: Mutex<ChaCha8Rng>,
    /// The 16 bytes `getauxval(AT_RANDOM)` points at, fixed at init.
    aux_random: [u8; 16],
}

impl Domains {
    #[must_use]
    pub fn new(seed: u64) -> Self {
        let key = expand_seed(seed);
        let mut aux_rng = stream_for(key, Domain::AuxRandom);
        let mut aux_random = [0u8; 16];
        aux_rng.fill_bytes(&mut aux_random);
        Self {
            seed,
            libc_rand: Mutex::new(stream_for(key, Domain::LibcRand)),
            getrandom: Mutex::new(stream_for(key, Domain::GetRandom)),
            dev_random: Mutex::new(stream_for(key, Domain::DevRandom)),
            aux_random,
        }
    }

    /// Seed-derived offset (seconds) for the virtual realtime clock base.
    #[must_use]
    pub fn clock_offset_secs(seed: u64) -> u64 {
        let key = expand_seed(seed);
        stream_for(key, Domain::ClockOffset).next_u64()
    }

    fn lock_of(&self, domain: Domain) -> &Mutex<ChaCha8Rng> {
        match domain {
            Domain::LibcRand => &self.libc_rand,
            Domain::GetRandom => &self.getrandom,
            // AuxRandom/ClockOffset/Scheduler don't draw from these locks
            // (they're consumed once at init or owned by the scheduler);
            // route any stray request to the dev stream.
            Domain::DevRandom | Domain::AuxRandom | Domain::ClockOffset | Domain::Scheduler => {
                &self.dev_random
            }
        }
    }

    /// Build the scheduler's own independent ChaCha8 stream from the run seed.
    /// Owned by the [`crate::sched::Scheduler`], so it lives outside the
    /// domain locks above.
    #[must_use]
    pub fn scheduler_stream(seed: u64) -> ChaCha8Rng {
        stream_for(expand_seed(seed), Domain::Scheduler)
    }

    /// Next 64 random bits from a domain stream.
    ///
    /// # Panics
    ///
    /// Panics only if a thread previously panicked while holding this domain's
    /// lock, which cannot happen: the critical section performs no panicking
    /// operations.
    pub fn next_u64(&self, domain: Domain) -> u64 {
        self.lock_of(domain).lock().unwrap().next_u64()
    }

    /// Fill `buf` from a domain stream.
    ///
    /// # Panics
    ///
    /// Same non-condition as [`Self::next_u64`].
    pub fn fill(&self, domain: Domain, buf: &mut [u8]) {
        self.lock_of(domain).lock().unwrap().fill_bytes(buf);
    }

    /// Fill possibly-uninitialized memory (what the `read`/`getrandom` hooks
    /// receive) from a domain stream, without a zero-init pass over caller
    /// memory in the hot path.
    ///
    /// # Panics
    ///
    /// Same non-condition as [`Self::next_u64`].
    pub fn fill_uninit(&self, domain: Domain, buf: &mut [MaybeUninit<u8>]) {
        let mut rng = self.lock_of(domain).lock().unwrap();
        fill_uninit_from(&mut rng, buf);
    }

    /// Construct an independent substream for the `index`-th `fopen`ed random
    /// device in this process (open order). Each `FILE` gets its own stream,
    /// so glibc stdio read-ahead only draws further into *that file's* own
    /// sequence — the bytes it discards at `fclose` are then a deterministic
    /// function of the open index, not of how threads interleave on a shared
    /// stream (which is what made the buffered `fopen` path nondeterministic).
    #[must_use]
    pub fn dev_file_stream(&self, index: u64) -> DevFileRng {
        let key = expand_seed(self.seed);
        let mut rng = ChaCha8Rng::from_seed(key);
        rng.set_stream(DEV_FILE_STREAM_BASE + index);
        DevFileRng(Mutex::new(rng))
    }

    /// Reset the `LibcRand` stream, mixing a user-supplied seed (`srand`,
    /// `srandom`, `srand48`) with the run seed. Same program seed + same run
    /// seed ⇒ same sequence; changing either changes the sequence.
    ///
    /// # Panics
    ///
    /// Same non-condition as [`Self::next_u64`].
    pub fn reseed_libc_rand(&self, user_seed: u64) {
        let mut mix = self.seed ^ user_seed.wrapping_mul(0x9E37_79B9_7F4A_7C15);
        let mut state = splitmix64(&mut mix);
        let key = expand_seed(splitmix64(&mut state));
        *self.libc_rand.lock().unwrap() = stream_for(key, Domain::LibcRand);
    }

    /// The fixed `AT_RANDOM` bytes for this process.
    #[must_use]
    pub fn aux_random(&self) -> &[u8; 16] {
        &self.aux_random
    }
}

/// Deterministic replacement for `rand_r`-style callers: advances the
/// caller-owned 32-bit state and returns the next value in `[0, 2^31)`.
/// Keyed by both the caller state and the run seed, so different `--seed`
/// runs diverge even for `rand_r`.
#[must_use]
pub fn rand_r_step(state: u32, run_seed: u64) -> (u32, i32) {
    let mut s = u64::from(state) ^ run_seed.rotate_left(17);
    let out = splitmix64(&mut s);
    #[allow(clippy::cast_possible_truncation)] // deliberate 32-bit fold
    let next_state = (out >> 32) as u32 ^ out as u32;
    #[allow(clippy::cast_possible_wrap)] // masked to 31 bits, never negative
    let value = (out & 0x7FFF_FFFF) as i32;
    (next_state, value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn domains_are_independent() {
        let a = Domains::new(7);
        let b = Domains::new(7);
        // Drawing from one domain must not move another.
        let _ = a.next_u64(Domain::GetRandom);
        assert_eq!(a.next_u64(Domain::LibcRand), b.next_u64(Domain::LibcRand));
    }

    #[test]
    fn same_seed_same_stream_different_seed_different_stream() {
        let a = Domains::new(1);
        let b = Domains::new(1);
        let c = Domains::new(2);
        let (first, repeat, other) = (
            a.next_u64(Domain::GetRandom),
            b.next_u64(Domain::GetRandom),
            c.next_u64(Domain::GetRandom),
        );
        assert_eq!(first, repeat);
        assert_ne!(first, other);
    }

    #[test]
    fn reseed_is_deterministic_and_mixes_run_seed() {
        let a = Domains::new(1);
        let b = Domains::new(1);
        let c = Domains::new(2);
        a.reseed_libc_rand(99);
        b.reseed_libc_rand(99);
        c.reseed_libc_rand(99);
        let (first, repeat, other) = (
            a.next_u64(Domain::LibcRand),
            b.next_u64(Domain::LibcRand),
            c.next_u64(Domain::LibcRand),
        );
        assert_eq!(first, repeat);
        assert_ne!(first, other); // same srand() arg, different --seed ⇒ different
    }

    #[test]
    fn fill_uninit_matches_fill() {
        let a = Domains::new(3);
        let b = Domains::new(3);
        let mut plain = [0u8; 1000];
        a.fill(Domain::DevRandom, &mut plain);
        let mut uninit = [MaybeUninit::<u8>::uninit(); 1000];
        b.fill_uninit(Domain::DevRandom, &mut uninit);
        // SAFETY: fill_uninit wrote every element of `uninit`.
        let written =
            unsafe { core::slice::from_raw_parts(uninit.as_ptr().cast::<u8>(), uninit.len()) };
        assert_eq!(plain, written);
    }

    #[test]
    fn concurrent_draws_form_the_same_multiset() {
        // Thread interleaving may hand different values to different threads,
        // but the *set* of values drawn from one stream must be a prefix of
        // the sequential stream — Phase 1's cross-thread guarantee.
        use std::sync::Arc;
        const PER_THREAD: usize = 20_000;
        const THREADS: usize = 8;
        let d = Arc::new(Domains::new(42));
        let mut handles = Vec::new();
        for _ in 0..THREADS {
            let d = Arc::clone(&d);
            handles.push(std::thread::spawn(move || {
                (0..PER_THREAD)
                    .map(|_| d.next_u64(Domain::GetRandom))
                    .collect::<Vec<_>>()
            }));
        }
        let mut drawn: Vec<u64> = handles
            .into_iter()
            .flat_map(|h| h.join().unwrap())
            .collect();
        let reference = Domains::new(42);
        let mut expected: Vec<u64> = (0..THREADS * PER_THREAD)
            .map(|_| reference.next_u64(Domain::GetRandom))
            .collect();
        drawn.sort_unstable();
        expected.sort_unstable();
        assert_eq!(drawn, expected);
    }

    #[test]
    fn dev_file_substreams_reproduce_by_index_and_diverge_across_indices() {
        let fill = |d: &Domains, idx: u64| {
            let s = d.dev_file_stream(idx);
            let mut buf = [MaybeUninit::<u8>::uninit(); 256];
            s.fill_uninit(&mut buf);
            // SAFETY: fill_uninit wrote every element.
            unsafe { core::slice::from_raw_parts(buf.as_ptr().cast::<u8>(), buf.len()) }.to_vec()
        };
        let a = Domains::new(42);
        let b = Domains::new(42);
        let c = Domains::new(7);
        // Same seed + same index ⇒ identical bytes.
        assert_eq!(fill(&a, 0), fill(&b, 0));
        // Same seed, different index ⇒ independent (different) bytes.
        assert_ne!(fill(&a, 0), fill(&a, 1));
        // Different seed, same index ⇒ different bytes.
        assert_ne!(fill(&a, 0), fill(&c, 0));
        // A substream must not collide with the shared DevRandom domain.
        let mut dom = [MaybeUninit::<u8>::uninit(); 256];
        a.fill_uninit(Domain::DevRandom, &mut dom);
        // SAFETY: fill_uninit wrote every element.
        let dom = unsafe { core::slice::from_raw_parts(dom.as_ptr().cast::<u8>(), dom.len()) };
        assert_ne!(fill(&b, 0), dom);
    }

    #[test]
    fn rand_r_step_is_seed_sensitive() {
        let (s1, v1) = rand_r_step(123, 1);
        let (s2, v2) = rand_r_step(123, 1);
        let (_, v3) = rand_r_step(123, 2);
        assert_eq!((s1, v1), (s2, v2));
        assert_ne!(v1, v3);
        assert!(v1 >= 0);
    }
}
