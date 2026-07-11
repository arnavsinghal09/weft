<!--
DRAFT ONLY — NOT SENT. No recipient has been chosen or contacted.

This is a template pitch for reaching out to a maintainer of an open-source
distributed system (a consensus implementation, a DHT, a replicated
database, etc.) about trying Weft against their project. It is generic on
purpose: fill in [PROJECT], [SPECIFIC COMPONENT], and [MAINTAINER NAME]
before sending anything, and only send it to a project where you've
actually read enough of the code to make the specific-component claim
honestly. Do not send this as-is to multiple projects as a form letter —
that's spam, and it undercuts the credibility this whole release phase was
about building.

Also worth deciding before sending: is this an issue/discussion on their
tracker, or a direct email? For most OSS projects, a public issue or
discussion thread is more appropriate and more respectful of the
maintainer's time than a cold email — it lets them (and their community)
evaluate it in the open, on their terms.
-->

Subject: Would [PROJECT] be interested in deterministic simulation testing?

Hi [MAINTAINER NAME],

I maintain Weft, an open-source deterministic simulation testing tool for
unmodified Linux binaries — no rewrite required, unlike FoundationDB-style
or TigerBeetle-style sim-first frameworks. It intercepts a program's
nondeterministic surface (time, randomness, thread scheduling, and — with
`--net` — network I/O) via `LD_PRELOAD`, so one seed determines an entire
run, and any failure becomes a permanent, byte-for-byte replayable
recording.

I'm reaching out because [PROJECT]'s [SPECIFIC COMPONENT — e.g. "leader
election," "replication protocol," "membership handling"] looks like a
good fit for the kind of thing this catches: concurrency- and
network-timing-dependent bugs that are hard to hit with unit tests and
hard to debug from a flaky CI failure alone.

As a validation exercise (not a claim about your code specifically), we
pointed Weft at minimal reimplementations of Chord and Raft and used it to
rediscover two independently, formally documented bugs: Zave's 2012 proof
that Chord's original stabilization protocol can silently break ring
connectivity (57/500 seeded runs reproduced it; published fixes reduced
that to 8/452 valid seeds, with the residual traced to a specific
detection-latency race), and Ongaro's dissertation edge case where an
unpersisted vote allows a double-election on crash-restart (3/300 seeds
reproduced it; 0/300 once the vote is persisted). Write-up, including the
honest negative results:
https://github.com/weft-dst/weft/blob/main/docs/case-study/CREDIBILITY_SUMMARY.md

If it's useful: Weft needs no changes to your binary. A rough integration
path would be running your existing test/fuzz workloads under
`weft run --net <fault-spec> --record`, and — if you have a state-consistency
invariant you already check (or would like to) — writing a small checker
over the recording (the Raft checker in our repo is about 150 lines and is
a reasonable template: parse a per-tick state report, fold it into a
verdict, exit non-zero on violation).

Totally understand if this isn't a priority right now — happy to answer
questions either way, and equally happy to just hear "we already have
[X] and it covers this." Repo: https://github.com/weft-dst/weft

Thanks for maintaining [PROJECT] — [one genuine, specific sentence about
why you respect this project, not boilerplate].

[YOUR NAME]
