# Formal cross-validation of the dynamic findings

Every violation the dynamic engine ever recorded (106 Chord, 3 Raft) was
replayed against an explicit-state formal model of the same protocol under
**synchronous execution and perfect failure detection** — the assumptions of
the source papers' analyses — and classified:

- **BOTH_CONFIRM** — the model reaches the *same invariant class* of
  violation under the *same fault schedule* (some interleaving of protocol
  steps). The dynamic finding is not an artifact of the harness's
  asynchrony.
- **DYNAMIC_ONLY** — the model **exhaustively** cannot reach that class
  under that schedule. The dynamic violation depends on something the model
  excludes by construction — for these protocols, detection latency.
- **MODEL_ONLY** — the model reaches a violation class the dynamic
  campaigns never produced. Possible dynamic-engine gap; see the honest
  interpretation below before reading it that way.
- **UNDECIDED** — the checker hit its state budget before exhausting.
  (None occurred; the budget was 20M states/run and the largest run needed
  12.77M.)

Machinery: [stateright](https://crates.io/crates/stateright) 0.30 BFS with
state deduplication; models in `crates/weft-chord/src/chord_model.rs` and
`crates/weft-raft/src/raft_model.rs`; classification harnesses
`chord-oracle` / `raft-oracle`. Fault schedules (join/fail sequences,
restart sequences) are extracted from each recording's state reports and
replayed as a **fixed order**, with all protocol-step interleavings around
them explored. Invariant definitions are the same code path shape as the
dynamic checkers (`AtLeastOneRing` / `AtMostOneRing` / `OrderedRing` /
`ConnectedAppendages`; `ElectionSafety`), evaluated over ground-truth
liveness instead of reported liveness.

## Results

### Chord — 106 recorded violations, one row per campaign arm

| arm | dynamic violations | model verdict | states/run |
|---|---|---|---|
| `CHORD_FIX=0` (original 2001) | 55 × AtLeastOneRing | **55/55 BOTH_CONFIRM** | 7.7k–1.2M |
| `CHORD_FIX=0` | 2 × ConnectedAppendages | **2/2 BOTH_CONFIRM** | — |
| `CHORD_FIX=1` (stabilize-only fix) | 39 × AtLeastOneRing | **39/39 BOTH_CONFIRM** | — |
| `CHORD_FIX=1` | 2 × ConnectedAppendages | **2/2 BOTH_CONFIRM** | — |
| `CHORD_FIX=2` (full liveness discipline) | 8 × AtLeastOneRing | **8/8 DYNAMIC_ONLY** (exhaustive) | — |

### Chord — MODEL_ONLY sweep (all schedules of the scenario shape, exhaustive)

| fix level | AtLeastOneRing | ConnectedAppendages | OrderedRing / AtMostOneRing | states |
|---|---|---|---|---|
| 0 | reachable | reachable | **reachable** | 67k |
| 1 | reachable | reachable | **reachable** | 23k |
| 2 | unreachable (exhaustive) | unreachable (exhaustive) | **reachable — MODEL_ONLY** | 12.77M |

### Raft — 3 recorded violations + controls

| run | model verdict | states |
|---|---|---|
| seeds 99, 148, 257 vs `RAFT_FIX=0` model | **3/3 BOTH_CONFIRM** | 152k–227k |
| same 3 schedules vs `RAFT_FIX=1` model | exhaustively safe (no violation reachable) | 2.7M–3.6M |
| exhaustive, fix 0, ≤3 restarts, terms ≤3 | violation reachable | 237k |
| exhaustive, fix 1, ≤3 restarts, terms ≤3 | **unreachable (exhaustive within bounds)** | 6.77M |

## Honest interpretation, per bucket

**BOTH_CONFIRM (101 of 109).** Every level-0 and level-1 Chord violation,
and every Raft violation, is reproducible in a model that has *no* network,
*no* message delay, and *perfect* failure detection — under the very fault
schedule the dynamic run drew. This is the strongest available answer to
"is the harness manufacturing these bugs?": no; the same faults break the
same invariants in the papers' own execution model. Caveat kept explicit:
the model confirms *∃-interleaving* reachability, not that the dynamic
run's specific interleaving is the one the model found; and the model's
step granularity (stabilize + notify atomic, grant + count atomic) is
coarser than the harness's real message passing, which makes confirmation
*conservative* — a coarser model finding the bug means a finer one would
too, but not vice versa.

**DYNAMIC_ONLY (8 of 109).** All eight `CHORD_FIX=2` residual violations.
The model, given each recording's exact join/fail schedule, exhaustively
proves no interleaving of the fully-disciplined protocol reaches an
`AtLeastOneRing` violation under perfect detection. This upgrades
LEVEL_2_RESULTS.md's trace-based argument ("every residual required an
in-flight death notice") from *forensic* to *exhaustive*: the 1.8% tail is
not merely unexplained-by-the-traces — it is **impossible without
detection latency**. These eight are, as previously documented, evidence
about dynamic testing against real networks, not about the protocol.
Nothing ambiguous survived: 0 UNDECIDED.

**MODEL_ONLY (one violation class, no dynamic counterpart).** The
`OrderedRing`/`AtMostOneRing` class is reachable in the model **at every
fix level, including full liveness discipline**, yet the dynamic campaigns
produced zero instances in 1,500 recorded runs. The model's witness: a node
legitimately holding *itself* as `succ2` (normal in a 2-ring) degenerates
into a disjoint self-ring when its first successor dies — liveness checks
never fire because every adopted pointer was live when adopted. Two honest
readings, both stated:

1. *Possible dynamic-engine gap.* The dynamic campaign's scenario timing
   (join early / fail late, jittered) may make the required
   interleaving — reconcile picking up a self-pointer in a shrunken
   ring — rare enough that 500 seeds per arm never sampled it. Until a
   dynamic seed reproduces it, we cannot rule out that some harness
   behavior (e.g., report cadence, DEAD-broadcast ordering) suppresses the
   window entirely. **Flagged as a possible gap; not resolved.**
2. *Sampling, not suppression.* The model explores *all* interleavings;
   500 seeds sample a vanishingly small fraction. A class needing a
   precise three-event window can easily have zero mass in the sampled
   distribution while being reachable. The Raft study already measured
   exactly this effect (0/100 hits under loose timeouts vs 4/100 tight).

What this bucket does **not** show: it does not show the dynamic engine
*missed a bug it should have caught* — no specific seed is known whose
recording contains the mechanism and whose verdict was OK. Distinguishing
readings 1 and 2 needs a targeted dynamic campaign (scenario shaped to the
model's witness path), which has not been run. It also independently
corroborates the published caveat that level 2 is "the *intent* of Zave's
best version" (chord-spec.md) rather than her proven-correct protocol: her
TSE 2017 correct version changes more than liveness checks, and this class
is presumably part of why.

**On the Raft fix-1 rows.** Running the *fixed* model against the buggy
arm's schedules answers a falsification-control question — "would
persistence have prevented these exact fault sequences?" — and the answer
is exhaustively yes (2.7–3.6M states each, no violation). The exhaustive
fix-1 sweep is a *bounded* safety result: no ElectionSafety violation with
≤3 restarts and terms ≤3. It is not a proof of Raft, and the bound is the
honest edge of the claim.

## Reproduce

```
cargo build --release -p weft-chord -p weft-raft
target/release/chord-oracle 0 6 target/chord-out-orig/seed-*.weftlog
target/release/chord-oracle 2 6 target/chord-out-fix2-full/seed-*.weftlog
target/release/chord-oracle --exhaustive 2 6
target/release/raft-oracle 0 target/raft-out-buggy-volatile-vote/seed-*.weftlog
target/release/raft-oracle --exhaustive 1 3
```

Model unit tests (`cargo test -p weft-chord -p weft-raft`) pin the three
load-bearing facts: level 0 breakable under perfect detection, level 2's
ring-loss exhaustively excluded, level 2's split class still reachable;
fix-0 Raft breakable with one restart, fix-1 exhaustively safe on the same
schedule.

## Limits of this cross-validation (read before citing it)

- The models inherit the **scenario shape** (6 members, 3-node base, r=2,
  m=6; 5-server election) — conclusions are about that shape, not Chord or
  Raft in general.
- Schedule extraction reduces a recording to its join/fail (restart) order;
  message-level timing is deliberately abstracted to nondeterminism. Two
  recordings with the same event order are the same model instance.
- "Exhaustive" always means *within the stated bounds* (term bounds for
  Raft; the fixed schedule plus protocol quiescence for Chord). Bounds are
  printed by the tools and quoted in the tables.
- The atomicity granularity (noted above) is the fidelity trade made by
  Zave's own shared-state abstraction, and it cuts in one direction:
  coarsening **removes** interleavings. BOTH_CONFIRM verdicts are therefore
  robust (a finer model contains every path the coarse one found). The
  DYNAMIC_ONLY verdicts are **not** automatically granularity-robust — a
  finer synchronous model has more interleavings and could in principle
  reach states the coarse one cannot. For the eight level-2 residuals the
  conclusion additionally rests on a granularity-independent structural
  argument: with ground-truth liveness checks, every adopted pointer is
  live at adoption; with the failure-assumption gate enforced at every
  death, every live member retains at least one live successor-list entry
  afterward; therefore `bestSucc` remains total on live members after the
  schedule, a total functional graph on a finite set always contains a
  cycle, and `AtLeastOneRing` cannot be violated — at any granularity. The
  model check is a mechanized instance of that argument, not its only
  support.
