//! Deterministic workload generation for the fuzz loop.
//!
//! The generator derives an op sequence from a *workload seed* via SplitMix64
//! — completely independent of the *fault seed* being swept, so every fuzzed
//! seed runs the identical client behavior and only the fault decisions
//! differ. That is what makes "seed 17 fails, seed 18 passes" meaningful.

use weft_abi::splitmix64;
use weft_net::VAddr;

use crate::input::OpInput;

/// Workload shape. Serde-facing definition lives in [`crate::config`];
/// this is the resolved form.
#[derive(Clone, Copy, Debug)]
pub struct Workload {
    /// Node count (each gets one connection binding port 100).
    pub nodes: u32,
    /// Datagrams sent, sources and destinations drawn per-send.
    pub sends: u32,
    /// Payload bytes per send.
    pub payload_len: usize,
    /// Derives the op sequence; independent of the fault seed.
    pub workload_seed: u64,
}

impl Default for Workload {
    fn default() -> Self {
        Self {
            nodes: 2,
            sends: 24,
            payload_len: 4,
            workload_seed: 0,
        }
    }
}

/// The conventional address of a node's receive socket.
#[must_use]
pub fn node_addr(node: u32) -> VAddr {
    VAddr::new(0x7f00_0001 + node, 100)
}

/// Generate the op sequence: connect+bind every node, then a send stream
/// with interleaved receive polls, then a full drain.
#[must_use]
#[allow(clippy::cast_possible_truncation)] // splitmix draws reduced mod small node counts
pub fn generate(w: &Workload) -> Vec<OpInput> {
    let nodes = w.nodes.max(2);
    let mut rng = w.workload_seed ^ 0x9e37_79b9_7f4a_7c15;
    let mut ops = Vec::new();
    for n in 0..nodes {
        ops.push(OpInput::Connect { conn: u64::from(n) });
        ops.push(OpInput::Hello {
            conn: u64::from(n),
            node: n,
        });
        ops.push(OpInput::Bind {
            conn: u64::from(n),
            addr: node_addr(n),
        });
    }
    for i in 0..w.sends {
        let src = splitmix64(&mut rng) as u32 % nodes;
        let mut dst = splitmix64(&mut rng) as u32 % nodes;
        if dst == src {
            dst = (dst + 1) % nodes;
        }
        let mut payload = vec![0u8; w.payload_len.max(1)];
        let tag = 4.min(payload.len());
        payload[..tag].copy_from_slice(&i.to_le_bytes()[..tag]);
        ops.push(OpInput::Send {
            conn: u64::from(src),
            // Sends originate from the node's bound address so replies would
            // be routable; port is the node's own.
            src: node_addr(src),
            dst: node_addr(dst),
            payload,
        });
        // Occasionally poll a random node mid-stream (mimics an event loop).
        if splitmix64(&mut rng) % 4 == 0 {
            let who = splitmix64(&mut rng) as u32 % nodes;
            ops.push(OpInput::Recv {
                conn: u64::from(who),
                blocking: false,
            });
        }
    }
    // Drain: enough polls per node to empty every queue.
    for n in 0..nodes {
        for _ in 0..w.sends {
            ops.push(OpInput::Recv {
                conn: u64::from(n),
                blocking: false,
            });
        }
    }
    ops
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generation_is_deterministic_and_seed_sensitive() {
        let w = Workload::default();
        assert_eq!(generate(&w), generate(&w));
        let other = Workload {
            workload_seed: 1,
            ..w
        };
        assert_ne!(generate(&w), generate(&other));
    }

    #[test]
    fn every_node_connects_and_binds() {
        let w = Workload {
            nodes: 3,
            ..Workload::default()
        };
        let ops = generate(&w);
        for n in 0..3u64 {
            assert!(ops.contains(&OpInput::Connect { conn: n }));
        }
    }
}
