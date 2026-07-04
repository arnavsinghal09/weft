#!/usr/bin/env bash
# Benchmark the simulated network against real kernel loopback UDP.
#
# Runs examples/udpbench.c (5000 datagram round trips between two threads)
# natively and under `weft run --net ""`, best of 5, and prints the slowdown.
# Linux only; run inside the container on macOS:
#   docker run --rm -v "$PWD":/work -w /work \
#     -e CARGO_TARGET_DIR=/work/target/linux rust:1.84-bookworm \
#     bash scripts/bench-net.sh
set -euo pipefail

cd "$(dirname "$0")/.."
TARGET="${CARGO_TARGET_DIR:-target}"
WEFT="$TARGET/debug/weft"
SHIM="$TARGET/debug/libweft_shim.so"

[ -x "$WEFT" ] || cargo build --workspace
cc -O2 -o /tmp/udpbench examples/udpbench.c -lpthread

best_ms() { # best-of-5 wall time of "$@" in milliseconds
    local best=999999999
    for _ in 1 2 3 4 5; do
        local t0 t1 dt
        t0=$(date +%s%N)
        "$@" > /dev/null
        t1=$(date +%s%N)
        dt=$(( (t1 - t0) / 1000000 ))
        [ "$dt" -lt "$best" ] && best=$dt
    done
    echo "$best"
}

native=$(best_ms /tmp/udpbench)
simulated=$(best_ms "$WEFT" run --seed 1 --net "" --shim "$SHIM" -- /tmp/udpbench)

echo "udpbench: 5000 round trips (10000 datagrams), best of 5"
echo "  native loopback UDP : ${native} ms"
echo "  weft simulated      : ${simulated} ms"
if [ "$native" -gt 0 ]; then
    echo "  slowdown            : $(( simulated / native ))x"
fi
