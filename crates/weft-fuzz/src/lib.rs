//! Seed fuzzing and failure shrinking (Phase 6).
//!
//! Phase 5 made a specific failure reproducible; this crate makes Weft *find*
//! failures and hand back the smallest reproducer:
//!
//! - [`gen`]: a deterministic workload generator (fixed client behavior,
//!   only the fault seed varies);
//! - [`input`]: op inputs + the pure executor that re-runs them through the
//!   broker's decision core;
//! - [`shrink`]: delta-debugging reduction to a 1-minimal reproducer that
//!   fails the same invariant on the same channel;
//! - [`fuzz`]: the seed-sweeping loop with parallelism, a time budget,
//!   per-violation shrinking, and a CI-friendly report;
//! - [`config`]: the JSON config file behind `weft fuzz --config`.
//!
//! Everything here is pure computation over `weft_net::core::Core` — no
//! sockets, no clock reads, no entropy — so results are identical across
//! machines and runs.

pub mod config;
pub mod fuzz;
pub mod gen;
pub mod input;
pub mod shrink;

pub use config::FuzzConfig;
pub use fuzz::{run_fuzz, Found, FuzzReport};
pub use input::{execute, execute_and_record, OpInput};
pub use shrink::{shrink, ShrinkStats, ViolationKey};
