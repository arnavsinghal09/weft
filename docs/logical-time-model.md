# Logical Time in Weft — Unified Model (Phase 4)

## Problem Statement

Phases 1–3 each handle time independently:

- **Phase 1**: Virtual clock (`AtomicU64` nanoseconds). Monotonic starts at 0, every `read` advances by 1 µs. Realtime = 2000-01-01 + seed-offset + monotonic.
- **Phase 2**: Scheduler token model. One thread runs at a time; which one is a function of seed. Time advances only via yield points.
- **Phase 3**: Broker ordering key. Network delays are modeled as entries in a `BinaryHeap<(delay, insertion_index)>`. "Delay is an ordering key, not wall time" — the broker has no synchronization with the virtual clock or cross-process time base.

**Phase 4 unifies these** by establishing a single logical timeline that governs:
- When network messages are delivered (Phase 3)
- When file I/O operations complete (Phase 4 new)
- When clock skew applies to each node (Phase 4 new)
- When process crashes and restarts occur (Phase 4 new)
- Virtual clock reads in each process (Phase 1 extension)

## Key Insight: Logical Time is Scheduling Order

A **logical time** `T` is an entry point into a deterministic event queue. The key realization:

- The broker's BinaryHeap already implements this: events are ordered by `(delay_key, insertion_order)`, and the queue is traversed deterministically.
- Virtual clock reads are *observations* of logical time, not a separate clock.
- File I/O delays, network latencies, and clock skew are all **scheduled events**, drawn from the same seeded PRNG stream as everything else.

## The Model

### Global Event Queue (Broker-wide)

The broker maintains a `BinaryHeap<ScheduledEvent>` where `ScheduledEvent` is:

```rust
pub struct ScheduledEvent {
    pub time_key: (u64, u64),  // (logical_time_ns, insertion_order)
    pub event_type: EventType,
}

pub enum EventType {
    NetDeliver { src, dst, seq, payload },
    FileIoComplete { node, fd, result },
    ClockSkewChange { node, offset_ns },
    ProcessCrash { node_id },
    ProcessStart { node_id },
    FaultActivate { node_id, fault_id },  // e.g., activate partition
}
```

### Per-Node Virtual Clock

Each node process has a virtual clock that is a **view** into the global timeline:

```
virtual_clock[node_i] = global_base_time + node_clock_offset[node_i]
```

Where `node_clock_offset[node_i]` is:
1. Initially 0 (or seed-derived to make realtime dates differ)
2. Changed by `ClockSkewChange` events
3. Visible to the node's `clock_gettime()` calls

### Determinism: Per-Timeline Seeding

The source of every time value is a seeded PRNG stream, not a shared pool:

- **Network message delays**: seeded by `f(seed, src, dst, seq)` (Phase 3, unchanged)
- **File I/O delays**: seeded by `f(seed, node_id, fd, op_type, seq_on_fd)` — same per-fd sequence determinism as network
- **Clock skew changes**: seeded by `f(seed, node_id, skew_index)` — one sequence per node
- **Process crash timing**: seeded by `f(seed, node_id, crash_index)` — one sequence per node

### Atomicity and Isolation

The broker is single-threaded (Phase 3 current design). Within one broker round-trip:

1. Node calls broker with (operation, details)
2. Broker looks up timing via seeded fate function
3. Broker enqueues the result with absolute `time_key`
4. Broker returns immediately (no waiting for delay)
5. Next receiver of that result sees it at the right logical position

**Cross-process ordering is OS-scheduled** (Phase 3 honest limitation). But *once* an event is enqueued, its position is deterministic.

## Examples

### Single-fault: Network latency causes reordering

```
Seed 42, --net "latency=uniform:1000-5000"

Monotonic timeline (insertion order):
0. Node-0 sends PING to Node-1 (seq=0)
   → fate(42, 0, 1, 0) = delay=3200ns
   → enqueue at time_key=(3200, 0)

1. Node-1 receives nothing; yields
2. Node-0 sends PING to Node-1 (seq=1)
   → fate(42, 0, 1, 1) = delay=1200ns
   → enqueue at time_key=(1200, 1)

3. Next Node-1 recv:
   → dequeue (1200, 1) first → seq=1 arrives before seq=0
   → seq=0 still pending at (3200, 0)
   → reordering!
```

### Multi-fault: Network loss + file sync-lies

Node writes a message to disk ("value=42"), then sends it to the replica.

```
Scenario:
  - latency=fixed:100ns (deterministic delivery timing)
  - loss=0.5 (50% drop, seeded)
  - fsync-lies: every other fsync returns success but drops recent writes

Execution (seed S):
1. Node-0 write("value=42") → local file (unsynced)
2. Node-0 fsync() → fsync-lies returns success but doesn't actually persist
3. Node-0 send("I have value=42") to Node-1
   - fate(S, 0, 1, seq) = {delay: 100ns, drop: no}
   - Replica receives "value=42"
4. Node-0 crashes (scheduled event)
5. Node-0 restarts
6. Node-0 read("value=42") → EOF or old value (because fsync lied)
7. Node-0 send("I have <old value>") to Node-1
   - Replica sees conflicting updates — bug surface!

Without fsync-lies or without crash: bug never manifests.
```

## Honest Limitations (Phase 4)

- **Cross-process ordering remains OS-scheduled.** Which process's syscall reaches the broker first is timing-dependent. But once in the queue, position is deterministic.
- **Per-fd I/O delay independence.** File descriptor delays are seeded per-fd, so an fd's delays are reproducible but concurrent writes to different fds have timing that may interleave differently per run (like network, until Phase 2 scheduler is extended to file I/O).
- **Crash-and-restart granularity.** Crashes are modeled as instantaneous events. Real OS crash recovery (log replay, state consistency) is out of scope — the scenario author must model what remains.
- **File-system consistency model is simplified.** No true journaling, crash atomicity, or order-of-durability constraints beyond what fsync-lies and ENOSPC model.

## Scenario DSL (Phase 4)

Timed fault event sequences are written in a JSON scenario configuration
(the implemented format — see `examples/scenarios/` for runnable files):

```json
{
  "name": "file-sync-reordering",
  "description": "Replica divergence when fsync lies and network reorders",
  "seed": 42,
  "nodes": [
    {"node_id": 0, "program": "./target", "args": ["--role=writer"]},
    {"node_id": 1, "program": "./target", "args": ["--role=replica"]}
  ],
  "network": { "latency": "uniform:100-10000", "loss": 0.2 },
  "filesystem": {
    "0": { "fsync_lies": true, "enospc_after_bytes": null }
  },
  "time_skew": {},
  "events": [
    { "time_ns": 1000000, "action": { "type": "crash", "node_id": 0 } },
    { "time_ns": 2000000, "action": { "type": "start", "node_id": 0 } }
  ]
}
```

## Validation and Testing (Phase 4)

1. **Parser property tests:** generated scenario JSON (valid and deliberately malformed) never panics; parse always returns `Result<Scenario, ParseError>`.
2. **Per-fault-type isolation:** each fault type (network loss, fsync-lies, ENOSPC, crash) is tested in isolation with a minimal scenario.
3. **Multi-fault interaction:** at least one scenario demonstrates a bug that manifests **only** when two or more fault types are active together.

## What Changes, What Stays the Same

### Changes
- Broker's `BinaryHeap` becomes the global logical timeline
- Virtual clock becomes a per-process view (with optional skew offset)
- New crate: `weft-scenario` for DSL parsing and validation
- File I/O hooks in shim for torn write, ENOSPC, fsync-lies

### Stays the Same
- Phase 1 determinism guarantees (same seed → same values)
- Phase 2 scheduler token model (extends to file I/O yield points)
- Phase 3 network per-message fate (integrated into global timeline)
- Interception architecture (hooks + engine)
