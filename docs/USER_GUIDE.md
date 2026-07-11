# Weft user guide

From zero to reproducing a distributed-systems bug. Every command in this
guide is copy-pasteable and was verified end-to-end in a clean container.
When something needs Linux, the guide says so and gives the Docker form.

Contents: [Quickstart](#quickstart) · [Concepts in 60 seconds](#concepts) ·
[Worked example 1: taming a racy program](#worked-1) ·
[Worked example 2: simulated network + record/replay](#worked-2) ·
[Worked example 3: fuzz and shrink](#worked-3) ·
[Case study walkthrough: breaking Chord](#case-study) ·
[Where to go next](#next)

<a name="quickstart"></a>
## Quickstart

Weft's interception runtime is Linux-only (x86-64, glibc, dynamically linked
targets — see [../LIMITATIONS.md](../LIMITATIONS.md) §1). On macOS or
Windows, run everything below inside Docker; the commands are identical
inside the container.

**On Linux** (needs Rust ≥ 1.84 and a C compiler):

```sh
git clone https://github.com/weft-dst/weft && cd weft
cargo build --release --workspace
cc -O2 -o /tmp/chrono examples/chrono.c
./target/release/weft run --seed 42 -- /tmp/chrono
./target/release/weft run --seed 42 -- /tmp/chrono   # byte-identical output
./target/release/weft run --seed 7  -- /tmp/chrono   # different timeline
```

**On macOS / anywhere with Docker** (one container, everything inside):

```sh
git clone https://github.com/weft-dst/weft && cd weft
docker run --rm -it -v "$PWD":/work -w /work \
  -e CARGO_TARGET_DIR=/work/target/linux rust:1.84-bookworm bash
# now inside the container:
cargo build --release --workspace
cc -O2 -o /tmp/chrono examples/chrono.c
target/linux/release/weft run --seed 42 -- /tmp/chrono
```

What you should see: `chrono.c` mixes every libc clock API, formats real
dates, and sleeps between iterations. Under Weft it (a) finishes instantly —
sleeps advance *virtual* time, not wall time; (b) prints the same
timestamps, dates, and values every run with `--seed 42`; (c) prints a
different-but-equally-stable timeline with any other seed.

If output *varies* between two same-seed runs, the target escaped
interception — check it is dynamically linked (`file <binary>` must not say
"statically linked") and not a Go binary (LIMITATIONS.md §1).

<a name="concepts"></a>
## Concepts in 60 seconds

- **One seed is the whole universe.** `--seed N` determines every timestamp,
  every random byte, every thread-scheduling decision within a process, and
  every network fate. A single process with the same seed always produces
  the same run; a live multi-process cluster can still diverge in *arrival
  order* (see below) — but a *recorded* run always replays identically. A
  failing seed, once recorded, is a permanent bug report.
- **Nothing is recompiled.** The shim (`libweft_shim.so`) is `LD_PRELOAD`ed
  into your unmodified binary and interposes on libc: time, randomness,
  pthreads, UDP sockets, file sync.
- **The broker is the network.** With `--net`, UDP traffic is diverted to a
  seeded broker that decides every message's latency, loss, and partition fate as
  a pure function of the seed.
- **Recordings replay exactly.** `--record` captures the broker order — the
  only non-seed input — so `weft replay` reproduces the run byte-for-byte,
  on any platform, forever. (Live cluster re-runs of the same seed are *not*
  identical — see LIMITATIONS.md §3c; record what you care about.)
- **The fuzzer closes the loop.** `weft fuzz` sweeps seeds, checks
  invariants, and shrinks every distinct violation to a minimal reproducer.

<a name="worked-1"></a>
## Worked example 1: taming a racy program

`examples/race_bank.c` has a textbook lost-update bug: it reads a balance
under a lock, releases the lock, then re-locks to write back. Natively the
race fires or not at the whim of the OS. Under Weft, the interleaving is the
seed's choice:

```sh
cc -O2 -o /tmp/race_bank examples/race_bank.c -lpthread

# seed 3: the race fires. Every time.
target/linux/release/weft run --seed 3 -- /tmp/race_bank 2 2
# → balance=2 lost=2  (expected 4)

# seed 2: the race is avoided. Every time.
target/linux/release/weft run --seed 2 -- /tmp/race_bank 2 2
# → balance=4 lost=0
```

Both results are 20/20 reproducible. To *understand* seed 3, re-run it with
`--strategy rr` for a convoy-like schedule that is easier to read, and
`--trace` to see every scheduling decision. This is the core workflow:
**find with `random`, study with `rr`, keep the seed forever.**

<a name="worked-2"></a>
## Worked example 2: simulated network + record/replay

`examples/pingpong.c` is a two-node client/server pair: node 0 answers, node
1 asks (each instance reads its role from `WEFT_NODE_ID`, which `--nodes`
assigns). Give the pair a high-variance network and record the run:

```sh
cc -O2 -o /tmp/pingpong examples/pingpong.c -lpthread

target/linux/release/weft run --seed 99 \
  --net "latency=uniform:1000-50000" --nodes 2 \
  --record /tmp/run.weftlog -- /tmp/pingpong

target/linux/release/weft replay /tmp/run.weftlog
# → replay identical: N op(s), stream digest xxxxxxxxxxxxxxxx
target/linux/release/weft replay /tmp/run.weftlog --check fifo,dup
```

`--nodes 2` matters: with one instance, the server side waits forever for a
client that was never launched. And know your target before adding `loss=`:
pingpong's client retransmits, but its server sends the reply exactly once
and exits — drop that one datagram and the client retries into the void.
(Loss-tolerant targets like `examples/chord/chord_node.c` handle `loss=0.1`
fine; that pairing is what the case study below uses.)

The replay re-derives every latency draw from the seed in the log header and
verifies the recorded outcomes match — byte for byte. Replay works on macOS
natively (it is pure computation; only `run` needs Linux).

<a name="worked-3"></a>
## Worked example 3: fuzz and shrink

Reordering under latency variance breaks per-channel FIFO by design — a
perfect demo target:

```sh
target/linux/release/weft fuzz --config examples/fuzz/demo.json
# → exit 2; for each distinct violation:
#   shrunk : 138 → 7 ops in 122 execution(s)
#   repro  : weft-fuzz-out/repro-seed0-per-channel-fifo-….weftlog
#   verify : weft replay weft-fuzz-out/repro-seed0-….weftlog --check fifo,dup
```

Each reproducer is a fresh, self-consistent weft-log — typically under ten
records — that replays identically and fails the same invariant on the same
channel as the original run. The reduction scales: at ~14,000 ops the
shrinker still lands on 7-op reproducers (docs/SCALABILITY.md §E). `examples/fuzz/ci.json` shows the CI
usage: a reliable-network config where any violation is a genuine regression
(exit 0 expected; the workflow in `.github/workflows/fuzz.yml` gates on it).

<a name="case-study"></a>
## Case study walkthrough: breaking Chord (simplified)

The full study is in [case-study/](case-study/CREDIBILITY_SUMMARY.md); this
is the shape of it, runnable end to end. Chord (SIGCOMM 2001) is a
distributed hash table whose published stabilization protocol was later
proven (Zave, 2012) unable to maintain its ring invariant. Weft rediscovered
that result dynamically, against an unmodified C implementation.

**1. The target.** `examples/chord/chord_node.c` (~300 lines of C) speaks
Chord's join/stabilize/notify protocol over real UDP. It knows nothing about
Weft. `CHORD_FIX` selects the protocol variant: `0` = the 2001 paper,
`1`/`2` = increasing liveness discipline from the literature.

**2. The invariant.** *At least one ring*: from any live node, following
successor pointers must reach a cycle containing all live nodes.
`chord-check` scans a recording's final state and renders the verdict
(exit 0 ok / 2 violation / 3 uninformative).

**3. One seeded run — and a live-run reality check.**

```sh
cc -O2 -o /tmp/chord_node examples/chord/chord_node.c
CHORD_NNODES=7 CHORD_FIX=0 target/linux/release/weft run --seed 17 \
  --net "latency=uniform:1000-60000" --nodes 7 \
  --record /tmp/chord-17.weftlog -- /tmp/chord_node 6 45 3
target/linux/release/chord-check /tmp/chord-17.weftlog 6
```

Run this a few times. You will *not* always get the same verdict — 3 live
runs of seed 17 during the writing of this guide came back OK, OK,
VIOLATION. This is not a bug in the example; it's the "Concepts" section's
live-run-arrival-order lesson, observed directly: which node's message
reaches the simulated network first is OS-scheduled, so a multi-process
seed's *verdict* isn't fixed the way a single-process seed's *output* is
(LIMITATIONS.md §3c). Once you get a VIOLATION, that recording is now a
permanent artifact — `weft replay /tmp/chord-17.weftlog` reproduces that
exact run byte-for-byte forever, even though re-running the live seed might
not hit it again.

**4. The campaign.** `scripts/chord-campaign.sh` sweeps many seeds and
buckets exit codes, so you don't have to hunt for a hit by hand. A 500-seed
campaign found **57/500 violating** for the 2001 protocol, **41/500** with
the first fix, **8/500** with full liveness discipline — every hit leaves a
recording, and the ordering (original ≫ partial fix ≫ full discipline) held
across every re-run even as the exact counts drifted
(docs/case-study/LEVEL_2_RESULTS.md).

**5. The autopsy.** Because a hit is a recording, you can interrogate the
exact moment it broke — using whichever `--record`ed log from steps 3–4
actually came back VIOLATION:

```sh
target/linux/release/chord-trace /tmp/chord-17.weftlog 6   # or your campaign's hit
```

This shows, op by op, a node adopting a successor that had died while the
notification was still in flight — Zave's Figure-6 mechanism, caught live.
The residual 8/500 traces to detection latency, not the protocol — the
honest analysis is in
[case-study/LEVEL_2_RESULTS.md](case-study/LEVEL_2_RESULTS.md).

**Reusing the pattern for your protocol:** print your node's state as a
datagram (`RPT <fields>`) each tick, run under `weft run --net … --record`,
and write a ~150-line checker over the recording — `crates/weft-raft/src/`
is the template (parse the report, fold into a `Verdict`, exit 0/2/3).

<a name="next"></a>
## Where to go next

- Every flag, format, and exit code: [REFERENCE.md](REFERENCE.md)
- How it actually works: [architecture.md](architecture.md)
- What it cannot do (read before trusting results): [../LIMITATIONS.md](../LIMITATIONS.md)
- Scheduling / network / fault semantics: [scheduling-model.md](scheduling-model.md), [network-model.md](network-model.md), [fault-model.md](fault-model.md)
- Contributing: [../CONTRIBUTING.md](../CONTRIBUTING.md)
