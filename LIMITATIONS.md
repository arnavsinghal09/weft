# Limitations

This document states exactly what Weft does not do, where its guarantees stop,
and how they can leak. It is maintained with the same rigor as the code: if
you find a boundary that is not listed here, that is a documentation bug —
please report it.

Nothing here is hedged. Where a limitation has a known fix, the fix is named;
where it does not, that is stated too.

## 1. Platform boundaries (exact)

- **Interception requires Linux with glibc and dynamic linking.** The shim is
  an `LD_PRELOAD` shared object; it is built, tested, and CI-gated only
  against **x86-64 Linux / glibc** (container image `rust:1.84-bookworm`,
  glibc 2.36).
- **Statically linked binaries are not intercepted at all.** No PLT, no
  interposition. The run completes, but nothing is deterministic. Weft does
  not currently detect this case and warn — it fails silently. (Planned
  fallback: seccomp-unotify syscall interception; not started.)
- **Go binaries are not supported.** The Go runtime issues raw syscalls,
  bypassing libc entirely. Same silent-failure mode as static linking.
- **musl is untested.** The `open`→`read` random-device path should work and
  `fopencookie` exists on musl, but no CI covers it. Treat musl as
  unsupported until it does.
- **macOS / Windows: the shim does not build.** `weft replay` and `weft fuzz`
  are pure computation and run on every platform; `weft run` interception is
  Linux-only. macOS development happens inside a Docker container
  (`scripts/linux-test.sh`).
- **setuid targets silently escape.** glibc secure-mode strips `LD_PRELOAD`
  on exec into a setuid binary. No detection, no warning.

## 2. Syscall / runtime coverage gaps (exact list)

Interposed surface is enumerated in [docs/architecture.md](docs/architecture.md).
What is **not** covered:

- **Raw syscalls** (`syscall(SYS_getrandom, …)`), inline `rdtsc`/`rdrand`,
  and **vDSO calls by address** rather than through libc symbols: real
  nondeterminism, undetected.
- **`random_r`/`initstate_r`** (glibc reentrant family) and **`arc4random*`**
  (glibc ≥ 2.36): not interposed.
- **CPU-time clocks** (`CLOCK_PROCESS_CPUTIME_ID`, `CLOCK_THREAD_CPUTIME_ID`)
  return virtual-monotonic time, not modeled CPU consumption.
- **`getcpu`, `sched_getcpu`, `times(2)`, `getrusage(2)`, `/proc/*/stat`**:
  not virtualized.
- **PIDs, TIDs, ASLR addresses**: real. A program seeding from `getpid()` or
  hashing pointer values is nondeterministic under Weft today.
- **Signals**: not modeled. Virtual sleeps never observe `SIGALRM`; signal
  delivery order is OS-scheduled.
- **`pthread_rwlock`, `pthread_barrier`, semaphores**: pass through to real
  libc — correct, but contention is OS-scheduled, not seed-scheduled.
- **`pthread_cond_timedwait` ignores its deadline** (modeled as untimed).
  Code relying on the timeout alone to make progress deadlock-detects
  instead of progressing.
- **`pthread_cancel` is not interposed.** A cancelled managed thread dies
  without returning its scheduler token, wedging every other managed
  thread forever (the deadlock detector cannot see it: the dead thread
  still looks runnable). Normal return and `pthread_exit` are handled.
- **`dup`/`dup2`/`fcntl(F_DUPFD)` of a random-device fd** is untracked: the
  duplicate reads real `/dev/null` (EOF).
- **Network coverage is `AF_INET`/`SOCK_DGRAM` (UDP) only.** TCP, Unix
  sockets, `sendmsg`/`recvmsg`, `epoll`/`poll`/`select` on sockets: not
  diverted; they use the real network.
- **Filesystem faults cover `write`/`pwrite`/`fsync`/`fdatasync`
  byte-tracking and fsync-lies.** ENOSPC injection is scaffolded but not
  active (`WEFT_ENOSPC_BYTES` is reserved, unimplemented). No torn-write
  injection at the syscall level yet; `mmap`-based I/O is invisible to the
  shim.

## 3. The determinism guarantee — precise strength and leaks

Three different strengths, often conflated. Weft's docs and this repo use
them precisely:

**(a) Replay of a recording is byte-exact. Always.**
A weft-log plus the seed reconstructs the run: `weft replay` re-executes
against the same pure decision core and verifies an identical stream digest.
Proven 10× in CI (Linux); verified manually on macOS during development —
CI runs `ubuntu-latest` only. This guarantee has no known leaks — the log
*is* the linearization.

**(b) Single-process runs (no `--net`) are output-deterministic** given the
covered surface: same seed ⇒ same timestamps, same random bytes, same thread
interleaving (scheduler on). Leak vectors, in decreasing order of likelihood:
  1. Any coverage gap from §2 the target happens to exercise.
  2. Unmanaged threads (created before shim activation, or via raw
     `clone(2)`) — real OS scheduling.
  3. Pure-compute data races: yield points exist only at interposed calls,
     so a racy region with no libc calls executes atomically per schedule —
     its internal races are never explored *or* detected (they are
     serialized).
  4. `exec` restarts virtual time; a process tree that round-trips state
     through exec sees a discontinuity.

**(c) Multi-process cluster runs (plain `--net`) are NOT seed-deterministic
live.** Which process's syscall reaches the broker first is OS-scheduled.
Once enqueued, every fate and delivery position is deterministic — but the
enqueue order itself is not. Consequences, measured in Phase 7:
  - The same seed reached a different verdict across 10 live Chord runs
    (1 violation / 8 clean / 1 discard).
  - Campaign counts (57/500 vs 41/500 vs 8/500) are **statistical**
    comparisons, not seed-for-seed identities; re-running a campaign moves
    individual counts (57 vs 74 observed across runs) while preserving the
    ordering.
  - The escape hatch is (a): record the run you care about; the recording
    replays exactly, forever.
This is the single most important limitation to understand before using
Weft on a distributed system **without `--window`**.

**(c′) The windowed multi-host broker (`--net … --window <NS>`) removes the
enqueue-order nondeterminism** by sealing virtual-time windows and ordering
each window's ops by a seed-derived key instead of arrival
(docs/MULTI_HOST_CLOCK_PROTOCOL.md). It is **validated on blocking and
poll-drain workloads, with a hard precondition**:
  - Validated: a 2-node request/reply (`pingpong`) is live and byte-identical
    across 10 runs and seed-sensitive; a 2-sender ordering workload
    (`netsched`) is identical across 6 runs — both single-host, multi-process,
    in one container (`net_e2e::windowed_multihost_pingpong_is_live_and_deterministic`).
  - **Precondition: lookahead (the network's minimum latency `L_min`) must be
    ≥ the window width.** With `L_min < W` a blocking receiver's reactivation
    bound stalls its own delivery (the L=0 deadlock); the orchestrator warns
    but does not abort. Reliable (`--net ""`) and exponential latency have
    `L_min = 0`, so windowed request/reply needs `latency=fixed:N` or
    `uniform:LO-HI` with `LO ≥ W`.
  - Validated: the 7-node Chord case study (`examples/chord/chord_node.c`,
    6 members + observer), which drains its socket with `recvfrom(MSG_DONTWAIT)`
    and paces itself with virtualized `usleep`, is deterministic under
    `--window 1000 --net latency=uniform:1000-60000 --record`: across 6 runs the
    `chord-check` verdict is identical (same exit code, same verdict body) and
    every node receives a byte-identical message stream. This closes the §4.2
    "polling-loop nondeterminism" the design flagged: a non-blocking `recvfrom`
    now advances the connection's frontier and returns only messages sealed
    below the guest's *virtual* time (`EAGAIN` once the pop-horizon reaches it),
    so the visible message set is a pure function of virtual time rather than of
    how far windows have sealed in real time. (`chord-check`'s human-readable
    node listing still prints in HashMap order, so the *unsorted* render text
    varies run to run — cosmetic to that tool, not a determinism defect; the
    verdict and its sorted body are stable.)
  - Failure modes (design §8), implemented and tested: **F1** a node killed by
    a signal mid-window discards the run (exit 3); **F3** `--watchdog <SECS>`
    aborts-and-discards on real-time no-progress (its firing is inherently
    nondeterministic, so it only ever discards); **F4/F5** a rejected sequencer
    op (non-monotone clock, late op, reconnect splice) is latched as a protocol
    violation and the run is discarded even if every node then exits 0; **F6**
    terminal quiescence (every connected guest blocked, nothing in flight) is
    detected deterministically and discards instead of hanging. Sealing also
    waits for a **join barrier** (all `--nodes` ids said Hello) so node startup
    order — OS scheduling — cannot race the horizon past a late joiner.
  - Validated across containers: the same 7-node Chord scenario split over
    two Docker containers on a bridge network (`--listen`/`--broker`/`--spawn`,
    nodes 0–2 with the broker on one, 3–6 on the other) produced the
    **byte-identical `chord-check` verdict as the single-host windowed run**
    (same sorted-verdict hash, 8/8 two-container runs vs 6/6 single-host) —
    the topology does not leak into the result. Measured max clock skew
    ~2.0 ms (`--stats`). Killing the remote container mid-run discards the
    run (exit 3) via the goodbye protocol: a windowed connection that ends
    without the shim's clean `Goodbye` (sent on `close(2)` and via `atexit`,
    both skipped by signal death) is latched as a crash, F1.
  - **Recording determinism boundary:** in a windowed recording the *send
    sequence* (the sealed linearization every delivery derives from) is
    identical across same-seed runs modulo broker connection ids, which are
    accept-order aliases — a node's stable identity is its address
    (`net_e2e::windowed_recording_send_order_is_identical_across_runs`).
    Whole-log **byte identity across runs is NOT guaranteed**: setup ops
    (hello/bind) and recv events are written in lock-arrival order, which is
    real time. Wait mechanics are excluded from the log (a blocking poll's
    empties and a non-blocking poll's shim-internal retries are not recorded;
    only the final, target-visible EAGAIN is). Every individual recording
    still replays byte-exactly, forever.
  - Validated on a second protocol: Raft leader election
    (`examples/raft/raft_node.c`, 5 members + observer, crash-restarts under
    test) under `--window 1000 --net latency=uniform:2000-10000`. Same-seed
    verdicts are identical across 6 single-host runs and across 5
    two-container runs with distinct `--host-id`s — the two-container hash
    equals the single-host hash, so neither topology nor host labeling leaks
    into the result. Windowed mode also *finds* the votedFor-persistence
    bug: a 60-seed sweep hit 1 ElectionSafety violation (seed 44,
    `RAFT_FIX=0`), the violation's semantic content (term, leader pair,
    report/restart counts) is identical across 6 re-runs, and the same seed
    is clean under `RAFT_FIX=1`. One caveat: `raft-check` cites the
    violation as "at op N", a raw log position — positions count
    arrival-ordered non-send entries, so N varies by ±1 across runs
    (the recording boundary above); it is exact within any one recording.
  - F2 (per-host frontier lag) and F7 (per-window buffer bound) are
    implemented: `--stats` on a windowed run reports each node's maximum
    observed frontier lag behind the pack (sampled in real time, so
    indicative), and `--window-ops N` discards the run (exit 3) if one node
    buffers more than N sends inside a single window. Real per-host ids
    flow from `--host-id` through the shim's `Hello` into the windowed sort
    key's second tier.
  - **Not done:** remote *spawning* (the design's `hostd`) — each host runs
    its own `weft run --listen`/`--broker` by hand or CI. A guest that exits
    via `_exit()`/`abort()` skips `atexit` and is indistinguishable from a
    crash: its windowed run is discarded, which is the conservative
    direction.

## 4. Shrinking: algorithm and worst case

The shrinker is delta debugging (ddmin) over op inputs, with a
truncate-after-violation pre-pass, parallel candidate evaluation
(lowest-index success adopted, so results are deterministic), a 1-minimal
single-op-removal fixpoint, payload truncation, and connect GC.

- **Worst case is O(n²) candidate executions** (standard ddmin bound) when
  the violation depends on ops spread across the whole log, defeating chunk
  removal so reduction happens one op at a time. Each candidate execution is
  a full re-run of the decision core over the candidate subsequence.
- **Measured behavior** at ~14k ops: 82–634 executions per violation,
  totaling ~5–40 ms *per violation* on commodity hardware — each candidate
  execution itself is well under a millisecond (docs/SCALABILITY.md §E). The
  bound is quadratic; observed behavior is far better because violations
  are local.
- **1-minimality, not global minimality.** ddmin guarantees no *single* op
  can be removed — it does not guarantee the smallest possible reproducer.
  Ground-truth tests (`crates/weft-fuzz/tests/shrink_ground_truth.rs`) pin
  exact minima for known cases, but a pathological violation could shrink to
  a locally-minimal but not globally-minimal subsequence.
- **The shrinker never reorders ops and never varies seed or net spec** —
  by design (changing them reproduces a *different* run). A reproducer is
  therefore always an interpretable subsequence of the original run, and
  never smaller than what subsequence-removal can reach.

## 5. Dynamic testing itself (what Phase 7 taught us)

- **Detection is bounded by message latency.** A checker cannot observe a
  fault faster than the notification travels. In the Chord study this left a
  quantified residual: 8 of 452 valid seeds (1.8%) violated under full
  liveness discipline, every one traced to a node adopting a pointer that
  was dead before the DEAD notice arrived. This is not a harness bug — it
  is what testing against a realistic network *means* — but it is a real
  blind spot
  relative to formal models that assume perfect failure detection
  (docs/case-study/LEVEL_2_RESULTS.md).
- **Absence of violations is not proof.** 300 clean Raft seeds falsify the
  specific buggy mechanism under the tested schedule distribution; they do
  not verify Raft. Schedule-sensitivity was measured directly: the same bug
  showed 0/100 under loose election timeouts and 4/100 under adversarially
  tight ones (docs/case-study/RAFT_VALIDATION.md).
- **Guest-side timing is unmeasurable.** Clocks inside the target are
  virtual, so per-operation latency percentiles cannot be collected
  in-process; only wall-clock from outside. Broker-side histograms are a
  recommended, unimplemented optimization
  (docs/SCALABILITY_RECOMMENDATIONS.md).

## 6. Performance boundaries (measured, docs/SCALABILITY.md)

- Broker path: ~125 µs per datagram vs ~0.4 µs native loopback (~300×).
  Predictable, but a high-throughput target will run much slower simulated.
- Shim overhead ~70% on a CPU-bound syscall-heavy microbenchmark;
  sleep-driven programs run *faster* than real time (sleeps are virtual).
- Recordings grow linearly: ~0.65 MB per Chord seed at 45 ticks; a
  5000-seed recorded campaign is ~3.2 GB. No log rotation or compaction
  exists.
- Broker memory is flat (~2.3 MB RSS at 7–14 nodes, the only scale tested).
  Nothing in this range suggests a scaling wall, but it has not been tested
  beyond 14 nodes — treat "scales to tens of nodes" as a hypothesis this
  data is consistent with, not a measured ceiling.

## 7. Interface stability

Pre-1.0. The scenario DSL, the weft-log format, and the CLI have independent
compatibility policies defined in [VERSIONING.md](VERSIONING.md); until 1.0,
breaking changes can occur in any of them with a changelog entry but no
deprecation cycle.
