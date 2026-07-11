# Phase 7 target selection — reasoning (read this first)

The phase asks for the toolchain's centerpiece result: point the *entire*
stack (interception, scheduling, network/fault sim, record/replay, fuzz,
shrink) at a real project we did not write, and find a real bug. The
credibility of that result depends entirely on the target choice and on the
bug being real — so this document is deliberately explicit and honest about
what is and is not achievable with **this** toolchain on **this** machine,
before any code is written.

## The three selection criteria (from the brief)

1. Genuine concurrency / distributed-systems complexity.
2. Tractable to integrate in the time available.
3. Not so trivial that a bug would be unconvincing to a skeptic.

## Two hard constraints the toolchain imposes (verified, not assumed)

These are load-bearing. They eliminate the naive reading ("just fuzz Redis")
and must be stated plainly, because a skeptical reader will ask exactly this.

**C1 — The interception path is Linux-only; this host is macOS.**
`weft run` (LD_PRELOAD shim injection: time, randomness, threads, UDP, file
faults) is `#[cfg(target_os = "linux")]`. On Darwin it is a compile-time stub
that returns an error (`crates/weft-dst/src/run_cmd.rs`). A Linux container is the
only way to exercise the shim at all. `docker` is available here, so this is
surmountable but real.

**C2 — The simulated network is UDP-datagram only, via libc symbols.**
Per `docs/network-model.md` and the shim's hooks: only
`socket(AF_INET, SOCK_DGRAM)` + `sendto`/`recvfrom` are diverted to the
broker. **TCP passes straight through, unsimulated.** `connect`+`send`/`recv`
on UDP is not intercepted. Raw syscalls (Go's runtime) and vDSO-by-address
bypass the shim. So a target must (a) be a UDP-datagram program, (b) use libc
socket symbols, (c) not be statically linked, (d) not be Go.

**C3 — The fuzzer is a model checker over the broker core, not a process
harness.** `weft-fuzz` drives `weft_net::core::Core` in-process with synthetic
`OpInput` sequences (`gen.rs`); it never launches an external binary. To fuzz
a *real* program's behavior you would run it under `weft run --net --record`
(Linux, UDP-only) and replay/shrink recordings — a different, heavier loop
that is not what `weft fuzz` does today.

### What C1–C3 eliminate

- etcd, TiKV, CockroachDB, Kafka, NATS, Redis Cluster, ScyllaDB, FoundationDB,
  any Raft/Paxos production implementation: **TCP and/or Go**. Ruled out by C2.
- Any target on this macOS host directly: ruled out by C1 (Linux container
  only).
- "Point `weft fuzz` at project X's binary and sweep seeds": not what the
  fuzzer does (C3). The fuzzer explores the *network model's* fault space
  against a workload, checking invariants — it is closest in spirit to a
  TLA+/`madsim`-style model checker, not AFL-style binary fuzzing.

Being honest: the toolchain is a from-scratch deterministic-simulation
framework whose network layer is intentionally a simplified UDP model. It is
**not** a drop-in harness for arbitrary production distributed systems, and no
amount of glue makes a TCP/Go system testable under it. Pretending otherwise
would produce exactly the un-credible result the brief warns against.

## The candidate that survives

Given C1–C3, the only route to a *real bug in a real system* — rather than a
bug we planted in our own toy — is to take a **real, published, precisely
specified distributed protocol** whose reference pseudocode is public, model
its replica logic faithfully as state machines exchanging datagrams through
the broker, encode the protocol's **own stated safety invariant**, and let the
fuzzer search the fault-schedule space (drop / reorder / delay / partition of
protocol messages) for a schedule that violates it.

The strongest such target: **the Chord distributed hash table's
ring-maintenance protocol** (Stoica et al., SIGCOMM 2001).

Why Chord specifically:

- **Real and non-trivial (criterion 1 & 3).** Chord is one of the most-cited
  systems papers (~15k citations); its "stabilization" protocol for
  maintaining a consistent successor ring under concurrent joins and failures
  is genuinely subtle. A correctness problem here is not a strawman.
- **A documented, independently-verifiable real bug exists.** Pamela Zave
  ("Using Lightweight Modeling to Understand Chord," CACM/formal-methods work,
  2012–2017) proved that the **originally published** Chord protocol is *not*
  correct: under certain interleavings of node failure and join, the ring
  invariant (one ordered ring) is broken and stabilization does not repair it.
  This is decisive for credibility: if our fuzzer finds a violation, a skeptic
  can check it against Zave's published counterexample to confirm the bug is
  **real and in the protocol**, not an artifact of our modeling. We would be
  *rediscovering a documented bug*, which validates the tool — not claiming a
  novel find.
- **Tractable (criterion 2).** The message-passing core (get-successor,
  notify, stabilize, check-predecessor) is a few hundred lines of replica
  logic driven by broker datagrams — a good fit for the UDP model.

### Honesty ledger for this route (stated up front, not buried)

- It is a **model** of the published algorithm, not upstream production source.
  The claim is "the tool finds a real, documented protocol bug," not "the tool
  found a bug in the Chord authors' C code."
- The bug is **already documented** (Zave). This is **rediscovery /
  tool-validation**, not a previously-unreported find. The brief's
  "prepare a write-up for upstream, but check with me first" gate is therefore
  moot unless we instead target something whose bug is *not* yet documented —
  a much larger and riskier undertaking.
- The remaining credibility attack surface is model faithfulness ("your model
  has the bug, not Chord"). Mitigation: transcribe the 2001 paper's pseudocode
  verbatim into the replica logic, cite line-for-line, and show the fuzzer's
  shrunk counterexample matches the *shape* of Zave's.

## The decision this leaves to the user

Because the literal brief (a novel bug in an unmodified real OSS binary) is
infeasible under C1–C3, and the honest alternatives differ substantially in
effort and in what they let us claim, the direction is a genuine judgment call
that should be made explicitly rather than assumed. See the options presented
alongside this document.
