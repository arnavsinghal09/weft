# Contributing to Weft

Thanks for your interest. This guide assumes you have never seen this
codebase before and gets you from clone to a merged PR.

## Orientation (read in this order)

1. [README.md](README.md) — what Weft is and the 5-minute quickstart.
2. [docs/architecture.md](docs/architecture.md) — how the pieces fit, before
   you open any code.
3. [LIMITATIONS.md](LIMITATIONS.md) — where the guarantees stop. Most
   "is this a bug?" questions are answered here.
4. The subsystem doc for whatever you're touching:
   `docs/{scheduling,network,fault,logical-time}-model.md`,
   `docs/recording-format.md`, `docs/fuzzing.md`.

Historical phase reports live in `docs/history/`; design notes and decisions
already made (language, crate naming, shim constraints) are in
`PROJECT_NOTES.md` — please don't reopen decided questions in a PR; open a
discussion issue instead.

## Repo layout

```
crates/
  weft-dst/       the `weft` CLI (run / replay / fuzz) + orchestrator
  weft-shim/      LD_PRELOAD cdylib: hooks + engine + scheduler  [unsafe lives here]
  weft-abi/       env-var names, seed expansion, domain ids (shim-safe, tiny)
  weft-net/       broker + pure decision core + fault model + wire protocol
  weft-scenario/  scenario DSL (JSON) parsing + validation
  weft-replay/    weft-log recording, replay verification, invariants
  weft-fuzz/      seed sweeping + ddmin shrinker
  weft-chord/     case study: Chord ring-invariant checker + trace tool
  weft-raft/      case study: Raft ElectionSafety checker
examples/         C targets (chrono, race_bank, pingpong, chord/, raft/, …)
examples/fuzz/    fuzz configs (ci.json = CI property test, demo.json = demo)
examples/scenarios/  runnable scenario DSL files
scripts/          linux-test.sh, campaign + bench + verification scripts
docs/             architecture, models, reference, user guide, case studies
```

## Development setup

Stable Rust (MSRV 1.84 — see `rust-version` in `Cargo.toml`) and a C
compiler for the example targets.

```sh
cargo build --workspace
cargo test --workspace
```

**On macOS**: the CLI, replay, fuzz, and all pure crates build and test
natively. Everything involving the shim (LD_PRELOAD, scheduler e2e, broker
e2e) is Linux-only. Use the container wrapper — it is exactly what CI runs:

```sh
scripts/linux-test.sh              # full workspace suite in rust:1.84-bookworm
scripts/linux-test.sh -p weft-shim # extra args pass through to cargo test
```

## Quality gates (CI blocks on all of these)

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace
cargo deny check          # advisories + licenses + bans + sources
```

`cargo deny` needs `cargo install cargo-deny` once. New dependencies must be
permissively licensed (see the allow-list and rationale in `deny.toml`;
copyleft is excluded because the shim loads into arbitrary user processes) —
and think twice: `weft-shim`/`weft-abi` deliberately keep a near-zero
dependency tree.

House rules the linters can't fully enforce:

- **Determinism is the product.** Anything that lets wall-clock time, host
  hash-map iteration order, or OS thread timing leak into scheduling, replay
  output, or recorded bytes is a bug even if every test passes. New sources
  of randomness must come from a seeded domain stream (see
  `weft-abi::Domain`).
- **Unsafe code** only where interposition requires it, and every `unsafe`
  block carries a `// SAFETY:` comment (clippy denies undocumented ones).
- **Shim hooks must be allocation-free and reentrancy-safe** on the hot
  path: no `std::env::var` per call, no stdio, no locks beyond the engine's
  own. Look at `crates/weft-shim/src/hooks/file.rs` for the canonical shape.
- **Error style**: `weft-scenario` uses `thiserror` enums; `weft-replay`
  hand-rolls its error enums; internal plumbing (`weft-net::config`,
  `weft-fuzz`) returns `Result<_, String>` that surfaces directly in CLI
  output. Match the style of the crate you are editing; converting a crate
  wholesale is its own PR.
- New subsystems come with a short design note under `docs/`; user-visible
  changes get a line in `CHANGELOG.md` under **Unreleased** and, if they
  touch the CLI, DSL, or log format, a check against
  [VERSIONING.md](VERSIONING.md).

## Recipes

**Add an invariant checker for your protocol.** Copy the `weft-raft` crate
(~150 lines): parse your node's state-report datagrams out of
`weft_replay::Log` records, fold them into a `Verdict`, and exit `0` (holds)
/ `2` (violation) / `3` (uninformative) / `1` (unreadable). Have your target
print `RPT <fields>` datagrams each tick, run campaigns with
`weft run --net … --record`, and point your checker at the recordings.

**Add a replay invariant** (checked by `weft replay --check` and
`weft fuzz`): implement the `Invariant` trait in
`crates/weft-replay/src/invariant.rs` (see `fifo` / `dup`), register it in
`replay_cmd::build_invariants`, and add it to the fuzz config enum.

**Add a fault type.** Network-level faults go in the pure decision core
(`weft-net/src/fault.rs` — must remain a pure function of seed + message
identity, or replay breaks). Syscall-level faults go in a shim hook
(`weft-shim/src/hooks/`) behind a `WEFT_*` env var registered in `weft-abi`,
plus a field in the scenario DSL with validation in
`weft-scenario/src/parse.rs`.

**Add an example target.** A single C file in `examples/` that prints its
observable state to stdout; keep it deterministic-modulo-the-bug so a seed
either triggers or avoids the behavior. Wire it into a test in
`crates/weft-dst/tests/` if it pins a guarantee.

## Testing philosophy

Every guarantee has a test that would fail without it: determinism e2e
(`weft-dst/tests/e2e.rs`), scheduler race pinning (`sched_e2e.rs`), replay
byte-identity (`weft-replay/tests/gzip.rs`), shrinker ground truth
(`weft-fuzz/tests/shrink_ground_truth.rs` — exact known minima must be
recovered exactly), parser no-panic sweep (10,000 mutated inputs,
`weft-scenario/tests/parser_robustness.rs`). If your PR adds a guarantee,
add its test; if it relaxes one, say so loudly in the description.

Sanitizer runs (ASan/UBSan, TSan with `--no-sched`) are part of the phase
verification suite: `scripts/verify-phases.sh` runs everything in the
container.

## Pull requests

- Keep PRs focused; unrelated refactors go in separate PRs.
- Commit messages explain *why*, not just *what*.
- CI (lint, audit, license, tests) must pass before review.
- Expect review feedback to focus heavily on determinism and safety in
  shim-adjacent code.
- For anything larger than a small fix, open an issue first.
- Security issues go through [SECURITY.md](SECURITY.md), never the public
  tracker.

## Licensing of contributions

Weft is dual-licensed MIT OR Apache-2.0. Unless you state otherwise, any
contribution intentionally submitted for inclusion in the work by you, as
defined in the Apache-2.0 license, shall be dual licensed as above, without
any additional terms or conditions.

## Code of conduct

Participation is governed by [CODE_OF_CONDUCT.md](CODE_OF_CONDUCT.md).
