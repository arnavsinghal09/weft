# Weft

**Deterministic simulation testing for unmodified Linux binaries.**

Weft is an open-source deterministic simulation testing (DST) framework in the
spirit of [FoundationDB's simulator](https://apple.github.io/foundationdb/testing.html)
and [Antithesis](https://antithesis.com/) — but general-purpose, and applied to
compiled programs *as they are*. You do not rewrite your system against a
special runtime: Weft intercepts the nondeterministic surface of an ordinary
Linux binary at runtime (threads, time, randomness, network, disk) and weaves
one deterministic, replayable order out of the many possible thread
interleavings, network conditions, and fault schedules.

The name comes from weaving: the *weft* is the crosswise thread carried
through the warp to make fabric.

> **Status: pre-alpha (Phase 0).** Nothing here runs your program yet. This
> repository currently holds the project skeleton, CI, and design notes. The
> roadmap below is real and being executed in order.

## What Weft will do

- **Intercept, don't instrument** — an `LD_PRELOAD` shim interposes on libc
  and syscall boundaries of unmodified binaries; no recompilation, no SDK.
- **Deterministic scheduling** — one seed fully determines thread
  interleaving, clock behavior, and randomness.
- **Simulated network & faults** — partitions, latency, reordering, disk
  errors, and process crashes injected on a controlled schedule.
- **Record & replay** — any failing run is a seed; replay it exactly,
  forever, under a debugger.
- **Schedule fuzzing** — search the space of interleavings and fault
  schedules for the ones that break you.

## Roadmap

| Phase | Deliverable |
|-------|-------------|
| 0 | Project skeleton, CI, community files *(this phase)* |
| 1 | `LD_PRELOAD` interception shim (`weft-shim`) |
| 2 | Deterministic thread scheduler |
| 3 | Simulated network |
| 4 | Fault-injection engine |
| 5 | Recording & replay |
| 6 | Schedule/fault fuzzer |
| 7–8 | Real-world integration harness & hardening |

## Installation

Not yet published. When it is:

```sh
cargo install weft-dst   # installs the `weft` binary
```

(The crate is `weft-dst` because the bare name `weft` is taken on crates.io
by an unrelated project; the binary is `weft`.)

## Building from source

```sh
cargo build
cargo test
./target/debug/weft --help
```

Linux is the target platform for the interception runtime; the CLI and
orchestrator build anywhere Rust does.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md). Design notes and the full project
layout live in [PROJECT_NOTES.md](PROJECT_NOTES.md). Security reports go
through [SECURITY.md](SECURITY.md).

## License

Dual-licensed under [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE), at
your option.
