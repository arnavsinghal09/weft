# The Weft network model (Phase 3)

Phase 3 extends seed-determinism to networking: processes communicate over
ordinary UDP sockets, but every datagram is routed through a central **broker**
that applies a seeded fault model — latency, loss, reordering, partitions, a
bandwidth cap — instead of reaching the kernel network stack.

## Architecture

```
 target process A                weft (parent process)         target process B
┌────────────────┐              ┌─────────────────────┐       ┌────────────────┐
│ app: sendto()  │              │  Broker              │       │ app: recvfrom()│
│   ↓ shim hook  │  Unix socket │   ├ routing table    │ Unix  │   ↑ shim hook  │
│ weft-shim ─────┼──────────────┼──▶├ per-dest queues  ├───────┼── weft-shim    │
│ (LD_PRELOAD)   │  wire proto  │   └ FaultModel(seed) │ socket│  (LD_PRELOAD)  │
└────────────────┘              └─────────────────────┘       └────────────────┘
```

- `weft run --net <SPEC> [--nodes N] -- prog` hosts the broker inside the
  `weft` process, then spawns N instances of the program with `WEFT_BROKER`
  (broker socket path), `WEFT_NODE_ID` (0..N-1), and the usual seed/scheduler
  environment. Node *i*'s conventional IP is `127.0.0.(i+1)`.
- In the target, the shim intercepts `socket(AF_INET, SOCK_DGRAM)`: instead of
  a kernel UDP socket, the returned fd is a Unix-stream connection to the
  broker. `bind`/`sendto`/`recvfrom` speak a small length-prefixed protocol
  (`weft-net::wire`) over it. The target closes the fd through the interposed
  `close(2)` like any other descriptor.
- The broker keeps one delivery queue per connection, a `bound: addr → conn`
  routing table, and per-channel sequence counters feeding the fault model.

## The determinism principle: per-message fate

Each datagram's fate — dropped or delivered, and with what delay — is a **pure
function of `(run seed, src addr, dst addr, per-channel sequence number)`**
(`weft-net::fault::FaultModel::fate`). It is *not* drawn from a shared PRNG in
broker-arrival order, so it cannot depend on how the OS scheduled the sending
processes. The k-th datagram on channel A→B meets the same fate in every run
with the same seed. This is what makes a network-triggered bug replayable.

Delivery *order* to a receiver is by `(sampled delay, global enqueue index)` —
a deterministic ordering key, not a wall-clock timer (see "delay is an
ordering key" below).

## The fault model (`--net` spec)

Comma-separated `key=value` clauses (`weft-net::config`):

| clause | meaning |
|---|---|
| `latency=fixed:N` | constant N ns delay |
| `latency=uniform:LO-HI` | uniform delay in [LO, HI] ns — variance ⇒ reordering |
| `latency=exp:MEAN` | exponential (heavy tail, clamped at 20×MEAN) — bursty reordering |
| `loss=P` | independent per-datagram loss probability |
| `bw=BYTES_PER_SEC` | bandwidth cap, modeled as `len/rate` serialization delay added per datagram |
| `partition=0+1\|2` | node groups; traffic *between* groups is dropped; unlisted nodes form an implicit "rest" group |

**Reordering is not a separate knob**: it emerges when a latency distribution
with variance gives a later datagram a smaller delay — the same mechanism as
real networks. `latency=fixed` (or the default empty spec) can never reorder.

**Two latency distributions, why both:** `uniform` gives dense, bounded jitter
— the right default for shaking out ordering assumptions, since every burst
gets thoroughly shuffled. `exp` is heavy-tailed: most datagrams are fast, a
few are very late, which is the realistic shape of congestion and better at
finding bugs that need one *straggler* (a stale ack arriving after a new
election, say) rather than wholesale shuffling.

## Scheduler integration (Phase 2 × Phase 3)

Network I/O is exactly the blocking-call class that Phase 2 yield points
cover, and the integration is deliberate:

- A broker round-trip (request + reply) happens **while holding the scheduler
  token**, so it is one atomic step in the deterministic schedule.
- A managed thread's `recvfrom` never parks inside the broker. It polls
  (non-blocking `Recv`), and on `Empty` calls the scheduler's `yield_now` —
  so "waiting for a message" is a Phase 2 yield point, and *which thread runs
  next* (e.g. the sender that will produce the message) is chosen
  deterministically from the seed.
- `sendto` yields after the broker acknowledges, giving the scheduler the
  chance to run the receiver next.

Consequence: a **multi-threaded single process whose threads act as nodes is
fully deterministic** — scheduling, message content, fates, and delivery
order all derive from the seed. This is the same modeling trick FoundationDB's
simulation uses (all simulated "processes" inside one real process), and it is
what `examples/kvreplica.c` does.

## Simulated vs. simplified — the explicit list

Simulated faithfully:
- UDP datagram semantics: unreliable, unordered, message-boundary-preserving;
  datagrams to unbound ports are silently discarded; receivers see truncated
  reads if their buffer is short.
- Loss, latency-driven reordering, partitions, bandwidth-as-delay, all seeded.
- Nodes joining, leaving (connection drop unbinds their addresses), and
  rejoining (re-binding the same address later works).

Deliberately simplified, by decision rather than accident:
- **TCP is not simulated.** `SOCK_STREAM` passes through to the kernel
  untouched. Simulating TCP means simulating connection state, flow control,
  and partial-delivery semantics — a Phase 4+ project. Programs that must be
  simulated today use UDP.
- **`connect`+`send`/`recv` on UDP sockets is not intercepted** — only
  `bind`/`sendto`/`recvfrom`. `getsockname`, `setsockopt`, `poll`/`select`/
  `epoll` on simulated sockets are also out of scope for now.
- **Delay is an ordering key, not wall time.** The broker delivers the
  lowest-delay pending datagram whenever the receiver asks; it does not hold
  datagrams for their delay in any clock. Latency values therefore shape
  *relative order* (and hence reordering), not measured round-trip time.
  There is no cross-process virtual time base to anchor real delays to — that
  unification is future work.
- **Bandwidth is per-datagram serialization delay** (`len/rate` added to the
  ordering key), not a shared queue with backpressure. It biases ordering
  against large datagrams, which is the property distributed-systems bugs
  care about; it does not model queue depth or drops under saturation.
- `AF_INET6` and raw sockets pass through.

## Honest limitations

- **Cross-process interleaving is not unified.** Fates and per-channel
  content are seed-deterministic across processes, but *which process's
  syscall reaches the broker first* is OS scheduling. Concretely: with
  `--nodes 2`, the two nodes' stdout lines may interleave differently across
  runs, and a datagram's arrival relative to a *different channel's* recv is
  timing-dependent. Where the racing entities are threads of one process, the
  Phase 2 scheduler removes this nondeterminism completely — hence the
  threads-as-nodes pattern for bug reproduction. True multi-process
  determinism requires extending the cooperative scheduler across process
  boundaries (a broker-side global turn model), which is future work.
- **Startup discard races are real UDP behavior.** A send racing a peer's
  `bind` may be discarded; robust UDP code retries (as `pingpong.c` does).
- **Managed polling spins.** A managed thread waiting for a datagram
  busy-polls broker + `yield_now`. Deterministic, but CPU-hungry while
  starved; a parked-thread integration with the scheduler is future work.
- **Broker requests hold the token**, so heavy per-datagram traffic in one
  thread starves others only at yield points, and everything network-bound is
  serialized process-wide. That is the price of determinism, and it shows in
  the benchmark below.

## Overhead (measured)

`scripts/bench-net.sh`, 5000 round trips (10 000 datagrams) between two
threads of one process, container on Apple-silicon host, best of 5:

| | wall time | per datagram |
|---|---|---|
| native kernel loopback UDP | 5 ms | ~0.5 µs |
| weft simulated (`--net ""`) | 1425 ms | ~142 µs |

**≈285× slower per datagram.** The cost is one Unix-socket round-trip to the
broker per operation plus token-serialized execution and poll-yield receive
loops. For simulation workloads (protocol logic, not bulk transfer) this is
acceptable; bulk-data phases should be modeled, not replayed byte-for-byte.

## The bug proof (`examples/kvreplica.c`)

A replicated register applies `UPDATE:v` messages **in arrival order with no
version check** — the classic "UDP from one sender is mostly ordered" fallacy.
A writer thread sends v=1..8 then a `READ`. Under
`--net latency=uniform:1000-50000`:

- **seed 1**: the tail of the write burst is reordered; the replica overwrites
  v=8 with an older write and the read returns **6** (stale) — 20/20 runs.
- **seed 0**: delivery happens to stay in order; the read returns **8** — 20/20
  runs.
- Any seed with `--net ""` (no variance): always correct — the bug needs
  reordering, and a zero-variance network cannot reorder.

`crates/weft-dst/tests/net_e2e.rs` pins all three facts in CI, plus
reproducibility under `latency=exp`. Broker-level behavior (deterministic
loss, partition blocking, join/leave, blocking-recv wakeup) is covered by
`crates/weft-net/tests/broker_integration.rs`.
