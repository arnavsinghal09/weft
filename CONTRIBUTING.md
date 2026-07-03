# Contributing to Weft

Thanks for your interest. Weft is young (pre-alpha), which means contributions
have outsized impact — and that the ground rules below matter more, not less.

## Before you start

1. Read `README.md` for what Weft is, and `PROJECT_NOTES.md` for the project
   layout, phase roadmap, and design decisions already made (language choice,
   crate naming, shim constraints). Please don't reopen decided questions in a
   PR; open a discussion issue instead.
2. For anything larger than a small fix, **open an issue first** describing
   what you want to change and why. This avoids wasted work on both sides.
3. Security issues go through [SECURITY.md](SECURITY.md), never the public
   tracker.

## Development setup

You need stable Rust (MSRV 1.84, see `rust-version` in `Cargo.toml`):

```sh
cargo build --workspace
cargo test --workspace
```

The interception runtime targets **Linux**. The CLI/orchestrator builds and
tests on macOS too, but shim work requires a Linux machine or container.

## Quality gates (CI blocks on all of these)

Run them locally before pushing:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace
cargo deny check          # advisories + licenses + bans + sources
```

`cargo deny` needs `cargo install cargo-deny` once. If you add a dependency,
its license must be on the allow-list in `deny.toml` (permissive only — see
the comment there for why copyleft is excluded).

Additional expectations:

- **Unsafe code** is allowed only where interposition requires it, and every
  `unsafe` block must carry a `// SAFETY:` comment (clippy enforces this).
- **Determinism is the product.** Anything that introduces nondeterminism
  into scheduling, replay, or the shim (wall-clock reads, hash-map iteration
  order leaking into output, thread races) is a bug even if tests pass.
- New subsystems come with a short design note under `docs/`.
- User-visible changes get a line in `CHANGELOG.md` under **Unreleased**.

## Pull requests

- Keep PRs focused; unrelated refactors go in separate PRs.
- Write commit messages that explain *why*, not just *what*.
- PRs must pass CI (lint, audit, license, tests) before review.
- A maintainer review is required to merge. Expect review feedback to focus
  heavily on determinism and safety in shim-adjacent code.

## Licensing of contributions

Weft is dual-licensed MIT OR Apache-2.0. By submitting a contribution, you
agree that it may be distributed under both licenses. Unless you state
otherwise, any contribution intentionally submitted for inclusion in the work
by you, as defined in the Apache-2.0 license, shall be dual licensed as above,
without any additional terms or conditions.

## Code of conduct

Participation is governed by [CODE_OF_CONDUCT.md](CODE_OF_CONDUCT.md).
