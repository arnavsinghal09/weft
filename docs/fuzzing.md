# Fuzzing and shrinking (`weft fuzz`)

Phase 5 made a specific failure reproducible from a log. `weft fuzz` closes
the loop: it *finds* failures by sweeping fault seeds, and hands back the
smallest reproducer for each distinct one.

```
weft fuzz --config examples/fuzz/demo.json
```

## How it works

1. **Sweep.** A fixed, deterministic workload (generated from
   `workload_seed`, independent of the fault seed) is executed against the
   broker's decision core once per fault seed, in parallel. The same
   invariants run on every execution. Regression seeds are queued *before*
   the sweep, so a tight time budget can never skip them.
2. **Dedup.** Violations are grouped by identity — invariant name + the
   channel (src → dst) of the violating event — not by message text, since
   sequence numbers legitimately differ between runs.
3. **Shrink.** Each distinct violation is reduced from its smallest failing
   seed by delta debugging over the *op inputs* (never the recorded log
   text: outcomes are derived data). Passes: truncate after the violation →
   ddmin chunk removal (candidates evaluated in parallel; lowest-index
   success adopted so results stay deterministic) → single-op removals to a
   1-minimal fixpoint → payload truncation → connect GC. Seed and net spec
   are never varied — changing them would reproduce a *different* run.
4. **Report + reproducers.** Every distinct violation gets a fresh,
   fully-consistent `weft-log` in `out_dir` that `weft replay <log> --check
   …` verifies byte-for-byte, plus a `report.txt`.

The shrinker never reorders ops, never renumbers connections or addresses,
and requires the *same* invariant to fail on the *same* channel — so the
minimal reproducer stays an interpretable subsequence of the original run
rather than a technically-smaller alien. Its correctness is pinned by three
ground-truth tests (`crates/weft-fuzz/tests/shrink_ground_truth.rs`) where a
known exact minimum is buried in hundreds of noise ops and must be recovered
*exactly*.

## Config file

One JSON document; every CLI flag maps onto it and overrides it.

```json
{
  "//": "optional comment slot, ignored",
  "net": "latency=uniform:0-8000,loss=0.02",
  "seed_start": 0,
  "seed_count": 1000,
  "jobs": 8,
  "time_budget_secs": 60,
  "invariants": ["fifo", "dup"],
  "workload": { "nodes": 3, "sends": 30, "payload_len": 4, "workload_seed": 0 },
  "out_dir": "weft-fuzz-out",
  "shrink": true,
  "regression_seeds": []
}
```

| field | default | meaning |
|---|---|---|
| `net` | *(required)* | fault model to explore (`weft_net::config` syntax) |
| `seed_start`, `seed_count` | 0, 1000 | the seed range swept |
| `jobs` | all cores | worker threads |
| `time_budget_secs` | 0 (off) | stop sweeping after this many seconds |
| `invariants` | `["fifo","dup"]` | `fifo` = per-channel-fifo, `dup` = no-duplicate-delivery |
| `workload` | 2 nodes, 24 sends | the deterministic client behavior |
| `out_dir` | `weft-fuzz-out` | reproducer logs + `report.txt` |
| `shrink` | true | shrink each distinct violation |
| `regression_seeds` | `[]` | seeds always tested first |

Unknown fields are rejected (typos fail loudly), except the `"//"` comment
slot.

## Exit codes (CI contract)

| code | meaning |
|---|---|
| 0 | sweep completed, no violations |
| 2 | violations found; reproducers and report written |
| 1 | configuration or setup error |

## CI integration

The real, working example is `.github/workflows/fuzz.yml` +
`examples/fuzz/ci.json`. The CI config is a *property test*: under
`latency=fixed:100` with no loss, FIFO and duplicate-freedom must hold for
**every** seed, so the sweep is expected to pass and any violation is a
genuine regression in the broker's decision core. (Do not put a
variance-latency net in CI with the `fifo` invariant — reordering under
variance is by design, and the job would fail every night by construction.
That configuration lives in `examples/fuzz/demo.json` for humans exploring
the shrinker.)

With `--regressions <file>`, every failing seed the sweep discovers is
written to the file (a plain JSON array), and future runs test those seeds
before sweeping — a regression corpus that grows on its own.

## Reading a failure

Each distinct violation in the report ends with a literal command:

```
repro  : weft-fuzz-out/repro-seed0-per-channel-fifo-on-127-0-0-2-100-127-0-0-1-100.weftlog
verify : weft replay weft-fuzz-out/repro-seed0-….weftlog --check fifo,dup
```

`weft replay` re-executes the reproducer to an identical stream digest and
prints the full violation report — invariant, op + virtual-time anchor,
seed, net spec, and the surrounding event window (typically the whole log:
shrunk reproducers are usually under ten records).

## Scope

`weft fuzz` explores the broker boundary (Phase 3's network model plus the
Phase 5 log/invariant machinery) as pure computation — no shim, no sockets —
so it runs identically on every platform. Fuzzing *live target programs*
(the LD_PRELOAD shim path) composes with `weft run --record` today: sweep
seeds by re-running the cluster and replay any failing recording; folding
that flow into `weft fuzz` is future work.
