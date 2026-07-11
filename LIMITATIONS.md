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
Proven 10× in CI on every platform. This guarantee has no known leaks —
the log *is* the linearization.

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

**(c) Multi-process cluster runs (`--net`) are NOT seed-deterministic live.**
Which process's syscall reaches the broker first is OS-scheduled. Once
enqueued, every fate and delivery position is deterministic — but the
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
Weft on a distributed system. Removing it requires scheduling *across*
processes (a cross-process token), which is designed but not built.

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
