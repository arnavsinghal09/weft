#!/usr/bin/env bash
# Run the full workspace test suite (including the Linux-only LD_PRELOAD e2e
# tests) inside a Linux container. This is the local stand-in for CI when
# developing on macOS.
#
# Usage: scripts/linux-test.sh [extra cargo test args...]
set -euo pipefail

cd "$(dirname "$0")/.."

IMAGE="${WEFT_LINUX_IMAGE:-rust:1.84-bookworm}"

# A named volume keeps the cargo registry warm across runs; a separate
# target dir avoids clobbering the host (macOS) build artifacts.
exec docker run --rm \
    -v "$PWD":/work \
    -v weft-cargo-registry:/usr/local/cargo/registry \
    -w /work \
    -e CARGO_TARGET_DIR=/work/target/linux \
    "$IMAGE" \
    sh -c 'cargo build --workspace && exec cargo test --workspace "$@"' -- "$@"
