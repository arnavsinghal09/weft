//! Comprehensive tests for scenario parser: robustness on valid, invalid, and edge-case inputs.
//!
//! Goal: parser never panics, always returns clear errors.

use weft_scenario::parse_scenario_yaml;

#[test]
fn parse_minimal_valid_scenario() {
    let json = r#"{
  "name": "test",
  "seed": 42,
  "nodes": [
    {"node_id": 0, "program": "./prog", "args": []}
  ]
}"#;
    let result = parse_scenario_yaml(json);
    assert!(result.is_ok());
    let scenario = result.unwrap();
    assert_eq!(scenario.name, "test");
    assert_eq!(scenario.seed, 42);
    assert_eq!(scenario.nodes.len(), 1);
}

#[test]
fn parse_multi_node_scenario() {
    let json = r#"{
  "name": "3-nodes",
  "seed": 100,
  "nodes": [
    {"node_id": 0, "program": "./writer", "args": ["--verbose"]},
    {"node_id": 1, "program": "./replica", "args": []},
    {"node_id": 2, "program": "./replica", "args": []}
  ]
}"#;
    let result = parse_scenario_yaml(json);
    assert!(result.is_ok());
    let scenario = result.unwrap();
    assert_eq!(scenario.nodes.len(), 3);
}

#[test]
fn parse_with_network_faults() {
    let json = r#"{
  "name": "with-net",
  "seed": 42,
  "nodes": [{"node_id": 0, "program": "./prog", "args": []}],
  "network": {"latency": "uniform:100-1000", "loss": 0.1}
}"#;
    let result = parse_scenario_yaml(json);
    assert!(result.is_ok());
    let scenario = result.unwrap();
    assert!(scenario.network.is_some());
}

#[test]
fn parse_with_events() {
    let json = r#"{
  "name": "with-events",
  "seed": 42,
  "nodes": [{"node_id": 0, "program": "./prog", "args": []}],
  "events": [
    {"time_ns": 1000, "action": {"type": "crash", "node_id": 0}},
    {"time_ns": 5000, "action": {"type": "start", "node_id": 0}}
  ]
}"#;
    let result = parse_scenario_yaml(json);
    assert!(result.is_ok());
    let scenario = result.unwrap();
    assert_eq!(scenario.events.len(), 2);
    assert!(scenario.events[0].time_ns < scenario.events[1].time_ns);
}

#[test]
fn parse_with_filesystem_faults() {
    let json = r#"{
  "name": "with-fs",
  "seed": 42,
  "nodes": [{"node_id": 0, "program": "./prog", "args": []}],
  "filesystem": {
    "0": {"fsync_lies": true, "enospc_after_bytes": 1000000, "torn_write_probability": 0.05}
  }
}"#;
    let result = parse_scenario_yaml(json);
    assert!(result.is_ok());
}

#[test]
fn reject_invalid_loss_probability_above_one() {
    let json = r#"{
  "name": "bad-loss",
  "seed": 42,
  "nodes": [{"node_id": 0, "program": "./prog", "args": []}],
  "network": {"loss": 1.5}
}"#;
    let result = parse_scenario_yaml(json);
    assert!(result.is_err());
}

#[test]
fn reject_invalid_loss_probability_negative() {
    let json = r#"{
  "name": "bad-loss",
  "seed": 42,
  "nodes": [{"node_id": 0, "program": "./prog", "args": []}],
  "network": {"loss": -0.1}
}"#;
    let result = parse_scenario_yaml(json);
    assert!(result.is_err());
}

#[test]
fn reject_malformed_latency_fixed() {
    let json = r#"{
  "name": "bad-latency",
  "seed": 42,
  "nodes": [{"node_id": 0, "program": "./prog", "args": []}],
  "network": {"latency": "fixed:abc"}
}"#;
    let result = parse_scenario_yaml(json);
    assert!(result.is_err());
}

#[test]
fn reject_malformed_latency_uniform_inverted() {
    let json = r#"{
  "name": "bad-latency",
  "seed": 42,
  "nodes": [{"node_id": 0, "program": "./prog", "args": []}],
  "network": {"latency": "uniform:5000-100"}
}"#;
    let result = parse_scenario_yaml(json);
    assert!(result.is_err(), "lo > hi should be rejected");
}

#[test]
fn reject_malformed_latency_exponential_zero_mean() {
    let json = r#"{
  "name": "bad-latency",
  "seed": 42,
  "nodes": [{"node_id": 0, "program": "./prog", "args": []}],
  "network": {"latency": "exp:0"}
}"#;
    let result = parse_scenario_yaml(json);
    assert!(result.is_err(), "exp with mean=0 should be rejected");
}

#[test]
fn reject_unknown_latency_type() {
    let json = r#"{
  "name": "bad-latency",
  "seed": 42,
  "nodes": [{"node_id": 0, "program": "./prog", "args": []}],
  "network": {"latency": "foobar:123"}
}"#;
    let result = parse_scenario_yaml(json);
    assert!(result.is_err());
}

#[test]
fn reject_non_sequential_node_ids() {
    let json = r#"{
  "name": "bad-nodes",
  "seed": 42,
  "nodes": [
    {"node_id": 0, "program": "./prog", "args": []},
    {"node_id": 2, "program": "./prog", "args": []}
  ]
}"#;
    let result = parse_scenario_yaml(json);
    // Validation happens during parsing, so we expect error
    assert!(result.is_err());
}

#[test]
fn reject_event_referencing_nonexistent_node() {
    let json = r#"{
  "name": "bad-event",
  "seed": 42,
  "nodes": [{"node_id": 0, "program": "./prog", "args": []}],
  "events": [{"time_ns": 1000, "action": {"type": "crash", "node_id": 5}}]
}"#;
    let result = parse_scenario_yaml(json);
    assert!(result.is_err() || result.unwrap().validate().is_err());
}

#[test]
fn reject_filesystem_fault_for_nonexistent_node() {
    let json = r#"{
  "name": "bad-fs",
  "seed": 42,
  "nodes": [{"node_id": 0, "program": "./prog", "args": []}],
  "filesystem": {"10": {"fsync_lies": true}}
}"#;
    let result = parse_scenario_yaml(json);
    assert!(result.is_err() || result.unwrap().validate().is_err());
}

#[test]
fn reject_malformed_partition_spec_empty_group() {
    let json = r#"{
  "name": "bad-partition",
  "seed": 42,
  "nodes": [{"node_id": 0, "program": "./prog", "args": []}],
  "network": {"partitions": "|0"}
}"#;
    let result = parse_scenario_yaml(json);
    assert!(result.is_err());
}

#[test]
fn reject_malformed_partition_spec_non_numeric() {
    let json = r#"{
  "name": "bad-partition",
  "seed": 42,
  "nodes": [{"node_id": 0, "program": "./prog", "args": []}],
  "network": {"partitions": "0+x"}
}"#;
    let result = parse_scenario_yaml(json);
    assert!(result.is_err());
}

#[test]
fn accept_valid_partition_spec() {
    let json = r#"{
  "name": "partition",
  "seed": 42,
  "nodes": [
    {"node_id": 0, "program": "./prog", "args": []},
    {"node_id": 1, "program": "./prog", "args": []},
    {"node_id": 2, "program": "./prog", "args": []}
  ],
  "network": {"partitions": "0+1|2"}
}"#;
    let result = parse_scenario_yaml(json);
    assert!(result.is_ok());
}

#[test]
fn parser_never_panics_on_empty() {
    let _ = parse_scenario_yaml("");
}

#[test]
fn parser_never_panics_on_garbage() {
    let garbage = "!@#$%^&*()_+-=[]{}|;:',.<>?/`~";
    let _ = parse_scenario_yaml(garbage);
}

#[test]
fn parser_never_panics_on_huge_seed() {
    let json = r#"{
  "name": "test",
  "seed": 18446744073709551615,
  "nodes": [{"node_id": 0, "program": "./prog", "args": []}]
}"#;
    let _ = parse_scenario_yaml(json);
}

#[test]
fn parser_never_panics_on_many_nodes() {
    let mut json = String::from(r#"{"name":"many","seed":42,"nodes":["#);

    for i in 0..100 {
        if i > 0 {
            json.push(',');
        }
        json.push_str(&format!(r#"{{"node_id":{},"program":"p","args":[]}}"#, i));
    }

    json.push_str("]}");

    let result = parse_scenario_yaml(&json);
    if let Ok(scenario) = result {
        assert_eq!(scenario.nodes.len(), 100);
    }
}

#[test]
fn parser_never_panics_on_huge_event_list() {
    let mut json = String::from(
        r#"{"name":"many-events","seed":42,"nodes":[{"node_id":0,"program":"p","args":[]}],"events":["#,
    );

    for i in 0..1000 {
        if i > 0 {
            json.push(',');
        }
        json.push_str(&format!(
            r#"{{"time_ns":{},"action":{{"type":"crash","node_id":0}}}}"#,
            i * 1000
        ));
    }

    json.push_str("]}");

    let result = parse_scenario_yaml(&json);
    if let Ok(scenario) = result {
        assert_eq!(scenario.events.len(), 1000);
    }
}
