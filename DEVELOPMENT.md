# Development

A complete path from clone to first change. If you only need the ground
rules (quality gates, PR expectations), see [CONTRIBUTING.md](CONTRIBUTING.md) —
this document is the longer, walk-through version for a first-time
contributor.

## 1. Clone and orient

```sh
git clone https://github.com/arnavsinghal09/weft && cd weft
```

Read in this order before writing code: [README.md](README.md) (what this
is), [docs/architecture.md](docs/architecture.md) (how the pieces fit),
[LIMITATIONS.md](LIMITATIONS.md) (where the guarantees stop — most "is this
a bug?" questions are answered here). `PROJECT_NOTES.md` has design
decisions already made; don't reopen them in a PR.

## 2. Build

```sh
cargo build --workspace
```

Stable Rust, MSRV 1.84 (`rust-version` in the root `Cargo.toml`). You'll also
want a C compiler (`cc`) — the example targets under `examples/` are C, used
as deterministic-behavior proofs and as fuzzer/scheduler test subjects.

**Platform split**: the CLI, `weft replay`, `weft fuzz`, and every pure
crate (`weft-net` core, `weft-scenario`, `weft-replay`, `weft-fuzz`,
`weft-chord`, `weft-raft`) build and test natively on macOS. `weft-shim`
(the `LD_PRELOAD` interception cdylib) and anything that loads it are
Linux-only. If you're on macOS, use the container wrapper — it runs exactly
what CI runs:

```sh
scripts/linux-test.sh                 # full workspace test suite, Linux container
scripts/linux-test.sh -p weft-shim    # args pass through to `cargo test`
```

## 3. Run the test suite

```sh
cargo test --workspace          # macOS: pure crates + CLI
scripts/linux-test.sh           # macOS: everything, in Docker
```

On native Linux, `cargo test --workspace` covers everything above. Every
guarantee has a test that would fail without it — see "Testing philosophy"
in [CONTRIBUTING.md](CONTRIBUTING.md) for what pins what.

## 4. Sanitizers and the fuzz targets

These are part of what "the test suite passes" means for shim-adjacent
code, and are not run by plain `cargo test`. The exact, current commands
(also in `scripts/verify-phases.sh`, which runs all of this plus the full
phase-by-phase reverification):

```sh
# ASan + UBSan: native and under the shim (determinism must not corrupt memory)
cc -O1 -g -fsanitize=address,undefined -fno-omit-frame-pointer \
   -o /tmp/entropy.asan examples/entropy.c -lpthread
/tmp/entropy.asan                                    # native: must be clean
ASAN_OPTIONS=verify_asan_link_order=0 \
  target/release/weft run --seed 42 --shim target/release/libweft_shim.so \
  -- /tmp/entropy.asan                                # under the shim: must be clean

# TSan positive control: the scheduler SERIALIZES threads, so run natively
# (WEFT_SCHED=0/--no-sched) to confirm TSan can still see the deliberate race
cc -O1 -g -fsanitize=thread -o /tmp/race_bank.tsan examples/race_bank.c -lpthread
/tmp/race_bank.tsan 4 25   # must report a race (this IS the point of the example)

# TSan negative control: a correctly-synchronized program must stay clean
cc -O1 -g -fsanitize=thread -o /tmp/prodcons.tsan examples/prodcons.c -lpthread
/tmp/prodcons.tsan         # must be clean
```

**The fuzz targets** (`weft fuzz`, not a `cargo fuzz` target — see
LIMITATIONS.md §2 for why cargo-fuzz was replaced with a deterministic
sweep):

```sh
target/release/weft fuzz --config examples/fuzz/ci.json     # expect exit 0 (property holds)
target/release/weft fuzz --config examples/fuzz/demo.json   # expect exit 2 (violations, by design)
```

`ci.json` is a property test (reliable network, FIFO/no-dup must hold for
every seed) — any violation there is a genuine regression.
`crates/weft-scenario/tests/parser_robustness.rs` is the deterministic
stand-in for a coverage-guided fuzzer: 10,000 seeded mutations of a valid
scenario file must never panic the parser. Run it with the rest of the
suite; it finishes in well under a second.

For the full picture — every phase's claims re-verified end to end,
including the case-study checkers — run `scripts/verify-phases.sh` inside
the Linux container (see the script header for the exact `docker run`
invocation).

## 5. Use the graphify workflow yourself

This repo is graphify-instrumented: a knowledge graph over the whole
codebase, kept fresh per session. Before making a non-trivial change, load
context the way prior sessions did:

```sh
graphify . --update --no-viz
cat graphify-out/GRAPH_REPORT.md
```

No LLM API key configured? Use the heuristic fallback instead (note the
different argument order, and no `--no-viz` flag):

```sh
graphify update .
cat graphify-out/GRAPH_REPORT.md
```

While working, query it instead of grepping blind:

```sh
graphify query "how does the scheduler pick the next thread?"
graphify explain "Core"                    # what is weft_net::core::Core
graphify path "weft run" "broker"          # how are two concepts connected
```

When you're done, refresh it — this is expected as part of finishing a
change, the same way updating `CHANGELOG.md` is:

```sh
graphify . --update --no-viz
```

If `GRAPH_REPORT.md` looks noisy after your change (build artifacts,
lockfiles showing up as nodes), fix `.graphifyignore` rather than working
around a broken graph. `graphify-out/` is gitignored and regenerated per
machine — don't commit it.

## 6. Make a first change

Good starting points, roughly in order of how self-contained they are:

- **A "good first issue"** — see the repo's issue tracker; each one is
  scoped and includes enough context to start without asking.
- **Add a replay invariant.** Implement the `Invariant` trait in
  `crates/weft-replay/src/invariant.rs` (see `fifo`/`dup` for the shape),
  register it in `replay_cmd::build_invariants`, and add it to the fuzz
  config's invariant enum. Small, self-contained, exercises the whole
  record→replay→fuzz pipeline.
- **Add an example target.** A single C file under `examples/` that prints
  observable state to stdout and is deterministic-modulo-the-bug it
  demonstrates (see `race_bank.c` for the shape: a real bug, controllable by
  seed). Good for learning the scheduler/network model by exercising it.
- **Write a protocol checker.** Copy `crates/weft-raft` (~150 lines):
  parse your protocol's state-report datagrams out of a `weft_replay::Log`,
  fold them into a verdict, exit 0/2/3/1. The template for testing your own
  system under Weft.

Before opening a PR, run the quality gates from
[CONTRIBUTING.md](CONTRIBUTING.md):

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace
cargo deny check          # advisories + licenses + bans + sources
```

`cargo deny` needs `cargo install cargo-deny` once.

## Where things live

Full layout in [CONTRIBUTING.md](CONTRIBUTING.md#repo-layout). The one-line
version: `crates/weft-shim` is the interception cdylib (unsafe lives here,
every block needs a `// SAFETY:` comment); `crates/weft-dst` is the `weft`
CLI; everything else (`weft-net`, `weft-scenario`, `weft-replay`,
`weft-fuzz`) is pure, platform-independent, and unit-testable without Linux.
