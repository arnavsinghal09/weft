//! Weft's network simulation layer: the wire protocol between a node's shim
//! and the broker, the seeded fault model, and the broker itself.
//!
//! Multiple target processes ("nodes") send UDP-style datagrams that are
//! intercepted by the shim ([`weft_shim`](../weft_shim/index.html)) and routed
//! here instead of to the kernel network stack, so that latency, loss,
//! reordering, partitions, and bandwidth are entirely under seed-driven
//! control. See `docs/network-model.md` for what is simulated vs. simplified.

pub mod broker;
pub mod config;
pub mod core;
pub mod fault;
pub mod window;
pub mod wire;

pub use broker::Broker;
pub use fault::{FaultModel, Latency, Partition};
pub use window::{SeqError, SeqSend, WindowSequencer};
pub use wire::{FromBroker, ToBroker, VAddr};
