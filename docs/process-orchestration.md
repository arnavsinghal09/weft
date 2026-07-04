# Process Orchestration (Phase 4b)

## Overview

Process orchestration extends the fault model to include scheduled process lifecycle events: crashes, restarts, and partition changes. A scenario's `events` array triggers these events at absolute logical times, allowing testing of distributed systems' recovery and resilience.

## Architecture

### Current State (Foundation)

- **Scenario DSL** (`weft-scenario` crate): describes crashes and restarts as scheduled events
- **Broker** (`weft-net`): network-only state machine; does not manage processes
- **Shim** (`weft-shim`): per-process interception; does not track process lifecycle

### Required Additions for MVP

#### 1. Process Registry

The **broker** or a separate **orchestrator** process needs to track:
- Which node processes are running
- Their PIDs and configuration
- Cluster membership and partition state

```rust
struct NodeState {
    node_id: usize,
    program: String,
    args: Vec<String>,
    pid: Option<u32>,
    status: NodeStatus, // Running, Crashed, Restarting
}

enum NodeStatus {
    Idle,
    Running(u32), // pid
    Crashed(u64), // time_crashed_ns
    Restarting(u64), // time_restart_ns
}
```

#### 2. Event Scheduler

A dedicated thread in the **broker** or **orchestrator** that:
1. Reads `scenario.events` from the loaded scenario
2. Waits for the logical clock to reach each event's `time_ns`
3. Executes the event action (crash/start/partition change)

```rust
// Pseudocode
for event in scenario.events.iter().sorted_by_key(|e| e.time_ns) {
    // Wait until global_logical_time >= event.time_ns
    wait_until(event.time_ns);
    
    match event.action {
        EventAction::Crash { node_id } => {
            kill_node(node_id);
            mark_crashed(node_id);
        }
        EventAction::Start { node_id } => {
            respawn_node(node_id);
            mark_running(node_id);
        }
        EventAction::ActivatePartition { spec } => {
            apply_partition(spec);
        }
        EventAction::ClearPartition => {
            clear_partitions();
        }
    }
}
```

#### 3. Global Logical Clock Exposure

The **broker** must expose the current logical time to the event scheduler:

- **Phase 1**: Virtual clock is per-process; broker aggregates via message timestamps
- **Phase 3**: Network broker observes datagram delivery times
- **Phase 4**: All faults use the same global timeline

A shared atomic counter (e.g., `AtomicU64`) in the broker's state tracks:
```rust
struct BrokerState {
    global_logical_time_ns: AtomicU64,
    // ... existing fields
}
```

Each message delivery updates this; the event scheduler polls it to trigger events.

#### 4. Process Signal Handling

**Crashing a process**: send `SIGKILL` (cannot be caught):
```rust
fn kill_node(pid: u32) {
    unsafe { libc::kill(pid as i32, libc::SIGKILL) };
}
```

**Restarting a process**: `fork` + `exec` with the same environment:
```rust
fn respawn_node(config: &NodeState) -> u32 {
    let pid = unsafe { libc::fork() };
    if pid == 0 {
        // Child: set up environment and exec
        std::env::set_var(weft_abi::ENV_SEED, format!("{}", seed));
        std::env::set_var("WEFT_NODE_ID", format!("{}", config.node_id));
        std::env::set_var(weft_abi::ENV_BROKER, &broker_socket);
        
        libc::execvp(
            config.program.as_ptr() as *const i8,
            config.args.as_ptr() as *const *const i8,
        );
        std::process::exit(1);
    }
    pid as u32
}
```

#### 5. State Preservation Across Restarts

**Ephemeral state is lost**:
- In-memory data structures
- Open file descriptors
- Network connections
- Thread state

**Persistent state is retained**:
- Files on disk (subject to file I/O faults like fsync_lies)
- Durable log entries (if the application uses them)

The restart process is **instantaneous** in logical time—no signal handlers, cleanup, or orderly shutdown.

## Integration Points

### 1. weft-dst CLI

The CLI must:
- Load the scenario file (already done by weft-scenario)
- Extract the event list
- Create a process registry for each node
- Spawn the orchestrator/event-scheduler thread

```bash
weft run scenarios/file-sync-network-reordering.json
```

### 2. Broker Startup

The broker's `new` or `bind` method must accept a loaded scenario:

```rust
pub fn bind(path: &Path, model: FaultModel, scenario: Option<&Scenario>) -> io::Result<Self> {
    // ... existing code
    
    if let Some(s) = scenario {
        // Clone events and spawn event-scheduler thread
        let events = s.events.clone();
        thread::spawn(|| event_scheduler(events, /* shared broker state */));
    }
    
    Ok(Self { /* ... */ })
}
```

### 3. Logical Clock Synchronization

Currently, logical time is **per-process** (Phase 1's virtual clock). Process orchestration needs a **global timeline**.

Options:
1. **Broker-centric**: The broker observes the maximum logical time seen in any message and broadcasts it back to nodes via heartbeats.
2. **Shared memory**: The broker writes the global time to shared memory; nodes read it.
3. **Phase 2 Scheduler integration**: If deterministic scheduling is active, the scheduler can track global time across threads and processes.

For MVP, option 1 (broker-centric) is simplest: the broker tracks max time, and the event scheduler uses it.

## File I/O Faults with Process Crashes

File I/O faults and process crashes interact:

**Scenario**: Node writes value=42 to disk, calls `fsync()` (returns success due to `fsync_lies`), then crashes.
- If `fsync_lies: true`: the write is **not persisted**; node restarts and reads old value.
- If `fsync_lies: false`: the write is **persisted**; node restarts and reads new value.

This creates **data loss bugs** that only manifest with simultaneous `fsync_lies` + `crash` events.

Implementation:
1. File I/O hooks track writes per file descriptor
2. On crash, the shim does NOT flush pending writes (already lost due to fsync_lies)
3. On restart, the node re-reads the disk and sees the pre-crash state

## Validation & Testing

### Property-Based Testing

For each scenario:
1. **Determinism**: same seed + same scenario → same crashes at same logical times
2. **Reproducibility**: runs 1, 2, 3 with seed=42 all crash node 0 at 5ms
3. **Isolation**: removing one fault (e.g., set `fsync_lies: false`) prevents the bug

Example test:
```rust
#[test]
fn multi_fault_requires_both() {
    let scenario_with_both = load_scenario("file-sync-network-reordering.json");
    let scenario_without_fsync = {
        let mut s = scenario_with_both.clone();
        s.filesystem.get_mut("0").unwrap().fsync_lies = false;
        s
    };
    
    let output_with_both = run(scenario_with_both, seed=42);
    let output_without_fsync = run(scenario_without_fsync, seed=42);
    
    // Bug manifests only with both faults
    assert_ne!(output_with_both, output_without_fsync);
}
```

## Future Work

1. **Stateful orchestrator**: Track node membership, quorum, and consensus protocol violations
2. **Partition topology**: Support complex partition patterns (asymmetric, cascading)
3. **Signal delivery**: Deliver signals to handlers before terminating (async cleanup)
4. **Crash dump simulation**: Checkpoint process state before crash, optionally corrupt it
5. **Recovery verification**: Assert that replicas converge after a crash/restart cycle

## References

- `docs/fault-model.md`: Complete fault vocabulary and reproducibility guarantees
- `docs/logical-time-model.md`: Global timeline unifying Phases 1–4
- `examples/scenarios/file-sync-network-reordering.json`: Multi-fault scenario example
