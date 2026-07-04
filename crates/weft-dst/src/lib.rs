//! Weft: deterministic simulation testing for unmodified Linux binaries.
//!
//! This crate is the CLI and orchestrator. The runtime interception shim,
//! deterministic scheduler, network simulator, fault engine, and record/replay
//! system live in sibling crates under `crates/` as they are built out
//! (see `PROJECT_NOTES.md` at the repository root for the full layout).

pub mod orchestrator;
pub mod run;

/// Weft's own version, as baked in at compile time.
#[must_use]
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

#[cfg(test)]
mod tests {
    #[test]
    fn version_matches_manifest() {
        assert_eq!(super::version(), env!("CARGO_PKG_VERSION"));
    }
}
