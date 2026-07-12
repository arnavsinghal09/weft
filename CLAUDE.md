# Weft — CLAUDE.md

Deterministic simulation testing (DST) framework for **unmodified Linux
binaries** (FoundationDB-simulator / Antithesis tradition, retrofit-style):
an `LD_PRELOAD` shim makes one 64-bit seed determine every clock read,
random byte, and thread interleaving; `--net` adds a seeded broker so every
datagram's latency/loss/partition fate is a pure function of the seed;
recordings replay byte-for-byte on any platform; `weft fuzz` sweeps seeds
and ddmin-shrinks violations to 1-minimal reproducers.

## Real state (as of 2026-07-12)

- **All nine phases (0–9) implemented and validated** — PROJECT_NOTES.md
  "Phase status". Version 0.0.1, **published to crates.io 2026-07-12**
  (all 6 crates: weft-dst, weft-abi, weft-net, weft-scenario, weft-replay,
  weft-fuzz); docs site live at https://arnavsinghal09.github.io/weft/;
  CHANGELOG.md is one big `[Unreleased]` section. main is pushed to
  github.com/arnavsinghal09/weft.
- Validated against real formally-proven bugs, not synthetic examples:
  Chord ring-maintenance flaw (57→41→8 / 500 seeds across fix levels), Raft
  votedFor-persistence double-election (3/300 buggy, 0/300 fixed) —
  docs/case-study/CREDIBILITY_SUMMARY.md.
- A skeptical pre-launch review (REVIEW_FINDINGS.md, 2026-07-11) found and
  fixed 6 BLOCKING + most SHOULD-FIX items. Still open per its disposition:
  re-verifying Chord `update()` semantics against the paper text and
  re-running the campaign (finding 3.1), and repo security settings (6.2).

## Stack and conventions (as actually used)

- Rust workspace, edition 2021, **MSRV 1.84**, 9 crates under `crates/`
  (layout table: CONTRIBUTING.md "Repo layout"). C example targets in
  `examples/`. Language/naming rationale in PROJECT_NOTES.md — **decided,
  do not revisit**: binary is `weft`, published crate is `weft-dst` (bare
  `weft` is taken on crates.io), Go was disqualified for the shim.
- Lints: clippy `all`+`pedantic` warn (CI promotes to `-D warnings`),
  `undocumented_unsafe_blocks = deny` — every `unsafe` block needs a
  `// SAFETY:` comment. Release profile keeps debug symbols (replay/trace
  tooling needs them).
- CI blocking gates (PROJECT_NOTES.md): fmt, clippy, `cargo deny check`
  (advisories + licenses; permissive only — copyleft rejected because the
  shim links into arbitrary processes), tests. New deps fail CI unless
  license is in `deny.toml`'s allow-list; `weft-shim`/`weft-abi` keep a
  near-zero dependency tree.
- Architecture rules (PROJECT_NOTES.md, CONTRIBUTING.md house rules):
  mechanism in the shim, policy in the orchestrator; shim hooks are
  panic-free, allocation-free, reentrancy-safe on the hot path (canonical
  shape: `crates/weft-shim/src/hooks/file.rs`); network faults live in the
  pure decision core (`weft-net/src/fault.rs`) and **must remain a pure
  function of seed + message identity or replay breaks**; live broker and
  replayer share `weft_net::core::Core` so they cannot drift.
- Error style is deliberately per-crate (thiserror in weft-scenario,
  hand-rolled enums in weft-replay, `Result<_, String>` in weft-net::config
  / weft-fuzz). Match the crate you're editing; wholesale conversion is its
  own PR.
- Linux x86-64/glibc is the only interception target. macOS is dev-only for
  pure crates; shim work goes through `scripts/linux-test.sh` (the exact CI
  container). Do not chase mac shim parity (ROADMAP ranks it below
  seccomp-notify).

## Stable vs in-progress vs unresolved

**Stable — don't churn without strong reason:**
- weft-log v1 format + FNV-1a integrity chain (docs/recording-format.md);
  breaking-change contracts for DSL/log/CLI are in VERSIONING.md.
- The scenario DSL is **JSON-only** — the YAML surface was removed as a
  breaking change (CHANGELOG "Changed").
- Settled design decisions in PROJECT_NOTES.md ("decided once — do not
  revisit" sections).

**In progress / planned (ROADMAP.md near-term):** broker latency
histograms, parallel campaign sharding, TCP in the simulated network
(today UDP only), ENOSPC injection (`WEFT_ENOSPC_BYTES` is reserved in the
ABI and byte-tracking exists, but it is **not wired up** — LIMITATIONS.md
§2), folding live-target fuzzing into `weft fuzz` (today `weft fuzz` sweeps
the broker's internal core; campaigns against real binaries are manual
`scripts/*-campaign.sh`).

**Explicitly unresolved / known limits (LIMITATIONS.md — read before
trusting any result):** static binaries and Go escape interception
*silently* (§1); live multi-process same-seed runs can diverge because
broker arrival order is OS-scheduled — only recordings are exact (§3, the
caveat the README leads with); `pthread_cancel` wedges the scheduler;
signals unmodeled. The residual Chord 8/500 is **detection latency, not a
protocol bug** (docs/case-study/LEVEL_2_RESULTS.md) — keep that framing.

## False starts and reverted approaches (don't rediscover these)

- **YAML scenario API**: `Scenario::from_yaml` parsed JSON while claiming
  YAML; removed outright rather than implemented (CHANGELOG "Changed").
- **`--trace` via `libc::write` deadlocked the target**: inside the shim it
  resolved to the shim's own interposed `write` and re-entered the
  initializing `OnceLock`. Tracing now uses the raw syscall (CHANGELOG
  "Fixed"). General lesson: shim internals must not call interposed libc
  symbols.
- **"Seed 17 reliably reproduces the Chord ring-break live"** — verified
  empirically false (2/3 fresh live runs clean); the user-guide walkthrough
  was rewritten to teach live-run drift instead (CHANGELOG Phase 9).
- **Planned `cargo fuzz` target** was replaced by the 10,000-input
  deterministic parser-robustness sweep (docs/PHASE_VERIFICATION.md).
- **Phase-0 planned crates `weft-sched`/`weft-faults`/`weft-harness` never
  existed** — scheduler landed in the shim, faults in weft-net/weft-scenario,
  harness became weft-chord/weft-raft (PROJECT_NOTES.md layout note). Don't
  reference them.

## Project-specific rigor (beyond global rules)

- **Determinism is the product** (CONTRIBUTING.md): wall-clock time, host
  hash-map iteration order, or OS thread timing leaking into scheduling,
  replay output, or recorded bytes is a bug *even if every test passes*.
  New randomness only via seeded domain streams (`weft-abi::Domain`).
- **Every guarantee has a test that would fail without it** (CONTRIBUTING
  "Testing philosophy"): determinism e2e, replay byte-identity ×10, shrinker
  ground truth (exact known minima must be recovered exactly), 10k-input
  parser no-panic sweep. Adding a guarantee ⇒ add its test; relaxing one ⇒
  say so loudly.
- Shrinking edits **op inputs only** — recorded outcomes are derived data,
  recomputed never edited; no reordering, no touching seed/net (CHANGELOG
  Phase 6).
- Docs are maintained adversarially: numbers must be internally consistent
  across documents (Phase 9 fixed real cross-doc arithmetic errors), and a
  boundary missing from LIMITATIONS.md is itself a documentation bug.
- Per-session workflow (PROJECT_NOTES.md): run the graphify refresh and read
  `graphify-out/GRAPH_REPORT.md` first; refresh again at phase end. Update
  CHANGELOG `[Unreleased]` in the same change that earns it; new subsystem ⇒
  new `docs/<subsystem>.md`; CLI/DSL/log-format changes get checked against
  VERSIONING.md.
