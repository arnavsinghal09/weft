# Scalability Recommendations: Phase 8+ Optimizations

Five prioritized optimization opportunities to improve Weft DST performance and observability for large-scale campaigns (5000+ seeds, multi-day runs).

---

## 1. Broker-Side Latency Histograms (HIGH PRIORITY)

**Problem**: Per-operation latency percentiles cannot be measured in-guest (guest clocks are virtual, seeded, not wall-clock). SCALABILITY.md reports mean (125 µs/op) but not p50/p99/p99.9 distribution.

**Solution**: Instrument the broker to record wall-clock timestamp of every send/recv pair. Compute percentiles broker-side, serialize to a summary JSON (p50, p99, p99.9, max).

**Impact**:
- Enables analysis of network bottlenecks (e.g., "p99 latency is 500µs; we have a hot path").
- Guides timeout tuning for protocol implementations (e.g., Raft election timeouts).
- No guest-side changes needed; purely broker instrumentation.

**Effort**: ~1–2 days (add timestamp fields to broker RPC, accumulate on each op, dump histogram on shutdown).

**Trade-off**: Minimal overhead (<1% broker CPU for histogram updates); small memory footprint (percentile sketch requires O(1) space).

---

## 2. Parallel Campaign Sharding (MEDIUM PRIORITY)

**Problem**: Weft fuzz runs seeds sequentially (1 broker per campaign). A 5000-seed campaign with 2-3 seeds per hour takes ~2000 hours wall-clock on 1 machine.

**Solution**: Spawn multiple broker instances, each running a subset of seeds (e.g., 5 brokers × 1000 seeds = 5000-seed campaign in parallel). Merge recorded logs and violation indices afterward.

**Impact**:
- 5–10× campaign speedup on multi-core systems (if brokers don't contend for I/O).
- Enabled by recording determinism: each seed replayed independently; seeds don't interact.

**Effort**: ~3–5 days (refactor campaign loop, add seed-range CLI args, implement merge logic for violation indices and logs).

**Trade-off**: Requires multiple machines or heavy CPU parallelism; disk I/O may become bottleneck (all brokers write to same log directory).

**Mitigation**: One log directory per broker, merge logs post-campaign.

---

## 3. Log Compaction for Long Campaigns (MEDIUM PRIORITY)

**Problem**: 5000-seed campaign produces ~3.25 GB of gzipped logs. Storing 10 campaigns for diff analysis → 32 GB disk. Archive strategies needed.

**Solution**: Implement log compaction: retain full logs for first 100 seeds (detailed diagnostics), then summarize subsequent seeds (one-line verdict summary: "seed 101 OK", "seed 102 VIOLATION [3 ops]").

**Impact**:
- Reduces long-campaign log storage by ~90% (retain detail, drop redundancy).
- Preserves ability to replay any seed for debugging (full logs of first 100 seeds).

**Effort**: ~2–3 days (add compaction logic to log writer, implement selective read on replay).

**Trade-off**: Cannot replay seed 101+ without re-executing; acceptable trade-off for long campaigns where violation discovery is primary goal.

---

## 4. Per-Node Clock Instrumentation (LOW PRIORITY, HIGH INSIGHT)

**Problem**: Individual nodes' local clocks are seeded and deterministic but not wall-clock. Cannot measure per-node latencies (e.g., "how long does Chord stabilize() take on this node?").

**Solution**: Have each node report wall-clock timestamps alongside seeded clocks. Broker reconciles reports via broker timestamp (provides precise wall-clock correlation).

**Impact**:
- Unlocks per-node performance analysis (latency profiles, hot paths in protocol logic).
- Enables detection of timing anomalies (e.g., one node's stabilize() 10× slower than others).

**Effort**: ~3–5 days (add wall-clock reporting to node implementations, broker reconciliation, analysis tools).

**Trade-off**: Changes to example binaries (raft_node.c, chord_node.c); requires opt-in per protocol.

---

## 5. Fuzz Shrinking Parallelization (LOW PRIORITY)

**Problem**: Delta-debugging is sequential (tries one removal at a time, re-executes). For large violations (10k+ ops), shrinking can take minutes per violation.

**Solution**: Parallelize shrinking: try removing multiple independent op ranges in parallel, merge results.

**Impact**:
- Shrinking time reduction: 5–10× speedup possible on 8-core systems.
- Enables real-time shrinking feedback during long fuzz campaigns.

**Effort**: ~2–3 days (refactor weft-fuzz ddmin loop into work-stealing queue, ensure thread-safe violation index).

**Trade-off**: Complexity (concurrent shrinking must preserve minimality). May not be worth ROI unless violations are very large (>1k ops).

---

## Implementation Roadmap

| Priority | Optimization | Effort | Speedup | Start Phase |
|----------|-------------|--------|---------|------------|
| **HIGH** | Broker-side latency histograms | 1–2d | Observability only | Phase 8 |
| **MEDIUM** | Parallel campaign sharding | 3–5d | 5–10× wall time | Phase 8 |
| **MEDIUM** | Log compaction | 2–3d | 90% storage reduction | Phase 8 |
| **LOW** | Per-node clock instrumentation | 3–5d | Per-node diagnostics | Phase 9 |
| **LOW** | Shrinking parallelization | 2–3d | 5–10× shrink time | Phase 9 |

---

## Conclusion

**Phase 7 (current) has delivered deterministic simulation testing for protocol validation.** The five recommendations enable:

1. **Phase 8 (next)**: Broker observability + multi-seed parallelism → production-scale campaigns.
2. **Phase 9+**: Per-node diagnostics + advanced shrinking → deep performance analysis.

All recommendations are **backwards compatible**; can be implemented incrementally without breaking existing workflows.
