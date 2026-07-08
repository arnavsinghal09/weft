# The Weft recording format (`weft-log`, version 1)

This document specifies the event-log format precisely enough that an
independent tool, given only this text, can read (and verify) a log
correctly. The reference implementation is `crates/weft-replay/src/log.rs`.

## 1. Why the log contains what it contains

The design question behind this format: *what is the minimal information that
must be recorded so that replay reproduces byte-for-byte identical execution,
regardless of the replaying machine's clock, thread timing, or entropy?*

In a Weft run, almost everything is already a **pure function of the run
seed** and therefore needs no recording — it is recomputed on replay:

| recomputed on replay          | from                                        |
|-------------------------------|---------------------------------------------|
| datagram fates (drop, delay)  | `fate(seed, src, dst, chan_seq, len)`       |
| virtual time                  | the virtual clock's defined advance rules   |
| PRNG output                   | seeded per-domain ChaCha8 streams           |
| managed-thread schedule       | the seeded scheduler stream                 |
| delivery order among pending  | smallest `(deliv_ns, tie)` pops first       |

Exactly **one** input to a simulated run is not seed-derived: the **broker
linearization order** — the order in which requests from independently
OS-scheduled processes acquire the broker's state lock. That order decides:

- which global `tie` value each enqueued datagram gets (the deterministic
  tiebreaker between equal delivery times),
- how per-channel sequence numbers interleave across channels,
- every send-vs-recv race the application can observe (whether a poll sees
  `empty` or a delivery).

A `weft-log` therefore records the linearized sequence of broker boundary
operations, each with its *inputs* (who, what address, what payload) and the
*outcome the broker computed* (for verification, not for trust — replay
recomputes every outcome and fails loudly on any mismatch). The header
carries the seed and the network-condition spec; nothing else is needed.

Deliberately **absent** from replay-relevant fields: wall-clock timestamps,
PIDs, hostnames, thread ids, file descriptors. Machine-specific facts appear
only inside the header's `meta` object, which is informational and MUST NOT
influence replay.

## 2. File layout

A log is a UTF-8 text file of newline (`\n`) delimited JSON values
(JSON Lines). No BOM. Writers emit no interior blank lines; readers must
tolerate one trailing newline at EOF.

```
line 1      Header object
line 2..N   Record objects, one per linearized operation
last record an "end" event (absent if the run crashed mid-recording;
            the surviving prefix is still chain-verifiable)
```

## 3. The header (line 1)

```json
{"format":"weft-log","version":1,"seed":3,"net":"latency=uniform:1000-100000","meta":{}}
```

| field     | type   | meaning                                                          |
|-----------|--------|------------------------------------------------------------------|
| `format`  | string | MUST be `"weft-log"`; reject anything else.                      |
| `version` | u32    | This spec is version `1`. Readers MUST reject unknown versions rather than guess. |
| `seed`    | u64    | The run seed. With the recorded order, reproduces every fate.    |
| `net`     | string | The network-condition spec exactly as the broker parsed it (`weft_net::config` syntax: `latency=…`, `loss=…`, `bw=…`, `partition=…` joined by commas). Empty string = reliable network. |
| `meta`    | object | Informational only. Known keys: `recorded_unix_ms` (u64), `weft_version` (string), `label` (string). All optional; readers MUST ignore unknown keys here. Replay-irrelevant by definition. |

## 4. Records (lines 2..N)

```json
{"op":7,"vt":41235,"e":{"k":"recv","conn":0,"blocking":false,"outcome":{...}},"chain":"9f3c2a1b8d4e5f60"}
```

| field   | type   | meaning                                                            |
|---------|--------|--------------------------------------------------------------------|
| `op`    | u64    | Position in the linearization. Dense, starting at 0, strictly increasing by 1. Readers MUST reject gaps or reordering. |
| `vt`    | u64    | The broker's virtual-time high-water mark, in nanoseconds, *after* applying this operation: the largest delivery time scheduled so far. This is the event's coordinate on the logical timeline; invariant violations report against it. |
| `e`     | object | The event, tagged by `"k"` (§5).                                   |
| `chain` | string | 16 lowercase hex digits: the integrity chain through this record (§6). |

### Addresses

An address object is `{"ip":2130706434,"port":200}` — the virtual IPv4
address as a host-byte-order u32 and a u16 port. By convention node *n* is
`127.0.0.(n+1)`, i.e. `ip = 0x7f000001 + n`.

### Payloads

Payload bytes are hex-encoded (lowercase, two digits per byte) in a string
field named `payload`. Payloads are recorded in full: replay must reproduce
delivered bytes exactly, and a hash alone could not.

## 5. Event types (`e.k`)

Serde representation: internally tagged by `k`, snake_case. Outcome objects
are tagged by `kind`.

| `k`          | fields                                                        | linearization point |
|--------------|---------------------------------------------------------------|---------------------|
| `connect`    | `conn` (u64)                                                  | connection registered under the state lock |
| `hello`      | `conn`, `node` (u32)                                          | first protocol message; no state change, recorded for node identity |
| `bind`       | `conn`, `addr`                                                | address claimed |
| `send`       | `conn`, `src`, `dst`, `chan_seq` (u64), `payload` (hex), `outcome` | datagram routed |
| `recv`       | `conn`, `blocking` (bool), `outcome`                          | the queue pop (or empty answer). A *blocking* recv is logged when it **succeeds** — the pop is its linearization point, so replay finds the datagram already enqueued. |
| `disconnect` | `conn`                                                        | connection dropped; its bindings released |
| `violation`  | `invariant` (string), `message` (string)                      | appended immediately after the op that completed the violation |
| `end`        | `events` (u64, total records incl. this), `sent` (u64), `dropped` (u64) | end of run |

`send.outcome` (tag `kind`):

- `{"kind":"dropped"}` — the fault model dropped it (loss or partition).
- `{"kind":"no_receiver"}` — no connection had bound `dst`; discarded like
  UDP to a closed port. **Consumes a channel sequence number but no tie.**
- `{"kind":"enqueued","to_conn":N,"deliv_ns":N,"tie":N}` — queued for
  delivery.

`recv.outcome` (tag `kind`):

- `{"kind":"empty"}`
- `{"kind":"delivered","src":{…},"dst":{…},"deliv_ns":N,"tie":N,"payload":"…"}`

Semantics a replayer must reproduce (all implemented by
`weft_net::core::Core`, which the live broker and the reference replayer
both execute):

1. `chan_seq` advances per (src, dst) channel on **every** send, dropped or
   not.
2. `tie` advances globally, but **only** when a datagram is actually
   enqueued.
3. `recv` pops the pending datagram with the smallest `(deliv_ns, tie)`.
4. `vt` after an operation = max(previous `vt`, any `deliv_ns` scheduled by
   it).
5. `disconnect` removes the connection's queue and every address bound to it.

## 6. The integrity chain

The chain detects truncation, reordering, and edits (it is not a defense
against adversaries — FNV-1a is not cryptographic; that is a deliberate
trade for spec-only implementability).

Define `FNV1a(state, bytes)` as 64-bit FNV-1a: for each byte
`state = (state XOR byte) * 0x100000001b3` (wrapping), starting from the
given state. The standard offset basis is `0xcbf29ce484222325`.

- `chain₀ = FNV1a(offset_basis, header_line)` where `header_line` is the
  exact bytes of line 1 **without** the trailing newline.
- For record *n* (0-based): `chainₙ₊₁ = FNV1a(chainₙ, canon(opₙ, vtₙ, eₙ))`
  where `canon` is the JSON serialization of the object
  `{"op":…,"vt":…,"e":…}` with exactly those three fields in exactly that
  order and no whitespace — i.e. the record line minus its `chain` field.
- Record *n*'s `chain` field is `chainₙ₊₁` as 16 lowercase hex digits,
  zero-padded.

A reader verifies by recomputing the chain over each line; the first
mismatch identifies the earliest corrupted or edited record. A truncated
file (e.g. the recorder crashed) verifies cleanly up to the truncation
point and simply lacks the `end` record.

### Stream digest

The **stream digest** of a log is `FNV1a` folded over `canon(op, vt, e)` of
every record from the offset basis. Two logs with equal stream digests
describe byte-identical executions; header `meta` differences do not affect
it. Replay reports its recomputed stream digest, which must equal the
recording's.

## 7. Replay contract

Given a verified log, a conforming replayer:

1. Parses `seed` and `net` from the header; rejects the log if `net` does
   not parse (`bad net spec` — the log is uninterpretable).
2. Re-executes records in `op` order, applying only each event's **inputs**
   (conn, addresses, payload, blocking flag) to the broker state machine of
   §5, and *recomputing* every outcome, `chan_seq`, `tie`, and `vt`.
3. Compares each recomputed record against the recorded one. The first
   mismatch is a **divergence** (report both sides at that `op`); a
   divergence means the log, seed, and code no longer agree — replay must
   never silently prefer either side.
4. On a clean run, the recomputed stream digest equals the recorded one.

The replayer must not consult the machine clock, spawn concurrency, or draw
entropy: everything it needs is the log plus this specification. That is
what makes replay results identical across machines.

`replay --until N` is the same procedure stopped after applying op `N`,
leaving the state machine inspectable at that point on the timeline.

## 8. Invariant checking against a log

Invariants (see `weft_replay::invariant`) consume the linearized event
stream and anchor violations to `(op, vt)` — a precise point in the
linearization and on the virtual-time axis, never "sometime during the
run". The same invariant code runs in-process during recording and from an
external checker over the log file; §5's `violation` events record what the
in-process monitor observed, and a replayer given the same invariants must
re-raise identical violations at identical positions.

## 9. Scope and honesty

- The log captures the **broker boundary**. A single-process run that never
  touches the network needs no log at all: it is already a pure function of
  the seed (re-running with the same seed *is* its replay).
- What the target processes do with delivered bytes is reproduced only
  insofar as their inputs (time, randomness, schedule, network) are under
  Weft's control — the shim's existing limitations (see
  docs/architecture.md) apply unchanged.
- File-I/O fault decisions (Phase 4) are seed/env-derived and thus
  recomputed, not recorded. If a future fault source stops being a pure
  function of the seed, its decisions must start being recorded — that is
  the rule this format encodes.

## 10. Versioning policy

Any change to canonical serialization, chain computation, event fields, or
semantics of §5 requires incrementing `version`. Readers reject unknown
versions. Version 1 readers must ignore unknown keys only inside `meta`;
unknown keys anywhere else are a malformed record.

## 11. Compression (optional transport encoding)

A log file MAY be gzip-compressed (RFC 1952) as a whole file. This is a
transport encoding only: the compressed payload is the exact byte sequence
§2–§6 describe, the `version` stays unchanged, and the chain and stream
digest are computed over the **uncompressed** text.

- **Detection is by content, never extension**: a reader MUST check the
  file's first two bytes for the gzip magic `1f 8b` and decompress before
  parsing; anything else is parsed as plain text.
- Writers SHOULD use a `.gz` suffix as a human convention (the reference
  recorder compresses exactly when the requested path ends in `.gz`).
- Trade-off: the reference writer sync-flushes after every record, but a
  gzip member is only complete once its trailer is written at the end of
  recording. A run that crashes mid-recording therefore leaves a compressed
  file without a trailer; standard decoders recover the flushed prefix but
  report an unexpected-EOF at the end. Prefer an uncompressed path while a
  scenario is still fragile; compress for archival.
