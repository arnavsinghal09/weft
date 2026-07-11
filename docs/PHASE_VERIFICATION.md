# Phase 1–6 Verification

Reverification of core Weft DST properties from Phase 1 through Phase 6, conducted as part of Phase 7 extended validation. This is a targeted spot-check against the claims each phase made when it shipped (one or two representative runs per property, plus the full workspace test suite) — not an exhaustive audit. It re-confirms the specific properties listed below; it does not certify the absence of unrelated defects.

## Summary

✓ **Phase 1: Seed Determinism** — Entropy program with same seed produces byte-identical outputs. Verified: seed 42 ×2 hash-match, seed 43 differs.

✓ **Phase 2: Race Detection** — Weft detects data races when schedule amplifies concurrency. Race control achieved via network latency tuning (uniform:0-1 triggers the race; higher latencies avoid it). Harness properly classifies verdicts.

✓ **Phase 3: Message Delivery & Partitions** — Broker enforces linearization order and models network faults (latency, loss, partitions). Partition heal verified via scenario events `activate_partition`/`clear_partition` in weft-net broker integration tests.

✓ **Phase 4: Deterministic Recording** — Weft-log v1 (gzip, seedable replay) captures broker-linearized events. Per-node clocks are deterministic (seeded). Recording size: ~0.6–0.7 MB per Chord seed, ~0.47 MB per Raft seed.

✓ **Phase 5: Replay Identical** — Recording replay is byte-for-byte deterministic. Verified: pingpong seed 99 replayed ×10 yields identical stream digest every time. Live-run drift (cross-process arrival order) documented as a Phase 3 limitation.

✓ **Phase 6a: Fuzzing** — Weft fuzz CLI exits with 0 (no violations) on ci.json, exit 2 (violations found + reproducers emitted) on demo.json. Fuzz exit codes: 0 safe, 2 violation, 3 discard (uninformative), 1 config error. Shrinker emits `shrunk : X → Y ops in N execution(s)` deterministically.

✓ **Phase 6b: Robustness** — 10,000 deterministic SplitMix64-mutated scenario inputs (byte flips, truncation, garbage, duplication) never panic the Scenario parser. Parser robustness test passes in 0.12s, replacing the originally-planned cargo-fuzz target (not delivered; documented as doc-gap).

✓ **Phase 6c: Sanitizers** — ASan (Address Sanitizer) and UBSan (Undefined Behavior Sanitizer) configured and passing on native binaries (entropy, prodcons) and under weft-shim (Linux/Docker). TSan (Thread Sanitizer) positive control verified: race_bank is properly flagged. Negative controls (prodcons) clean.

## Workspace Test Summary

```
cargo test --workspace --release:
✓ weft-abi: 3 tests passed
✓ weft-chord: 7 tests passed
✓ weft-dst: 10 tests passed
✓ weft-fuzz: 3 tests passed
✓ weft-net: 6 tests passed
✓ weft-raft: 3 tests passed (new: ElectionSafety)
✓ weft-replay: 3 tests passed
✓ weft-scenario: 16 tests passed (incl. 10K parser robustness)
✓ weft-shim: 6 tests passed

Total: 57 unit tests, all passing.
```

**Correction (2026-07-11):** the `weft-shim: 6 tests passed` line above
cannot have described a completed `sched_harness` run:
`threads_that_exit_at_different_times_all_join` contained a latent
out-of-bounds index (worker tids are 1..=N, its result array was indexed
0..N-1) that made it hang deterministically on every platform since Phase 4,
masked by `cargo test`'s fail-fast ordering. The test and the harness's
panic handling are fixed; the suite now genuinely passes (6/6, verified ×3
on Linux).

## Known Limitations & Honest Assessment

**Phase 3 Limitation (Live-Run Drift):** Cross-process arrival order is re-rolled on each live execution (OS-scheduled, non-deterministic). Campaign verdicts are statistical, not seed-for-seed. Recording replay is identical (Phase 5), but two consecutive live runs of the same seed may reach different verdicts. Documented as intentional and irreducible without kernel-level determinism enforcement.

**Phase 4 Gap (Fuzzer Target):** Originally planned as `cargo +nightly fuzz 10K` — never delivered due to Fuzz compiler / rustc mismatch. Replaced with deterministic parser_robustness sweep (10,000 mutated inputs, SplitMix64 PRNG, no LLM). Both achieve the goal (panic-free parser), with the deterministic version being more reproducible. Classified as **doc-gap** (gap documented, not hidden).

**Phase 6 Sanitizers (Guest Clocks):** ASan/UBSan run cleanly on the shim. TSan positive control works. Per-operation latency percentiles cannot be measured in-guest (clocks are virtual under the shim); broker-side instrumentation is a recommended future optimization for Phase 8 (documented in SCALABILITY_RECOMMENDATIONS.md).

## Conclusion

Phases 1–6 core claims verified:
- Seed determinism holds (Phase 1).
- Race detection works (Phase 2).
- Partitions heal (Phase 3).
- Recordings are reproducible (Phases 4–5).
- Fuzzing finds violations and shrinks them (Phase 6a).
- Parser is robust (Phase 6b).
- Sanitizers clean or catch races as intended (Phase 6c).

No bugs found in the properties re-checked here. This is a spot-check, not
an exhaustive audit — see [LIMITATIONS.md](../LIMITATIONS.md) for the full,
separately-maintained list of known coverage gaps and guarantee boundaries
across the whole project; those are documented as limitations, not treated
as bugs, because each is either an intentional scope boundary or an
unimplemented (not broken) feature.
