# Weft vs. Antithesis and TigerBeetle

The two most credible public reference points for deterministic simulation
testing right now are **Antithesis** (hypervisor-level, commercial) and
**TigerBeetle's VOPR** (language-level, sim-first design). This document
says plainly where Weft stands next to each, based on their public
materials — what Weft does today, what it deliberately does not attempt,
and when you should reach for one of them instead of Weft. It is an
engineering comparison, not marketing: for several rows below, the other
system's choice is strictly stronger.

## TL;DR table

| | TigerBeetle VOPR | Antithesis | **Weft** |
|---|---|---|---|
| Target must be rewritten? | yes (Zig, sim-first design) | no | no |
| Interception layer | language runtime | custom hypervisor (whole VM) | `LD_PRELOAD` / libc boundary |
| What runs deterministically | TigerBeetle itself | anything in the hypervisor | unmodified dynamic Linux binaries |
| Determinism strength | total within sim | total within VM | total single-process; recording-exact multi-process (LIMITATIONS.md §3) |
| Coverage of the target | only TigerBeetle | everything incl. kernel | the libc-visible surface only |
| Cost to adopt | rewrite your system | commercial service, VM packaging | run your binary under a CLI |
| Open source | yes | no | yes |

## vs. TigerBeetle's VOPR

TigerBeetle owns the strongest guarantee on this list: because the
database was *designed* against a simulated environment from the start —
a single-threaded, statically-allocated core with its own event loop and
storage abstraction — every line of production code runs under simulation,
deterministically, including disk and scheduler behavior. Weft cannot match that depth. A raw syscall, or a
lock the shim doesn't model, escapes Weft's simulation entirely
(LIMITATIONS.md §2); TigerBeetle's VOPR has no such escape hatch because
there is no OS-level nondeterminism left in the loop to escape through.

What Weft offers that VOPR structurally cannot: sim-first is a *decision you
make on day one*. TigerBeetle's public writing is explicit that this was a
founding constraint, and it only pays off because they controlled the whole
codebase from the first commit. Weft exists for the other case — the
C/C++/Rust/Go service you already have, that nobody is rewriting. If you are
starting a new system where correctness is paramount (a database, a
consensus layer) and you can afford to build it deterministic from scratch,
**VOPR's approach is better than Weft's and you should use it or its
pattern, not retrofit Weft onto a system not yet written.**

## vs. Antithesis

Antithesis intercepts at the hypervisor, below the kernel, so *nothing*
escapes: raw syscalls, Go runtimes, static binaries, multi-process timing —
all deterministic, because the whole VM is the simulation. That is strictly
stronger interception than Weft's libc boundary, full stop. If your target
does raw syscalls, is statically linked, or is written in Go (all
undetected-nondeterminism cases for Weft — LIMITATIONS.md §1–2), Antithesis
covers you and Weft does not.

The trades Weft makes against it: (1) open source vs. commercial service —
Weft has no signup, no VM packaging step, no vendor relationship; (2) a
per-process shim is `weft run -- ./binary`, versus packaging your system
into a VM image; (3) Weft's failing artifact is a megabyte-scale recording
replayable in milliseconds on a laptop, not a VM snapshot; (4) Weft's *interception* is at the libc
boundary, so its fault vocabulary is stated in libc terms — "this fsync
lies", "this fd's writes tear" — where hypervisor-level interception
expresses faults at the device level. (Antithesis pairs its hypervisor
with in-guest SDK assertions and instrumentation, so this is a contrast of
interception layers, not of overall observability.) For the distributed-protocol logic bugs Phase 7 targeted
(Chord's ring invariant, Raft's ElectionSafety), the libc surface proved
sufficient to find both. **If you need whole-system fidelity — kernel
interaction, unsupported languages, static binaries — Antithesis is the
correct tool and Weft will silently miss what it can't see.**

## What Weft does today

- Deterministically replays unmodified dynamically-linked Linux binaries at
  the libc boundary: time, randomness, thread scheduling, UDP networking,
  a subset of file I/O faults.
- Turns any failure into a permanent, portable recording that replays
  byte-for-byte, including on macOS where the shim itself doesn't build.
- Finds failures automatically (`weft fuzz`) and shrinks them to minimal
  reproducers.
- Has been checked against two protocols with independently, formally
  documented bugs (Chord/Zave 2012, Raft/Ongaro 2014) and reproduced both —
  see docs/case-study/CREDIBILITY_SUMMARY.md.

## What Weft deliberately does not attempt

- **True whole-VM / kernel-internal determinism.** Even if the planned
  seccomp-unotify work (below, and [ROADMAP.md](../ROADMAP.md) item 6)
  closes the static-binary and Go gaps, it still intercepts at the syscall
  boundary *within* the same kernel — it will never give you Antithesis's
  actual moat: deterministic kernel scheduling, block-device timing, and
  everything below the syscall layer. That requires a hypervisor, which is
  a different architecture Weft isn't building. If you need that level of
  fidelity, use Antithesis; no amount of additional libc/syscall hooking
  gets Weft there.
- **Sim-first, ground-up determinism.** Weft is retrofit onto existing
  binaries on purpose. It will never be as complete as a system built
  against a simulated environment from its first commit — that is
  TigerBeetle's territory.
- **Proving correctness.** 300 clean seeds falsify a specific mechanism
  under a specific schedule distribution; they are not a proof the system
  is correct. See LIMITATIONS.md §5 and the Raft study's schedule-sensitivity
  discussion.

(Raw syscalls/static binaries/Go and TCP support are *not* on this list —
they're currently uncovered but actively planned; see
[ROADMAP.md](../ROADMAP.md) items 3 and 6.)

## The broader landscape

FoundationDB's simulator is VOPR's ancestor and shares its trade exactly:
whole-system determinism, paid for with a from-scratch rewrite (Flow,
single-threaded async) — the same "use it if you're starting fresh"
argument applies. **Jepsen** is the field's referee in the opposite
direction from all of the above: real clusters, real networks, external
faults, a checker over observed histories, and *no* determinism — a Jepsen
failure reproduces only statistically (its generators are seeded, but a
seed does not determine cluster timing, so replaying one does not
reproduce the failure).
Weft's faults are simulated (less real than Jepsen's), but every failure is
a replayable artifact; the two compose well — a Weft recording of a
Jepsen-discovered bug class is the debugging story Jepsen deliberately
doesn't provide.

## What is genuinely novel here

Not the idea of DST — FoundationDB proved it a decade ago. The specific
combination:

1. **Unmodified binaries at the libc boundary** with a do-no-harm rule (a
   preloaded but unseeded shim is behaviorally invisible), rather than a
   language runtime or a hypervisor.
2. **The recording as the unit of truth.** The broker linearization is the
   only non-seed input, so seed + log replays exactly on any platform —
   including macOS, where the shim doesn't even build. Campaign statistics
   live on Linux; debugging is portable.
3. **Validation against published ground truth.** The Chord and Raft studies
   (docs/case-study/) test the *tool* against protocol bugs with formal
   provenance (Zave 2012, Ongaro 2014) — including quantified negative
   results (the 1.8% detection-latency tail) rather than only success
   stories. The evidence design is a controlled A/B: identical seed sets
   with only the fix flag differing (Chord 57 → 41 → 8 across liveness
   levels; Raft 3/300 buggy vs 0/300 fixed), so the deltas isolate the
   protocol change, not the harness.

## What a non-Linux port would require

Ordered by how much of the codebase each layer touches. The pure crates
(`weft-net` core, `weft-replay`, `weft-fuzz`, `weft-scenario`) are already
platform-independent — `weft replay`/`weft fuzz` pass their suites on macOS
today. The port is entirely about the shim and process control.

**macOS (the plausible one):**
- `LD_PRELOAD` → `DYLD_INSERT_LIBRARIES`. Two hard walls: SIP strips dyld
  env vars from protected/system binaries (fine for user-built targets), and
  flat-namespace interposition needs `DYLD_FORCE_FLAT_NAMESPACE` or
  `__DATA,__interpose` sections — the latter is the robust path and means
  rewriting the hook-declaration layer (`real!` dlsym-next resolution →
  interpose pairs).
- libc surface: BSD libc + Mach. `clock_gettime` exists, but code commonly
  uses `mach_absolute_time`; `getentropy` exists, `getrandom` does not;
  `/dev/urandom` handling carries over. The pthread hook set carries over
  mostly intact; the scheduler's token model is OS-agnostic by design (it
  models mutexes rather than wrapping them).
- Network: the broker protocol is a Unix socket + wire format, portable
  as-is; only the `socket/bind/sendto/recvfrom` hooks move.
- Estimate: the engine and models port cleanly; the interposition layer is a
  rewrite (~the size of `hooks/` + `real.rs`). No new design work.

**Windows (the expensive one):** no preload mechanism; interception means
Detours-style binary patching or a driver, the syscall surface
(`QueryPerformanceCounter`, `BCryptGenRandom`, IOCP, SRW locks) shares
nothing with the current hook set, and process orchestration (env
inheritance, exec semantics) diverges everywhere. This is a new sibling
implementation reusing only the pure crates — not a port.

**The strategic alternative** (also the static-binary/Go answer on Linux):
seccomp-unotify syscall-boundary interception, sketched in
docs/architecture.md. It trades per-call latency for coverage and would make
the Linux story strictly stronger before any second OS is attempted. Given
finite effort, that ranks above a macOS port: macOS is where developers
*write* the code, but recording-exact replay already works there, which is
the part of the loop developers actually run locally.
