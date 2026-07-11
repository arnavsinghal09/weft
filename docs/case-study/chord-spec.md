# Chord: the spec we implement and the invariants we check (from primary sources)

Sources (fetched and read in full, not paraphrased):
- Zave, **"Using Lightweight Modeling to Understand Chord"**, ACM SIGCOMM
  CCR 2012 (`chord-ccr.pdf`). The counterexamples (Figures 2–8) and the Alloy
  fragments (Figure 9) below are transcribed from this paper.
- Zave, **"Reasoning about Identifier Spaces: How to Make Chord Correct"**,
  IEEE TSE 2017 (arXiv:1502.06461). Source of the r+1 minimum-ring-size result
  and the refined inductive invariant.

We implement the **[SIGCOMM] 2001 version** of join/stabilize/notified (the
version Zave shows is incorrect), plus the [PODC] failure-recovery events
(reconcile/update/flush). This is deliberately the *original, incorrect*
protocol — reproducing a documented bug requires the buggy version.

## Node state (r = 2)

From Figure 9 (Alloy), a node has: `succ` (first successor), `succ2` (second
successor — successor-list length r = 2), `prdc` (predecessor), and the
derived `bestSucc`.

- **best successor** (`bestSucc`), from arXiv:1502.06461: *"first successor
  pointing to a live node."* So `bestSucc = succ if live(succ) else succ2 if
  live(succ2) else none`.
- **ring member**: a member that can reach itself by following the chain of
  best successors — i.e. `n ∈ n.(^bestSucc)` (transitive closure).
- **appendage member**: a member that is not a ring member.
- **Between[a,b,c]**: b lies strictly in the clockwise arc (a, c) on the
  m-bit identifier circle. Standard Chord half-open interval, mod 2^m.

## The invariants (verbatim Alloy, Figure 9)

```alloy
pred OneOrderedRing [t: Time] {
  let ringMembers = { n: Node | n in n.(^(bestSucc.t)) } |
     some ringMembers                                   -- AtLeastOneRing
  && (all disj n1, n2: ringMembers |
        n1 in n2.(^(bestSucc.t)) )                      -- AtMostOneRing
  && (all disj n1, n2, n3: ringMembers |
        n2 = n1.bestSucc.t => ! Between[n1,n3,n2] )     -- OrderedRing
}
```

The seven properties [PODC] *claimed* invariant, which Zave shows are **not**
invariant of the 2001 protocol, split into:

**Useful (help key/data consistency; violation is repairable):**
- **OrderedMerges** — an appendage merges into the ring at the right place
  (Fig 2).
- **OrderedAppendages** — members are correctly ordered within an appendage
  (Fig 3).
- **ValidSuccessorList** — if w's successor list skips over v, then v is not
  in the successor list of any immediate antecedent of w (Fig 4).

**Required for correctness (violation creates a disruption that CANNOT be
repaired by the protocol, no matter how long it runs without further
join/fail):**
- **ConnectedAppendages** — from each appendage member, the ring is reachable
  via best successors (Fig 5).
- **AtLeastOneRing** — there is a cycle of members (Fig 6).
- **OrderedRing** — the ring is ordered by identifiers; a class of
  counterexamples, one per odd ring size > 2 (Fig 7).
- **AtMostOneRing** — the network has not split into ≥2 cycles; a class of
  counterexamples, one per even ring size ≥ 2 (Fig 8).

## Operations (2001 [SIGCOMM] + [PODC] recovery)

- **join(n)**: n (a NonMember) finds an existing member m with
  `Between[m, n, m.succ]` and `Member[m.succ]`, sets `n.succ = m.succ`,
  `n.prdc = none`. (Verbatim `JoinEvent` fact, Figure 9.)
- **stabilize(n)**: n asks its (live) successor s for `s.prdc = p`; if
  `Between[n, p, s]` then `n.succ := p`; then n notifies its successor.
- **notified(s, n)**: s adopts n as predecessor if `s.prdc` is dead or
  `Between[s.prdc, n, s]`.
- **reconcile(n)**: `n.succ2 := n.succ.succ` (adopt successor's successor).
- **update(n)**: replace a dead `succ` with the first live entry in
  `[succ, succ2]`. (Note: "live" here is the *model's* judgment — Zave's
  model assumes perfect failure detection, so operations may consult true
  liveness. An implementation of the 2001 protocol has no failure
  detector; our `CHORD_FIX=0` therefore promotes without a liveness test.
  See LEVEL_2_RESULTS.md, "Transcription caveat".)
- **flush(n)**: if `n.prdc` is dead, `n.prdc := none`.

## Failure assumption (from the Chord papers, enforced by our harness)

*"A member never fails if its failure would leave another member with no live
successor in its successor list."* With r = 2 this is a real constraint: the
harness must not inject a failure that would strand a node with an all-dead
successor list. Injecting such a failure would break the model's own
assumption and any resulting "bug" would be ours, not Chord's — so we enforce
it.

## The target counterexample (what the campaign hunts for)

**AtLeastOneRing (Fig 6), r = 2, the r+1 minimum-ring-size anomaly.** Verbatim
setup: *"Before the first stage of Figure 6, 10 has joined, stabilized, and
notified 12. After 10 fails and 6 stabilizes, there is a gap in the ring, with
no successor from 10 to 12."* The 6→12 section can be part of a ring of any
size. A join (10 into the gap between 6 and 12) followed by 10's failure in a
specific window leaves 6 unable to reach 12 through its length-2 successor
list, the ring loses a member, **and continued stabilization cannot rebuild
it** — precisely the "ring may be broken and never repair itself" result.

Success condition for the campaign: the seed sweep finds a run whose final
*quiescent* state (fault injection stopped, extra stabilization rounds
elapsed) violates `AtLeastOneRing` (or `ConnectedAppendages`), i.e. the
violation does not self-heal.

## Honesty note carried into the case study

Zave found these by *exhaustive* Alloy enumeration over a *shared-state* model
(members read each other's state atomically). We reproduce one at *runtime*
with *real message passing* and asynchronous delays — a dynamic rediscovery of
a statically-discovered result. That difference is a feature worth stating,
not hiding: it is weaker as a proof (a sampled search, not exhaustive) but
stronger as evidence that the anomaly survives real async execution, not just
an atomic-read abstraction.
