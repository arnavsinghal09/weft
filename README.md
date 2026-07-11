# Weft

**Weft weaves deterministic order out of concurrent chaos.**

Point it at a compiled Linux binary — no rewrite, no SDK, no special
runtime — and one 64-bit seed determines every clock read, every random
byte, and every thread interleaving in that process; with a simulated
network, every message's latency, loss, and partition fate is a pure
function of the seed too. Record a run and it replays byte-for-byte on any
platform Rust runs — a failing seed becomes a permanent, portable bug
report. `weft fuzz` sweeps thousands of fault seeds against invariant
checks and shrinks every violation to a minimal (1-minimal, not provably
smallest) reproducer. Its built-in workload exercises the framework's own
network core; seed campaigns against *your* binary are scripted today, not
one-command — see [docs/fuzzing.md](docs/fuzzing.md) and the Chord campaign
scripts for the pattern.

One honest caveat up front, not buried in the docs: in a live multi-process
run, which process's message reaches the simulated network first is
OS-scheduled, so re-running the *same seed* live can reach a different
outcome. Record the run you care about — the recording replays identically,
always. Details: [LIMITATIONS.md](LIMITATIONS.md) §3.

Weft is in the tradition of
[FoundationDB's simulator](https://apple.github.io/foundationdb/testing.html)
and [Antithesis](https://antithesis.com/). Unlike sim-first designs
(FoundationDB, TigerBeetle) it retrofits onto binaries you already have.
Antithesis also runs unmodified software — at the hypervisor level, as a
commercial platform; Weft is open source, self-hosted, and intercepts at
the libc boundary, with the narrower coverage that implies
([docs/comparison.md](docs/comparison.md) states the trade-offs in both
directions). The name comes from weaving: the *weft* is the crosswise
thread carried through the warp to make fabric.

> **Status: working, pre-1.0.** Interception, deterministic scheduling,
> simulated network, fault injection, record/replay, and fuzzing with
> shrinking are all implemented and validated against two protocols with
> formally-proven bugs (Chord, Raft) in unmodified C. Interfaces may still
> change — see [VERSIONING.md](VERSIONING.md). Read
> [LIMITATIONS.md](LIMITATIONS.md) before you trust a result.

## See it work

From a checkout (`git clone https://github.com/arnavsinghal09/weft && cd
weft`), with the `weft` binary and shim built and on PATH per
[Install](#install) below, and a C compiler present:

```console
$ cc -O2 -o /tmp/chrono examples/chrono.c
$ cc -O2 -o /tmp/race_bank examples/race_bank.c -lpthread
$ weft run --seed 42 -- /tmp/chrono | tail -1
total virtual elapsed: 2800026 us, c11 time 962138923

$ weft run --seed 42 -- /tmp/chrono | tail -1     # same seed
total virtual elapsed: 2800026 us, c11 time 962138923

$ weft run --seed 7  -- /tmp/chrono | tail -1     # different seed
total virtual elapsed: 2800026 us, c11 time 957028369

$ weft run --seed 3 -- /tmp/race_bank 2 2         # a real lost-update race
threads=2 iters=2 expected=4 balance=2 lost=2     # ← fires. every time.

$ weft run --seed 2 -- /tmp/race_bank 2 2
threads=2 iters=2 expected=4 balance=4 lost=0     # ← avoided. every time.
```

`chrono.c` mixes every libc clock API and sleeps between iterations; under
Weft the sleeps advance virtual time instead of wall time, so it finishes
instantly. `race_bank.c` has a classic split-critical-section bug — under
Weft, whether the race fires is not luck, it's the seed's choice, and it's
100% reproducible either way. The full walkthrough, including a network
fault, a recorded/replayed run, and the fuzzer, is in the
[user guide](docs/USER_GUIDE.md).

## Install

From a clone (not yet published to crates.io):

```sh
git clone https://github.com/arnavsinghal09/weft && cd weft
cargo install --path crates/weft-dst     # the `weft` binary
cargo build --release -p weft-shim       # libweft_shim.so (Linux only)
```

`weft run` finds the shim via `WEFT_SHIM`, or next to the `weft` binary —
copy `target/release/libweft_shim.so` beside `~/.cargo/bin/weft` (or pass
`--shim <path>`). `weft replay` and `weft fuzz` are pure computation and
need no shim; they work on every platform, including macOS.

(The crate is `weft-dst` because the bare name `weft` is already taken on
crates.io by an unrelated project; the installed binary is `weft`.)

Interception itself needs Linux (x86-64, glibc, dynamically linked targets —
see [LIMITATIONS.md](LIMITATIONS.md) §1). On macOS, run everything inside
Docker; the [user guide](docs/USER_GUIDE.md) has the exact container
recipe.

## What it found

Pointed at our own minimal, uninstrumented C implementation of the 2001
**Chord** protocol (~300 lines; it knows nothing about Weft), Weft
dynamically rediscovered the ring-maintenance flaw Zave proved formally in
2012: 57 of 500 seeded runs violate her correctness-critical invariants
under the original protocol rules (55 broken rings, 2 permanently stranded
appendages), falling to 8 once published liveness fixes are applied. Pointed at a
minimal **Raft** leader-election implementation, it reproduced the
dissertation's votedFor-persistence edge case — 3 of 300 runs elect two
leaders in the same term when vote state isn't persisted across a crash
restart, 0 of 300 once it is. Both studies, including where detection has a
measurable blind spot, are in
[docs/case-study/CREDIBILITY_SUMMARY.md](docs/case-study/CREDIBILITY_SUMMARY.md).

## Documentation

| document | contents |
|---|---|
| [docs/USER_GUIDE.md](docs/USER_GUIDE.md) | quickstart, worked examples, Chord case-study walkthrough |
| [docs/REFERENCE.md](docs/REFERENCE.md) | every flag, env var, format, and exit code |
| [docs/architecture.md](docs/architecture.md) | how it works, before you read any code |
| [docs/comparison.md](docs/comparison.md) | honest comparison with Antithesis and TigerBeetle |
| [LIMITATIONS.md](LIMITATIONS.md) | exactly what Weft does not do — read before trusting results |
| [VERSIONING.md](VERSIONING.md) | compatibility contracts: DSL, log format, CLI |
| [ROADMAP.md](ROADMAP.md) | what's next, and what's explicitly not planned |
| [docs/case-study/](docs/case-study/CREDIBILITY_SUMMARY.md) | the Chord & Raft validation evidence |

## Contributing

[DEVELOPMENT.md](DEVELOPMENT.md) is a complete onboarding path: clone, build,
run the full sanitizer/fuzz suite, make a first change. Ground rules and
quality gates are in [CONTRIBUTING.md](CONTRIBUTING.md). Design decisions
already made are recorded in [PROJECT_NOTES.md](PROJECT_NOTES.md). Security
reports go through [SECURITY.md](SECURITY.md).

## License

Dual-licensed under [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE), at
your option.
