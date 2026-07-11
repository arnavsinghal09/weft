# Weft reference

The complete user-facing surface: CLI commands and flags, exit codes,
environment variables, the network-condition spec, the scenario DSL, the fuzz
config, and the recording format. If behavior differs from this document,
one of the two is a bug.

Compatibility policy for everything on this page: [../VERSIONING.md](../VERSIONING.md).

---

## 1. CLI

```
weft <COMMAND> [OPTIONS]
```

| command | purpose | platforms |
|---|---|---|
| `weft run` | execute a program (or a cluster) deterministically | Linux (glibc, dynamic) |
| `weft replay` | re-execute a recording, verify byte-identity, check invariants | all |
| `weft fuzz` | sweep fault seeds, shrink violations to minimal reproducers | all |
| `weft -V` / `--version` | print version | all |

### 1.1 `weft run`

```
weft run --seed <N> [OPTIONS] -- <program> [args...]
```

| flag | meaning |
|---|---|
| `--seed <N>` | run seed, decimal or `0x`-hex u64. **Required.** |
| `--strategy <S>` | scheduler interleaving strategy: `random` (default) or `rr` (round-robin with 0.2 perturbation). `random` to find bugs, `rr` to understand one. |
| `--no-sched` | disable deterministic thread scheduling; time and randomness stay deterministic, the OS schedules threads. (Use for TSan runs.) |
| `--net <SPEC>` | simulate the network through a seeded broker; see §3. An empty SPEC (`--net ""`) is a reliable network. |
| `--nodes <N>` | with `--net`: launch N instances of the program, node ids `0..N-1` (default 1). |
| `--record <LOG>` | with `--net`: record every broker operation to LOG for `weft replay`. A `.gz` path is gzip-compressed. |
| `--trace`, `--verbose` | log every intercepted call to stderr. |
| `--stats` | print scheduler statistics at exit. |
| `--shim <PATH>` | path to `libweft_shim.so` (default: `WEFT_SHIM` env, then next to the `weft` binary). |

**Exit code:** the target's exit status passes through (single-process runs
`exec` the target, so the status *is* the target's). Cluster runs combine
node statuses — 0 iff every node exited 0, otherwise a failing node's status
clamped to 1–255. Weft's own failures print `weft run: <message>` and exit 1.

### 1.2 `weft replay`

```
weft replay <LOG> [--until <OP>] [--check <LIST>]
```

| flag | meaning |
|---|---|
| `<LOG>` | weft-log file, plain or gzipped (detected by content, not extension). |
| `--until <OP>` | stop after replaying op OP (inclusive) — halt right after a violating operation. |
| `--check <LIST>` | comma-separated invariants: `fifo` (per-channel FIFO), `dup` (no duplicate delivery), or `all`. Default: none (pure byte-identity verification). |

**Exit codes:** `0` replay identical and invariants hold · `2` invariant
violation · `1` unreadable log / replay divergence.

On success prints `replay identical: N op(s), stream digest %016x` — the
digest is deterministic and safe to compare across runs and machines.

### 1.3 `weft fuzz`

```
weft fuzz --config <FILE> [OPTIONS]
```

| flag | meaning |
|---|---|
| `--config <FILE>` | JSON config (**required**; every flag below overrides its config counterpart). See §5. |
| `--seeds <START:N>` | sweep N seeds starting at START (e.g. `0:1000`). |
| `--time-budget <SEC>` | stop sweeping after SEC seconds (regression seeds always run first and are never skipped). |
| `--jobs <N>` | worker threads. |
| `--out <DIR>` | output directory for reproducer logs and `report.txt`. |
| `--no-shrink` | keep full-size reproducers. |
| `--regressions <FILE>` | JSON array of seeds tested before the sweep; refreshed with all failing seeds on failure. |

**Exit codes (CI contract):** `0` no violations · `2` violations found,
reproducers + report written · `1` configuration or setup error.

### 1.4 Case-study checkers (`chord-check`, `chord-trace`, `raft-check`)

Standalone binaries from `weft-chord` / `weft-raft`; they scan a recording:

```
chord-check <recording.weftlog> <M>     # M = identifier bits (ring size 2^M)
chord-trace <recording.weftlog> <M>     # per-node pointer-state timeline
raft-check  <recording.weftlog>
```

**Exit codes (shared contract):** `0` invariant holds · `2` VIOLATION ·
`3` DISCARD · `1` unreadable recording. The meaning of DISCARD differs:
for `raft-check` the seed exercised nothing (no leader was ever elected —
uninformative); for `chord-check` the scenario violated the papers' failure
precondition (a failure stranded some node with no live successor), so the
run cannot count against Chord.

---

## 2. Environment variables

All canonical names live in `crates/weft-abi/src/lib.rs`. `weft run` sets
the starred ones for you; you only set them yourself when bypassing the CLI.

| variable | set by | meaning |
|---|---|---|
| `WEFT_SEED` * | `weft run` | u64 seed (decimal or `0x`-hex). Presence activates the shim; unset ⇒ every hook is a passthrough. Malformed ⇒ reported and treated as unset. |
| `WEFT_TRACE` * | `--trace` | `"1"` logs every intercepted call to stderr. |
| `WEFT_STRATEGY` * | `--strategy` | `random` or `rr`. |
| `WEFT_SCHED` * | `--no-sched` | `"0"`/`"off"` disables the deterministic scheduler. |
| `WEFT_SCHED_STATS` * | `--stats` | `"1"` prints scheduler statistics at exit. |
| `WEFT_BROKER` * | `--net` | path to the broker's Unix-domain socket; presence activates network interception. |
| `WEFT_NODE_ID` * | `--nodes` | this process's node index (decimal u32). |
| `WEFT_NET` * | `--net` | the network-condition spec (consumed by the broker). |
| `WEFT_SHIM` | user | path to `libweft_shim.so`, checked before the built-in search. |
| `WEFT_FSYNC_LIES` | user / scenario | `"1"` makes `fsync`/`fdatasync` return success without persisting. |
| `WEFT_ENOSPC_BYTES` | — | **reserved, unimplemented** (planned ENOSPC injection threshold). |

---

## 3. Network-condition spec (`--net`, `WEFT_NET`)

Comma-separated `key=value` clauses; all keys optional; empty spec = reliable
network. Grammar implemented in `crates/weft-net/src/config.rs`.

| clause | forms | meaning |
|---|---|---|
| `latency=` | `fixed:N` · `uniform:LO-HI` · `exp:MEAN` | per-message delay in **nanoseconds of logical time** (an ordering key, not wall time). `uniform` requires LO ≤ HI. |
| `loss=` | `P` in `[0.0, 1.0]` | per-message drop probability, seeded per `(src, dst, seq)`. |
| `bw=` | bytes/sec > 0 | bandwidth cap; adds serialization delay. |
| `partition=` / `part=` | `0+1\|2` | `+` joins nodes into a group, `\|` separates groups; traffic across groups is dropped. Empty value clears partitions. |

Example: `latency=uniform:1000-5000,loss=0.1,bw=2000000,partition=0+1|2`

Every fate (delay, drop, order) is a pure function of `(seed, src, dst, seq)`
— the same seed deals every message the same fate on every platform.

---

## 4. Scenario DSL (JSON)

Parsed and validated by `weft-scenario` (`Scenario::from_json`). Format is
**JSON only** (YAML is not supported). Runnable examples:
`examples/scenarios/*.json`.

```json
{
  "name": "string (required)",
  "description": "string | null",
  "seed": 42,
  "nodes": [
    {"node_id": 0, "program": "./path", "args": ["--flag"]}
  ],
  "network": {
    "latency": "uniform:500-10000",
    "loss": 0.0,
    "bandwidth": null,
    "partitions": "0+1|2"
  },
  "filesystem": {
    "0": {"fsync_lies": true, "enospc_after_bytes": null, "torn_write_probability": 0.0}
  },
  "time_skew": { "0": 0 },
  "events": [
    {"time_ns": 1000000, "action": {"type": "crash", "node_id": 0}},
    {"time_ns": 2000000, "action": {"type": "start", "node_id": 0}},
    {"time_ns": 3000000, "action": {"type": "activate_partition", "spec": "0|1"}},
    {"time_ns": 4000000, "action": {"type": "clear_partition"}}
  ]
}
```

Validation rules (all violations produce a specific `ScenarioError`):

- `nodes[*].node_id` must be sequential from 0.
- `events`, `filesystem`, `time_skew` may only reference existing node ids.
- `network.latency` must parse per §3; `loss` and `torn_write_probability`
  must be in `[0.0, 1.0]`; `bandwidth` must be > 0 if present.
- `partitions` must match the `0+1|2` grammar; empty string clears.
- Events are sorted by `time_ns` during parsing.

Event actions: `crash`, `start`, `activate_partition` (takes `spec`),
`clear_partition`.

---

## 5. Fuzz config (JSON)

Full semantics in [fuzzing.md](fuzzing.md). Unknown fields are rejected
(typos fail loudly), except the `"//"` comment slot.

| field | default | meaning |
|---|---|---|
| `net` | *(required)* | fault model to explore (§3 syntax) |
| `seed_start`, `seed_count` | 0, 1000 | seed range swept |
| `jobs` | all cores | worker threads |
| `time_budget_secs` | 0 (off) | wall-clock sweep budget |
| `invariants` | `["fifo","dup"]` | invariant set checked on every execution |
| `workload` | 2 nodes, 24 sends | `{nodes, sends, payload_len, workload_seed}` — deterministic client behavior, independent of the fault seed |
| `out_dir` | `weft-fuzz-out` | reproducer logs + `report.txt` |
| `shrink` | `true` | delta-debug each distinct violation |
| `regression_seeds` | `[]` | seeds always tested first |

---

## 6. Recording format (weft-log v1)

Full specification: [recording-format.md](recording-format.md). Essentials:

- Line-oriented JSON; line 1 is the header:
  `{"format":"weft-log","version":1,"seed":…,"net":"…","meta":{…}}`.
- Readers MUST reject unknown `version` values; `meta` is informational only
  and MUST be ignored for replay purposes.
- Gzip is detected by content (magic bytes), never by file extension.
- The log records the broker linearization order — the only non-seed input
  to a run — so `seed + log` reconstructs the run exactly; replay verifies a
  FNV-1a chain digest over every record.

---

## 7. Rust API (crates)

Published entry points a test harness is expected to use — everything else
is implementation detail:

- `weft_scenario::Scenario::from_json` / `::validate`,
  `weft_scenario::parse_scenario`, `LatencyDistribution::parse`,
  `ScenarioError`.
- `weft_replay::Log::read`, invariants (`fifo`, `dup`) via `weft replay --check`.
- `weft_chord` (report parsing + log-scanning verdict types) and
  `weft_raft::{check, parse_report, Verdict}` — models for writing your own
  recording checkers (scan `log.records`, parse your protocol's state
  reports, accumulate a verdict; ~150 lines each).
- `weft_abi::ENV_*` constants — the canonical env-var names.
