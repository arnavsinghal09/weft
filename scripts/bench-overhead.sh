#!/usr/bin/env bash
# Measure shim overhead: wall-clock each example natively vs under
# `weft run` (release build), and print the percentage delta.
#
# Run on Linux (or via: docker run --rm -v "$PWD":/work -w /work \
#   -e CARGO_TARGET_DIR=/work/target/linux rust:1.84-bookworm \
#   scripts/bench-overhead.sh).
set -euo pipefail

cd "$(dirname "$0")/.."
TARGET="${CARGO_TARGET_DIR:-target}"

cargo build --release --workspace >&2

mkdir -p "$TARGET/bench"
for ex in chrono montecarlo entropy; do
    cc -O2 -o "$TARGET/bench/$ex" "examples/$ex.c" -lpthread
done

RUNS=5
bench() { # bench <label> <cmd...>: prints best-of-N milliseconds
    local best=999999999 t0 t1 dt
    for _ in $(seq "$RUNS"); do
        t0=$(date +%s%N)
        "${@:2}" >/dev/null 2>&1
        t1=$(date +%s%N)
        dt=$(( (t1 - t0) / 1000000 ))
        (( dt < best )) && best=$dt
    done
    echo "$best"
}

printf "%-12s %10s %10s %10s\n" example native_ms weft_ms overhead
for ex in chrono montecarlo entropy; do
    native=$(bench native "$TARGET/bench/$ex")
    weft=$(bench weft "$TARGET/release/weft" run --seed 42 \
        --shim "$TARGET/release/libweft_shim.so" -- "$TARGET/bench/$ex")
    if (( native > 0 )); then
        pct=$(( (weft - native) * 100 / native ))
    else
        pct="n/a(native<1ms)"
    fi
    printf "%-12s %10s %10s %9s%%\n" "$ex" "$native" "$weft" "$pct"
done
