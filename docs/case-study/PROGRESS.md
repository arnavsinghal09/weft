# Phase 7 progress (checkpoint before /compact)

## Done
- Target chosen + reasoning: docs/case-study/target-selection.md. User directed:
  ORIGINAL 2001 Chord, REAL OS processes over REAL UDP through the shim+broker
  (not in-process sim), r=2, stable base = r+1 = 3, appendages join/fail,
  invariants = Zave's 7 (we check the 4 correctness-critical ones + ring set).
- Primary sources READ IN FULL (not paraphrase): Zave CCR 2012
  (chord-ccr.pdf, extracted to scratchpad/zave-ccr.txt) + arXiv:1502.06461.
  Verbatim invariant defs + counterexamples transcribed: docs/case-study/chord-spec.md.
  Key: `OneOrderedRing` Alloy predicate (Fig 9), Fig-6 AtLeastOneRing gap at r=2.
- De-risked Docker/Linux real-UDP-record path with pingpong: WORKS
  (weft run --net --record → valid weft-log → weft replay identical, 16 ops).
- chord_node.c (examples/chord/): faithful original protocol — stabilize adopts
  successor's pred with NO liveness check (2001 has none; part of bug); reconcile/
  update/flush SEPARATE jittered periodic events; fail-silent + DEAD broadcast;
  per-node RPT state reports to observer node; usleep virtualized for pacing.
- weft-chord crate: Snapshot::from_log reconstructs final config from RPT
  datagrams; check() evaluates AtLeastOneRing/AtMostOneRing/OrderedRing/
  ConnectedAppendages exactly per Alloy defs; ChordInvariant = streaming
  weft_replay::Invariant for the shrinker. 7 unit tests green.
  failure_assumption_held() reconstructs deaths chronologically and DISCARDS
  seeds that violate the papers' precondition (would strand a node with no live
  successor) — chord-check exit 3 = DISCARD, 2 = VIOLATION, 0 = OK.
- Campaign infra: examples/chord/campaign.sh (in-container) + scripts/chord-campaign.sh (host wrapper).

## Campaign result (500 seeds, net=latency=uniform:1000-60000, m=6, ticks=45, members=6/base=3)
tested=500  violating=57  assumption-discards=103  errors=0  elapsed=195s
Violating seeds: 0 14 17 22 23 37 52 73 76 79 80 93 94 111 122 127 129 135 143
160 180 209 218 220 223 229 243 258 259 263 272 274 281 293 297 301 309 314 315
318 327 328 329 349 350 357 410 412 431 437 443 445 450 459 468 477 483
First hit (seed 0): AtLeastOneRing violated — no cycle; nodes 1,22,43 alive but
43.succ=43.succ2=4(dead) → bestSucc NONE; appendages 25,4,46 all FAILED.

## UPDATE (session 2): trace + falsification done — result is NUANCED, not clean
### Tools built
- `chord-trace` binary (crates/weft-chord/src/bin/chord_trace.rs): walks the
  recorded RPT stream in linearization order, dedups per-tick chatter, prints
  each node's succ/succ2/prdc changes + ring set, flags the op where
  AtLeastOneRing breaks permanently. Runs on host (pure Rust over recordings).
- CHORD_FIX is now a LEVEL in chord_node.c: 0=original (no liveness checks),
  1=liveness check on stabilize adoption only, 2=FULL liveness discipline
  (stabilize + reconcile/SUCC + update promotion + GETSUCC responder all
  refuse known-dead nodes). campaign.sh labels out dirs orig/fix1-stabilize/
  fix2-full and exports CHORD_FIX to children.

### Trace of seed-17 (original protocol) — matches Fig 6 mechanism
Full 6-node ordered ring forms (op 536: [1,4,22,25,43,46], 1→4→22→25→43→46→1).
Appendages 25,4,46 fail (ops 563,806,844). Assumption HELD at each death
(every live node still had a live successor). THEN in the quiescent tail the
three surviving base nodes ADOPT DEAD appendages as successors and discard
their live pointers (op855: node22 succ=25 succ2=46 both dead, dropping live
43; op858: node43 succ=25). Final: all base nodes point only at dead nodes,
bestSucc=NONE everywhere, ring gone permanently at op855. This is Fig-6's
mechanism (stabilize adopts a node with NO liveness check; that node is/becomes
dead; length-2 list can't cover the gap).

### Falsification (500 seeds each, IDENTICAL config, only CHORD_FIX differs)
NOTE: counts drift run-to-run because cross-process arrival order is
OS-scheduled (documented Phase-3 limitation) — so this is a STATISTICAL
comparison, not seed-for-seed. First run of orig gave 57; this run gave 74.
- CHORD_FIX=0 (original):        violating=74/500 (88 discards → 412 valid, ~18%)
- CHORD_FIX=1 (stabilize only):  violating=30/500 (54 discards → 446 valid, ~6.7%)
- CHORD_FIX=2 (full discipline): NOT YET RUN — this is the next step.

The stabilize fix roughly HALVES violations (74→30) but does NOT eliminate
them. Per the user's branch rule, persistent violations ≠ clean confirmation.

### Trace of seed-16 (FIXED level-1) — why residuals remain
Final state: node1 succ=22 succ2=43 (bestSucc=22 LIVE), node22 succ=43 succ2=4
(bestSucc=43 LIVE), node43 succ=4 succ2=4 (both dead, bestSucc=NONE). So 1→22→43
is a LIVE chain but 43 fails to point back to 1 to close the ring. 43 adopted
dead node 4 via reconcile/update (NOT stabilize — level-1 fix only guards
stabilize). reconcile (`SUCC` handler) and update promote dead nodes with no
liveness check in the original — these are ALSO 2001-faithful no-liveness-check
flaws, untouched by level-1.

### Two confounds identified (must keep separate in the writeup)
(A) Zave's real structural flaw: length-2 successor list + adopt-without-
    liveness-check → node stuck with no live successor, unrepairable. FAITHFUL.
(B) Async detection latency: my failure detection is message-delayed (DEAD
    broadcasts ride the faulty network), whereas ZAVE ASSUMES PERFECT
    (instantaneous) FAILURE DETECTION. A node can adopt a not-yet-known-dead
    node, lose its live successor, and get stuck. This mechanism is NOT in
    Zave's synchronous model — it is a genuine divergence of the dynamic
    approach. Latency-only net (no loss) means all DEAD notices eventually
    arrive, so confound (B) is bounded but nonzero.

### HYPOTHESIS for level-2 (full liveness discipline)
If residuals were the OTHER unchecked adoptions (reconcile/update), level 2
should drive violations to ~0 (small tail from confound B). That would
reproduce ZAVE'S ENTIRE ARC: original incorrect (74) → partial fix partial
(30) → full liveness discipline ≈ correct (~0), matching her "the best version
may be correct." That IS a clean, credible, honest result.
If level-2 violations PERSIST materially (>~a handful), there is a STRUCTURAL
harness bug (node loses a reachable live successor for reasons other than
adopting-dead), which must be found & fixed at source before any claim.

### NEXT SESSION: run the level-2 experiment (code is ready)
    docker run --rm -v "$PWD":/work -v weft-cargo-registry:/usr/local/cargo/registry \
      -w /work -e CARGO_TARGET_DIR=/work/target/linux rust:1.84-bookworm bash -c '
        CHORD_FIX=0 SEEDS=500 bash examples/chord/campaign.sh 2>&1 | grep "^\[result\]"
        CHORD_FIX=1 SEEDS=500 bash examples/chord/campaign.sh 2>&1 | grep "^\[result\]"
        CHORD_FIX=2 SEEDS=500 bash examples/chord/campaign.sh 2>&1 | grep "^\[result\]"'
Then branch:
 - level2 ≈ 0  → clean arc; trace a level-0 hit as root cause; write it up as
   real rediscovery, being explicit re confound (B) as a stated limitation.
 - level2 still high → structural harness bug; trace a level-2 hit, find where
   a node drops a reachable live successor, fix in chord_node.c, re-run 0/1/2.
 - genuinely ambiguous → say so in PROGRESS.md + name the additional evidence
   needed (e.g. instrumenting adoption events with the adopter's liveness
   knowledge at adoption time).
C compiles clean (syntax-checked on host). chord_node.c changes are DURABLE.

## OPEN SKEPTICAL QUESTION (must resolve before writing case study — this is the crux)
14% violation rate is HIGH for a "mostly correct" protocol, and in seed 0 the
BASE ring (idents 1,22,43 = node ids 0,1,2) itself broke even though only
APPENDAGES failed. Base node 43 ended with both successors pointing at dead
node 4 and never recovered to its ring successor (should be 1, wrapping).

MUST verify this is Zave's real Chord bug and NOT a harness artifact, e.g.:
 (a) base node adopting a dead/appendage successor and my update/stabilize
     logic never restoring the ring successor (a bug in MY C, not Chord);
 (b) identifier layout creating a degenerate/misordered base ring;
 (c) reconcile pulling a dead succ2 that update then promotes into succ,
     with no path back — which IS the documented mechanism, but I must PROVE
     it matches Fig-6/Fig-5, not just assert it.
Approach: pick the SHORTEST violating recording, trace the exact RPT timeline
of the base node that lost its ring successor, step-by-step, and map each
transition to a specific line of Zave's pseudocode. If it's a harness bug,
fix at the source (Phase 7 brief explicitly requires this) and re-run. If it's
faithful, the trace IS the root-cause section of the case study.
Cross-check: does removing the "no liveness check on stabilize adoption"
(i.e. the correct [PODC] fix) make the violations vanish? If yes, strong
evidence the bug is the real protocol flaw, not my harness.

## Remaining
- Resolve the skeptical question above (investigate seed 0 or shortest hit).
- Shrink a violating recording via Phase 6 (ChordInvariant + weft_fuzz::shrink)
  to a minimal reproducer; confirm shrink preserves the AtLeastOneRing break.
- Root-cause write-up tied to Zave Fig-6/Fig-5.
- Rigorous native-vs-weft benchmark (reproducible methodology) — Chord run
  native (no shim) vs under weft; also determinism check (same seed → same
  final config across repeated runs, incl. across machines if possible).
- Case study docs/case-study/README.md: skeptic-facing, honest about
  dynamic-rediscovery-vs-static-exhaustive, limitations, what was
  impossible-to-find-without-tooling.
- Gates (cargo test/clippy/fmt across workspace incl. weft-chord) + end-of-phase
  graphify . --update --no-viz.

## Key paths
- target/chord-out/ (in container / host mount): seed-*.weftlog for hits, hits.txt
- Build in container: CARGO_TARGET_DIR=/work/target/linux; binaries at
  target/linux/release/{weft,libweft_shim.so,chord-check}
- Run campaign: scripts/chord-campaign.sh [SEEDS]  (env: NET=, MEMBERS=, etc.)
