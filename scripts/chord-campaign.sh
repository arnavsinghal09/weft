#!/usr/bin/env bash
# Host-side wrapper: run the Chord fuzzing campaign (examples/chord/campaign.sh)
# inside the Linux container, since the interception shim is Linux-only.
#
# Usage: scripts/chord-campaign.sh [SEEDS] [extra env, e.g. NET=...]
set -euo pipefail
cd "$(dirname "$0")/.."

IMAGE="${WEFT_LINUX_IMAGE:-rust:1.84-bookworm}"
SEEDS="${1:-200}"
shift || true

exec docker run --rm \
    -v "$PWD":/work \
    -v weft-cargo-registry:/usr/local/cargo/registry \
    -w /work \
    -e CARGO_TARGET_DIR=/work/target/linux \
    -e SEEDS="$SEEDS" \
    "$@" \
    "$IMAGE" \
    bash examples/chord/campaign.sh
