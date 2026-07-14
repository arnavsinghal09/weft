//! The seeded network fault model.
//!
//! # Determinism principle
//!
//! Every datagram's fate — dropped or not, and if not, its delivery delay — is
//! a pure function of `(run seed, source, destination, per-channel sequence
//! number)`. It is **not** drawn from a shared stream in broker-arrival order,
//! so it does not depend on how the OS happened to schedule the sending
//! processes. The k-th datagram on the channel A→B always meets the same fate,
//! which is what makes a network-triggered bug reproducible from a seed.
//!
//! Reordering is not a separate knob: it emerges naturally when a latency
//! distribution with variance (`Uniform`, `Exponential`) gives a later-sent
//! datagram a smaller delivery time than an earlier one — exactly how real
//! networks reorder.

use weft_abi::splitmix64;

use crate::wire::VAddr;

/// A latency distribution. All values are in nanoseconds of virtual delay.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Latency {
    /// Constant delay.
    Fixed(u64),
    /// Uniform in `[lo, hi]`.
    Uniform { lo: u64, hi: u64 },
    /// Exponential with the given mean (heavy tail → occasional big spikes and
    /// lots of reordering; a realistic model of a congested path).
    Exponential { mean: u64 },
}

impl Latency {
    /// The smallest delay this distribution can produce — the lookahead
    /// `L_min` the windowed multi-host protocol needs (a send admitted now
    /// cannot deliver before `send_vt + L_min`). `Exponential` can produce an
    /// arbitrarily small delay, so its floor is 0.
    #[must_use]
    pub fn min_ns(self) -> u64 {
        match self {
            Self::Fixed(d) => d,
            Self::Uniform { lo, .. } => lo,
            Self::Exponential { .. } => 0,
        }
    }

    /// Sample a delay from a uniform deviate `u` in `[0, 1)`.
    #[must_use]
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::cast_precision_loss
    )]
    pub fn sample(self, u: f64) -> u64 {
        match self {
            Self::Fixed(d) => d,
            Self::Uniform { lo, hi } => {
                let span = hi.saturating_sub(lo);
                lo + (span as f64 * u) as u64
            }
            Self::Exponential { mean } => {
                // Inverse-CDF: -mean * ln(1 - u). Clamp the tail so a deviate
                // very close to 1 can't produce an absurd delay.
                let x = -(mean as f64) * (1.0 - u).ln();
                (x as u64).min(mean.saturating_mul(20))
            }
        }
    }
}

/// A network partition: a list of node groups. Two nodes can exchange traffic
/// only if they fall in the same group. Nodes not named in any group share one
/// implicit "rest" group. An empty partition blocks nothing.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Partition {
    groups: Vec<Vec<u32>>,
}

impl Partition {
    #[must_use]
    pub fn none() -> Self {
        Self { groups: Vec::new() }
    }

    #[must_use]
    pub fn from_groups(groups: Vec<Vec<u32>>) -> Self {
        Self { groups }
    }

    /// The index of `node`'s group, or `None` for the implicit "rest" group.
    fn group_of(&self, node: u32) -> Option<usize> {
        self.groups.iter().position(|g| g.contains(&node))
    }

    /// Whether traffic from node `a` to node `b` is blocked by this partition.
    #[must_use]
    pub fn blocked(&self, a: u32, b: u32) -> bool {
        if self.groups.is_empty() {
            return false;
        }
        self.group_of(a) != self.group_of(b)
    }
}

/// The complete fault model applied by the broker.
#[derive(Clone, Debug)]
pub struct FaultModel {
    pub seed: u64,
    pub latency: Latency,
    /// Independent per-datagram loss probability in `[0, 1]`.
    pub loss: f64,
    /// Bandwidth cap in bytes/second; `0` means unlimited. Modeled as a
    /// per-datagram serialization delay of `len / rate` (a deliberate
    /// simplification of true queuing — see docs/network-model.md).
    pub bandwidth_bps: u64,
    pub partition: Partition,
}

/// What the model decides for one datagram.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Fate {
    pub dropped: bool,
    /// Virtual nanoseconds between send and delivery (0 if dropped).
    pub delay_ns: u64,
}

impl FaultModel {
    #[must_use]
    pub fn reliable(seed: u64) -> Self {
        Self {
            seed,
            latency: Latency::Fixed(0),
            loss: 0.0,
            bandwidth_bps: 0,
            partition: Partition::none(),
        }
    }

    /// The lookahead `L_min` (ns): the least virtual delay any non-dropped
    /// datagram can incur. Bandwidth only adds delay, so latency's floor is
    /// the model's floor. Used by the windowed multi-host sequencer.
    #[must_use]
    pub fn min_delay_ns(&self) -> u64 {
        self.latency.min_ns()
    }

    /// Decide the fate of the `seq`-th datagram on the channel `src`→`dst`,
    /// carrying `len` bytes. Deterministic in all of its inputs.
    #[must_use]
    #[allow(clippy::cast_precision_loss)]
    pub fn fate(&self, src: VAddr, dst: VAddr, seq: u64, len: usize) -> Fate {
        if self.partition.blocked(src.node_of(), dst.node_of()) {
            return Fate {
                dropped: true,
                delay_ns: 0,
            };
        }
        let mut state = channel_stream(self.seed, src, dst, seq);
        if unit(&mut state) < self.loss {
            return Fate {
                dropped: true,
                delay_ns: 0,
            };
        }
        let mut delay = self.latency.sample(unit(&mut state));
        if let Some(serialize) = (len as u64)
            .saturating_mul(1_000_000_000)
            .checked_div(self.bandwidth_bps)
        {
            delay = delay.saturating_add(serialize);
        }
        Fate {
            dropped: false,
            delay_ns: delay,
        }
    }
}

/// Derive an independent PRNG state from a datagram's identity.
fn channel_stream(seed: u64, src: VAddr, dst: VAddr, seq: u64) -> u64 {
    let mut x = seed ^ 0x51_7c_c1_b7_27_22_0a_95;
    for v in [
        u64::from(src.ip),
        u64::from(src.port),
        u64::from(dst.ip),
        u64::from(dst.port),
        seq,
    ] {
        x ^= v.wrapping_mul(0x9E37_79B9_7F4A_7C15);
        x = splitmix64(&mut x);
    }
    x
}

/// Next uniform deviate in `[0, 1)` from `state`, using the top 53 bits.
#[allow(clippy::cast_precision_loss)] // top 53 bits are exactly representable
fn unit(state: &mut u64) -> f64 {
    let r = splitmix64(state);
    ((r >> 11) as f64) / ((1u64 << 53) as f64)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn addr(n: u8, p: u16) -> VAddr {
        VAddr::new(0x7f00_0000 | u32::from(n), p)
    }

    #[test]
    fn fate_is_deterministic_in_identity() {
        let m = FaultModel {
            seed: 42,
            latency: Latency::Uniform { lo: 1000, hi: 5000 },
            loss: 0.2,
            bandwidth_bps: 0,
            partition: Partition::none(),
        };
        let a = addr(1, 100);
        let b = addr(2, 200);
        for seq in 0..1000 {
            assert_eq!(m.fate(a, b, seq, 64), m.fate(a, b, seq, 64));
        }
    }

    #[test]
    fn different_seeds_change_fate() {
        let a = addr(1, 100);
        let b = addr(2, 200);
        let mk = |seed| FaultModel {
            seed,
            latency: Latency::Uniform { lo: 0, hi: 10_000 },
            loss: 0.3,
            bandwidth_bps: 0,
            partition: Partition::none(),
        };
        let one: Vec<_> = (0..200).map(|s| mk(1).fate(a, b, s, 64)).collect();
        let two: Vec<_> = (0..200).map(|s| mk(2).fate(a, b, s, 64)).collect();
        assert_ne!(one, two);
    }

    #[test]
    fn loss_probability_is_roughly_honored() {
        let a = addr(1, 100);
        let b = addr(2, 200);
        let m = FaultModel {
            seed: 7,
            latency: Latency::Fixed(0),
            loss: 0.25,
            bandwidth_bps: 0,
            partition: Partition::none(),
        };
        let dropped = (0..10_000).filter(|s| m.fate(a, b, *s, 64).dropped).count();
        // 25% of 10k with generous slack.
        assert!((2000..3000).contains(&dropped), "dropped={dropped}");
    }

    #[test]
    fn partition_blocks_across_groups_only() {
        let p = Partition::from_groups(vec![vec![0, 1]]);
        assert!(!p.blocked(0, 1)); // same group
        assert!(!p.blocked(2, 3)); // both in "rest"
        assert!(p.blocked(0, 2)); // across
        assert!(p.blocked(1, 3));
    }

    #[test]
    fn bandwidth_adds_serialization_delay() {
        let m = FaultModel {
            seed: 1,
            latency: Latency::Fixed(1000),
            loss: 0.0,
            bandwidth_bps: 1_000_000, // 1 MB/s → 1000 bytes = 1ms = 1_000_000ns
            partition: Partition::none(),
        };
        let f = m.fate(addr(1, 1), addr(2, 2), 0, 1000);
        assert!(!f.dropped);
        assert_eq!(f.delay_ns, 1000 + 1_000_000);
    }
}
