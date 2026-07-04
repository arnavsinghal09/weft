# The Weft Fault Model (Phase 4)

## Overview

Phase 4 extends Weft's deterministic simulation to include **timed fault events**: controlled injection of network latency, loss, corruption, file I/O faults, process crashes, and clock skew. A scenario is a JSON configuration describing a distributed system test with these faults. Every event is scheduled against a global logical timeline, driven by a seeded PRNG, so:

- Same seed → identical faults, identical timings, identical crashes/restarts.
- Different seed → different fault sequence (useful for fuzzing).

## Logical Time

All faults are scheduled against a **global logical timeline** that unifies the concepts from Phases 1–3:

- **Phase 1 (Virtual Clock)**: per-process observation of elapsed time, starting at 0, advancing monotonically.
- **Phase 2 (Scheduler)**: one thread runs at a time; yield points are where the next thread is chosen from the seed.
- **Phase 3 (Network Broker)**: per-message fate determines whether a message is dropped or delayed; delivery order is a `BinaryHeap` of `(delay_key, insertion_index)`.
- **Phase 4 (Global Timeline)**: all faults (network, file I/O, crashes, clock skew) are scheduled events with absolute `(logical_time, tiebreaker)` keys.

A process's local virtual clock is a **view** into this global timeline:

```
process[node_i]::clock = global_timeline_time + clock_skew[node_i]
```

Where `clock_skew[node_i]` is a per-node offset, seeded by `f(seed, node_i, skew_index)`.

## Fault Model Vocabulary

### Network Faults

Inherited from Phase 3, integrated into the global timeline:

| fault | meaning | reproducibility |
|---|---|---|
| `latency=fixed:N` | constant N ns delay per datagram | fate(seed, src, dst, seq) |
| `latency=uniform:LO-HI` | uniform [LO, HI] ns — causes reordering | fate(seed, src, dst, seq) |
| `latency=exp:MEAN` | exponential distribution (20×MEAN max) | fate(seed, src, dst, seq) |
| `loss=P` | independent loss probability P ∈ [0,1] | fate(seed, src, dst, seq) |
| `bandwidth=B` | serialization delay `len/B` per datagram | fate(seed, src, dst, seq) |
| `partitions=G1\|G2\|G3` | nodes in different groups cannot communicate | static, set at scenario start |

### File I/O Faults (Phase 4)

Per-node file system fault configuration:

| fault | meaning | reproducibility |
|---|---|---|
| `fsync_lies` | `fsync(2)` returns success but does not persist writes | binary (on/off); if on, all fsync() calls lie |
| `enospc_after_bytes` | simulate ENOSPC after N bytes written per node | absolute byte count; per-node independent |
| `torn_write_probability` | probability [0,1] that a write is torn (partially written) on process crash | seeded per-write: fate(seed, node_id, fd, op_seq) |

### Process Orchestration (Phase 4)

Scheduled events control node lifecycle:

| action | meaning |
|---|---|
| `crash` at time T | terminate node process at absolute logical time T |
| `start` at time T | respawn node process at time T (clears ephemeral state, retains disk state) |
| `activate_partition` at time T | enable network partition (one-way or bidirectional drop) |
| `clear_partition` at time T | disable all network partitions (recovery) |

### Clock Skew (Phase 4)

Per-node virtual clock offset:

```json
"time_skew": {
  "0": 0,
  "1": 1000000000,
  "2": -500000000
}
```

Each node's `clock_gettime()` and `sleep()` operate on its skewed timeline. Seed determinism: if skew is specified, the skew values are reproduced; if omitted, each node gets a seed-derived random offset in [-1 year, +1 year).

## Scenario Format (JSON)

Complete schema:

```json
{
  "name": "scenario-name",
  "description": "human-readable purpose",
  "seed": 42,
  
  "nodes": [
    {"node_id": 0, "program": "./prog", "args": ["arg1"]}
  ],
  
  "network": {
    "latency": "uniform:100-5000",
    "loss": 0.1,
    "bandwidth": 1000000,
    "partitions": "0+1|2"
  },
  
  "filesystem": {
    "0": {
      "fsync_lies": true,
      "enospc_after_bytes": 1000000,
      "torn_write_probability": 0.05
    }
  },
  
  "time_skew": {
    "0": 0,
    "1": 1000000000
  },
  
  "events": [
    {"time_ns": 5000000, "action": {"type": "crash", "node_id": 0}},
    {"time_ns": 10000000, "action": {"type": "start", "node_id": 0}}
  ]
}
```

All fields except `name`, `seed`, and `nodes` are optional. Defaults:
- No network faults (reliable network).
- No filesystem faults.
- No time skew (each node gets `seed_derived % 1_year`).
- No scheduled events.

## Determinism Guarantees

**Exact reproducibility**: same seed + same scenario → byte-identical process outputs, same order of operations, same crashes at the same absolute logical times.

**Scope of determinism**:
- Seeded: network faults, file I/O faults, process crashes, clock skew.
- **Not seeded**: cross-process arrival order at the broker (OS-scheduled, same as Phase 3 limitation). This is why the multi-node tests sort output instead of comparing verbatim.
- **Managed threads** (Phase 2 scheduler): order within one process is fully deterministic.
- **Unmanaged threads** (created before shim activation): order is OS-scheduled, not deterministic.

## Limitations

### File I/O
- **No actual write corruption** (bit flips, silent corruption). `fsync_lies` controls only sync semantics.
- **No device I/O errors** (EIO, EROFS). ENOSPC is the only modeled hardware failure.
- **No journal or crash recovery semantics**. The application must model idempotency, log replay, or write-ahead logging itself.
- **No order-of-sync guarantees**. Which writes persist across a crash is not modeled; `fsync_lies` is binary (on or off).

### Processes
- **Crashes are instantaneous**. Real OS behavior (buffered output, signal handlers, cleanup code) is not simulated.
- **Restart clears process state but preserves disk**. State in shared memory, pipes, or IPC is lost.
- **No explicit cluster coordination** (leader election, consensus protocol failures). The scenario author must provide explicit crash/recovery events.

### Clock Skew
- **No drift**: clock offset is fixed per node, not a continuous drift rate.
- **No NTP/clock sync failures**. If skew is desired, specify it explicitly.
- **No CLOCK_REALTIME discontinuities**. Skew is additive; the timeline is linear.

### Network
- Inherited from Phase 3 (see docs/network-model.md):
  - TCP is not simulated.
  - `connect`/`send`/`recv` on UDP is not intercepted.
  - Cross-process arrival order is OS-scheduled.

## Validation & Error Handling

The scenario parser returns **detailed, actionable errors** on malformed input:

```
$ weft run scenarios/bad-scenario.json
weft run: JSON/YAML parse error: missing field `nodes` at line 1 column 42
```

Parser guarantees:
- **Never panics** on any input (property-tested).
- **Clear error messages** citing what is wrong and how to fix it.
- **Exhaustive validation**: node ID gaps, event references to non-existent nodes, invalid probability values, malformed partition specs, etc.

## Example Scenarios

### Scenario 1: Network Reordering (Single Fault)

File: `examples/scenarios/network-reordering.json`

**What it demonstrates**: network latency variance causes message reordering, triggering a replica divergence bug that requires **no crash, no file corruption, just unfair message ordering**.

```
seed=1: latency variance reorders WRITE-8 and READ.
        Replica applies WRITE-7 as most recent.
        Read returns stale value.
        Bug manifests.

seed=0: latency variance happens not to occur;
        messages arrive in order.
        Replica applies WRITE-8.
        Read returns correct value.
        Bug does not manifest.
```

This validates that Phase 3's network fault model is working.

### Scenario 2: Crash & Restart (Single Fault)

File: `examples/scenarios/crash-and-restart.json`

**What it demonstrates**: process crash discards in-flight messages and ephemeral state, forcing applications to handle missing updates.

```
seed=42:
- Node-0 sends 10 WRITE messages in quick succession.
- Node-1 crashes at 5ms (receives ~3 messages).
- Node-1 restarts at 15ms.
- Node-0 retries unacknowledged writes (application-level retry).
- Replica must detect duplicates and avoid double-apply.
```

This validates that process orchestration events are scheduled correctly.

### Scenario 3: Multi-Fault (Future: File Sync Reordering)

**What it demonstrates**: a bug that manifests **only** when two or more fault types combine.

```
Precondition:
- fsync_lies: true (sync returns success but doesn't persist)
- network: latency=uniform:100-10000 (causes reordering)

Execution:
1. Node-0 writes value=42 to disk
2. Node-0 calls fsync() — returns success but doesn't persist
3. Node-0 sends "value=42" to Node-1 (message reordered)
4. Node-0 crashes (due to scheduled event)
5. Node-0 restarts, reads disk: gets old value (fsync lied)
6. Node-0 sends "old_value" to Node-1
7. Node-1 sees conflicting updates due to reordering.

Bug trigger: requires fsync_lies AND network reordering AND crash.
Removing any one: bug does not manifest.
```

This is the critical deliverable showing that the fault model is powerful enough to expose multi-fault bugs.

## Property-Based Testing

The scenario parser is validated by a suite of Rust tests that:

1. **Assert no panic** on arbitrary input.
2. **Validate clear error messages** for every class of malformed input.
3. **Fuzz the boundary conditions** (max values, empty collections, deeply nested structures).

Run with:
```bash
cargo test -p weft-scenario
```

All tests pass: 8 unit + 22 integration, covering both happy path and error cases.

## Future Work

- **File corruption**: model bit flips or partial sectors, not just lost writes.
- **Disk-full recovery**: simulate recovery on a full disk with cleanup.
- **Inter-node clock sync**: drift and NTP failures.
- **TCP simulation**: connection state, retransmissions, window size.
- **Signal handling in crashes**: deliver signals to handlers before terminating.
- **Fault linter**: warn when a scenario is impossible (e.g., crash before start).
- **Disk full variation sampling**: random ENOSPC injection at every write.
