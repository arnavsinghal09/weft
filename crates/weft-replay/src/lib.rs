//! Recording, deterministic replay, and invariant checking (Phase 5).
//!
//! # What gets recorded, and why it is minimal
//!
//! In a Weft run every source of nondeterminism except one is already a pure
//! function of the seed: datagram fates, virtual time, PRNG output, and the
//! managed-thread schedule are all recomputable. The one input that is not is
//! the **broker linearization order** — the order in which requests from
//! different OS-scheduled processes acquire the broker's state lock. That
//! order decides tie assignment, channel sequence interleaving, and every
//! send-vs-recv race the application can observe.
//!
//! A `weft-log` therefore records exactly that: the linearized sequence of
//! broker boundary operations (with payloads), plus the run header (seed and
//! network spec). Replay re-executes the sequence against the same seeded
//! fault model and must reproduce a byte-identical event stream — on any
//! machine, independent of its clock, thread timing, or entropy. See
//! docs/recording-format.md for the wire-level specification.
//!
//! This crate runs in the `weft` CLI / broker process only, never inside
//! target processes, so it is free to allocate and use serde.

pub mod hash;
pub mod invariant;
pub mod log;
pub mod recorder;
pub mod replay;
pub mod report;

pub use invariant::{Invariant, Monitor, ViolationRecord};
pub use log::{Event, Header, Log, LogError, LogWriter, Record};
pub use recorder::Recorder;
pub use replay::{replay_log, ReplayOutcome};
