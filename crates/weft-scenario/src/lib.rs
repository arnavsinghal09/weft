//! Scenario DSL for deterministic fault injection testing.
//!
//! A scenario describes a distributed system test with timed faults:
//! network latency/loss, file I/O faults (torn writes, fsync-lies, ENOSPC),
//! and process crashes/restarts. The scenario parser validates inputs strictly
//! and returns clear error messages.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

mod latency;
mod parse;

pub use latency::LatencyDistribution;
pub use parse::{parse_scenario, ScenarioError};

/// A complete fault scenario: processes, network faults, file I/O faults, events.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Scenario {
    /// Human-readable name and description.
    pub name: String,
    pub description: Option<String>,

    /// Random seed for fault injection.
    pub seed: u64,

    /// Nodes (processes) in the scenario.
    pub nodes: Vec<Node>,

    /// Network fault configuration.
    pub network: Option<NetworkFaults>,

    /// Per-node file I/O fault configuration.
    pub filesystem: Option<HashMap<usize, FileSystemFaults>>,

    /// Per-node clock skew (additional offset from global timeline).
    pub time_skew: Option<HashMap<usize, i64>>,

    /// Scheduled events: crashes, restarts, fault activations.
    #[serde(default)]
    pub events: Vec<ScheduledEvent>,
}

/// A process node in the scenario.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Node {
    pub node_id: usize,
    pub program: String,
    pub args: Vec<String>,
}

/// Network fault configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetworkFaults {
    /// Latency distribution: "fixed:N", "uniform:LO-HI", "exp:MEAN".
    pub latency: Option<String>,

    /// Loss probability: 0.0..1.0.
    pub loss: Option<f64>,

    /// Bandwidth cap: bytes per second.
    pub bandwidth: Option<u64>,

    /// Network partitions: "0+1|2" means {0,1} and {2} are isolated.
    pub partitions: Option<String>,
}

/// File system fault configuration (per-node).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileSystemFaults {
    /// Fsync returns success but doesn't persist writes.
    #[serde(default)]
    pub fsync_lies: bool,

    /// Simulate ENOSPC (no space) after N bytes written.
    pub enospc_after_bytes: Option<u64>,

    /// Probability of tearing writes (partially written on crash).
    pub torn_write_probability: Option<f64>,
}

/// A timed event: crash, restart, or fault activation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScheduledEvent {
    /// Time in nanoseconds (absolute from start).
    pub time_ns: u64,

    /// Action to perform.
    pub action: EventAction,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum EventAction {
    #[serde(rename = "crash")]
    Crash { node_id: usize },

    #[serde(rename = "start")]
    Start { node_id: usize },

    #[serde(rename = "activate_partition")]
    ActivatePartition { spec: String },

    #[serde(rename = "clear_partition")]
    ClearPartition,
}

impl Scenario {
    /// Parse a scenario from JSON. Returns detailed error messages on failure.
    ///
    /// # Errors
    /// Returns `ScenarioError` on malformed input or failed validation; see
    /// [`parse_scenario`].
    pub fn from_json(text: &str) -> Result<Self, ScenarioError> {
        parse_scenario(text)
    }

    /// Validate the scenario for internal consistency.
    ///
    /// # Errors
    /// Returns detailed error if:
    /// - Nodes are not sequentially numbered starting at 0.
    /// - Events reference non-existent node IDs.
    /// - Time skew or filesystem config reference non-existent nodes.
    /// - Network faults are malformed.
    pub fn validate(&self) -> Result<(), ScenarioError> {
        let node_ids: Vec<usize> = self.nodes.iter().map(|n| n.node_id).collect();

        // Check sequential numbering.
        for (i, &id) in node_ids.iter().enumerate() {
            if id != i {
                return Err(ScenarioError::InvalidNodeId(
                    id,
                    format!("nodes must be sequentially numbered; expected {i}, got {id}"),
                ));
            }
        }

        // Check events reference valid nodes.
        for event in &self.events {
            let node_id = match &event.action {
                EventAction::Crash { node_id } | EventAction::Start { node_id } => *node_id,
                _ => continue,
            };
            if node_id >= self.nodes.len() {
                return Err(ScenarioError::InvalidNodeId(
                    node_id,
                    format!(
                        "event references node {}, but only {} nodes exist",
                        node_id,
                        self.nodes.len()
                    ),
                ));
            }
        }

        // Check filesystem and time_skew references.
        if let Some(fs) = &self.filesystem {
            for &node_id in fs.keys() {
                if node_id >= self.nodes.len() {
                    return Err(ScenarioError::InvalidNodeId(
                        node_id,
                        format!(
                            "filesystem faults for node {}, but only {} nodes exist",
                            node_id,
                            self.nodes.len()
                        ),
                    ));
                }
            }
        }

        if let Some(skew) = &self.time_skew {
            for &node_id in skew.keys() {
                if node_id >= self.nodes.len() {
                    return Err(ScenarioError::InvalidNodeId(
                        node_id,
                        format!(
                            "time skew for node {}, but only {} nodes exist",
                            node_id,
                            self.nodes.len()
                        ),
                    ));
                }
            }
        }

        Ok(())
    }
}
