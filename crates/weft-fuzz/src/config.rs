//! The `weft fuzz` config file: one JSON document instead of fifteen flags.
//!
//! Everything has a sensible default except `net` — a fuzz run with no fault
//! model finds nothing, so it must be stated. Field reference in
//! docs/fuzzing.md; CLI flags override the file.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::gen::Workload;

fn default_seed_count() -> u64 {
    1000
}
fn default_jobs() -> usize {
    std::thread::available_parallelism().map_or(4, std::num::NonZeroUsize::get)
}
fn default_invariants() -> Vec<String> {
    vec!["fifo".into(), "dup".into()]
}
fn default_out_dir() -> PathBuf {
    PathBuf::from("weft-fuzz-out")
}
fn default_true() -> bool {
    true
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkloadConfig {
    #[serde(default = "two")]
    pub nodes: u32,
    #[serde(default = "twenty_four")]
    pub sends: u32,
    #[serde(default = "four")]
    pub payload_len: usize,
    #[serde(default)]
    pub workload_seed: u64,
}
fn two() -> u32 {
    2
}
fn twenty_four() -> u32 {
    24
}
fn four() -> usize {
    4
}

impl Default for WorkloadConfig {
    fn default() -> Self {
        Self {
            nodes: 2,
            sends: 24,
            payload_len: 4,
            workload_seed: 0,
        }
    }
}

impl WorkloadConfig {
    #[must_use]
    pub fn resolve(&self) -> Workload {
        Workload {
            nodes: self.nodes,
            sends: self.sends,
            payload_len: self.payload_len,
            workload_seed: self.workload_seed,
        }
    }
}

/// The fuzz run description (`weft fuzz --config <file>`).
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FuzzConfig {
    /// Free-form comment slot (`"//"` in the JSON); ignored by the tool.
    #[serde(rename = "//", default, skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
    /// Network-condition spec (`weft_net::config` syntax). Required: a fuzz
    /// run needs a fault model to explore.
    pub net: String,
    /// First fault seed to try.
    #[serde(default)]
    pub seed_start: u64,
    /// How many consecutive seeds to sweep from `seed_start`.
    #[serde(default = "default_seed_count")]
    pub seed_count: u64,
    /// Worker threads.
    #[serde(default = "default_jobs")]
    pub jobs: usize,
    /// Wall-clock budget in seconds; 0 = no budget (run all seeds).
    #[serde(default)]
    pub time_budget_secs: u64,
    /// Invariants to check: "fifo" (per-channel-fifo) and/or "dup"
    /// (no-duplicate-delivery).
    #[serde(default = "default_invariants")]
    pub invariants: Vec<String>,
    #[serde(default)]
    pub workload: WorkloadConfig,
    /// Where shrunk reproducer logs and the report land.
    #[serde(default = "default_out_dir")]
    pub out_dir: PathBuf,
    /// Shrink the first occurrence of each distinct violation.
    #[serde(default = "default_true")]
    pub shrink: bool,
    /// Seeds that previously failed (the regression file's content): always
    /// checked first, before the sweep, so CI catches regressions
    /// immediately even under a tight time budget.
    #[serde(default)]
    pub regression_seeds: Vec<u64>,
}

impl FuzzConfig {
    /// Parse and validate a config document.
    ///
    /// # Errors
    /// A message naming the offending field.
    pub fn from_json(text: &str) -> Result<Self, String> {
        let cfg: Self = serde_json::from_str(text).map_err(|e| format!("config: {e}"))?;
        cfg.validate()?;
        Ok(cfg)
    }

    /// Load from a file.
    ///
    /// # Errors
    /// I/O or validation errors, as a message.
    pub fn load(path: &std::path::Path) -> Result<Self, String> {
        let text =
            std::fs::read_to_string(path).map_err(|e| format!("config {}: {e}", path.display()))?;
        Self::from_json(&text)
    }

    fn validate(&self) -> Result<(), String> {
        weft_net::config::parse(0, &self.net).map_err(|e| format!("config net: {e}"))?;
        if self.seed_count == 0 {
            return Err("config: seed_count must be at least 1".into());
        }
        if self.jobs == 0 {
            return Err("config: jobs must be at least 1".into());
        }
        if self.invariants.is_empty() {
            return Err("config: at least one invariant is required (fifo, dup)".into());
        }
        for inv in &self.invariants {
            if !matches!(
                inv.as_str(),
                "fifo" | "per-channel-fifo" | "dup" | "no-duplicate-delivery"
            ) {
                return Err(format!(
                    "config: unknown invariant {inv:?} (known: fifo, dup)"
                ));
            }
        }
        if self.workload.nodes < 2 {
            return Err("config: workload.nodes must be at least 2".into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn minimal_config_gets_defaults() {
        let cfg = FuzzConfig::from_json(r#"{"net":"latency=uniform:0-5000"}"#).unwrap();
        assert_eq!(cfg.seed_count, 1000);
        assert_eq!(cfg.invariants, vec!["fifo", "dup"]);
        assert!(cfg.shrink);
        assert_eq!(cfg.workload.nodes, 2);
    }

    #[test]
    fn bad_fields_are_named() {
        assert!(FuzzConfig::from_json(r#"{"net":"latency=bogus"}"#)
            .unwrap_err()
            .contains("net"));
        assert!(FuzzConfig::from_json(r#"{"net":"","invariants":["nope"]}"#)
            .unwrap_err()
            .contains("nope"));
        // Typos are rejected, not ignored.
        assert!(FuzzConfig::from_json(r#"{"net":"","seedcount":5}"#)
            .unwrap_err()
            .contains("seedcount"));
    }
}
