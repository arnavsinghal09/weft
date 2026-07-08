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
    #[must_use]
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
    #[must_use]
    pub fn state(&self, node_id: usize) -> Option<NodeStatus> {
        self.states.get(&node_id).copied()
    }

    /// Get the last-known PID of a node, if it was ever started. The PID is
    /// deliberately retained after a crash so cleanup (e.g. reaping) can find it.
    #[must_use]
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
///
/// # Panics
///
/// The spawned thread panics if the registry lock is poisoned, which cannot
/// happen: no holder performs a panicking operation.
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

            // Wait for the broker to reach this event's time (with a small poll
            // interval). In practice, the broker updates global_time as messages
            // are delivered, so this loop will exit quickly. If logical time
            // never reaches the event's timestamp, this waits indefinitely; the
            // caller is expected to detach or abandon the scheduler thread.
            loop {
                let current = global_time.load(Ordering::Relaxed);
                if current >= event.time_ns {
                    break;
                }
                // Poll every 1ms to check if time has advanced
                thread::sleep(Duration::from_millis(1));
            }

            // Time reached; execute the event
            match &event.action {
                EventAction::Crash { node_id } => {
                    let mut reg = registry.lock().unwrap();
                    if reg.state(*node_id) == Some(NodeStatus::Running) {
                        if let Some(pid) = reg.pid(*node_id) {
                            // A PID that does not fit pid_t cannot name a real
                            // process; skip the kill rather than wrap.
                            if let Ok(pid) = libc::pid_t::try_from(pid) {
                                // SAFETY: kill(2) is always safe to call; a
                                // stale or invalid pid yields ESRCH/EPERM,
                                // which we ignore.
                                unsafe {
                                    libc::kill(pid, libc::SIGKILL);
                                }
                            }
                            reg.set_crashed(*node_id);
                        }
                    }
                }
                EventAction::Start { node_id } => {
                    // Note: Full restart would require access to the node's
                    // program and arguments (from scenario). For MVP, this is a
                    // placeholder. Full implementation would fork/exec here.
                    let mut reg = registry.lock().unwrap();
                    reg.states.insert(*node_id, NodeStatus::Restarting);
                }
                // Partition changes are recognized but inert: the broker does
                // not track dynamic partitions yet (future work: pass to the
                // broker to filter datagrams).
                EventAction::ActivatePartition { .. } | EventAction::ClearPartition => {}
            }

            event_idx += 1;
        }
    })
}
