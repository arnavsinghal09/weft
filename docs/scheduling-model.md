# The Weft scheduling model (Phase 2)

Phase 2 makes multi-threaded execution order deterministic, entirely in
userspace: exactly one logical thread makes forward progress at a time, and
which one runs next is a pure function of the run seed.

## What counts as a yield point

A **yield point** is any interposed call where the running thread hands
control back to the scheduler, which then deterministically selects the next
thread. The precise set (see `crates/weft-shim/src/hooks/thread.rs` and
`hooks/socket.rs`):

| yield point | scheduler action |
|---|---|
| `pthread_mutex_lock` | acquire in the model, or block on the owner |
| `pthread_mutex_trylock` | acquire-or-EBUSY in the model, then yield |
| `pthread_mutex_unlock` | release, wake modeled waiters, yield |
| `pthread_cond_wait` / `timedwait` | release mutex, block on cond, reacquire |
| `pthread_cond_signal` / `broadcast` | wake one (chosen from the seed) / all |
| `pthread_create` | register the child, yield (child may run now) |
| `pthread_join` | block until the target finishes |
| `pthread_exit` / start-routine return | mark finished, wake joiners, hand off |
| `sched_yield` | plain yield |
| simulated `sendto` / `recvfrom` (Phase 3) | yield after send; poll+yield while empty |

Everything *between* two yield points executes atomically with respect to
other managed threads — see the limitations section.

## The token model

Real OS threads exist, but a thread may run only while it holds the single
scheduler token (`Scheduler::wait_turn`). At every yield point the holder
calls into the scheduler (`pick_next`), which:

1. computes the **enabled set** — threads whose modeled state is Runnable
   (not blocked on an owned mutex, an unsignaled condvar, or an unfinished
   join target);
2. selects one deterministically (see strategies below);
3. wakes it via a process-internal `Condvar` and parks everyone else.

The scheduler *models* mutexes, condition variables, and joins rather than
delegating to the real pthread primitives: managed threads never call the
real `pthread_mutex_lock`/`cond_wait` at all. Mutual exclusion follows from
the token (one runner at a time); the happens-before edges the target expects
from its locks are provided by the scheduler's own internal `std::sync`
primitives, which every hand-off crosses. Modeled state is keyed by the
pthread object's address, so distinct mutexes contend independently.

Registration is **lazy**: the main thread registers at its first
`pthread_create`. A program that never creates a thread never pays for the
scheduler, and every pre-existing Phase 1 behavior is unchanged.

## Interleaving-selection strategies

Selected with `--strategy` (`WEFT_STRATEGY`), both driven by a dedicated
ChaCha8 stream (`Domain::Scheduler`) of the run seed:

- **`random`** (default): uniform draw from the enabled set at every
  scheduling point. Explores the interleaving space aggressively — across
  seeds you visit maximally diverse schedules, which finds concurrency bugs
  fastest in a fuzzing loop. The cost: adjacent decisions share no structure,
  so a human replaying a trace sees threads ping-pong arbitrarily.
- **`rr` (round-robin with perturbation)**: rotate through enabled threads in
  tid order, but with probability 0.2 make a random pick instead. Runs are
  mostly convoy-like and easy to follow when debugging a specific failure —
  "thread 2 ran, then 3, then 4" — while the perturbation keeps a single seed
  family from being blind to order-dependent bugs. The cost: per seed it
  explores far fewer distinct interleavings than `random`.

The tradeoff in one line: **`random` to find bugs, `rr` to understand one.**

Both are pure functions of the seed; a schedule found with either replays
byte-identically.

## Deadlock detection

If the enabled set is empty while unfinished threads remain, the target is
deadlocked (e.g. an ABBA lock cycle: `examples/deadlock.c`). The scheduler
prints `[weft] DEADLOCK: N thread(s) blocked …` to stderr and aborts —
deterministically, so a deadlocking seed is a perfectly reproducible test
case. Seeds where the interleaving avoids the cycle complete normally.
Threads blocked in *external* work (a broker `recv` poll loop) never enter a
blocked model state — they poll with `yield_now` — so network waiting cannot
be misreported as deadlock.

## The race proof (`examples/race_bank.c`)

The bug is a **split critical section**: `deposit` reads the balance under
the lock, releases it, then re-locks to write back `read+1` — a real refactor
pattern ("minimize time under lock"). The unlock/lock boundary is a yield
point, so the scheduler can (or can not) interleave another thread's
read-modify-write into the window, purely by seed:

- 2 threads × 2 iterations, `--strategy random`:
  - **seed 3 triggers** the lost update: `balance=2 lost=2`, 20/20 runs.
  - **seed 2 avoids** it: `balance=4 lost=0`, 20/20 runs.
- At scale (4 threads × 25), the race is pervasive: e.g. seed 1 loses exactly
  62 updates, every run.

Pinned in CI by `crates/weft-dst/tests/sched_e2e.rs`; scheduler-level
reproducibility, thread churn, nested locks, and condvar rendezvous are
covered in-process by `crates/weft-shim/tests/sched_harness.rs` (sanitizer-
friendly).

## Coverage / stats

`--stats` (`WEFT_SCHED_STATS=1`) prints, at exit: logical thread count, total
scheduling decisions, and the set of **distinct yield-point sites** hit
(`mutex_lock`, `cond_wait`, `net_recv_wait`, …). A rough but useful coverage
signal for a future fuzzing loop: a seed corpus can be ranked by which sites
(and how many decisions) it reaches.

## Honest limitations

- **No preemption at arbitrary instructions.** Yield points exist only at
  interposed libc calls. A pure-compute region — including one that races on
  shared memory with plain loads/stores and no locking at all — runs
  atomically under Weft and its internal races are *invisible* to the
  scheduler. A hypervisor or instruction-level instrumentation (ptrace
  single-step, binary translation) would be required to interleave inside
  such regions; that is explicitly out of scope. Weft explores the
  interleavings of *synchronization and I/O operations*, which is where most
  distributed-systems bugs live, and `race_bank`'s bug is reachable precisely
  because its racy window is bracketed by yield points.
- **Data races are serialized, not detected.** Because only one thread runs
  at a time, TSan-style simultaneous-access detection cannot fire under the
  scheduler. Weft finds *order* bugs (lost updates, stale reads, deadlocks)
  by exploring schedules, not access-level races. Run the suite with
  `WEFT_SCHED=0` under TSan for the latter (the `--no-sched` mode exists
  partly for this).
- **`pthread_cond_timedwait` ignores its deadline** — modeled as an untimed
  wait. Code that relies on the timeout alone (no signaler) to make progress
  will deadlock-detect instead. Wiring timeouts to virtual time is future
  work.
- **Unmanaged threads pass through.** Threads created before the shim
  activates, or by `clone(2)` directly, use real pthread primitives and real
  OS scheduling. Interaction between managed and unmanaged threads is
  passthrough-correct but not deterministic.
- **Reentrancy no-ops.** While inside scheduler/bootstrap machinery, `pthread_mutex_*`
  calls return success without locking (safe: such contexts are effectively
  single-threaded by construction — startup, or holding the token). A
  hostile-weird target that ships its *own* interposed pthread symbols ahead
  of ours in the preload chain is out of scope.
- **Spawn/`fork` boundaries.** Each process has its own scheduler; Phase 3's
  broker gives cross-process *message* determinism, but cross-process
  *interleaving* determinism is future work (see docs/network-model.md).
- **`pthread_rwlock`, `pthread_barrier`, semaphores** are not yet modeled and
  pass through to the real libc. They behave correctly but their contention
  is scheduled by the OS, not the seed. Extending the model is mechanical
  (same pattern as mutexes) and planned.
