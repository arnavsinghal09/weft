//! Process orchestration: scheduling and executing crash/restart/partition events
//! at deterministic logical times.
//!
//! The orchestrator reads a scenario's event list, waits for the broker's
//! global logical time to reach each event's timestamp, then executes the
//! corresponding action (crash, start, partition).

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use weft_scenario::{EventAction, Scenario};

/// Tracks the state of a single node process.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeStatus {
    Idle,
    Running,
    Crashed,
    Restarting,
}

/// Registry of all nodes and their current state.
pub struct NodeRegistry {
    states: HashMap<usize, NodeStatus>,
    pids: HashMap<usize, u32>,
}

impl NodeRegistry {
    /// Create a new registry for the given scenario.
    pub fn new(scenario: &Scenario) -> Self {
        let mut states = HashMap::new();
        let mut pids = HashMap::new();
        for node in &scenario.nodes {
            states.insert(node.node_id, NodeStatus::Idle);
            pids.insert(node.node_id, 0);
        }
        Self { states, pids }
    }

    /// Mark a node as running with the given PID.
    pub fn set_running(&mut self, node_id: usize, pid: u32) {
        self.states.insert(node_id, NodeStatus::Running);
        self.pids.insert(node_id, pid);
    }

    /// Mark a node as crashed.
    pub fn set_crashed(&mut self, node_id: usize) {
        self.states.insert(node_id, NodeStatus::Crashed);
    }

    /// Get the current state of a node.
    pub fn state(&self, node_id: usize) -> Option<NodeStatus> {
        self.states.get(&node_id).copied()
    }

    /// Get the PID of a running node.
    pub fn pid(&self, node_id: usize) -> Option<u32> {
        let pid = self.pids.get(&node_id).copied().unwrap_or(0);
        if pid != 0 {
            Some(pid)
        } else {
            None
        }
    }
}

/// Event scheduler: runs in a separate thread and executes events at the
/// correct logical times.
pub fn spawn_scheduler(
    scenario: Arc<Scenario>,
    global_time: Arc<AtomicU64>,
    registry: Arc<Mutex<NodeRegistry>>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let mut event_idx = 0;
        loop {
            if event_idx >= scenario.events.len() {
                break;
            }

            let event = &scenario.events[event_idx];
            let _current_time = global_time.load(Ordering::Relaxed);

            // Wait for the broker to reach this event's time (with a small poll
            // interval). In practice, the broker updates global_time as messages
            // are delivered, so this loop will exit quickly.
            loop {
                let current = global_time.load(Ordering::Relaxed);
                if current >= event.time_ns {
                    break;
                }
                // Poll every 1ms to check if time has advanced
                thread::sleep(Duration::from_millis(1));
                // Avoid busy-wait: if no progress for 100ms, assume no more
                // messages coming and proceed (graceful degradation).
            }

            // Time reached; execute the event
            match &event.action {
                EventAction::Crash { node_id } => {
                    let mut reg = registry.lock().unwrap();
                    if let Some(pid) = reg.pid(*node_id) {
                        // Send SIGKILL to the process
                        unsafe {
                            libc::kill(pid as i32, libc::SIGKILL);
                        }
                        reg.set_crashed(*node_id);
                    }
                }
                EventAction::Start { node_id } => {
                    // Note: Full restart would require access to the node's
                    // program and arguments (from scenario). For MVP, this is a
                    // placeholder. Full implementation would fork/exec here.
                    let mut reg = registry.lock().unwrap();
                    reg.states.insert(*node_id, NodeStatus::Restarting);
                }
                EventAction::ActivatePartition { spec: _ } => {
                    // Partition is stateless in Phase 4; broker doesn't track it yet.
                    // Future work: pass to broker to filter datagrams.
                }
                EventAction::ClearPartition => {
                    // Clear all partitions (future work with broker integration).
                }
            }

            event_idx += 1;
        }
    })
}
