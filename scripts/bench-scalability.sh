#!/usr/bin/env bash
# Scalability measurements for docs/SCALABILITY.md. Linux-only; run inside
# the container:
#   docker run --rm -v "$PWD":/work -v weft-cargo-registry:/usr/local/cargo/registry \
#     -w /work -e CARGO_TARGET_DIR=/work/target/linux rust:1.84-bookworm \
#     bash scripts/bench-scalability.sh
# Run it on an otherwise-idle machine: these are wall-clock measurements.
set -uo pipefail
cd /work
T=${CARGO_TARGET_DIR:-/work/target/linux}
WEFT=$T/release/weft
SHIM=$T/release/libweft_shim.so
EX=/tmp/bench-examples
mkdir -p "$EX"

echo "[bench] build (release)…"
cargo build --release --workspace 2>&1 | tail -1
# Section C needs GNU time (-v) for max-RSS; rust:*-bookworm ships without it.
if ! command -v /usr/bin/time >/dev/null; then
    { apt-get update -qq && apt-get install -y -qq time; } >/dev/null 2>&1 \
        || echo "[bench] WARN: could not install GNU time; section C RSS will be empty"
fi
for ex in chrono montecarlo entropy udpbench pingpong; do
    cc -O2 -o "$EX/$ex" "examples/$ex.c" -lpthread
done
cc -O2 -o "$EX/chord_node" examples/chord/chord_node.c

ms() { # wall-clock of "$@" in ms
    local t0 t1
    t0=$(date +%s%N); "$@" >/dev/null 2>&1; t1=$(date +%s%N)
    echo $(( (t1 - t0) / 1000000 ))
}
best_of() { # best_of N cmd...
    local n=$1; shift
    local best=999999999 dt
    for _ in $(seq "$n"); do dt=$(ms "$@"); (( dt < best )) && best=$dt; done
    echo "$best"
}

echo
echo "== A. shim overhead (phase-1 programs, best of 5, wall ms) =="
printf "%-12s %10s %10s %10s\n" example native_ms weft_ms overhead
for ex in chrono montecarlo entropy; do
    native=$(best_of 5 "$EX/$ex")
    weft=$(best_of 5 "$WEFT" run --seed 42 --shim "$SHIM" -- "$EX/$ex")
    if (( native > 0 )); then pct="$(( (weft - native) * 100 / native ))%"; else pct="n/a(<1ms)"; fi
    printf "%-12s %10s %10s %10s\n" "$ex" "$native" "$weft" "$pct"
done
echo "note: chrono sleeps ~2.8s natively; the shim virtualizes sleeps (they return"
echo "      instantly), so its row measures time acceleration, not overhead."

echo
echo "== B. broker datagram RTT (udpbench: 5000 round trips = 10000 broker ops) =="
native=$(best_of 5 "$EX/udpbench")
echo "native loopback : total ${native} ms  => $(( native * 1000 / 10000 )) µs/datagram"
for run in 1 2 3 4 5; do
    w=$(ms "$WEFT" run --seed "$run" --net "latency=fixed:0" --shim "$SHIM" -- "$EX/udpbench")
    echo "weft run $run    : total ${w} ms  => $(( w * 1000 / 10000 )) µs/datagram (mean; per-op percentiles need broker-side instrumentation — guest clocks are virtual)"
done

echo
echo "== C. node-count scaling + broker memory (chord workload, 1 seed) =="
printf "%-8s %10s %12s\n" nodes wall_ms weft_maxRSS
for members in 6 9 13; do
    n=$((members + 1))
    log=/tmp/scale-$n.weftlog
    t0=$(date +%s%N)
    CHORD_NNODES=$n CHORD_FIX=0 \
      /usr/bin/time -v "$WEFT" run --seed 1 --net "latency=uniform:1000-8000" --nodes "$n" \
        --record "$log" -- "$EX/chord_node" 6 45 3 >/dev/null 2>/tmp/time-$n.log
    t1=$(date +%s%N)
    rss=$(grep "Maximum resident" /tmp/time-$n.log | awk '{print $6}')
    printf "%-8s %10s %10s kB   log=%s bytes\n" "$n" "$(( (t1-t0)/1000000 ))" "$rss" "$(stat -c %s "$log")"
done

echo
echo "== D. recording size vs run length (chord, 1 seed, growing ticks) =="
for ticks in 45 150 450; do
    log=/tmp/scale-t$ticks.weftlog
    t0=$(date +%s%N)
    CHORD_NNODES=7 CHORD_FIX=0 "$WEFT" run --seed 1 --net "latency=uniform:1000-8000" --nodes 7 \
        --record "$log" -- "$EX/chord_node" 6 "$ticks" 3 >/dev/null 2>&1
    t1=$(date +%s%N)
    echo "ticks=$ticks : wall $(( (t1-t0)/1000000 )) ms, log $(stat -c %s "$log") bytes"
done

echo
echo "== E. shrinking at ~10k events (weft fuzz, latency variance + loss) =="
cat > /tmp/shrink10k.json <<'EOF'
{
  "//": "scalability probe: ~10k ops per seed, expect fifo violations, time the shrink",
  "net": "latency=uniform:0-8000,loss=0.02",
  "seed_start": 0,
  "seed_count": 8,
  "invariants": ["fifo", "dup"],
  "workload": { "nodes": 3, "sends": 3300, "payload_len": 4 },
  "out_dir": "/tmp/shrink10k-out"
}
EOF
rm -rf /tmp/shrink10k-out
t0=$(date +%s%N)
"$WEFT" fuzz --config /tmp/shrink10k.json 2>&1 | grep -E "shrunk|violation|seeds|elapsed" | head -12
t1=$(date +%s%N)
echo "total fuzz+shrink wall: $(( (t1-t0)/1000000 )) ms"

echo
echo "== F. live-run verdict reproducibility (chord seed 0, 10 live runs) =="
# Replay of a RECORDING is byte-identical (Phase 5, tested). A fresh LIVE
# run of the same seed re-rolls cross-process arrival order (OS-scheduled,
# the documented Phase-3 limitation). This measures how often the same seed
# reaches the same verdict live — the honest number behind "statistical,
# not seed-for-seed" campaign comparisons.
viol=0; okc=0; disc=0
for i in $(seq 1 10); do
    log=/tmp/repro-$i.weftlog
    CHORD_NNODES=7 CHORD_FIX=0 "$WEFT" run --seed 0 --net "latency=uniform:1000-60000" --nodes 7 \
        --record "$log" -- "$EX/chord_node" 6 45 3 >/dev/null 2>&1
    "$T/release/chord-check" "$log" 6 >/dev/null 2>&1
    case $? in
        2) viol=$((viol+1));;
        3) disc=$((disc+1));;
        0) okc=$((okc+1));;
    esac
done
echo "chord seed 0 live x10: violation=$viol ok=$okc discard=$disc"

echo
echo "[bench] done"
