# Versioning policy

Weft's users depend on three surfaces that change at different rates: the
**scenario DSL**, the **event-log format (weft-log)**, and the **CLI**. Each
has its own compatibility contract, defined here. The Rust crate APIs are a
fourth, weaker surface. Crate versions follow SemVer; this document defines
what "breaking" *means* for each surface, because SemVer alone doesn't.

**Current status: pre-1.0 (0.0.x).** Until 1.0, breaking changes are allowed
on any surface with (a) a `CHANGELOG.md` entry under **Unreleased** marked
**BREAKING**, and (b) for the log format, a version-number bump (that rule is
absolute even pre-1.0, see below). There is no deprecation cycle yet. From
1.0 on, the contracts below get the deprecation policy in the final section.

---

## 1. Event-log format (weft-log) — the strictest contract

A recording is a *bug report that replays forever*. People archive these
files; a format change that orphans old recordings destroys exactly the
value the tool exists to provide. So this surface is versioned explicitly
and independently of everything else:

- The header carries `"format":"weft-log","version":N`. **Any** change to
  record semantics — new required fields, changed digest computation,
  changed meaning of an existing field, changed op numbering — increments
  `N`. This applies pre-1.0 too; version 1 is already a published contract
  (docs/recording-format.md).
- Readers MUST reject unknown versions rather than guess. `weft replay`
  does; your tooling should too.
- **Non-breaking**: adding keys inside the header's `meta` object (readers
  MUST ignore unknown `meta` keys; it is informational by definition), and
  gzip vs. plain encoding (detected by content).
- **Breaking, requires version bump**: everything else. In particular the
  digest chain: two weft-log files with equal digests MUST describe
  byte-identical executions across all Weft releases that can read that
  version.
- Aspiration for 1.0+: ship a `weft log migrate` tool or retain read
  support for at least one previous version. Not promised pre-1.0.

## 2. Scenario DSL — strict on reading, additive-friendly

A scenario file encodes institutional knowledge about how to break a system;
teams check them into their repos and expect them to keep working.

- **Non-breaking**: adding a new *optional* field with a default that
  preserves old behavior; adding a new event `type`; adding a new latency
  distribution or net-spec clause; improving error messages.
- **Breaking**: removing or renaming a field; changing a default; making an
  optional field required; changing validation so a previously-valid file is
  rejected (tightening) or a previously-rejected file is accepted *with
  different semantics*; changing the meaning of an existing value (e.g.
  units of `time_ns`).
- Note the DSL deliberately has **no version field** today. Before 1.0 it
  will either gain one or the format freezes as-is; until then, breaking DSL
  changes are CHANGELOG-flagged and the parser's "unknown field" strictness
  is the compatibility tripwire (a file using newer fields fails loudly on
  older Weft, never silently misbehaves).
- Removed surface is rejected *by name* where feasible: e.g. YAML input was
  never implemented and its vestigial API was removed pre-release rather
  than left as a JSON-parsing trap.

## 3. CLI — the everyday contract

Scripts and CI pipelines consume the CLI. "Breaking" here includes output
that machines parse, not just flags:

- **Breaking**: removing/renaming a subcommand or flag; changing a flag's
  default; **changing any documented exit code** (`weft fuzz`'s 0/2/1 and
  the checkers' 0/2/3/1 are load-bearing CI contracts); changing the format
  of machine-parsed output lines (`replay identical: N op(s), stream digest
  %016x`, the `shrunk : X → Y ops` report lines); changing env-var names or
  activation semantics (`WEFT_*` — presence-activated variables are
  especially sensitive).
- **Non-breaking**: adding subcommands, flags, or env vars; adding output
  lines; improving human-facing prose on stderr; adding *new* exit codes for
  *new* failure modes (never reusing 0/1/2/3 meanings).
- Seed semantics are part of the CLI contract in one narrow, important way:
  we do **not** promise that seed N produces the same schedule across Weft
  releases (engine improvements legitimately change the seed→schedule map);
  we DO promise a recording made by any release replays identically on every
  later release that reads its log version. Archive recordings, not seeds.

## 4. Rust crate APIs

`weft-scenario`, `weft-replay`, `weft-chord`, `weft-raft` expose small
intentional APIs (see docs/REFERENCE.md §7) and follow standard Rust SemVer.
Everything not listed there is implementation detail and may change in any
release regardless of version number. `weft-shim`'s exported C symbols are
an interposition surface, not an API — they mirror libc by construction and
carry no compatibility promise of their own.

## 5. From 1.0 onward

- Breaking changes on any surface require a **minor-version deprecation
  window** (old form keeps working with a warning for ≥ 1 minor release)
  except where technically impossible (log-format semantics — handled by
  version rejection instead).
- The workspace releases in lockstep (one version number across crates), so
  "Weft 1.3" unambiguously names the behavior of every surface.
- `CHANGELOG.md` gains a per-surface compatibility table per release:
  DSL / log / CLI / crates, each marked unchanged · additive · breaking.
