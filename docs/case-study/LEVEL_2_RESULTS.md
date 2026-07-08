# Chord liveness-discipline experiment: level 0 / 1 / 2 results

**Question resolved here** (carried from PROGRESS.md): is the ~14% violation
rate of the original 2001 protocol Zave's documented `AtLeastOneRing` flaw,
or an artifact of this harness?

**Answer: it is the real flaw, with a small, mechanistically-explained tail
attributable to one stated modeling divergence (asynchronous failure
detection).** Evidence below.

## The experiment

Three protocol variants, identical in every other respect, each swept over
the same 500 fault seeds (`net=latency=uniform:1000-60000`, m=6, 6 members =
3-node stable base + 3 appendages, 45 ticks + quiescent repair tail):

- `CHORD_FIX=0` — the original 2001 protocol: **no** liveness check on any
  pointer adoption (the version Zave proved incorrect).
- `CHORD_FIX=1` — liveness check on stabilize's adoption of the successor's
  predecessor only (the single correction referenced from [PODC]).
- `CHORD_FIX=2` — full liveness discipline: stabilize, reconcile, update
  promotion, and the GETSUCC responder all refuse **known-dead** nodes (the
  intent of Zave's "best version").

Runs whose failure schedule broke the papers' precondition (a failure that
strands some node with no live successor at the moment of death) are
discarded by `chord-check` (exit 3), so violation counts are over valid runs
only.

## Results (one campaign, 2026-07-07, single container run)

| variant | violating | discarded | valid | rate |
|---|---|---|---|---|
| 0 original | 57 / 500 | 96 | 404 | **14.1%** |
| 1 stabilize-only fix | 41 / 500 | 60 | 440 | **9.3%** |
| 2 full discipline | 8 / 500 | 48 | 452 | **1.8%** |

Caveat (documented Phase-3 limitation): cross-process arrival order is
OS-scheduled, so counts drift run-to-run; comparisons are statistical, not
seed-for-seed. Two earlier 500-seed runs of variant 0 gave 57 and 74; of
variant 1 gave 30 and 41. The ordering `orig ≫ fix1 ≫ fix2` held in every
run.

## Root cause of the level-0 violations (traced, seed 17)

`chord-trace target/chord-out-orig/seed-17.weftlog` (session-2 trace,
recorded in PROGRESS.md): the full ordered 6-ring forms; appendages 25, 4,
46 fail (assumption held at each death); then in the quiescent tail the
surviving base nodes **adopt dead appendages as successors via stabilize
with no liveness check, discarding their live pointers** (op 855: node 22
sets succ=25, succ2=46, both dead, dropping live 43; op 858: node 43 sets
succ=25). All bestSucc become NONE; `AtLeastOneRing` breaks permanently at
op 855. This is exactly the mechanism of Zave's Figure 6 (CCR 2012): a
length-2 successor list cannot cover the gap once a dead node is adopted.

The level-1 residuals (traced, seed 16, session 2) come from the OTHER
unchecked adoptions — reconcile and update — which are equally faithful to
the 2001 pseudocode and untouched by the stabilize-only fix.

## Root cause of the level-2 residual (traced, seed 120)

`chord-trace target/chord-out-fix2-full/seed-120.weftlog`:

- op 899: node 43 holds succ=46, **succ2=1 (live)** — a healthy state.
- Node 46 fails (op 1003), node 4 fails (op 1051).
- op 1069: node 43 reports succ=46, **succ2=4** — reconcile overwrote its
  live succ2=1 with node 4, learned from a GETSUCC reply produced before
  the replier knew 4 was dead, and accepted because **43's own liveness
  check consulted local knowledge and 4's DEAD notice was still in
  flight**. Both pointers now dead; level-2 discipline (correctly) refuses
  to promote a known-dead succ2; bestSucc=NONE; permanent break at op 1069.

Every step is protocol-faithful **given each node's local knowledge**. The
residual mechanism is therefore not a harness bug (no node ever drops a
pointer it knows to be live); it is the divergence between this harness's
real asynchronous failure detection (DEAD notices ride the same delayed
network as everything else) and Zave's model, in which liveness is global
state visible to every operation instantly (perfect failure detection).
With message latency up to 60 virtual ms and failure ticks only ~15 virtual
ms apart, a ~1.8% tail from in-flight staleness is unsurprising, and it
disappears by construction in Zave's synchronous model.

## Falsification statement

The claim "the observed violations are Zave's unchecked-adoption flaw" was
falsifiable and survived three tests:

1. **Mechanism trace**: the level-0 break (seed 17) reproduces Figure 6's
   mechanism step-for-step (dead-node adoption via stabilize, r=2 gap).
2. **Partial fix**: guarding only stabilize's adoption reduces violations
   (57→41 valid-rate 14.1%→9.3%) but does not eliminate them — as predicted,
   because reconcile/update adoption is equally unchecked in the original.
3. **Full fix**: guarding every adoption against known-dead nodes collapses
   violations 57→8 (14.1%→1.8%), and each traced residual requires
   in-flight death notices, impossible in Zave's perfect-detection model.

This reproduces Zave's arc dynamically: original protocol incorrect →
single published fix insufficient alone → full liveness discipline
(approximately) correct — "approximately" quantified at 1.8% under
asynchronous detection, with the exact mechanism of the divergence traced.

## Minimal reproducer

- Original flaw: `chord-trace target/chord-out-orig/seed-0.weftlog 6` —
  node 43 holds live succ2=1 at op 840, then adopts dead node 4 into both
  pointers via unchecked reconcile/update (op 1254), leaving the live chain
  1→22→43 unable to close the ring. Same class as the seed-17 trace
  (Figure-6 stabilize adoption) recorded in PROGRESS.md; seed 17 was not a
  hit in this run (count drift), so its recording no longer exists on disk.
- Detection-latency residual: `chord-trace
  target/chord-out-fix2-full/seed-120.weftlog 6` (ops 899→1069).

Recordings are self-contained (weft-log v1); the checker and tracer rebuild
the full pointer timeline from the recording alone.
