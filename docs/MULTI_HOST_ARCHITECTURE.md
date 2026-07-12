# Multi-host deterministic execution — architecture

Extends Weft's single-machine cluster mode (`weft run --net --nodes N`) to
real multiple hosts: target processes on different machines, one master
broker, real TCP between them, one recording. Design principle: **the
broker's linearization order stays the single source of truth**; multi-host
support changes the *transport* under that truth, never the truth itself.
Phase 1–6 core logic is untouched except at the two points multi-host
support structurally requires: the shim↔broker wire protocol (transport +
clock piggyback) and the broker's accept loop (TCP alongside Unix sockets).

## Components

```
host A (master)                      host B                    host C
┌──────────────────────────┐   ┌─────────────────┐   ┌─────────────────┐
│ weft run --hosts B:7601, │   │ weft hostd :7601 │   │ weft hostd :7601 │
│   C:7601 --net … --record │   │   │ spawn/kill    │   │   │ spawn/kill    │
│  ├─ broker (TCP :7600)    │◄──┼───┤ target procs  │   │   │ target procs  │
│  ├─ recorder (weft-log v1)│   │   │  + shim ──────┼───┼───┼── TCP ────────┤
│  └─ control conns to hostd│   └─────────────────┘   └─────────────────┘
└──────────────────────────┘
```

- **Master** (`weft run --hosts`): hosts the seeded broker on TCP, connects
  to each host agent, distributes node spawns, records, reaps exits.
- **Host agent** (`weft hostd`): one per host. Receives spawn specs over a
  TCP control channel (JSON lines), launches target processes under the
  shim with `WEFT_BROKER=<master>:<port>`, reports exits, kills on command.
- **Shim**: unchanged interception; its broker connection now dials TCP
  when `WEFT_BROKER` looks like `host:port` (a Unix path otherwise). The
  shim's broker I/O already operates on raw fds, so a TCP fd flows through
  the same code as a Unix-socket fd.
- **Recorder/replay**: byte-for-byte unchanged. The log stays **weft-log
  v1**: broker operations in linearization order. Host→node mapping is
  recorded in the header's informational `meta` object (readers MUST
  ignore unknown meta keys — that rule exists for exactly this kind of
  addition, VERSIONING.md §1).

## The logical clock protocol

> **Superseded.** The merge-on-response rule below was implemented, broke
> same-seed determinism (broker `vt` advances in real arrival order, and
> merging it fed OS scheduling into guest-visible time), and was reverted —
> the shim now sends `local_vt` but ignores response `vt` (CHANGELOG,
> "Added (multi-host groundwork)"). The replacement — seed-derived windowed
> sealing with frontier-gated assignment — is specified in
> [MULTI_HOST_CLOCK_PROTOCOL.md](MULTI_HOST_CLOCK_PROTOCOL.md) and is
> design-only. This section is kept as the historical record the
> post-mortem refers to.

**Problem.** Each shimmed process has a virtual clock (Phase 1): monotonic
ns, advancing on reads and sleeps, independent per process. On one machine
that independence is invisible; across hosts, causally-related events could
carry wildly diverging virtual timestamps.

**Protocol (Lamport-style, broker as master).**

- The broker's `Core` already maintains a seeded virtual-time high-water
  mark `vt` that advances as messages are scheduled and delivered — this is
  the **master logical clock**.
- Every broker response now carries `vt` (piggybacked; no extra round
  trips). Requests carry the sender's current local virtual time
  `local_vt` (for skew observability, below).
- On receiving any broker response, the shim merges:
  `local_clock = max(local_clock, vt)` — the existing
  `advance_mono_to` primitive, which is a monotone `fetch_max`; local time
  never moves backward.

**Bounded-skew proof sketch.** Define skew(p) at any instant as
`local_vt(p) − vt(broker)` (signed). Claim: for a process whose broker
interactions are at most `W` local-clock ns apart (W = the largest amount
its virtual clock advances between two consecutive broker calls —
`chord_node`/`raft_node` tick loops give W ≈ one tick: 1 ms + a few µs of
reads):

1. *Downward bound*: immediately after a response, local ≥ vt(response) by
   the merge; vt is read under the broker lock, so local can lag the
   broker's **current** vt only by what the broker schedules between that
   response and the process's next call — but lag never affects
   correctness: delivery order is decided by the broker alone; a lagging
   local clock merely timestamps local events earlier. On the next
   interaction the merge erases the lag.
2. *Upward bound*: local exceeds vt(last response) by at most W before the
   next interaction forces a new merge, so
   `local_vt(p) ≤ vt(broker at last response) + W`.

Hence |skew| observed **at interaction points** ≤ W plus whatever the
broker advanced concurrently — both measurable, neither affects the
recording's determinism (the log's op order and virtual times are all
assigned broker-side under one lock). This is deliberately a *weak*
guarantee with an honest shape: Weft's determinism story never depended on
synchronized wall clocks, and multi-host does not change that; the clock
protocol exists so that per-node local timestamps (traces, reports) stay
comparable across hosts, with quantified error.

**Skew observability.** The broker records
`max |local_vt(request) − vt(core)|` across all operations and reports it
in `--stats` — the measured bound for MULTI_HOST_VALIDATION.md.

**Host crash and rejoin.** A host death closes its shims' TCP connections;
the broker's existing disconnect path unbinds their addresses and wakes
blocked receivers (already how single-host process death works — same code
path, now transport-agnostic). The recording simply stops containing sends
from those nodes: still a valid v1 log. A rejoining host reconnects and
re-Hellos; the first response re-syncs its clock via the monotone merge —
no special-case protocol. What multi-host does **not** attempt: migrating a
dead host's *node state* (that is the target system's job — Weft kills and
restarts processes; it does not checkpoint them).

## Deterministic ordering across hosts — the honest statement

Single-machine cluster runs already have the documented Phase-3 limitation:
which process's request reaches the broker first is OS-scheduled, so live
runs of one seed are not verdict-deterministic; the recording is the
determinism artifact. Multi-host makes the arrival race *wider* (real
network latency), but **the guarantee is unchanged in kind**:

- Every fate decision remains a pure function of `(seed, src, dst, seq)` —
  the fault model is broker-level and transport-independent, so the *same*
  arrival order yields the *same* run on any transport.
- The recording captures the arrival order that actually happened; replay
  (pure computation, no hosts, no sockets) reproduces it byte-for-byte on
  any hardware. "Replay across different hardware" is validated by
  recording on x86-64 Linux containers and replaying on an arm64 macOS
  host (MULTI_HOST_VALIDATION.md).
- **Live seed-determinism across hosts is not claimed.** The design for
  closing it — a broker-granted turn token, i.e. lockstep admission of one
  in-flight operation at a time in seeded order over registered nodes — is
  compatible with this architecture (the broker already owns a global lock
  at exactly the right point) but is **designed, not built**: it needs
  per-node idle/computing signals to avoid deadlocking on nodes that are
  busy computing rather than blocked on the network, and its latency cost
  (one cluster-wide serialization point per op) would change the
  performance envelope enough that it should be an explicit opt-in mode.

"Mid-replay host failure" is therefore a non-event by construction: replay
involves no hosts. Host failure during a *live recorded run* is handled (a
node's ops just stop); the resulting log replays like any other.

## Fault simulation over real TCP

Unchanged in substance: latency/loss/reordering/partitions were never
implemented at the localhost-socket level — they live in the pure decision
`Core` the broker consults under its lock (Phase 3 design). Moving the
shim↔broker transport from Unix sockets to TCP moves the *carrier*, not
the fault model. Real-network latency between host and broker adds to the
(virtual) simulated latency only in wall-clock terms; it cannot reorder
the broker's linearization retroactively, because order is assigned at
lock acquisition, exactly as before.

## What changed where (Phase 1–6 delta, complete list)

| file | change | why required |
|---|---|---|
| `weft-net/src/wire.rs` | `vt` piggyback on responses, `local_vt` on Send/Recv | clock protocol |
| `weft-net/src/broker.rs` | stream-generic conn handler; `bind_tcp`/`run` over both listeners; skew tracking in stats | TCP transport |
| `weft-shim/src/hooks/socket.rs` | dial TCP when `WEFT_BROKER` is `host:port`; merge `vt` into vclock; send `local_vt` | transport + clock |
| `weft-dst` | new `hostd` module + `--hosts` in run path; host↔node map into log `meta` | orchestration |

Everything else — scheduler, vclock internals, recorder, replayer, fuzz,
scenario, Core — is untouched.
