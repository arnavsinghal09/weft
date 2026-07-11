<!--
DRAFT — not published anywhere. Written for the human maintainer to review,
edit, and post (e.g. on a project blog, dev.to, or as a HN/lobste.rs text
post) at their discretion. Every number below is drawn directly from
docs/case-study/CREDIBILITY_SUMMARY.md, LEVEL_2_RESULTS.md, and
RAFT_VALIDATION.md — do not add new claims here without adding the
underlying evidence to those documents first.
-->

# We pointed a deterministic simulator at Chord and it found the bug Zave proved existed in 2012

Weft is a deterministic simulation testing (DST) tool: point it at a
compiled Linux binary, and one seed determines every clock read, every
random byte, every thread interleaving, and — with a simulated network —
every message's fate. A failing seed becomes a permanent, byte-for-byte
replayable recording. We built it to test unmodified binaries the way
FoundationDB and TigerBeetle test their own systems, without requiring a
rewrite.

Before trusting a testing tool, you should ask it to find a bug you already
know is there. So we did.

## The target: Chord, 2001

Chord is the canonical distributed hash table paper — join a ring,
stabilize your successor pointer, and lookups resolve in O(log n) hops.
It's foundational reading in every distributed systems course. It also has
a known flaw: in 2012, Pamela Zave published "How to Not Prove Your
Distributed Algorithm" (ACM SIGCOMM CCR), formally proving that Chord's
original stabilization protocol cannot maintain its own core invariant
(`AtLeastOneRing`) under concurrent joins and failures. The ring can
silently break into multiple disjoint rings, and the published protocol has
no mechanism to detect or repair it.

That's a formally proven result about a paper design. We wanted to know:
does an actual C implementation of that paper's protocol, run under
realistic network conditions, actually exhibit the flaw — and can a dynamic
tool find it without knowing the proof in advance?

## Building the smallest thing that could be wrong

We wrote `chord_node.c` — about 300 lines of C implementing Chord's
join/stabilize/notify loop over real UDP sockets. It has three modes,
selected by an environment variable:

- `CHORD_FIX=0` — the original 2001 protocol, no liveness checks on
  successor adoption.
- `CHORD_FIX=1` — one targeted liveness check (the correction most often
  cited in follow-up literature).
- `CHORD_FIX=2` — full liveness discipline: every pointer adoption
  (stabilize, reconcile, update, and the GETSUCC responder) refuses a
  known-dead node.

We wrote a checker, `chord-check`, that scans a recording of a run and
verifies one invariant: from any live node, following successor pointers
reaches a cycle containing every live node. Simple to state, and it's
exactly `AtLeastOneRing`.

Then we ran 500 seeds — same network conditions, same seven-node cluster,
same failure schedule distribution, only `CHORD_FIX` changed:

| Protocol variant | Violating seeds | Rate |
|---|---|---|
| Original (2001) | 57 / 500 | 14.1% |
| Partial fix | 41 / 500 | 9.3% |
| Full liveness discipline | 8 / 452 valid seeds | 1.8% |

The ordering held across every re-run we did. The original protocol breaks
its own core invariant in roughly one seed out of seven. That's not a
theoretical result anymore — it's a reproducible recording you can replay
on your laptop.

## What breaking actually looks like

Because every hit leaves a recording, you don't have to take the aggregate
number on faith — you can watch the exact moment it happened:

```sh
weft run --seed 17 --net "latency=uniform:1000-60000" --nodes 7 \
  --record chord.weftlog -- ./chord_node 6 45 3
chord-trace chord.weftlog 6
```

The trace shows a node holding two successor pointers, both still
formally "live" from its own perspective. Both die. Before the node
processes either DEAD notification — because those notifications are
travelling over the same latency-variant network as everything else — it
attempts to adopt a new successor, and adopts one that turns out to already
be dead too. Now every pointer it has points at a corpse, and there's no
mechanism to route around it. This is Zave's Figure 6 mechanism, caught
live, not derived from the proof.

## The honest part: what full discipline doesn't fix

8 out of 452 valid seeds still break under the fully-hardened protocol.
That's not a bug in our harness, and it's not the protocol failing in a new
way — it's the same mechanism, one layer down. In every one of the 8 cases,
a node's local liveness check passed *at the moment it queried*, and the
DEAD notification for the node it was about to adopt was already in flight
but hadn't arrived yet. The check is locally correct; the information it's
checking against is stale by a network round-trip.

We think this is worth stating plainly: Zave's proof (and most protocol
proofs like it) assumes perfect, instantaneous failure detection. Real
networks don't give you that. Our full-discipline result is a dynamic,
independent confirmation of a subtler thing than "the paper had a bug" — it's
a measurement of how much residual risk survives even a textbook-correct
fix, once you stop assuming the network is instant. 1.8% under this specific
latency distribution isn't a universal constant; it's a number that would
move if the latency distribution did. That's the point — it's measurable at
all.

## Raft, briefer

We ran the same kind of study against a minimal Raft leader-election
implementation, targeting one specific, well-known edge case from Ongaro's
dissertation: `votedFor` must be persisted to stable storage before a node
responds to a vote request, or a crash-restart can lose that state and let
the node vote twice in the same term — two leaders, one term, safety
violated.

With `votedFor` held only in memory: 3 violations in 300 seeded runs. With
it persisted: 0 in 300. Both the failure and its fix are exactly where the
dissertation says they'd be.

We'll say the same honest thing here we said about Chord: 300 clean seeds
under one schedule distribution falsifies *this specific mechanism*, not
"Raft is correct." We had to deliberately tune election timeouts to be
tight relative to network latency to get the bug to show up reliably at
all (0/100 under loose timeouts, 4/100 under tight ones) — a reminder that
dynamic testing results are conditional on the schedule you tested, always.

## Why this matters more than the numbers

The point of this exercise wasn't really Chord or Raft — both bugs were
already known, proven, and published. The point was checking whether a
general-purpose dynamic testing tool, pointed at an unmodified binary with
no protocol-specific instrumentation beyond "print your state each tick,"
could rediscover results that took formal methods to establish the first
time. It could, for both. And where it couldn't get all the way to zero
(Chord's 1.8% residual), it told us exactly why, down to the specific race
window — which is the kind of answer a static proof doesn't hand you for
free.

Full writeup, including the falsification tests and minimal reproducers:
[docs/case-study/CREDIBILITY_SUMMARY.md](https://github.com/weft-dst/weft/blob/main/docs/case-study/CREDIBILITY_SUMMARY.md).
