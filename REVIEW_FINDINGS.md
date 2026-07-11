# Pre-launch review findings

Skeptical, adversarial pass over the entire public-facing surface of Weft,
conducted 2026-07-11, as a technically-hostile stranger would read it. Every
finding cites its file and quotes the offending text where relevant. Nothing
here has been fixed — this document is the input to a human decision about
what to act on.

Rankings: **BLOCKING** = must fix before any public launch. **SHOULD-FIX** =
meaningfully improves credibility. **MINOR** = polish.

Methodology note: claims were verified against the actual code and, where
possible, against artifacts on disk (campaign verdict files, CI workflow,
test sources) — not just against what other documents say. Two checks are
flagged inline as requiring access this review did not have: the
primary-source paper text (3.1) and repo security settings (6.2).

---

## 1. Overclaiming

**1.1 BLOCKING — README implies `weft fuzz` fuzzes your binary. It does not.**
`README.md` opening paragraph: "Point it at a compiled Linux binary … a
failing seed becomes a permanent, portable bug report. **`weft fuzz` finds
those seeds automatically** and shrinks each one…". In context, "those seeds"
are seeds that break *your program*. But `docs/fuzzing.md` (Scope): "Fuzzing
*live target programs* (the LD_PRELOAD shim path) … is future work," and
`ROADMAP.md` item 5 confirms live-target fuzzing is manual today
(`scripts/*-campaign.sh`). `weft fuzz` sweeps the broker's internal decision
core against a built-in workload. This is the single most quotable
overclaim on the page: the flagship pitch attributes to the fuzzer exactly
the capability the fuzzing doc's own Scope section disclaims.

**1.2 SHOULD-FIX — "the smallest sequence of operations" overstates ddmin.**
Same README sentence: "…shrinks each one down to **the smallest sequence of
operations** that still reproduces it." `LIMITATIONS.md` §4 states the
correct, weaker guarantee: "**1-minimality, not global minimality.** ddmin
guarantees no *single* op can be removed — it does not guarantee the
smallest possible reproducer." The README should say "a minimal" not "the
smallest."

**1.3 SHOULD-FIX — "Proven 10× in CI on every platform" is false as
phrased.** `LIMITATIONS.md` §3(a): "Proven 10× in CI **on every platform**."
The 10× replay test is real (`crates/weft-replay/tests/record_replay.rs`,
loop `for run in 0..10`), but `.github/workflows/ci.yml` runs every job on
`ubuntu-latest` only. macOS replay verification happened manually during
development, never in CI. Say "proven 10× in CI (Linux) and verified
manually on macOS."

**1.4 SHOULD-FIX — the checker's verdict message asserts more than the run
observed.** `crates/weft-chord/src/bin/chord_check.rs` prints, for *any*
invariant violation: "verdict : VIOLATION — the ring is broken **and cannot
self-repair**". Two problems. (a) "Cannot self-repair" is inherited from
Zave's theorem (violations of the four "required for correctness" properties
are unrepairable *in her model*); the run itself observes only "still broken
after the 8-round quiescent tail" (`examples/chord/chord_node.c`, quiescent
tail loop). The docs handle this honestly (`docs/case-study/chord-spec.md`
attributes unrepairability to the papers), but the tool's output states it
as an observed fact. (b) The message says "the ring is broken" even when the
fired invariant is `OrderedRing` (ring exists but is misordered) or
`ConnectedAppendages` (ring intact, appendage stranded). See also 3.4.

**1.5 MINOR — "forever" / "on any platform."** `README.md`: "Record a run
and it replays byte-for-byte, **forever, on any platform**." "Forever" is
backed only by the weft-log versioning policy (readers reject unknown
versions — `VERSIONING.md` §1), i.e. a promise, not a property; "any
platform" means "any platform Rust supports," tested on exactly two.
Rhetoric a skeptic will poke; consider "on any platform Rust runs, for as
long as the log format is supported."

**1.6 MINOR — PHASE_VERIFICATION.md claims a garbled root cause for the
skipped cargo-fuzz target.** `docs/PHASE_VERIFICATION.md`: "never delivered
due to **Fuzz compiler / rustc mismatch**" — this phrase is word salad
(likely meant: cargo-fuzz requires a nightly toolchain and the project pins
stable). State the real reason or drop the clause.

---

## 2. The comparison document (docs/comparison.md)

**2.1 BLOCKING — README's framing contradicts the comparison doc and is
unfair to Antithesis.** `README.md`: "Weft is in the tradition of
[FoundationDB's simulator] and [Antithesis] — **but general-purpose,
retrofit onto binaries you already have rather than a runtime you build
against from day one.**" This lumps Antithesis into "a runtime you build
against from day one." The project's own comparison table says the opposite
(`docs/comparison.md`: "Target must be rewritten? … Antithesis: **no**").
Antithesis *is* general-purpose and runs unmodified software; the honest
differentiators against it are open-source/self-hosted/no-VM-packaging, and
the comparison doc gets this right. The README sentence is the one that will
be screenshot-quoted, and it's wrong. (Also present in
`drafts/outreach-draft.md`: "no rewrite required, **unlike
FoundationDB-style or TigerBeetle-style** sim-first frameworks" — that
sentence is fine because it names only the two rewrite-required systems; the
README names Antithesis.)

**2.2 SHOULD-FIX — "a hypervisor sees only device-level state" undersells
Antithesis's actual product.** `docs/comparison.md` (vs. Antithesis, trade
4): "at the libc boundary, Weft can be *semantic* … **where a hypervisor
sees only device-level state**." Antithesis's public materials describe an
SDK with in-guest semantic assertions ("sometimes assertions", properties),
a test composer, and guest-level instrumentation — the *interception* is
hypervisor-level but the *observability story* is not device-level-only.
As written this reads as dismissive of their tooling and will be corrected
in public by people who use it. Reword to compare interception layers, not
observability.

**2.3 SHOULD-FIX — verify the TigerBeetle characterization against their
current public docs before launch.** `docs/comparison.md`: "its own event
loop, its own storage abstraction, **no OS threads**." TigerBeetle's public
writing describes a single-threaded control loop with static allocation,
but the "no OS threads" absolute (I/O rings, etc.) is the kind of detail
their community will fact-check. One sentence of hedging ("a
single-threaded, statically-allocated core") is safer and equally strong.

**2.4 MINOR — Jepsen "no seed to hand a developer" is slightly overbroad.**
`docs/comparison.md`: "a Jepsen failure reproduces only statistically; there
is no seed to hand a developer." Jepsen's generators are in fact seeded; the
correct claim is that a seed doesn't determine cluster timing, so replaying
it doesn't reproduce the failure. The conclusion stands; the mechanism as
stated is imprecise.

**2.5 MINOR — the doc undersells Phase 7's strongest methodological point.**
The "What is genuinely novel here" section cites the validation and the 1.8%
tail but never mentions the controlled A/B structure — identical seed sets,
only the fix flag differing (57 vs 41 vs 8; Raft 3/300 vs 0/300) — which is
the strongest evidence design in the project and the thing a skeptical
reader would find most persuasive. One sentence would fix this.

---

## 3. The Chord rediscovery claim (load-bearing; verified against code)

What checks out — stated first because it's most of the picture:

- The checker (`crates/weft-chord/src/lib.rs`) implements Zave's Alloy
  definitions faithfully: `bestSucc` = first live entry of {succ, succ2}
  (matching the arXiv:1502.06461 wording quoted in chord-spec.md), ring
  membership via transitive closure of bestSucc, and all four
  correctness-critical predicates (`AtLeastOneRing`, `AtMostOneRing`,
  `OrderedRing`, `ConnectedAppendages`), with unit tests for each including
  a Figure-6-shaped case.
- The failure precondition ("a member never fails if its failure would leave
  another member with no live successor") is enforced by discarding seeds,
  which is *more* honest than counting them.
- The "does not self-heal" observation is genuinely observed within a
  window: `chord_node.c` runs an 8-round quiescent maintenance tail after
  all faults stop, and the checker evaluates only the final state. Traced
  recordings (seed-0, seed-120) exist on disk with op-level mechanisms
  matching Zave's Figure 6.
- The stabilize path at `CHORD_FIX=0` adopts the successor's reported
  predecessor with no liveness check — exactly the original 2001 flaw.

Now the problems:

**3.1 BLOCKING — two project documents contradict each other about what the
original protocol's `update` does, and the headline number depends on it.**
`docs/case-study/chord-spec.md` (Operations, presented as transcribed from
primary sources): "**update(n)**: replace a dead `succ` with the first
**live** entry in `[succ, succ2]`" — i.e., the *original* update has a
liveness check. But `examples/chord/chord_node.c` gates that check behind
level 2 (`fix_level < 2 || is_live(succ2)` — at levels 0 and 1, a dead
succ2 is promoted), and `docs/case-study/LEVEL_2_RESULTS.md` asserts the
opposite of chord-spec: "reconcile and update — which are **equally
unchecked in the original**" / "equally faithful to the 2001 pseudocode."
One of these is wrong. If chord-spec's transcription is right, then
`CHORD_FIX=0` is *buggier than the protocol Zave analyzed*, and some
fraction of the 57/500 headline violations may arrive through a path the
original protocol does not have. Mitigating context: the traced level-0
root cause (seed 17, stabilize adoption at op 855) is the genuine Figure-6
mechanism, and level 1 (still-unchecked update) vs level 0 isolates the
stabilize fix — but the level-1→level-2 delta (41→8) bundles the reconcile,
update, and GETSUCC checks together, so update's contribution is not
isolated anywhere. **Resolve against the paper text (CCR 2012 Figure 9 /
arXiv:1502.06461) before launch, and either fix the code/docs or publish
the per-level semantics table explicitly.** A reviewer with Zave's paper
open will find this in an afternoon.

**3.2 BLOCKING — the citation file cites a paper that does not exist.**
`CITATION.cff` (references): `title: "How to Not Prove Your Distributed
Algorithm"`, attributed to Zave, 2012, SIGCOMM CCR. No such paper. The real
CCR 2012 paper — correctly cited in `docs/case-study/chord-spec.md` — is
"Using Lightweight Modeling to Understand Chord." The same fabricated title
appears in `drafts/blog-chord-raft-case-study.md` ("in 2012, Pamela Zave
published 'How to Not Prove Your Distributed Algorithm'"). A fabricated
citation in CITATION.cff, on the project's most load-bearing claim, is the
single worst kind of credibility bug.

**3.3 SHOULD-FIX — "unmodified C implementation" reads as third-party
provenance; it is the project's own harness.** `README.md`: "Pointed at an
**unmodified C implementation of Chord** (SIGCOMM 2001)…"; `CHANGELOG.md`:
"an unmodified Chord (2001) implementation loses ring connectivity…".
`chord_node.c` was written *by this project, for this experiment*, from the
papers, with the `CHORD_FIX` falsification switch built in. "Unmodified"
here means "not instrumented for Weft" — true and worth saying — but a
cold reader will parse it as "someone else's existing Chord codebase," and
the correction ("they wrote both the bug and the bug-finder") is the top
HN reply as written. Say "our own minimal, uninstrumented implementation of
the 2001 protocol (it knows nothing about Weft)" everywhere the claim
appears. The case-study docs themselves are honest about this; the
top-of-funnel docs are not.

**3.4 SHOULD-FIX — the published counts lump two violation classes, and the
prose is strictly wrong for 2 of the 57.** The campaign counts exit-2 from
`chord-check`, which fires on *any* of the four invariants
(`examples/chord/campaign.sh`; `chord_check.rs`), while README says "57 of
500 seeded runs **break the ring**" and CHANGELOG says "**loses ring
connectivity**." This review ran the exhaustive tally over all 106
surviving verdict files (`target/chord-out-*/seed-*.verdict`):

| arm | AtLeastOneRing | ConnectedAppendages | OrderedRing / AtMostOneRing |
|---|---|---|---|
| original (57) | 55 | 2 | 0 |
| stabilize-fix (41) | 39 | 2 | 0 |
| full discipline (8) | 8 | 0 | 0 |

So the headline is *almost* exactly right — but for 2 of the 57 (and 2 of
the 41), the ring itself is intact and the violation is a permanently
stranded appendage. Both classes are in Zave's "required for correctness /
unrepairable" set, so the substance survives; the phrasing "break the
ring" does not, quite. Publish this table in LEVEL_2_RESULTS.md and adjust
the README/CHANGELOG phrasing to "violate Chord's ring-maintenance
correctness invariants (55 broken rings + 2 stranded appendages)" or
similar. This is exactly the kind of detail a referee finds, and finding
it pre-published — with the table already in the docs — flips it from a
gotcha into evidence of rigor.

**3.5 SHOULD-FIX — the blog draft claims the tool found the bug "without
knowing the proof in advance." It knew.** `drafts/blog-chord-raft-case-
study.md`: "can a dynamic tool find it **without knowing the proof in
advance**?" and "no protocol-specific instrumentation beyond 'print your
state each tick'". The harness was *built from the proof*: chord-spec.md
transcribes Zave's invariants verbatim, the checker encodes them, and the
scenario shape (3-node stable base = r+1, appendage join/fail pattern) was
chosen from her anomaly's preconditions (`campaign.sh` comments say so).
What's honestly true — and still impressive — is that the *runtime search*
found seeds exhibiting the anomaly without being pointed at a specific
schedule. The draft as written will be called out. (Draft is unpublished;
fix before it ships.)

---

## 4. Internal inconsistency

**4.1 BLOCKING — every repository URL in the project points to a GitHub
org/repo that does not exist.** `Cargo.toml` (`repository =
"https://github.com/weft-dst/weft"`), `README.md` quickstart (`git clone
https://github.com/weft-dst/weft`), `docs/USER_GUIDE.md` quickstart,
`CITATION.cff` (`repository-code`), `.github/ISSUE_TEMPLATE/config.yml`
(security-advisory URL), `drafts/blog…` and `drafts/outreach-draft.md`
links. Verified: `gh repo view weft-dst/weft` → "Could not resolve to a
Repository." The actual remote is `github.com/arnavsinghal09/weft` (where
the five good-first-issues were filed). A new user's literal first command
fails. Either create/transfer to the `weft-dst` org before launch or update
every URL.

**4.2 SHOULD-FIX — PHASE_VERIFICATION.md describes the Phase-2 race with
the wrong mechanism.** `docs/PHASE_VERIFICATION.md`: "Race control achieved
via **network latency tuning** (uniform:0-1 triggers the race; higher
latencies avoid it)." Wrong subsystem: `race_bank.c` uses no network at
all; the race is controlled by the *scheduler* seed (`--strategy random`,
seed 3 triggers / seed 2 avoids — `docs/scheduling-model.md`, pinned by
`crates/weft-dst/tests/sched_e2e.rs`). The fact being verified is true and
was later verified empirically in a clean container; the stated mechanism
is not.

**4.3 SHOULD-FIX — PROJECT_NOTES.md's directory layout is stale and
contradicts the real crate map.** `PROJECT_NOTES.md` still says "Only
`crates/weft-dst` exists today" and lists planned crates `weft-sched`,
`weft-faults`, `weft-harness` that were never created (the scheduler lives
in `weft-shim`, faults in `weft-net`/`weft-scenario`, the harness role in
`weft-chord`/`weft-raft`). The Phase-status section of the same file is
current; the layout section two screens up is from Phase 0. A contributor
sent here by CONTRIBUTING.md gets a wrong map.

**4.4 MINOR — checker exit-code documentation disagrees with itself.**
`chord_check.rs`'s own header comment lists exit codes "0 / 2 / 1" but the
code returns 3 for assumption-violated discards. `docs/REFERENCE.md` §1.4
glosses exit 3 for both checkers as "DISCARD (seed exercised nothing —
uninformative)," which is right for `raft-check` (no leader elected) but
wrong for `chord-check` (3 = the scenario broke the papers' failure
precondition — a different, stronger statement).

**4.5 MINOR — `Invariantt` (double-t) type name.**
`crates/weft-chord/src/lib.rs` `pub enum Invariantt` — presumably to avoid
clashing with the `Invariant` trait, but it reads as a typo in a crate the
docs hold up as "the template for writing your own checker."

**4.6 MINOR — USER_GUIDE's one-line invariant definition conflates two of
Zave's predicates.** `docs/USER_GUIDE.md` (case study, step 2): "*At least
one ring*: from any live node, following successor pointers must reach a
cycle **containing all live nodes**." That is (roughly) the whole
`OneOrderedRing` conjunction; Zave's `AtLeastOneRing` is only "some cycle
exists." The checker implements the four predicates separately and
correctly; the guide's summary sentence does not match the named invariant.

---

## 5. Quickstart integrity

**5.1 BLOCKING — step 1 of both quickstarts fails.** `git clone
https://github.com/weft-dst/weft` (README quickstart and
docs/USER_GUIDE.md) — repository does not exist (see 4.1).

**5.2 SHOULD-FIX — README's demo uses a binary it never builds.** The "See
it work" console block compiles `chrono` (`cc -O2 -o /tmp/chrono
examples/chrono.c`) then runs `weft run --seed 3 -- /tmp/race_bank 2 2`
with no `cc` line for race_bank (it needs `-lpthread`, too). A user pasting
the block verbatim gets "No such file or directory" at the demo's
punchline.

**5.3 SHOULD-FIX — the Install section assumes a clone it never
instructs.** `README.md` Install: "`cargo install --path crates/weft-dst`"
— a `--path` install only works inside a checkout, but the Install section
contains no clone step (the clone lives in the earlier demo section, which
a user skimming to "Install" will skip). Also unstated: a C compiler is
required for every example in the demo, and `weft run` in the demo is
invoked bare (assumes `~/.cargo/bin` on PATH *and* the shim copied next to
the binary — the shim-copy instruction appears only after the demo).

**5.4 MINOR — demo output will not match what users see.** The chrono lines
in "See it work" were captured from a real container run (good), but the
wall-clock-formatted fields (`20:48:42` etc.) don't appear; the shown line
(`total virtual elapsed: 2800026 us, c11 time 962138923`) is stable across
machines only because time is virtual. Worth a comment in the block that
byte-identical output is the *expected* result on any machine, since that's
the point being demonstrated.

---

## 6. Security and supply-chain honesty

**6.1 BLOCKING — the security contact is an unexplained third-party email.**
`SECURITY.md`: "email **atharvagandhi2005@gmail.com**" (also the contact in
`CODE_OF_CONDUCT.md`). The repo's sole committer is Arnav Singhal
(`arnavsinghal06@gmail.com`); the GitHub account is `arnavsinghal09`.
Whoever `atharvagandhi2005` is, a vulnerability reporter has no way to know
this address is monitored, and the mismatch with the maintainer identity
looks like an unedited template or a copy from another project. A security
process is only real if the mailbox is.

**6.2 SHOULD-FIX — "GitHub private vulnerability reporting" is instructed
but not verifiably enabled.** SECURITY.md's preferred channel requires the
repo setting to be turned on (Settings → Security → Private vulnerability
reporting). *(Unresolved in this review: repo-settings access.)* The
issue-template's fallback advisory URL points at the nonexistent
`weft-dst/weft` (4.1), so today **both** documented reporting channels are
broken-or-unverifiable.

**6.3 SHOULD-FIX — SBOM freshness is not enforced; the release notes imply
more than CI checks.** `CHANGELOG.md`: "SBOM for the release … 34 packages,
all permissive, `cargo deny check` fully green." CI (`ci.yml`) runs
cargo-deny (advisories + licenses/bans/sources) on every push — that claim
is real. But the SBOM files in `sbom/` were generated manually on
2026-07-09 and nothing regenerates or validates them when `Cargo.lock`
changes; they are already one dependency-churn away from silently
misdescribing the tree. `docs/RELEASE.md` is honest about generation but
should list "regenerate SBOM" as a release step, or CI should diff it.

**6.4 MINOR — LIMITATIONS.md handles the Phase-2 safety boundary honestly
(no finding, recorded as verified).** The serialized-races gap ("Data races
are serialized, not detected… run with `WEFT_SCHED=0` under TSan") is
stated in both `docs/scheduling-model.md` and `LIMITATIONS.md` §3(b)3, and
the TSan positive/negative controls exist in `scripts/verify-phases.sh`.
Checked because the review brief specifically asked; nothing is being
downplayed here.

---

## 7. Tone

**7.1 SHOULD-FIX — the checker's "cannot self-repair" (1.4) and the blog
draft's "without knowing the proof in advance" (3.5)** are the two places
tone crosses from confident into unsupported. Both already itemized.

**7.2 MINOR — "← fires. every time." / "← avoided. every time."**
(`README.md` demo comments). Backed by 20/20 pinned runs
(`sched_e2e.rs`), so it survives scrutiny — but "every time" invites a
pedant to run it 21 times; "deterministic — seed's choice, not luck" says
the same thing unfalsifiably.

**7.3 MINOR — CHANGELOG's framing sentence.** "This release is the
culmination of building that stack…" is mild, but "culmination" +
unreleased-0.0.1 reads slightly grand; the facts that follow carry the
paragraph without it.

**7.4 Positive note (no finding):** `LIMITATIONS.md`, `docs/case-study/
LEVEL_2_RESULTS.md` (the falsification-statement structure), and
`docs/case-study/chord-spec.md`'s "Honesty note" are the strongest-toned
documents in the project — precise, quantified, and self-critical. The
review found nothing in them to flag beyond 1.3 and 3.1; the rest of the
public surface should be brought down to their temperature.

---

## Summary counts

| Rank | Count | The one-line list |
|---|---|---|
| BLOCKING | 6 | fuzz-scope overclaim (1.1); README's Antithesis framing (2.1); update-semantics contradiction under the 57/500 (3.1); fabricated Zave citation (3.2); nonexistent repo URL everywhere (4.1/5.1); unverifiable security contact (6.1) |
| SHOULD-FIX | 12 | 1.2, 1.3, 1.4, 2.2, 2.3, 2.5, 3.3, 3.4, 3.5, 4.2, 4.3, 5.2/5.3, 6.2, 6.3 |
| MINOR | 9 | 1.5, 1.6, 2.4, 4.4, 4.5, 4.6, 5.4, 7.2, 7.3 |

One check left unresolved by this review (access limits, not effort):
whether GitHub private vulnerability reporting is enabled on the actual
repo (6.2). The per-invariant verdict tally (3.4) was completed
exhaustively during the review: 102 of 106 surviving hits are
`AtLeastOneRing`, 4 are `ConnectedAppendages`, none are `OrderedRing` or
`AtMostOneRing` — see the table in 3.4.
