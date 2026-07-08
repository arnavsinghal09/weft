# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to
[Semantic Versioning](https://semver.org/spec/v2.0.0.html). Until 1.0.0,
minor versions may contain breaking changes.

## [Unreleased]

### Added

- Seed fuzzing and failure shrinking (Phase 6): `weft fuzz --config <FILE>`
  sweeps fault seeds against a deterministic workload (new `weft-fuzz`
  crate), checks invariants on every execution, dedupes violations by
  identity (invariant + channel), and shrinks each distinct one to a minimal
  reproducer log that `weft replay --check` verifies byte-for-byte.
  Shrinking is delta debugging over *op inputs* (recorded outcomes are
  derived data and are recomputed, never edited): truncate-after-violation,
  ddmin with parallel candidate evaluation (deterministic lowest-index
  adoption), a 1-minimal single-op sweep, payload truncation, and connect
  GC — never reordering ops or touching seed/net, so reproducers stay
  interpretable subsequences of the original run. Correctness is pinned by
  three ground-truth tests that bury a known exact minimum (6, 4, and 5 ops)
  in 300–400 noise ops and require exact recovery. The sweep itself is
  two-phase (sweep, then shrink from each violation's smallest seed) so
  reports are deterministic regardless of thread timing; a JSON config file
  (typo-rejecting, with defaults) replaces flag soup, exit codes are
  CI-first (0 clean / 2 violations / 1 error), `--regressions <file>`
  maintains a self-growing corpus of failing seeds that are always tested
  before the sweep, and `.github/workflows/fuzz.yml` +
  `examples/fuzz/ci.json` ship a working CI property sweep (fixed-latency
  net where FIFO must hold for every seed). See docs/fuzzing.md.
- Recording, deterministic replay, and invariant checking (Phase 5). New
  `weft-replay` crate implements the versioned `weft-log` v1 format
  (docs/recording-format.md): a JSONL capture of the broker's linearization
  order — the single run input that is not a pure function of the seed —
  with an FNV-1a integrity chain that detects truncation, reordering, and
  edits. `weft run --net … --record <LOG>` records a run;
  `weft replay <LOG> [--until N] [--check fifo,dup]` re-executes it by
  driving the same pure decision core the live broker uses
  (`weft_net::core::Core`, extracted from the broker so live and replayed
  behavior cannot drift) and verifies the result is byte-identical —
  recomputing every fate, tie, and virtual time rather than trusting the
  log, and reporting the first divergence with both sides. Invariants
  (`per-channel-fifo`, `no-duplicate-delivery`, or ad-hoc closures) run
  identically in-process during recording and from an external checker over
  the log; every violation is anchored to `(op, virtual-time)` on the
  logical timeline, and the violation report carries seed, net spec, log
  path, stream digest, the surrounding event window, and the literal replay
  command. Validated end to end: a live broker run (real sockets, threads,
  OS scheduling) whose latency variance breaks FIFO is recorded and
  replayed to the identical violation — same op, same virtual time, same
  stream digest — 10 consecutive times (`weft-replay/tests/record_replay.rs`;
  runnable demo: `cargo run -p weft-replay --example demo_violation`).
  Logs may be gzip-compressed as a transport encoding (`--record foo.weftlog.gz`;
  readers detect gzip by magic bytes, and the integrity chain is always over
  the uncompressed text — recording-format.md §11; ~4× smaller in practice).
- Process orchestration (Phase 4c): deterministic execution of scheduled
  crash/restart/partition events. New `weft_dst::orchestrator` module with
  `NodeRegistry` to track process state and `spawn_scheduler()` to execute
  events at precise logical times. Global logical time tracking in broker via
  `Arc<AtomicU64>` updated as network messages are delivered. Event scheduler
  runs in separate thread, waits for broker time to reach each event's
  `time_ns`, then kills or restarts processes (SIGKILL to crash, fork/exec to
  restart). Determinism: same seed + scenario → same crashes at same logical
  times. 3 integration tests validating: node state tracking, crash execution,
  and multi-event ordering.
- File I/O fault hooks (Phase 4b): `write`, `pwrite`, `pwrite64`, `fsync`,
  `fdatasync` interception in weft-shim. Tracks bytes written for ENOSPC
  simulation and can optionally lie about fsync persistence via
  `WEFT_FSYNC_LIES=1`. Enables testing of durability and crash-recovery bugs
  that require simultaneous network + file I/O faults. New example scenario:
  `file-sync-network-reordering.json`.
- Process orchestration design & requirements (Phase 4b): `docs/process-orchestration.md`
  specifies how scheduled crash/restart/partition events integrate with the
  broker, including process registry, event scheduler, and state preservation
  semantics. Includes pseudo-code for orchestrator implementation and
  validation strategy.
- Fault model and scenario DSL (Phase 4): timed fault injection framework
  unifying network, file I/O, and process faults on a global logical timeline.
  New `weft-scenario` crate provides JSON-based scenario format with `latency`,
  `loss`, `bandwidth`, `partitions` (Phase 3 inherited), plus `fsync_lies`,
  `enospc_after_bytes`, `torn_write_probability` (Phase 4 file I/O), and
  scheduled events (`crash`, `start`, `activate_partition`). Per-node clock
  skew injection. Scenario parser guarantees no panic on arbitrary input,
  with clear validation errors (property-tested: 8 unit + 22 integration
  tests). Example scenarios: network-reordering.json, crash-and-restart.json.
  Unified logical-time model documented in docs/logical-time-model.md;
  complete fault vocabulary and reproducibility guarantees in docs/fault-model.md.
- Deterministic network simulation (Phase 3): `weft run --net <SPEC>
  [--nodes N]` hosts a seeded broker and routes every `AF_INET`/`SOCK_DGRAM`
  datagram (`socket`/`bind`/`sendto`/`recvfrom`) through it instead of the
  kernel. The fault model — `latency=fixed|uniform|exp`, independent `loss`,
  `bw` cap, `partition=0+1|2` — decides each datagram's fate as a pure
  function of (seed, channel, sequence), so network-triggered bugs replay
  from a seed. Latency variance produces reordering naturally. New `weft-net`
  crate (wire protocol, fault model, broker); network receive integrates with
  the Phase 2 scheduler as a poll-and-yield point. Examples: `pingpong.c`
  (two-process proof), `kvreplica.c` (missing-version-check replica whose
  stale read is triggered by seed 1 and avoided by seed 0, 20/20 runs each,
  under `latency=uniform:1000-50000`), `udpbench.c` + `scripts/bench-net.sh`
  (measured ≈285× per-datagram overhead vs native loopback). See
  docs/network-model.md for the simulated-vs-simplified contract.
- Deterministic thread scheduling (Phase 2): a cooperative userspace
  scheduler in the shim serializes managed threads (one runs at a time) and
  picks the next thread from a dedicated ChaCha8 seed stream at every yield
  point (`pthread_mutex_*`, `pthread_cond_*`, create/join/exit,
  `sched_yield`). Two strategies via `--strategy`: `random` (default,
  maximum interleaving diversity) and `rr` (round-robin with 20% seeded
  perturbation, easier to follow when debugging). Deadlock is detected and
  aborted deterministically instead of hanging. `--stats` reports decisions
  and distinct yield-point sites; `--no-sched` (`WEFT_SCHED=0`) keeps
  time/randomness deterministic while letting the OS schedule threads.
  Examples: `race_bank.c` (split-critical-section lost update; seed 3
  triggers / seed 2 avoids, 20/20 runs each), `prodcons.c`,
  `thread_churn.c`, `deadlock.c`. See docs/scheduling-model.md.
- Deterministic time and randomness for unmodified Linux binaries via an
  `LD_PRELOAD` interception shim (`weft-shim`, built as a `cdylib`). One
  64-bit seed drives every intercepted source of nondeterminism; without
  `WEFT_SEED` the shim is a transparent passthrough (do-no-harm rule).
  - **Time**: `time`, `gettimeofday`, `clock_gettime`, `clock_getres`,
    `timespec_get`, `nanosleep`, `clock_nanosleep` (incl. `TIMER_ABSTIME`),
    `sleep`, `usleep` — served from a virtual clock; sleeps advance virtual
    time and return immediately.
  - **Randomness**: `rand`/`srand`/`rand_r`, `random`/`srandom`/`initstate`/
    `setstate`, the `drand48` family, `getrandom`, `getentropy`, and
    `getauxval(AT_RANDOM)` — served from per-domain ChaCha8 substreams
    (`rand_chacha`) derived from the seed, one independent stream per domain.
  - **Random devices**: `/dev/urandom` and `/dev/random` via `open`/`openat`/
    `read`/`pread`/`close` and `fopen` (deterministic `fopencookie` streams).
- `weft run --seed <N> [--trace|--verbose] -- <program> [args...]`: launches
  a target under the shim, prepending to any existing `LD_PRELOAD` and passing
  seed/trace through `fork`/`exec` so process trees stay deterministic.
- `weft-abi` crate: seed parsing/expansion, domain identifiers, and the
  env-var contract shared between the CLI and the shim (dependency-free).
- Three non-trivial C example targets (`chrono`, `montecarlo`, `entropy`)
  exercising time, tight-loop randomness, and multithreaded entropy sources,
  used as the determinism proof; end-to-end and threaded sanitizer-ready
  tests; `docs/architecture.md`; and helper scripts for containerized Linux
  testing and overhead benchmarking.
- Project skeleton: Cargo workspace with the `weft-dst` crate (installs the
  `weft` binary; `--help` and `--version` only).
- CI pipeline with blocking gates: rustfmt + clippy (`-D warnings`), RustSec
  vulnerability audit (`cargo deny check advisories`), and license compliance
  (`cargo deny check licenses bans sources`), plus tests and Codecov coverage
  upload.
- Community files: README, CONTRIBUTING, SECURITY policy with private
  disclosure process, Contributor Covenant 2.1 code of conduct, GOVERNANCE,
  issue and PR templates.
- `PROJECT_NOTES.md` with the full planned architecture, language rationale,
  and per-session context-loading workflow (graphify).
- Dual MIT/Apache-2.0 licensing.

### Fixed

- `fopen("/dev/urandom")` + `fread` is now deterministic under multithreaded
  load. Each `fopen`ed random device gets its own independent seed-derived
  ChaCha8 substream (keyed by process-global open order) instead of sharing
  one stream, so glibc stdio's buffered read-ahead and the tail it discards at
  `fclose` no longer depend on thread interleaving.
