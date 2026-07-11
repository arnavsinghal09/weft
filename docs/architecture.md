# Weft architecture

This document describes the implemented deterministic simulation system: time &
randomness (Phase 1), thread scheduling (Phase 2), network simulation (Phase 3),
fault model & scenarios (Phase 4), recording & replay (Phase 5), fuzzing &
shrinking (Phase 6), and the protocol case studies that validated it (Phase 7).
Aspirational content is confined to the final section and clearly marked.

Read this before opening any code. Per-subsystem detail lives in the sibling
documents: [scheduling-model.md](scheduling-model.md),
[network-model.md](network-model.md), [fault-model.md](fault-model.md),
[logical-time-model.md](logical-time-model.md),
[recording-format.md](recording-format.md), [fuzzing.md](fuzzing.md),
[process-orchestration.md](process-orchestration.md). Known boundaries are
collected without spin in [../LIMITATIONS.md](../LIMITATIONS.md).

## Big picture

```
weft run --seed 42 -- ./target-program args...
 │
 │  sets LD_PRELOAD=libweft_shim.so, WEFT_SEED=42 (+ WEFT_TRACE=1)
 │  then exec()s the target — exit codes and signals pass through
 ▼
target process
 ├─ libweft_shim.so (first in symbol resolution order)
 │   ├─ hooks: libc-ABI functions  ── seed active? ──► engine
 │   │                                    │ no
 │   │                                    ▼
 │   │                            dlsym(RTLD_NEXT) → real libc
 │   └─ engine: virtual clock + ChaCha8 domain streams
 └─ unmodified program code & real libc for everything else
```

One 64-bit seed fully determines: every wall-clock and monotonic timestamp,
every value from the libc PRNG families, every byte from `getrandom`,
`getentropy`, `/dev/urandom`, `/dev/random`, and `AT_RANDOM`. Same seed ⇒
same values, byte for byte; different seed ⇒ different values. Children
inherit `LD_PRELOAD` and `WEFT_SEED` through `fork`/`exec`, so whole process
trees are covered (each `exec` restarts virtual time — see limitations).

## Crate layout

| crate | role | loaded into target? |
|---|---|---|
| `weft-dst` | `weft` CLI: env plumbing, exec, broker hosting (`--net`), `replay` | no |
| `weft-shim` | cdylib with the hooks + engine + scheduler | yes |
| `weft-abi` | env-var names, seed parsing, domain IDs, SplitMix64 | yes (via shim) |
| `weft-net` | broker + pure decision core, fault model, wire protocol (Phase 3) | yes (via shim) |
| `weft-scenario` | scenario DSL: JSON parsing + validation (Phase 4) | no |
| `weft-replay` | event-log recording, deterministic replay, invariants (Phase 5) | no |
| `weft-fuzz` | seed sweeping, delta-debugging shrinker, `weft fuzz` engine (Phase 6) | no |
| `weft-chord` | Chord case study: `chord-check` / `chord-trace` invariant tools (Phase 7) | no |
| `weft-raft` | Raft case study: `raft-check` ElectionSafety checker (Phase 7) | no |

`weft-shim` and `weft-abi` keep a near-zero dependency tree (`libc`,
`rand_chacha`, `rand_core`; all `no_std`-capable) because they execute inside
arbitrary user processes.

## How interception works

Every hook is a `#[no_mangle] extern "C"` function with a libc-identical
signature, compiled into `libweft_shim.so`. With `LD_PRELOAD`, the dynamic
linker resolves the target's (and its libraries') calls to our definitions.
Each hook follows one shape:

1. `state::shim()` — lazily (once, on the first intercepted call) reads
   `WEFT_SEED`. Unset → `None` forever; the hook tail-calls the real function
   via `dlsym(RTLD_NEXT, ...)` (cached per call site in an `AtomicPtr`).
   **This is the do-no-harm rule: a preloaded but unseeded shim is
   behaviorally invisible.** A *malformed* seed is reported on stderr and
   treated as unset rather than half-working.
2. Seed active → answer from the engine. No allocation, no stdio, no locks
   other than the engine's own, so hooks are safe from odd contexts (early
   init, tight loops, many threads).

Initialization is deliberately lazy rather than an ELF constructor: ctor
ordering across preloaded libraries is unspecified, whereas by the time any
libc call is interposed, libc is fully up.

### Intercepted surface (complete list)

**Time** — `time`, `gettimeofday`, `clock_gettime`, `clock_getres`,
`timespec_get`, `nanosleep`, `clock_nanosleep` (incl. `TIMER_ABSTIME`),
`sleep`, `usleep`.

**Randomness** — `rand`, `srand`, `rand_r`, `random`, `srandom`, `initstate`,
`setstate`, `drand48`, `erand48`, `lrand48`, `nrand48`, `mrand48`, `jrand48`,
`srand48`, `getrandom`, `getentropy`, `getauxval(AT_RANDOM)`.

**Device files** — `open`, `open64`, `openat`, `openat64`, `read`, `pread`,
`pread64`, `close`, `fopen`, `fopen64` — only diverted for the exact paths
`/dev/urandom` and `/dev/random`; everything else passes straight through.

**Thread scheduling (Phase 2)** — `pthread_create`, `pthread_join`,
`pthread_exit`, `pthread_mutex_lock`/`trylock`/`unlock`, `pthread_cond_wait`/
`timedwait`/`signal`/`broadcast`, `sched_yield` — the deterministic
cooperative scheduler's yield points. See docs/scheduling-model.md.

**Network (Phase 3)** — `socket` (`AF_INET`/`SOCK_DGRAM` only), `bind`,
`sendto`, `recvfrom` — diverted to the seeded broker when `WEFT_BROKER` is
set. See docs/network-model.md.

**File I/O (Phase 4)** — `write`, `pwrite`, `pwrite64`, `fsync`, `fdatasync`
— track bytes written and optionally lie about fsync persistence when
`WEFT_FSYNC_LIES=1` is set. See docs/process-orchestration.md.

### The virtual clock

A single `AtomicU64` of nanoseconds:

- **Monotonic** time starts at 0. Every *read* advances it 1 µs
  (`fetch_add`), so (a) loops that poll the clock always make progress and
  (b) every observation is unique and strictly increasing, even across
  threads — concurrent readers get disjoint ticks.
- **Realtime** = 2000-01-01T00:00:00Z + a seed-derived offset in [0, 1 year)
  + monotonic. Different seeds land on different dates on purpose:
  date-dependent target logic gets exercised.
- **Sleeps never sleep.** `nanosleep(750ms)` advances virtual time 750 ms
  and returns 0 immediately; `TIMER_ABSTIME` deadlines `fetch_max` the
  counter. A million sleeping iterations run in real milliseconds.
- All clock ids map to one of those two timelines (CPU-time clocks report
  the monotonic value — see limitations).

### The PRNG: ChaCha8, per-domain streams

The generator is **ChaCha8** (`rand_chacha::ChaCha8Rng`) — a named, published
algorithm; nothing hand-rolled. Why this one:

- **Statistical quality**: cryptographic-grade; passes PractRand/TestU01 with
  margin. A fuzzer will never trip over generator artifacts (LCG lattices,
  xorshift linearity).
- **Speed**: multi-GB/s; a draw is trivially cheap next to the interposed
  call itself (measured overhead below).
- **Sub-streams natively**: ChaCha has a 64-bit stream counter orthogonal to
  the key. Each interception *domain* gets its own stream of the same key:

| stream | domain |
|---|---|
| 0 | `rand`/`random`/`*48` families |
| 1 | `getrandom` / `getentropy` |
| 2 | `/dev/urandom`, `/dev/random` reads |
| 3 | `AT_RANDOM` (16 bytes, fixed at init) |
| 4 | seed → realtime-clock offset |

  Domain isolation means adding a `getrandom` call to a program does not
  shift the values its `rand()` loop sees — failures stay reproducible under
  small program changes.

Seed flow: `WEFT_SEED` (u64) → SplitMix64-expanded to a 32-byte ChaCha key
(`weft_abi::expand_seed`) → five streams. `srand(x)`/`srand48(x)` re-key
stream 0 from `mix(run_seed, x)`: a program's own reseeding stays meaningful,
but a different `--seed` still changes everything. The caller-state variants
(`rand_r`, `erand48`/`nrand48`/`jrand48`) advance the *caller's* state buffer
through SplitMix64 mixed with the run seed — deterministic, seed-sensitive,
and safe for concurrent distinct state buffers.

Thread safety: each domain stream sits behind its own `Mutex`; the clock is
lock-free. **Cross-thread guarantee**: the *sequence* each stream emits is
fixed, so the multiset of values a group of racing threads draws is always
deterministic. With the Phase 2 scheduler active (the default), thread order
is itself a function of the seed, so *which thread gets which value* is
deterministic too. Under `--no-sched`, or for threads the scheduler does not
manage, attribution falls back to the OS schedule and only the multiset
guarantee holds. (The `entropy.c` test target is built around exactly that
weaker invariant: everything it prints is commutative across threads.)

### /dev/urandom mechanics

`open("/dev/urandom")` actually opens `/dev/null` — reserving a genuine fd so
`close`/`fstat`/`dup` remain sane — records the fd in a fixed 64-slot atomic
table, and `read`/`pread` on recorded fds fill from the shared stream 2.
`read(2)` has no read-ahead, so every byte drawn from stream 2 is a byte the
caller received; concurrent `read`s draw a scheduling-dependent *interleaving*
of one fixed sequence, so their multiset is deterministic (the Phase 1
cross-thread guarantee).

`fopen` cannot reuse the fd trick (glibc stdio reads through an internal,
non-interposable alias of `read`), so it returns a `fopencookie` stream. Here
buffering matters: glibc reads ahead in ~8 KiB chunks and *discards* the
unconsumed tail at `fclose`. If every stream shared stream 2, which byte
ranges got discarded would depend on how threads interleave their chunked
read-aheads — making the bytes actually delivered to the application vary run
to run. So each `fopen`ed random device instead gets its **own independent
substream** (`DevFileRng`): a fresh ChaCha8 stream keyed by the run seed with
a stream id of `0x1000_0000 + N`, where `N` is the process-global open order.
Read-ahead then only advances that file's private sequence, the discarded
tail is a deterministic function of `N`, and the substream's own `Mutex`
(not glibc's `FILE` lock) makes concurrent `fread`s data-race free. The base
`0x1000_0000` sits far above the fixed domain stream ids (0..=4), so a
substream can never collide with a domain. If the fd table is ever full (64
concurrently-open random fds), we log under `--trace` and hand out the real
device rather than fail.

### Tracing

`weft run --trace` (or `WEFT_TRACE=1`) makes every hook log one line to
stderr — formatted in a stack buffer, written with one raw `write(2)`, no
allocation — e.g. `[weft] clock_gettime(1) -> 3.000004000`.

## Above the shim: broker, recording, replay, fuzzing

The pieces outside the target process compose in one pipeline:

- **Broker (`weft-net`, Phase 3).** With `--net`, `weft run` hosts a broker on
  a Unix-domain socket; the shim diverts `AF_INET/SOCK_DGRAM` traffic to it.
  All fault decisions (latency, loss, reordering, partitions, bandwidth) come
  from a *pure decision core* — a function of `(seed, src, dst, seq)` — so the
  same seed always deals every message the same fate. The broker's
  linearization order is the single source of truth for "what happened".
- **Recording (`weft-replay`, Phase 5).** `--record <LOG>` streams every
  broker operation to a weft-log file (v1, gzip-aware). The broker
  linearization order is the only non-seed input to a run, so the log plus
  the seed reconstructs the run exactly. See recording-format.md.
- **Replay (`weft replay`).** Re-executes a recording against the same pure
  core and verifies byte-for-byte identity (a stream digest), optionally
  checking invariants (`--check fifo,dup`). Replay of a recording is exact on
  every platform — no shim required.
- **Fuzzing (`weft fuzz`, Phase 6).** Sweeps fault seeds over a deterministic
  workload against the decision core, dedups violations by identity, and
  delta-debugs each one to a minimal reproducer log that `weft replay`
  verifies. See fuzzing.md.
- **Orchestrator + scenarios (`weft-scenario`, Phase 4).** A JSON scenario
  describes nodes, network faults, filesystem faults, and timed events
  (crash/restart/partition changes); the orchestrator executes it. See
  process-orchestration.md.

**Validation (Phase 7).** The whole stack was pointed at real protocol
implementations: Chord (2001) stabilization — falsified, 57/500 seeds break
the ring, reduced to 8/500 with published fixes — and Raft leader election —
the dissertation's votedFor-persistence edge case reproduced (3/300) and
falsified by the fix (0/300). See docs/case-study/CREDIBILITY_SUMMARY.md.

## Empirical results (Phase 1 exit criteria)

Measured on the CI configuration (Linux, x86-64; see
`crates/weft-dst/tests/e2e.rs` and `scripts/bench-overhead.sh`):

- **Reproducibility**: `chrono`, `montecarlo`, `entropy` produce
  byte-identical stdout across repeated runs of the same seed (checked for
  seeds 0, 1, 42, 0xDEADBEEF, u64::MAX), and different stdout across
  different seeds.
- **Passthrough**: with `LD_PRELOAD` set but no `WEFT_SEED`, outputs vary
  run-to-run and programs behave normally.
- **Overhead**: see the table in the phase notes / CHANGELOG, produced by
  `scripts/bench-overhead.sh` (best-of-5). The dominant cost is ~5M
  interposed PRNG calls in `montecarlo`; `chrono` is *faster* under Weft
  because sleeps are virtual.

## Current limitations (the honest list)

- **Statically linked binaries are not intercepted.** `LD_PRELOAD` works by
  interposing dynamic symbol resolution; a static binary has no PLT to
  interpose. Planned fallback: ptrace/seccomp-notify syscall interception
  (future work below).
- **Raw syscalls bypass the shim.** A program that issues `syscall(SYS_getrandom, ...)`
  or inlines `rdtsc`/`rdrand` instructions gets real
  nondeterminism. This includes **Go binaries** (runtime does raw syscalls)
  and any use of **vDSO by address** rather than through libc symbols.
- **`vfork`/`posix_spawn` children are covered only via env inheritance**;
  an `exec` into a *setuid* binary drops `LD_PRELOAD` (glibc secure-mode) and
  escapes determinism silently.
- **Cross-thread value attribution is scheduling-dependent under
  `--no-sched`** and for unmanaged threads (see above). With the scheduler
  active (the default), attribution is deterministic; thread-*safety* is
  guaranteed and sanitizer-checked in both modes.
- **CPU-time clocks are approximated** (`CLOCK_PROCESS_CPUTIME_ID` /
  `CLOCK_THREAD_CPUTIME_ID` return virtual-monotonic time, not modeled CPU
  consumption).
- **`getcpu`, `sched_getcpu`, `/proc/*/stat` timings, `times(2)`,
  `getrusage(2)` are not virtualized** — programs deriving entropy or logic
  from them stay nondeterministic.
- **PIDs, TIDs, ASLR addresses, and `gettid()` are real.** A program seeding
  from `getpid()` or hashing pointer values is not yet deterministic.
  (Weft's own `srand(getpid())`-shaped gap; ASLR pinning arrives with the
  orchestrator's namespace work.)
- **`random_r`/`initstate_r`** (glibc reentrant family) and **`arc4random*`**
  (glibc ≥ 2.36) are not yet interposed.
- **`initstate`/`setstate` return the caller's buffer** rather than the
  previous internal buffer; glibc programs that *swap* state buffers and
  expect the old pointer back will see a benign lie (their sequences are
  seed-derived under Weft anyway).
- **musl**: the `open`→`read` fd path works, but `fopen("/dev/urandom")`
  uses `fopencookie`, which musl also provides; however the shim is only
  CI-tested against glibc today.
- **`fopen` substream *indices* are assigned in open order.** With the
  scheduler active, open order is seed-deterministic and this is a non-issue.
  Under `--no-sched` the order is OS-scheduled: each stream's bytes are still
  reproducible given its index, and a program that combines every stream
  commutatively (like `entropy.c`'s XOR/sum) stays fully deterministic, but
  logic depending on *which thread* opened *which* stream sees
  scheduling-dependent attribution.
- **File-descriptor duplication of random fds** (`dup`, `dup2`, `fcntl(F_DUPFD)`)
  is not tracked: a duped random fd reads real `/dev/null` (EOF). No real
  program observed doing this yet; fix is a straightforward hook addition.
- **`sleep` interaction with SIGALRM** is not modeled (virtual sleeps never
  observe signals). Signal determinism in general is out of scope until the
  scheduler phase.

## Future work

- **ptrace/seccomp-unotify fallback for static binaries & raw syscalls** —
  intercept at the syscall boundary instead of the PLT. Sketch: seccomp
  filter marks `clock_gettime`/`getrandom`/`openat` for user-notify; a
  supervisor answers from the same engine. Slower (context switch per call)
  but closes the static-binary and Go gaps. Not started; the engine was
  deliberately built process-external-safe (pure functions of seed + counter)
  so it can serve both mechanisms.
- Folding live-target fuzzing (the `weft run --record` path) into the
  `weft fuzz` sweep loop, so shim-path campaigns get the same
  dedup-and-shrink treatment the broker-core sweep has today.
