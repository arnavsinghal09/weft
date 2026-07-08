//! Scenario parsing and validation with detailed error messages.

use crate::{LatencyDistribution, NetworkFaults, Scenario};
use thiserror::Error;

/// Detailed scenario parsing or validation error.
#[derive(Error, Debug)]
pub enum ScenarioError {
    #[error("JSON/YAML parse error: {0}")]
    ParseError(String),

    #[error("Invalid node ID {0}: {1}")]
    InvalidNodeId(usize, String),

    #[error("Invalid latency spec '{0}': {1}")]
    InvalidLatency(String, String),

    #[error("Invalid loss probability {0}: must be in [0.0, 1.0]")]
    InvalidLossProbability(f64),

    #[error("Invalid bandwidth {0}: must be > 0")]
    InvalidBandwidth(u64),

    #[error("Invalid partition spec '{0}': {1}")]
    InvalidPartition(String, String),

    #[error("Missing required field: {0}")]
    MissingField(String),

    #[error("Invalid scenario: {0}")]
    InvalidScenario(String),
}

/// Parse a scenario from YAML or JSON text (currently accepts JSON format).
/// For now, the scenario format is JSON; YAML support is future work.
pub fn parse_scenario_yaml(text: &str) -> Result<Scenario, ScenarioError> {
    parse_scenario(text)
}

/// Parse a scenario from JSON text.
pub fn parse_scenario(text: &str) -> Result<Scenario, ScenarioError> {
    let mut scenario: Scenario =
        serde_json::from_str(text).map_err(|e| ScenarioError::ParseError(e.to_string()))?;

    // Validate network faults if present.
    if let Some(net) = &scenario.network {
        validate_network_faults(net)?;
    }

    // Validate filesystem faults if present.
    if let Some(fs) = &scenario.filesystem {
        for (node_id, faults) in fs {
            validate_filesystem_faults(*node_id, faults)?;
        }
    }

    // Validate and sort events.
    scenario.events.sort_by_key(|e| e.time_ns);

    scenario.validate()?;
    Ok(scenario)
}

fn validate_network_faults(faults: &NetworkFaults) -> Result<(), ScenarioError> {
    if let Some(latency) = &faults.latency {
        LatencyDistribution::parse(latency)?;
    }

    if let Some(loss) = faults.loss {
        if !(0.0..=1.0).contains(&loss) {
            return Err(ScenarioError::InvalidLossProbability(loss));
        }
    }

    if let Some(bw) = faults.bandwidth {
        if bw == 0 {
            return Err(ScenarioError::InvalidBandwidth(bw));
        }
    }

    // Basic partition format check: "0+1|2+3" etc.
    if let Some(partition) = &faults.partitions {
        validate_partition_spec(partition)?;
    }

    Ok(())
}

fn validate_filesystem_faults(
    _node_id: usize,
    faults: &crate::FileSystemFaults,
) -> Result<(), ScenarioError> {
    if let Some(prob) = faults.torn_write_probability {
        if !(0.0..=1.0).contains(&prob) {
            return Err(ScenarioError::InvalidLossProbability(prob));
        }
    }
    Ok(())
}

fn validate_partition_spec(spec: &str) -> Result<(), ScenarioError> {
    let spec = spec.trim();
    if spec.is_empty() {
        return Ok(()); // Empty partition clears all partitions.
    }

    for group in spec.split('|') {
        if group.is_empty() {
            return Err(ScenarioError::InvalidPartition(
                spec.to_string(),
                "empty group (consecutive '|' or leading '|')".to_string(),
            ));
        }

        for node_str in group.split('+') {
            if node_str.is_empty() {
                return Err(ScenarioError::InvalidPartition(
                    spec.to_string(),
                    "empty node ID in group".to_string(),
                ));
            }
            if node_str.parse::<usize>().is_err() {
                return Err(ScenarioError::InvalidPartition(
                    spec.to_string(),
                    format!("'{}' is not a valid node ID", node_str),
                ));
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_minimal_scenario() {
        let json =
            r#"{"name":"test","seed":42,"nodes":[{"node_id":0,"program":"./prog","args":[]}]}"#;
        let scenario = parse_scenario_yaml(json).unwrap();
        assert_eq!(scenario.name, "test");
        assert_eq!(scenario.seed, 42);
        assert_eq!(scenario.nodes.len(), 1);
    }

    #[test]
    fn parse_with_network_faults() {
        let json = r#"{"name":"net-test","seed":42,"nodes":[{"node_id":0,"program":"./prog","args":[]}],"network":{"latency":"uniform:100-1000","loss":0.1}}"#;
        let scenario = parse_scenario_yaml(json).unwrap();
        assert!(scenario.network.is_some());
    }

    #[test]
    fn invalid_loss_probability() {
        let json = r#"{"name":"test","seed":42,"nodes":[{"node_id":0,"program":"./prog","args":[]}],"network":{"loss":1.5}}"#;
        assert!(parse_scenario_yaml(json).is_err());
    }

    #[test]
    fn test_partition_spec_validation() {
        // Valid partitions
        assert!(super::validate_partition_spec("0+1|2").is_ok());
        assert!(super::validate_partition_spec("0|1|2").is_ok());
        assert!(super::validate_partition_spec("").is_ok()); // empty clears partitions

        // Invalid partitions
        assert!(super::validate_partition_spec("|0").is_err()); // leading |
        assert!(super::validate_partition_spec("0||1").is_err()); // consecutive |
        assert!(super::validate_partition_spec("0+|1").is_err()); // empty node
        assert!(super::validate_partition_spec("0+x").is_err()); // non-numeric node
    }
}
