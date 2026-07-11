# Weft — Project Notes

Working notes for contributors and for future sessions picking this project up
cold. Read this file plus `graphify-out/GRAPH_REPORT.md` and you should know
exactly where things stand and what to do next.

## Context-loading workflow (run this first, every session)

```
graphify . --update --no-viz
cat graphify-out/GRAPH_REPORT.md
```

**No LLM API key?** `graphify . --update` errors out if no key (GEMINI/ANTHROPIC/
OPENAI/etc.) is set, because markdown docs want semantic extraction. The
no-key fallback works fine for this corpus and is what Phase 0 validated:

```
graphify update .        # heuristic extraction, no LLM needed
cat graphify-out/GRAPH_REPORT.md
```

Note the argument order differs (`graphify update .`, not `graphify . --update`)
and it takes no `--no-viz` flag. If the rebuild shrinks the graph (e.g. after
adding `.graphifyignore` entries or deleting code), it refuses to overwrite —
rerun with `--force`.

Then, as needed while working:

```
graphify query "<question about the codebase>"
graphify explain "<node name, e.g. a struct or module>"
graphify path "<concept A>" "<concept B>"
```

At the end of every phase, refresh and sanity-check the graph:

```
graphify . --update --no-viz
```

If `GRAPH_REPORT.md` looks noisy (build artifacts, lockfiles, license text
showing up as nodes), fix `.graphifyignore` — do not work around a broken
graph. `graphify-out/` is gitignored and regenerated per machine.

## What Weft is

A deterministic simulation testing (DST) framework, FoundationDB-simulator /
Antithesis style, that works on **unmodified compiled Linux binaries** via
runtime interception (`LD_PRELOAD` first; deeper mechanisms like
ptrace/seccomp-notify later if needed) — not a special-purpose runtime the
target must be written against. One seed determines thread interleaving,
time, randomness, network behavior, and fault schedule; any failure replays
exactly.

## Naming (decided once — do not revisit)

- CLI binary: **`weft`**
- Published crate: **`weft-dst`** (bare `weft` is taken on crates.io by an
  unrelated templating library — never use it in a `Cargo.toml` `name` field)
- Repo-internal crates are prefixed `weft-` (see layout below); only
  `weft-dst` is guaranteed a crates.io publication; others publish as needed.

## Implementation language: Rust (decided, with reasoning)

The deciding constraint: Phase 1 must produce a shared library exposing
libc-compatible symbols for `LD_PRELOAD` interposition, and every later phase
stacks more systems work (scheduling, syscall interception, process
orchestration) on top.

- **Rust** compiles to a `cdylib` with `#[no_mangle] extern "C"` symbols and —
  critically — brings **no runtime** into the target process. Code inside an
  interposed `malloc` or `read` cannot tolerate a GC, signal-based scheduler,
  or lazy runtime init.
- **Go is disqualified**: `c-shared` libraries embed the Go runtime
  (goroutine scheduler, GC, signal handlers) into the intercepted process,
  which is both fragile under interposition and itself a source of
  nondeterminism — the exact thing we're removing.
- **C/C++ would work** for the shim but costs memory safety across eight
  phases of scheduler/fuzzer/fault-engine logic, where Rust's ownership model
  pays for itself. `unsafe` is confined to the interposition boundary and
  gated by `clippy::undocumented_unsafe_blocks = deny` (every unsafe block
  needs a `// SAFETY:` comment).
- **Zig** was considered (excellent for this niche) but has a far thinner
  ecosystem for the CLI/orchestrator/fuzzer layers and a less stable compiler.

Toolchain: stable Rust, edition 2021, MSRV 1.84 (recorded in
`workspace.package.rust-version`; CI pins nothing newer than stable).

## Directory layout (as built)

Cargo workspace. The Phase-0 plan reserved separate `weft-sched`,
`weft-faults`, and `weft-harness` crates; in practice the scheduler landed
inside the shim (it must live in the target process), fault mechanisms
landed in `weft-net`/`weft-scenario`/the shim's file hooks, and the
"harness" role became the two protocol-checker crates:

```
weft-dst-app/
├── Cargo.toml              # workspace root (lints, shared package metadata)
├── crates/
│   ├── weft-dst/           # CLI (`weft` binary): run/replay/fuzz commands,
│   │                       #   cluster orchestration, event scheduler.
│   ├── weft-shim/          # cdylib LD_PRELOAD shim: libc hooks (time, rand,
│   │                       #   devices, sockets, file I/O, pthreads), the
│   │                       #   deterministic scheduler, virtual clock, RNG.
│   ├── weft-abi/           # env-var contract + seed parsing shared by CLI
│   │                       #   and shim (dependency-free).
│   ├── weft-net/           # simulated network: seeded broker, pure decision
│   │                       #   core, fault model, wire protocol, net spec.
│   ├── weft-scenario/      # JSON fault-scenario DSL: parsing + validation.
│   ├── weft-replay/        # weft-log format, recorder, byte-exact replayer,
│   │                       #   invariant API.
│   ├── weft-fuzz/          # seed sweeps + ddmin shrinking to minimal
│   │                       #   reproducers.
│   ├── weft-chord/         # Chord case study: invariant checker, tracer,
│   │                       #   stateright cross-validation oracle.
│   └── weft-raft/          # Raft case study: ElectionSafety checker + oracle.
├── examples/               # C target programs (incl. chord/, raft/, fuzz/)
├── scripts/                # campaign drivers, benchmarks, sanitizer runs
├── docs/                   # design docs per subsystem + case-study evidence
└── .github/workflows/      # CI (see below)
```

Rules of thumb already decided:
- The shim (`weft-shim`) stays minimal and panic-free; policy lives in the
  orchestrator, mechanism lives in the shim.
- Anything loaded into the target process (`weft-shim`, `weft-abi`) keeps a
  near-zero dependency tree.
- Linux is the supported target for interception. macOS is a dev platform for
  the orchestrator/CLI only (interposition differs: `DYLD_INSERT_LIBRARIES`);
  do not chase mac parity for the shim.

## CI (`.github/workflows/ci.yml`) — blocking gates from Phase 0

Runs on every push and PR, on `ubuntu-latest`:

1. **Lint (blocking):** `cargo fmt --check` + `cargo clippy --workspace
   --all-targets -- -D warnings` (workspace lints already set clippy
   `all`+`pedantic` to warn, promoted to errors here).
2. **Vulnerability audit (blocking):** `cargo deny check advisories`
   (RustSec database).
3. **License compliance (blocking):** `cargo deny check licenses bans sources`
   — allow-list lives in `deny.toml` (permissive licenses only; copyleft
   rejected because the shim links into arbitrary user processes).
4. **Tests:** `cargo test --workspace`.
5. **Coverage (non-blocking report):** `cargo llvm-cov` → uploads to Codecov
   if `CODECOV_TOKEN` is configured; job succeeds regardless so forks aren't
   broken.

`deny.toml` at the repo root controls gates 2–3. When adding a dependency,
expect CI to fail unless its license is in the allow-list.

## Phase status

All nine phases are implemented and validated (details in each phase's
design note under `docs/`, history in `docs/history/` and `CHANGELOG.md`):

- **Phase 0 (done):** skeleton workspace, CI with the three blocking gates,
  community files, graphify workflow.
- **Phase 1 (done):** `weft-shim` + `weft-abi` — time, randomness, and
  device-file interception; virtual clock; ChaCha8 domain streams.
- **Phase 2 (done):** deterministic thread scheduler (token model,
  `random`/`rr` strategies, deadlock detection) — docs/scheduling-model.md.
- **Phase 3 (done):** simulated UDP network via seeded broker
  (`weft-net`) — docs/network-model.md.
- **Phase 4 (done):** fault model, scenario DSL (`weft-scenario`), file-I/O
  faults, process orchestration — docs/fault-model.md.
- **Phase 5 (done):** weft-log recording + byte-exact replay
  (`weft-replay`) — docs/recording-format.md.
- **Phase 6 (done):** seed fuzzing + ddmin shrinking (`weft-fuzz`) —
  docs/fuzzing.md.
- **Phase 7 (done):** validation case studies — Chord falsified (57→8/500),
  Raft edge case reproduced (3/300 vs 0/300) —
  docs/case-study/CREDIBILITY_SUMMARY.md.
- **Phase 8 (done):** release engineering — full documentation set
  (USER_GUIDE, REFERENCE, LIMITATIONS, VERSIONING, comparison, RELEASE),
  SBOM, reproducible-build verification, container-verified quickstart.
- **Phase 9 (done):** project readiness — landing-page README, focused
  Antithesis/TigerBeetle comparison (docs/comparison.md), ROADMAP.md with an
  explicit not-planned section, DEVELOPMENT.md onboarding path,
  CITATION.cff, 5 scoped good-first-issue GitHub issues, and an adversarial
  self-review pass that found and fixed real cross-document contradictions
  and arithmetic errors (see CHANGELOG.md's Phase 9 entry for the list).

## Working conventions

- Update `CHANGELOG.md` (Unreleased section) in the same change that earns it.
- Every phase ends with the graphify refresh (top of this file) and a check
  that `GRAPH_REPORT.md` reflects reality.
- New subsystem ⇒ new `docs/<subsystem>.md` design note before or with the
  code.
