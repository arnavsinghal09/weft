#!/usr/bin/env bash
# Raft ElectionSafety campaign: sweep fault seeds over REAL multi-process
# Raft leader elections through Weft's shim + broker, record each run, and
# scan every state report for two leaders in one term.
#
# Edge case (Ongaro Fig. 3.2): RAFT_FIX=0 makes crash-restart LOSE votedFor
# (breaking the persistence requirement); RAFT_FIX=1 persists it. The same
# seeds should violate under 0 and be safe under 1.
#
# Runs inside the Linux container. Invoked by scripts/raft-campaign.sh.
set -uo pipefail
cd /work

TICKS=${TICKS:-40}
MEMBERS=${MEMBERS:-5}
SEEDS=${SEEDS:-300}
SEED_START=${SEED_START:-0}
# Latency deliberately on the same scale as the election timeout (5-10
# ticks of 1000 µs): replies must land inside the candidacy that asked.
NET=${NET:-latency=uniform:2000-10000}
RAFT_FIX=${RAFT_FIX:-0}
LABEL=$([ "$RAFT_FIX" = "0" ] && echo buggy-volatile-vote || echo fixed-persistent-vote)
OUT=${OUT:-/work/target/raft-out-$LABEL}

NNODES=$((MEMBERS + 1))        # +1 observer
export RAFT_NNODES=$NNODES
export RAFT_FIX

WEFT=/work/target/linux/release/weft
SHIM=/work/target/linux/release/libweft_shim.so
CHECK=/work/target/linux/release/raft-check
export WEFT_SHIM="$SHIM"

echo "[build] weft + shim + raft-check (release)…"
cargo build --release -p weft-dst -p weft-shim -p weft-raft 2>&1 | tail -2
echo "[build] raft_node.c…"
cc -O2 -Wall -o /tmp/raft_node examples/raft/raft_node.c

mkdir -p "$OUT"
: > "$OUT/hits.txt"
: > "$OUT/discarded.txt"
: > "$OUT/errors.txt"
echo "[sweep] seeds $SEED_START..$((SEED_START+SEEDS-1)), net=$NET, members=$MEMBERS, ticks=$TICKS, RAFT_FIX=$RAFT_FIX ($LABEL)"

hits=0; tested=0; errors=0; discarded=0
start_ts=$(date +%s)
for ((s=SEED_START; s<SEED_START+SEEDS; s++)); do
    log="$OUT/seed-$s.weftlog"
    if ! timeout 60 "$WEFT" run --seed "$s" --net "$NET" --nodes "$NNODES" \
        --record "$log" -- /tmp/raft_node "$TICKS" >/dev/null 2>&1; then
        errors=$((errors+1)); echo "$s run-failed" >> "$OUT/errors.txt"
        rm -f "$log"; continue
    fi
    tested=$((tested+1))
    "$CHECK" "$log" > "$OUT/seed-$s.verdict" 2>&1
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

echo "[result] tested=$tested violating=$hits no-leader-discards=$discarded errors=$errors elapsed=$((end_ts-start_ts))s"
if [ "$hits" -gt 0 ]; then
    echo "[result] violating seeds: $(tr '\n' ' ' < "$OUT/hits.txt")"
    first=$(head -1 "$OUT/hits.txt")
    echo "[result] --- raft-check, first hit (seed $first) ---"
    cat "$OUT/seed-$first.verdict"
fi
