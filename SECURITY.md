# Security Policy

Weft loads code into other processes and injects faults by design, so we take
reports about it seriously — a bug in Weft can corrupt or expose the systems
it is testing.

## Supported versions

Weft is pre-release (0.x). Only the latest published release and the tip of
`main` receive security fixes.

## Reporting a vulnerability

**Do not open a public issue for security bugs.**

Preferred channel: **GitHub private vulnerability reporting** — use
"Report a vulnerability" under the repository's *Security* tab, which opens a
private advisory visible only to maintainers.

If you cannot use GitHub, email **arnavsinghal06@gmail.com** with subject
line `[weft security]`. Include: affected version/commit, a reproduction or
proof of concept, and your assessment of impact.

## What to expect

- **Acknowledgement within 72 hours** of your report.
- An assessment (accepted / duplicate / not-a-vulnerability, with reasoning)
  within **14 days**.
- If accepted: a fix developed in a private branch, a coordinated release,
  and a GitHub Security Advisory crediting you (unless you prefer anonymity).
- We ask for **90 days** before public disclosure; we will usually be far
  faster, and we will tell you immediately if we need longer.

## Scope notes

In scope: anything where Weft breaks the safety of the *host* system beyond
the process under test — e.g. the shim escaping its target process, the
orchestrator executing untrusted input, path traversal in trace/replay files,
CI supply-chain issues in this repo.

Out of scope: crashes *of the program under test* (that is Weft's purpose),
and vulnerabilities in programs you choose to run under Weft.
