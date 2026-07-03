<!-- Thanks for contributing! See CONTRIBUTING.md for the full expectations. -->

## What & why

<!-- What does this change, and why is it needed? Link the issue if one exists. -->

Closes #

## Checklist

- [ ] `cargo fmt --all --check` passes
- [ ] `cargo clippy --workspace --all-targets --all-features -- -D warnings` passes
- [ ] `cargo test --workspace` passes
- [ ] `cargo deny check` passes (required if dependencies changed)
- [ ] Every new `unsafe` block has a `// SAFETY:` comment
- [ ] `CHANGELOG.md` updated under **Unreleased** (user-visible changes only)
- [ ] Design note added/updated under `docs/` (new subsystems only)

## Determinism impact

<!-- Weft-specific: does this touch scheduling, time, randomness, the shim, or
     trace/replay formats? If yes, explain why determinism and replay
     compatibility are preserved. If no, write "none". -->
