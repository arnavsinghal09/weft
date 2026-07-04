# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to
[Semantic Versioning](https://semver.org/spec/v2.0.0.html). Until 1.0.0,
minor versions may contain breaking changes.

## [Unreleased]

### Added

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
