//! Integration tests for process orchestration: crash/restart/partition events.

use std::sync::{Arc, Mutex};
use std::sync::atomic::AtomicU64;

use weft_dst::orchestrator::{NodeRegistry, spawn_scheduler};
use weft_scenario::Scenario;

#[test]
fn node_registry_tracks_state() {
    let scenario = Scenario {
        name: "test".to_string(),
        description: Some("test scenario".to_string()),
        seed: 42,
        nodes: vec![
            weft_scenario::Node {
                node_id: 0,
                program: "./prog".to_string(),
                args: vec![],
            },
            weft_scenario::Node {
                node_id: 1,
                program: "./prog".to_string(),
                args: vec![],
            },
        ],
        network: None,
        filesystem: Default::default(),
        time_skew: Default::default(),
        events: vec![],
    };

    let mut registry = NodeRegistry::new(&scenario);

    // Initially, nodes are idle
    assert_eq!(
        registry.state(0),
        Some(weft_dst::orchestrator::NodeStatus::Idle)
    );
    assert_eq!(registry.pid(0), None);

    // After setting running, state and pid are tracked
    registry.set_running(0, 1234);
    assert_eq!(
        registry.state(0),
        Some(weft_dst::orchestrator::NodeStatus::Running)
    );
    assert_eq!(registry.pid(0), Some(1234));

    // After crash, state changes
    registry.set_crashed(0);
    assert_eq!(
        registry.state(0),
        Some(weft_dst::orchestrator::NodeStatus::Crashed)
    );
    assert_eq!(registry.pid(0), Some(1234)); // PID persists (for cleanup)
}

#[test]
fn event_scheduler_executes_on_time() {
    // Create a simple scenario with one crash event at time 1000ns
    let scenario = Arc::new(Scenario {
        name: "crash-at-1000".to_string(),
        description: Some("crash node 0 at 1000ns".to_string()),
        seed: 42,
        nodes: vec![weft_scenario::Node {
            node_id: 0,
            program: "./prog".to_string(),
            args: vec![],
        }],
        network: None,
        filesystem: Default::default(),
        time_skew: Default::default(),
        events: vec![weft_scenario::ScheduledEvent {
            time_ns: 1000,
            action: weft_scenario::EventAction::Crash { node_id: 0 },
        }],
    });

    let global_time = Arc::new(AtomicU64::new(0));
    let registry = Arc::new(Mutex::new(NodeRegistry::new(&scenario)));

    // Start the node
    {
        let mut reg = registry.lock().unwrap();
        reg.set_running(0, 9999); // Fake PID
    }

    // Spawn the scheduler
    let scheduler_handle = spawn_scheduler(
        Arc::clone(&scenario),
        Arc::clone(&global_time),
        Arc::clone(&registry),
    );

    // Advance global time to trigger the event
    global_time.store(1000, std::sync::atomic::Ordering::Relaxed);

    // Give the scheduler thread a moment to process the event
    std::thread::sleep(std::time::Duration::from_millis(100));

    // Check that the node is now marked as crashed
    let reg = registry.lock().unwrap();
    assert_eq!(
        reg.state(0),
        Some(weft_dst::orchestrator::NodeStatus::Crashed)
    );

    // Clean up the scheduler thread
    drop(reg);
    let _ = scheduler_handle.join();
}

#[test]
fn event_scheduler_respects_event_ordering() {
    // Create a scenario with multiple events at different times
    let scenario = Arc::new(Scenario {
        name: "multi-event".to_string(),
        description: Some("multiple events".to_string()),
        seed: 42,
        nodes: vec![
            weft_scenario::Node {
                node_id: 0,
                program: "./prog".to_string(),
                args: vec![],
            },
            weft_scenario::Node {
                node_id: 1,
                program: "./prog".to_string(),
                args: vec![],
            },
        ],
        network: None,
        filesystem: Default::default(),
        time_skew: Default::default(),
        events: vec![
            weft_scenario::ScheduledEvent {
                time_ns: 500,
                action: weft_scenario::EventAction::Crash { node_id: 0 },
            },
            weft_scenario::ScheduledEvent {
                time_ns: 1000,
                action: weft_scenario::EventAction::Crash { node_id: 1 },
            },
        ],
    });

    let global_time = Arc::new(AtomicU64::new(0));
    let registry = Arc::new(Mutex::new(NodeRegistry::new(&scenario)));

    // Start both nodes
    {
        let mut reg = registry.lock().unwrap();
        reg.set_running(0, 1000);
        reg.set_running(1, 2000);
    }

    // Spawn scheduler
    let scheduler_handle = spawn_scheduler(
        Arc::clone(&scenario),
        Arc::clone(&global_time),
        Arc::clone(&registry),
    );

    // Advance to first event time
    global_time.store(500, std::sync::atomic::Ordering::Relaxed);
    std::thread::sleep(std::time::Duration::from_millis(100));

    // At this point, node 0 should be crashed but node 1 should still be running
    {
        let reg = registry.lock().unwrap();
        assert_eq!(
            reg.state(0),
            Some(weft_dst::orchestrator::NodeStatus::Crashed)
        );
        assert_eq!(
            reg.state(1),
            Some(weft_dst::orchestrator::NodeStatus::Running)
        );
    }

    // Advance to second event time
    global_time.store(1000, std::sync::atomic::Ordering::Relaxed);
    std::thread::sleep(std::time::Duration::from_millis(100));

    // Now both should be crashed
    {
        let reg = registry.lock().unwrap();
        assert_eq!(
            reg.state(0),
            Some(weft_dst::orchestrator::NodeStatus::Crashed)
        );
        assert_eq!(
            reg.state(1),
            Some(weft_dst::orchestrator::NodeStatus::Crashed)
        );
    }

    drop(registry);
    let _ = scheduler_handle.join();
}
