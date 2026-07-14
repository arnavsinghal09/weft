# Multi-host deterministic event ordering — protocol design

Status: **design only, nothing implemented.** This document supersedes the
"logical clock protocol" section of
[MULTI_HOST_ARCHITECTURE.md](MULTI_HOST_ARCHITECTURE.md) (whose
merge-on-response rule was implemented, shown to break same-seed
determinism, and reverted — post-mortem below) and turns that document's
"designed, not built" closing sketch into a full specification. No shim,
broker, or transport work should proceed against multi-host ordering until
the open questions at the end of this document are resolved.

Prior art this design instantiates: conservative parallel discrete-event
simulation (Chandy–Misra–Bryant null messages / lower-bound-on-timestamp
sealing) with Lamport-clock merging, specialized to Weft's constraint that
simulated processes are **real, unmodifiable OS processes** that cannot be
checkpointed or rolled back.

---

## 1. Post-mortem: why merge-on-arrival broke

The reverted design piggybacked the broker's virtual-time high-water mark
`vt` on every response, and the shim merged it into its local clock
(`local = max(local, vt)`) on receipt.

The flaw is one sentence: **the broker's `vt` at the moment it answers a
request is a function of which other process's request happened to arrive
first**, which is OS scheduling and real network timing — not the seed.
Merging it made guest-visible time (every subsequent `clock_gettime`, every
sleep deadline, every trace timestamp, and — through timing-dependent guest
branches — the guest's entire future behavior) a function of real arrival
order. Two live runs of one seed diverged; `net_e2e`'s same-seed tests
failed; the merge was removed (CHANGELOG "Added (multi-host groundwork)").

The general lesson, stated as an invariant this protocol must maintain:

> **I-SEED.** No value that can influence a guest's observable behavior may
> be derived from real arrival order, real wall-clock time, or OS
> scheduling. Every such value must be a pure function of
> `(seed, static configuration, guest program)`.

The wire fields the reverted attempt added (`local_vt` on requests, `vt` on
responses) are kept: `local_vt` is load-bearing in this protocol (§4), and
a response-carried virtual time is again merged into the guest clock — but
it is a different quantity (§6): a **seed-derived assigned time**, not an
arrival-ordered observation.

## 2. Model and definitions

- **Guest** — one shimmed target process. Identified by `node_id` (assigned
  by the orchestrator, stable across the run) on a `host_id`.
- **Local virtual time (LVT)** — the guest's Phase-1 vclock: monotonic ns,
  advancing on clock reads and (virtualized) sleeps. Per Phases 1–2, a
  single guest's execution — its sequence of intercepted operations, each
  op's content, and the LVT at which it issues each op — is a pure function
  of the seed *and of what the network delivers to it and at what virtual
  times* (Lemma L1, §7).
- **Op** — one guest→broker operation: `Send{src,dst,payload,local_vt}` or
  `Recv{addr,blocking,local_vt}` (plus `Hello`/`Bind`). `local_vt` is the
  guest's LVT when the op was issued.
- **Connection sequence number `conn_seq`** — ops on one connection are
  FIFO (TCP/Unix stream), numbered 0,1,2… by the broker per connection.
  Program order within a guest is therefore visible to the broker.
- **Frontier `F(g)`** — a per-guest promise: "guest g will never again emit
  an op with `local_vt < F(g)`". How frontiers are established is §4.
- **Window `k`** — the virtual-time interval `[kW, (k+1)W)` for a fixed
  configured width `W` ns. Windows are the unit of ordering.

## 3. Requirements

- **R1 (determinism).** The global order of all ops, every fate decision,
  every delivery's virtual time, and every guest's vclock trajectory are
  pure functions of `(seed, config)` — invariant I-SEED.
- **R2 (sound merging).** Guest clocks may merge only broker-assigned,
  seed-derived times. Nothing arrival-ordered ever flows into a guest.
- **R3 (liveness).** If every guest is either making progress or blocked on
  the network, the protocol makes progress. Deadlock and quiescence are
  detected and reported deterministically.
- **R4 (honest failure handling).** Real-world failures (host crash, TCP
  drop, wedged guest) never silently produce a *different* ordering; they
  either map onto an injected-fault path with a virtual-time coordinate, or
  abort the run loudly. A run either completes identically to every other
  same-seed run, or does not complete.
- **R5 (compatibility).** Recording and replay remain the audit artifact;
  format changes go through VERSIONING.md's log-format contract.

## 4. Protocol

### 4.1 The broker becomes a sequencer

The broker's role changes from *passive relay that timestamps on arrival*
to *active scheduler in three duties*:

1. **Admission**: accept ops from all connections, append each to a
   per-connection FIFO buffer. Arrival order across connections is
   **recorded nowhere and used for nothing**.
2. **Sealing**: window `k` seals when every live connection's frontier
   satisfies `F(g) ≥ (k+1)W`. Sealing is the only place the protocol
   *waits* on the real world, and waiting affects only wall-clock duration,
   never content (proof: §7).
3. **Assignment**: after sealing, the broker takes every buffered op with
   `local_vt` in window `k`, sorts them by the **total order key**

   ```
   (local_vt, host_id, node_id, conn_seq)
   ```

   and feeds them through `Core` in that order. This sorted sequence — not
   arrival order — is the linearization, the recording content, and the
   input to every fate draw.

Every component of the key is seed-derived (`local_vt` by Lemma L1; the
identifiers are static config; `conn_seq` is program order, seed-derived by
L1), so the assigned order satisfies R1.

Tie-break note: lexicographic `(host_id, node_id)` on equal `local_vt` is
deterministic but always favors the same guest. An alternative is a
seeded permutation of guest identity per window (extra schedule diversity
across seeds, equally deterministic). **v1 choice: lexicographic** — one
less mechanism; diversity already enters through per-channel latency
draws. Recorded as open question OQ-2.

### 4.2 Frontier establishment

A guest's frontier advances through three mechanisms, all piggybacked —
there is no mandatory extra round-trip in busy phases:

- **Op-carried**: every op's `local_vt` is, by clock monotonicity plus
  connection FIFO, a frontier declaration `F(g) ≥ local_vt`. (The shim's
  vclock is strictly monotone; the broker rejects a `local_vt` below the
  connection's previous one as a protocol violation → abort, R4.)
- **Explicit `Frontier{local_vt}` message**: sent by the shim when the
  guest's LVT has advanced ≥ one window width past its last declaration
  without any network op (a guest sleeping through virtual hours, or
  computing). Sent from the shim's existing hook path — no new threads in
  the guest; the check rides on whichever intercepted call advanced the
  clock. A guest executing a long stretch of *uninstrumented* pure
  computation advances neither its LVT (nothing intercepted) nor its
  frontier — see failure mode F5.
- **Release-on-block**: a guest entering **blocking** `Recv` stops emitting
  spontaneously. The original claim here — that it may declare `F(g) = +∞` —
  is **unsound for request/reply and was corrected in implementation.** If a
  blocked receiver goes to `+∞`, the whole cluster can seal every window; the
  receiver's *reaction* to a just-delivered message is then admitted at its
  post-delivery virtual time, which lands in an already-sealed window and is
  rejected as a late op. Correct rule: a blocked guest waiting on address `A`
  contributes its **reactivation bound** — the least `local_vt` of a pending
  send aimed at `A`, plus lookahead `L_min` (§5) — i.e. the earliest virtual
  time it could emit once woken. It contributes `+∞` only when *no* pending
  send targets it (nothing can wake it). Combined with releasing deliveries at
  `deliv_vt < sealed_horizon + L_min` (the §5 lookahead, no longer optional),
  this keeps the window a woken guest emits into open until it has emitted.
  **Consequence: windowed mode requires `L_min ≥ W`** (lookahead at least the
  window width); with `L_min = 0` a receiver's reactivation bound equals a
  send's own time and stalls that send's delivery — the degenerate deadlock.
  A guest that exits (connection close or process exit) is `F(g) = +∞`
  permanently (nothing more to react to). A managed multi-threaded receiver,
  which polls the broker non-blocking and so cannot advance the horizon it
  waits on, announces its block with an explicit `Park{addr, local_vt}`
  message when it goes idle.

The multi-threaded guest case is where the current implementation has a
hole that this protocol must close, because the existing **managed-thread
polling loop is nondeterministic under windowing**: today a managed thread
polls non-blocking `Recv` in a loop, calling `sched.yield_now` between
polls, and *each yield consumes scheduler RNG*. Under windowing, the
number of Empty polls before a delivery depends on how fast windows seal
in real time — real-time-dependent RNG consumption, violating I-SEED.
Design consequence (flagged for implementation, not done here): blocking
network waits by managed threads must become a **modeled scheduler state**
(`BlockedNet(addr)`, alongside `BlockedMutex`/`BlockedCond`), entered at
the recv yield point and woken by delivery. The thread then consumes
scheduler decisions only at deterministic points, and a fully-blocked
guest (all managed threads `BlockedNet`/`BlockedJoin`) releases its
frontier to `+∞` exactly like the single-threaded case. Until that
scheduler change exists, windowed multi-host must not ship (OQ-5).

### 4.3 Delivery assignment

For each admitted `Send` in assigned order, `Core` draws the fate exactly
as today — `fate(seed, src, dst, chan_seq, len)`, unchanged and
transport-independent — and schedules delivery at

```
deliv_vt = send.local_vt + fate.delay_ns
```

**This is a semantic change to `Core`**: today `deliv` is the bare latency
draw, un-anchored to send time (fault.rs draws a relative `delay_ns`;
core.rs uses it as the absolute queue coordinate). Anchoring on
`send.local_vt` is required so that delivery times are meaningful on a
shared cross-host timeline, and it changes recomputed values for existing
recordings — a log-format compatibility event (R5, §9).

A `Recv` in assigned order pops the receiver's queue only among messages
with `deliv_vt` **inside an already-sealed window** (`deliv_vt <
sealed_horizon`); a message scheduled for a future window is not yet
poppable even if queued — otherwise delivery visibility would depend on
how early the send was admitted in real time. Pop order is
`(deliv_vt, assignment tie)` as today.

### 4.4 Vclock merge — the corrected rule

Replacing the reverted merge-on-arrival:

- **On `Deliver`**: the guest merges `local = max(local, deliv_vt)`.
  `deliv_vt` is seed-derived (R2 satisfied). This is the Lamport rule with
  the message's *assigned* timestamp — receiving a message can only move
  you forward to the (virtual) moment it arrived.
- **On `Empty`** (non-blocking recv against an empty queue): the guest
  merges nothing. The reply carries the sealed horizon for observability
  only; merging it would make guest time depend on real sealing progress.
- **On `Ack`** (send/bind): merge nothing. Sends do not teleport a sender
  forward.

Result: guest LVT is driven only by its own execution and by seed-derived
delivery times — exactly the quantities Lemma L1 permits.

### 4.5 Wire protocol delta (design level)

- `Frontier { local_vt }` — new guest→broker message (§4.2).
- `Deliver { …, vt }` — existing field, reinterpreted: carries `deliv_vt`
  (assigned), not the arrival-ordered high-water mark.
- `Ack { vt }` / `Empty { vt }` — retained for skew observability and
  diagnostics; explicitly **must not** be merged (enforced by the shim, and
  by a determinism e2e test that fails if they are).

### 4.6 Window lifecycle summary

```
OPEN(k)      ops with local_vt ∈ [kW,(k+1)W) accumulate, unordered
  │  every live connection reports F(g) ≥ (k+1)W
SEALED(k)    contents frozen — provably complete (no guest may emit into it)
  │  sort by (local_vt, host_id, node_id, conn_seq); fates; deliveries
ASSIGNED(k)  ops appended to linearization + recording; deliveries with
             deliv_vt < (k+1)W become poppable; k := k+1
```

## 5. Buffering strategy and its trade-offs

Windowing is buffering; the design decisions are the width `W` and the
idle-frontier cadence.

- **Small `W`**: sealing overhead dominates — every window needs every
  connection's frontier to cross a boundary, so idle guests must emit
  explicit `Frontier` messages at fine grain; real-time cost ≈ one
  cross-host RTT per window in the worst (idle) case. Virtual latency
  resolution is fine-grained.
- **Large `W`**: fewer seals, better throughput, but a delivery scheduled
  early in a window is withheld until the window seals — added *real*
  latency for request/reply guests (a ping's reply waits for the whole
  cluster's frontiers), and coarser interleaving granularity (all of a
  window's sends order before any of its deliveries become visible).
- The degenerate case `W → 0` is the old document's "turn token / lockstep
  admission" sketch: strictest, slowest, and its busy-guest deadlock is
  exactly the missing-frontier problem that §4.2's explicit `Frontier`
  message and the `BlockedNet` state solve. Windowing generalizes the
  token design rather than replacing it.
- **Lookahead (future optimization, OQ-4)**: if the configured latency
  distribution has a minimum `L_min > 0`, a send admitted in window `k`
  cannot deliver before `kW + L_min`, so deliveries into the first
  `⌊L_min/W⌋` subsequent windows can be released before those windows seal
  — the classic CMB lookahead. Requires `L_min` to be a static property of
  the net spec (true for `fixed:` and `uniform:LO-HI` with LO>0; false for
  `exp:`).
- **Adaptive `W`** is admissible only if adaptation keys on *virtual*
  quantities (e.g., op density per window), never on real measurements —
  otherwise `W` itself becomes an arrival-order channel. v1: fixed `W`
  from config; default suggestion 1 virtual ms (matches the case-study
  tick scale). OQ-3.

Broker memory is bounded by ops-per-window; a window cannot seal with
unbounded content unless a guest emits unboundedly within one window
width, which is bounded by guest op rate × W.

## 6. What the broker's `vt` means now

`Core.vt` (high-water mark) remains for stats, but the **authoritative
clock is the sealing horizon**: `sealed_horizon = (k_sealed+1)·W`. Global
virtual time advances by sealing, which requires unanimous frontier
progress — time is *pulled forward by the slowest guest*, not pushed by
whoever talks fastest. That inversion is the essence of the fix: in the
reverted design, chatty guests dragged everyone's clocks forward in
arrival order; here, the laggard bounds the horizon and order within the
horizon is arrival-independent.

## 7. Correctness argument

**Lemma L1 (single-guest determinism — existing, tested).** Given the
seed and a fixed sequence of deliveries `(payload_i, deliv_vt_i)` presented
at deterministic points, a guest's execution — every op, its content, its
`local_vt`, every scheduler decision, every RNG byte — is a pure function
of `(seed, node_id, deliveries)`. This is the Phase 1–2 guarantee (e2e
determinism tests), extended by the `BlockedNet` scheduler state (§4.2) so
that *waiting* for a delivery consumes no nondeterministic decisions.
Scope caveat: L1 holds only for guests within the interception surface
(LIMITATIONS.md §1 — static binaries, raw syscalls, etc. escape it).

**Lemma L2 (frontier honesty).** No op is ever assigned to a window after
that window sealed. By construction: sealing waits for `F(g) ≥ (k+1)W` for
every live connection; frontiers are monotone; an op violating its
connection's declared frontier aborts the run (R4). Guest exit and
blocking-recv release are sound because a blocked or dead guest emits
nothing until the broker itself acts.

**Lemma L3 (no side channels).** Guests interact only through the broker.
Cross-guest channels outside the model (shared files on one host, raw
un-intercepted sockets) void the theorem — documented limitation, same
class as LIMITATIONS.md §1/§2, restated here because multi-host makes
shared-filesystem side channels *less* likely (different hosts) but
NFS-style shared mounts reintroduce them.

**Theorem (run determinism).** For two executions of the same
`(seed, config, binaries)` on arbitrary real hardware, networks, and OS
scheduling, the assigned linearization, every fate, every `deliv_vt`, and
every guest's LVT trajectory and output are identical.

*Proof sketch — induction on window index `k`.*

- *Invariant I(k)*: the assigned contents and order of windows `0..k`, and
  the complete state of every guest up to its last event with
  `local_vt < (k+1)W`, are pure functions of `(seed, config)`.
- *Base*: before any delivery, each guest's prefix (ops with `local_vt` in
  window 0, and frontier declarations) is deterministic by L1 with an
  empty delivery sequence. Sealing waits until all frontiers pass `W` —
  waiting is real-time-dependent, but by L2 the *set* of window-0 ops is
  exactly the guests' deterministic prefixes, and the sort key contains no
  arrival component; so window 0's assignment is deterministic.
- *Step*: assume I(k). Deliveries with `deliv_vt < (k+1)W` are computed
  from windows `≤ k` (assigned order + seeded fates + anchored delivery
  times), hence deterministic. By L1, each guest's continued execution up
  to `local_vt < (k+2)W` — including which ops it emits into window `k+1`
  and its frontier messages — is a pure function of those deliveries.
  Sealing of `k+1` again affects only when, not what (L2). Assignment
  sorts by an arrival-free key. I(k+1) holds. ∎

*What would break each leg* (the falsifiable surface, in the project's
testing idiom — each gets a test that fails without it):

- L1 broken by: RNG consumed in a real-time-dependent loop (the polling
  hole, §4.2), a guest reading an unvirtualized clock, un-intercepted I/O.
- L2 broken by: a non-monotone `local_vt` (shim bug), a lost `Frontier`
  message (transport must be reliable — TCP, or abort on drop).
- Assignment broken by: any arrival-ordered input to the sort key or to
  `Core` (e.g., reintroducing high-water-mark merging).

## 8. Failure modes and deterministic handling

| # | failure | detection | handling (must satisfy R4) |
|---|---|---|---|
| F1 | Real host/guest crash mid-window | TCP close without goodbye | Abort the run and mark it invalid (campaign discard, like `chord-check` exit 3). A real crash is an out-of-model event; continuing would assign an ordering that depends on *when* (real time) the crash landed. Crashes as *faults under test* must instead be injected via orchestrator events with virtual-time coordinates, which kill deterministically at a seal boundary. |
| F2 | Slow host (real-time laggard) | Sealing stalls; horizon stops | Correctness unaffected (stall changes duration, not content). Liveness: everyone waits — conservative PDES's price. Report per-host frontier lag in `--stats`. |
| F3 | Guest wedged in uninstrumented compute (frontier frozen, not blocked) | No frontier progress + connection alive + not `BlockedNet` | Real-time watchdog (configurable). Firing is inherently nondeterministic ⇒ the only permitted action is **abort the whole run** — never skip, never seal without the frontier. A watchdog abort can differ between runs; a *completed* run cannot. |
| F4 | TCP drop / reconnect of a live guest | Connection close + re-`Hello` | v1: abort. Connection identity is ordering identity (`conn_seq`); resuming would splice streams by real-time coincidence. (Deterministic reconnect for *injected* restarts goes through the orchestrator, which assigns the restart a virtual time.) |
| F5 | Guest emits `local_vt` below its frontier | Broker-side check | Protocol violation ⇒ abort loudly (indicates a shim clock bug — this is an invariant with a test, not a recoverable state). |
| F6 | Distributed quiescence: all guests `F=+∞` (blocked/exited), no poppable deliveries | Broker state scan at seal attempt | Deterministic global deadlock report, mirroring the single-host scheduler's DEADLOCK abort — same seed always quiesces at the same point. |
| F7 | Window buffer growth | ops-per-window bound exceeded (config) | Deterministic abort with the window census (a diagnostic, and a backpressure guard against a guest spamming sends inside one window). |

## 9. Impact on existing guarantees and documents

- **Upgrades the headline guarantee.** Today: "live multi-process same-seed
  runs may diverge; the recording is the determinism artifact"
  (LIMITATIONS.md §3c — the caveat the README leads with). Under this
  protocol, live same-seed determinism becomes *claimable* for `--net`
  runs, single-host and multi-host alike — the windowed broker fixes the
  single-host arrival race too, since nothing in §4 is TCP-specific.
  Whether to adopt windowing for plain single-host `--net` (and retire the
  §3c caveat at the cost of sealing latency and the scheduler change) is
  OQ-1 — the largest product decision here.
- **Recording becomes redundant for reproduction, kept as artifact.** The
  linearization is seed-derived, so seed alone reproduces a run; the log
  remains the checkable artifact (checkers, shrinker input) and the
  compatibility surface. `deliv_vt` anchoring (§4.3) changes what replay
  recomputes ⇒ **weft-log format bump** per VERSIONING.md §1 (v1 logs stay
  replayable under v1 semantics; the version field gates which `Core`
  semantics replay applies).
- **Scheduler**: new `BlockedNet` state (§4.2) — an extension of the
  Phase-2 model with the same determinism obligations (its own harness
  tests).
- **MULTI_HOST_ARCHITECTURE.md**: its clock-protocol section and its
  "changed where" table row for the shim merge describe the reverted
  design; superseded by this document.

## 10. Rejected alternatives

- **Merge broker high-water mark on receipt** — implemented, broke
  same-seed determinism, reverted (§1). Rejected by evidence.
- **Optimistic ordering (Time Warp): run on arrival order, roll back on
  violation** — requires checkpoint/rollback of guests. Weft's guests are
  unmodified OS processes; rollback would need CRIU-class snapshotting per
  message, wildly outside the do-no-harm envelope. Rejected structurally.
- **Turn-token lockstep (the prior sketch)** — sound but is the `W→0`
  special case of windowing with strictly worse constants and the same
  need for frontier/idle signals; subsumed (§5).
- **Real clock synchronization (NTP/PTP) + timestamp ordering** — replaces
  OS-scheduling nondeterminism with clock-sync nondeterminism; violates
  I-SEED by construction. Rejected.

## 11. Open questions (deliberately unresolved)

- **OQ-1**: Adopt windowing for single-host `--net` and retire
  LIMITATIONS.md §3c? Gains the strongest possible claim ("same seed ⇒
  same run, live"); costs sealing latency on every run and makes the
  `BlockedNet` scheduler change load-bearing everywhere. Needs a
  benchmark (bench-scalability.sh extension) before deciding.
- **OQ-2**: Tie-break within equal `local_vt`: lexicographic (v1 proposal)
  vs seeded per-window permutation (more schedule diversity per seed).
- **OQ-3**: Window width `W` — fixed config (v1: 1 virtual ms) vs adaptive
  on virtual-only signals. What is the right default for targets whose
  tick loops are much faster/slower than the case studies'?
- **OQ-4**: CMB lookahead using `L_min` of the latency distribution —
  worth the Core complexity? (Only helps `fixed:`/`uniform:` with LO>0.)
- **OQ-5**: The `BlockedNet` scheduler state is a precondition (the
  polling loop consumes RNG per real-time poll today, §4.2). Is there a
  cheaper interim rule — e.g., yield sites that consume no RNG when the
  yielder is the only runnable thread — that preserves I-SEED without the
  full state? (Suspected no: multi-threaded guests still poll while
  siblings run. Needs a worked counterexample either way.)
- **OQ-6**: Frontier cadence for guests that sleep across many windows in
  one `nanosleep` — should the shim split a long virtual sleep into
  per-window frontier reports (chatty, simple) or report the sleep's end
  as an immediate frontier jump (one message; requires the broker to
  accept far-future frontiers — it does, they're monotone promises)?
  Proposal: the latter; verify no interaction with injected crash events
  scheduled mid-sleep.
- **OQ-7**: Exact log-format v2 shape: record window boundaries as first
  -class records (better auditability, bigger logs) or reconstruct them
  during replay from `local_vt` (smaller, more recomputation)?
