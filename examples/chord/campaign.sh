#!/usr/bin/env bash
# Chord fuzzing campaign: sweep fault seeds over REAL multi-process Chord runs
# (original 2001 protocol) through Weft's shim + broker, record each run, and
# check the final quiescent state against Zave's correctness invariants.
#
# Scenario (per docs/case-study/chord-spec.md): BASE=3 permanent nodes (the
# r+1 "stable base") start as the ideal ring; APPENDAGES join at seed-jittered
# ticks and fail at seed-jittered later ticks; message delays/reordering come
# from the broker's fault model under the same seed. A seed "hits" when the
# final state — after a quiescent repair tail with no further faults —
# violates AtLeastOneRing / AtMostOneRing / OrderedRing / ConnectedAppendages.
#
# Runs inside the Linux container. Invoked by scripts/chord-campaign.sh.
set -uo pipefail
cd /work

M=${M:-6}
TICKS=${TICKS:-45}
BASE=${BASE:-3}
MEMBERS=${MEMBERS:-6}          # 3 base + 3 appendages
SEEDS=${SEEDS:-200}
SEED_START=${SEED_START:-0}
NET=${NET:-latency=uniform:1000-60000}
CHORD_FIX=${CHORD_FIX:-0}
case "$CHORD_FIX" in
  0) LABEL=orig ;;
  1) LABEL=fix1-stabilize ;;
  2) LABEL=fix2-full ;;
  *) LABEL=fix$CHORD_FIX ;;
esac
OUT=${OUT:-/work/target/chord-out-$LABEL}

NNODES=$((MEMBERS + 1))        # +1 observer
export CHORD_NNODES=$NNODES
export CHORD_FIX             # propagates to chord_node children via weft run

WEFT=/work/target/linux/release/weft
SHIM=/work/target/linux/release/libweft_shim.so
CHECK=/work/target/linux/release/chord-check
export WEFT_SHIM="$SHIM"

echo "[build] weft + shim + chord-check (release)…"
cargo build --release -p weft-dst -p weft-shim -p weft-chord 2>&1 | tail -2
echo "[build] chord_node.c…"
cc -O2 -Wall -o /tmp/chord_node examples/chord/chord_node.c

mkdir -p "$OUT"
: > "$OUT/hits.txt"
: > "$OUT/discarded.txt"
: > "$OUT/errors.txt"
echo "[sweep] seeds $SEED_START..$((SEED_START+SEEDS-1)), net=$NET, members=$MEMBERS (base=$BASE), m=$M, ticks=$TICKS, protocol=$LABEL (CHORD_FIX=$CHORD_FIX)"

hits=0; tested=0; errors=0; discarded=0
start_ts=$(date +%s)
for ((s=SEED_START; s<SEED_START+SEEDS; s++)); do
    log="$OUT/seed-$s.weftlog"
    if ! timeout 60 "$WEFT" run --seed "$s" --net "$NET" --nodes "$NNODES" \
        --record "$log" -- /tmp/chord_node "$M" "$TICKS" "$BASE" >/dev/null 2>&1; then
        errors=$((errors+1)); echo "$s run-failed" >> "$OUT/errors.txt"
        rm -f "$log"; continue
    fi
    tested=$((tested+1))
    "$CHECK" "$log" "$M" > "$OUT/seed-$s.verdict" 2>&1
    rc=$?
    if [ "$rc" = "2" ]; then
        hits=$((hits+1)); echo "$s" >> "$OUT/hits.txt"
    elif [ "$rc" = "3" ]; then
        discarded=$((discarded+1)); echo "$s" >> "$OUT/discarded.txt"
        rm -f "$log" "$OUT/seed-$s.verdict"
    else
        [ "$rc" != "0" ] && { errors=$((errors+1)); echo "$s check-rc=$rc" >> "$OUT/errors.txt"; }
        rm -f "$log" "$OUT/seed-$s.verdict"
    fi
done
end_ts=$(date +%s)

echo "[result] tested=$tested violating=$hits assumption-discards=$discarded errors=$errors elapsed=$((end_ts-start_ts))s"
if [ "$hits" -gt 0 ]; then
    echo "[result] violating seeds: $(tr '\n' ' ' < "$OUT/hits.txt")"
    first=$(head -1 "$OUT/hits.txt")
    echo "[result] --- chord-check, first hit (seed $first) ---"
    cat "$OUT/seed-$first.verdict"
fi
