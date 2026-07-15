# Roadmap

What's actually planned, in rough priority order, and — just as important —
what is explicitly not. Both halves are promises about scope, not just a
wishlist. If something you need isn't in either list, open an issue; it
means we haven't thought about it yet, not that it's rejected.

Compatibility impact of anything here is governed by
[VERSIONING.md](VERSIONING.md).

## Near-term (next)

1. **Broker-side latency histograms.** Per-operation latency currently
   can't be measured in-guest (clocks are virtual). Instrument the broker
   to timestamp every send/recv pair and emit p50/p99/p99.9 on shutdown.
   No guest-side changes. Details:
   [docs/SCALABILITY_RECOMMENDATIONS.md](docs/SCALABILITY_RECOMMENDATIONS.md) §1.
2. **Parallel campaign sharding.** `weft fuzz` sweeps seeds sequentially
   through one broker; a 5000-seed campaign is currently a multi-hour
   single-machine run. Spawning N brokers over disjoint seed ranges and
   merging violation indices afterward is a 5–10× wall-time win with no
   correctness risk (seeds are independent by construction). Same doc, §2.
3. **TCP support in the simulated network.** Today the broker only
   diverts `AF_INET`/`SOCK_DGRAM` — this is about what *targets* may
   speak, distinct from the multi-host broker transport (which already
   runs over TCP). Most real services speak TCP; extending the wire
   protocol and fault model to stream sockets is the single biggest
   expansion of what Weft can test without touching the shim's core
   determinism machinery.
4. **`weft hostd` — remote spawning for multi-host runs.** The windowed
   multi-host layer is implemented and validated (deterministic Chord and
   Raft across containers — LIMITATIONS.md §3(c′)), but each host runs its
   own `weft run --listen`/`--broker`/`--spawn` by hand. The design's host
   agent (docs/MULTI_HOST_ARCHITECTURE.md "Components") — one `weft hostd`
   per host receiving spawn specs over a control channel, master-side
   `--hosts A:PORT,B:PORT` — turns that into one command. Prerequisites
   (real per-host ids, goodbye/crash detection) have landed.
5. **ENOSPC fault injection.** `WEFT_ENOSPC_BYTES` is reserved in the ABI
   and referenced in the file-fault hook but not wired up
   (LIMITATIONS.md §2). Byte-tracking already exists; this is finishing
   what fsync-lies started.
6. **Fold live-target fuzzing into `weft fuzz`.** The fuzzer currently
   sweeps the broker's pure decision core; sweeping real `weft run --record`
   clusters (the shim path) is done by hand today
   (`scripts/*-campaign.sh`). Unifying these gives shim-path campaigns the
   same dedup-and-shrink treatment broker-core campaigns already have.

## Medium-term

7. **seccomp-unotify syscall-boundary interception.** The single change
   that would close the largest gap in what Weft can see: static binaries
   and Go's raw-syscall runtime currently escape interception entirely and
   silently (LIMITATIONS.md §1). A seccomp-notify supervisor answering from
   the same pure decision engine would cover both, at the cost of a
   context switch per intercepted call. Sketched in
   [docs/architecture.md](docs/architecture.md#future-work); not started.
8. **Log compaction for long campaigns.** 5000 recorded seeds is currently
   ~3.25 GB; retaining full detail for the first N seeds and summarizing the
   rest is a ~90% storage win for archived campaigns.
   [docs/SCALABILITY_RECOMMENDATIONS.md](docs/SCALABILITY_RECOMMENDATIONS.md) §3.
9. **Per-node clock instrumentation.** Lets a protocol implementation
   report wall-clock timestamps the broker can reconcile, unlocking
   per-node performance analysis (e.g. "which node's `stabilize()` is slow")
   that guest-side virtual clocks can't provide today. Same doc, §4.
10. **Shrinking parallelization.** The ddmin loop is sequential; for
   10k+-op violations, parallel candidate removal is a further 5–10× on
   multi-core hardware. Same doc, §5.
11. **macOS interception port.** `weft replay`/`weft fuzz` already work on
    macOS (pure computation); `weft run`'s shim does not build there. Porting
    `LD_PRELOAD` → `DYLD_INSERT_LIBRARIES` interposition is scoped in
    [docs/comparison.md](docs/comparison.md#what-a-non-linux-port-would-require)
    as a rewrite of the hook-declaration layer, not new design work — but it
    ranks below item 7, since recording-exact replay already works on macOS
    and static/Go coverage on Linux is the bigger gap.
12. **1.0 and the deprecation-window policy.** Once the scenario DSL, log
    format, and CLI surface are stable enough to commit to,
    [VERSIONING.md](VERSIONING.md) §5's minor-version deprecation window
    takes effect. No date attached; gated on real usage surfacing which
    parts of the surface are actually load-bearing.

## Not planned

- **Windows support.** Not a port of the existing shim — no preload
  mechanism, no shared syscall surface, no shared process-orchestration
  model. Would be a new sibling implementation reusing only the
  platform-independent crates. Out of scope until there is a concrete
  reason to fund it as its own project.
- **Whole-VM / hypervisor-level interception.** That is a different,
  harder problem with a different, better answer already on the market —
  [Antithesis](https://antithesis.com/). Weft's bet is the libc boundary;
  going lower means abandoning "point it at a binary you already have,"
  which is the whole premise. See [docs/comparison.md](docs/comparison.md).
- **A sim-first runtime you build against from day one.** That is
  TigerBeetle's and FoundationDB's bet, and it is a *better* guarantee than
  Weft's if you control the codebase from its first commit. Weft is
  deliberately the retrofit tool for the other case. Not a roadmap gap —
  a different product.
- **Formal correctness proofs.** Weft falsifies specific mechanisms under
  specific schedule distributions (see the Chord and Raft studies); it will
  never produce a proof of absence of bugs. If you need that, you want a
  model checker (TLA+, etc.), not a dynamic testing tool — the two compose,
  but Weft won't grow into one.
- **Byzantine fault injection** (nodes lying or behaving adversarially,
  beyond fsync-lies and torn writes). The fault model targets realistic
  crash/omission/timing faults; adversarial-node simulation is a
  meaningfully different tool with different threat assumptions and isn't
  on this roadmap.
- **CPU-time clock virtualization** (`CLOCK_PROCESS_CPUTIME_ID` etc.).
  These currently return virtual-monotonic time rather than modeled CPU
  consumption (LIMITATIONS.md §2); modeling real CPU accounting under a
  cooperative userspace scheduler is a different kind of simulator than
  Weft is trying to be.
