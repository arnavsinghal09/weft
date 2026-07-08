#!/usr/bin/env bash
# Host wrapper: run the Raft ElectionSafety campaign inside the Linux
# container (the shim is Linux-only). Usage: scripts/raft-campaign.sh [SEEDS]
set -euo pipefail
cd "$(dirname "$0")/.."
SEEDS=${1:-${SEEDS:-300}}
docker run --rm -v "$PWD":/work -v weft-cargo-registry:/usr/local/cargo/registry \
  -w /work -e CARGO_TARGET_DIR=/work/target/linux \
  -e SEEDS="$SEEDS" -e RAFT_FIX="${RAFT_FIX:-0}" -e NET="${NET:-latency=uniform:2000-10000}" \
  -e TICKS="${TICKS:-40}" -e MEMBERS="${MEMBERS:-5}" \
  rust:1.84-bookworm bash examples/raft/campaign.sh
