# Raft validation: ElectionSafety under lost `votedFor`

Second validation target (after Chord), to show the Chord result is not a
one-off: a different protocol, a different invariant, the same
find → falsify structure.

## The edge case (primary source)

Ongaro, *Consensus: Bridging Theory and Practice* (Stanford dissertation,
2014), Figure 3.2: **ElectionSafety** — "at most one leader can be elected
in a given term" — and the state table marking `currentTerm` and `votedFor`
as **persistent** state, "updated on stable storage before responding to
RPCs". The known consequence of violating that persistence requirement: a
server that crashes and restarts having lost `votedFor` can grant a second
vote in the same term, letting two candidates each assemble a majority for
the same term — two leaders, ElectionSafety broken.

## Harness

- `examples/raft/raft_node.c`: minimal Raft **leader election only** (no
  log replication) — 5 servers + 1 observer, real OS processes over real
  UDP through Weft's shim + broker, same pattern as the Chord harness.
- Crash-restart is simulated in-process at 3 seed-jittered ticks per
  server: volatile state (role, tally, election timer) is dropped;
  `RAFT_FIX=0` **also drops `votedFor`** (the bug), `RAFT_FIX=1` keeps
  `currentTerm`/`votedFor` (Figure 3.2 honored). Nothing else differs.
- `crates/weft-raft`: `raft-check` scans **every** state report in the
  recording (leadership is transient) for a term with two distinct
  leaders. Exit 0 safe / 2 violation / 3 no-leader-elected (discard).
  4 unit tests cover the accumulator.
- Election timeouts are deliberately **adversarially tight** (6–8 ticks,
  latency 2–10 ticks): a production Raft randomizes timeouts over a wide
  range precisely to avoid simultaneous candidacies, but the edge case
  needs two candidates sharing a term. This is a stress schedule, stated
  as such — with wide timeouts (first attempt: 4–8 ticks against 1–60 ms
  latency) the double-vote window almost never opened (1 hit in 360 runs).
  Fault-finding here was schedule-sensitive in exactly the way the
  literature predicts.

## Results (300 seeds each, identical config, only RAFT_FIX differs)

| variant | violating | no-leader discards | rate |
|---|---|---|---|
| `RAFT_FIX=0` lost votedFor | **3 / 300** (seeds 99, 148, 257) | 0 | 1.0% |
| `RAFT_FIX=1` persistent votedFor | **0 / 300** | 0 | 0% |

First hit (seed 99): term 1 elected **two leaders, nodes 0 and 2** (second
leader at op 393 of the recording); 15 crash-restarts occurred in the run.
An earlier 100-seed probe of the same buggy config hit 4/100 (seeds 22,
53, 60, 70) — counts drift run-to-run because cross-process arrival order
is OS-scheduled (the documented Phase-3 limitation); the fixed arm was 0
in every run.

**Reproduced: yes.** The known persistence edge case produces ElectionSafety
violations under Weft, and restoring Figure 3.2's persistence requirement
eliminates them on the same seed set.

## Honest limits

- Leader election only; log replication, commitment, and membership change
  are not implemented, so this validates exactly one dissertation
  requirement, not Raft at large.
- The violation rate (1–4%) is schedule-dependent and required tuning the
  timeout/latency ratio; an untuned harness can easily miss the bug. This
  is evidence about fuzzing yield, not about the bug's reality — the fixed
  arm's 0/300 on identical seeds is the controlled comparison.
- Crash-restart is simulated in-process (state reset), not a real
  `SIGKILL` + re-exec; Weft's orchestrator does support real restarts, but
  the in-process model keeps the recording single-continuous per node and
  is sufficient to express the lost-`votedFor` semantics under test.

## Reproduce

```
scripts/raft-campaign.sh 300                 # RAFT_FIX=0 (buggy)
RAFT_FIX=1 scripts/raft-campaign.sh 300      # fixed
# inspect a hit:
target/linux/release/raft-check target/raft-out-buggy-volatile-vote/seed-99.weftlog
```
