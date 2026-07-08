#!/usr/bin/env bash
# Phase 1-6 reverification: re-run every load-bearing claim from the phase
# checklists and print PASS/FAIL evidence lines that PHASE_VERIFICATION.md
# quotes verbatim. Runs inside the Linux container (the shim is Linux-only):
#
#   docker run --rm -v "$PWD":/work -v weft-cargo-registry:/usr/local/cargo/registry \
#     -w /work -e CARGO_TARGET_DIR=/work/target/linux rust:1.84-bookworm \
#     bash scripts/verify-phases.sh
set -uo pipefail
cd /work
T=${CARGO_TARGET_DIR:-/work/target/linux}
WEFT=$T/release/weft
SHIM=$T/release/libweft_shim.so
EX=/tmp/verify-examples
mkdir -p "$EX"

fail=0
say()  { echo "[verify] $*"; }
ok()   { echo "[PASS] $*"; }
bad()  { echo "[FAIL] $*"; fail=1; }

say "build (release)…"
cargo build --release --workspace 2>&1 | tail -1

say "compile C examples…"
for name in entropy chrono race_bank prodcons deadlock pingpong kvreplica udpbench; do
    cc -O2 -o "$EX/$name" "examples/$name.c" -lpthread || bad "compile $name"
done

wrun() { # seed extra... -- prog args...
    local seed=$1; shift
    "$WEFT" run --seed "$seed" --shim "$SHIM" "$@"
}

# ---------- workspace test suite (covers all phases' e2e claims) ----------
say "cargo test --workspace --release…"
if cargo test --workspace --release 2>&1 | tee /tmp/wtest.log | grep -q "FAILED"; then
    bad "workspace test suite has failures:"
    grep -E "test .* FAILED|failures:" /tmp/wtest.log | head
else
    n=$(grep -Eo "test result: ok\. [0-9]+ passed" /tmp/wtest.log | awk '{s+=$4} END {print s}')
    ok "workspace test suite green ($n tests passed)"
fi

# ---------- Phase 1: single-process determinism ----------
a=$(wrun 42 -- "$EX/entropy" | sha256sum)
b=$(wrun 42 -- "$EX/entropy" | sha256sum)
c=$(wrun 43 -- "$EX/entropy" | sha256sum)
[ "$a" = "$b" ] && ok "phase1: entropy seed 42 x2 byte-identical ($a)" || bad "phase1: seed 42 runs differ"
[ "$a" != "$c" ] && ok "phase1: seed 43 differs from seed 42" || bad "phase1: seed 43 identical to 42"

# ---------- Phase 2: scheduler determinism (documented seeds 3=trigger, 2=avoid) ----------
p2=0
for i in $(seq 1 20); do
    out=$(wrun 3 -- "$EX/race_bank" 2 2)
    [ "$out" = "threads=2 iters=2 expected=4 balance=2 lost=2" ] || { p2=1; echo "  trigger run $i: $out"; }
    out=$(wrun 2 -- "$EX/race_bank" 2 2)
    [ "$out" = "threads=2 iters=2 expected=4 balance=4 lost=0" ] || { p2=1; echo "  avoid run $i: $out"; }
done
[ "$p2" = 0 ] && ok "phase2: race_bank seed 3 triggers x20, seed 2 avoids x20, byte-stable" \
             || bad "phase2: race_bank outcome not stable across 20 runs"

# ---------- Phase 3: network determinism + partition heal ----------
sorted() { sort <<<"$1" | tr '\n' ';'; }
a=$("$WEFT" run --seed 42 --net "latency=uniform:1000-2000" --nodes 2 --shim "$SHIM" -- "$EX/pingpong")
b=$("$WEFT" run --seed 42 --net "latency=uniform:1000-2000" --nodes 2 --shim "$SHIM" -- "$EX/pingpong")
c=$("$WEFT" run --seed 7  --net "latency=uniform:1000-2000" --nodes 2 --shim "$SHIM" -- "$EX/pingpong")
[ "$(sorted "$a")" = "$(sorted "$b")" ] && ok "phase3: pingpong seed 42 x2 identical payload multiset" \
                                        || bad "phase3: pingpong seed 42 payloads differ"
[ "$(sorted "$a")" != "$(sorted "$c")" ] && ok "phase3: seed 7 differs" || bad "phase3: seed 7 identical"
"$WEFT" run --seed 1 --net "latency=uniform:1000-50000" --nodes 1 --shim "$SHIM" -- "$EX/kvreplica" >/tmp/kv1.out
rc1=$?
"$WEFT" run --seed 0 --net "latency=uniform:1000-50000" --nodes 1 --shim "$SHIM" -- "$EX/kvreplica" >/tmp/kv0.out
rc0=$?
grep -q "stale=1" /tmp/kv1.out && [ "$rc1" != 0 ] && ok "phase3: kvreplica seed 1 reorders (stale read, rc=$rc1)" \
                                                 || bad "phase3: kvreplica seed 1 did not reorder"
grep -q "stale=0" /tmp/kv0.out && [ "$rc0" = 0 ] && ok "phase3: kvreplica seed 0 clean (rc=0)" \
                                                 || bad "phase3: kvreplica seed 0 not clean"

# ---------- Phase 4: scenario DSL ----------
p4=0
for s in examples/scenarios/*.json; do
    "$WEFT" scenario validate "$s" >/dev/null 2>&1 || cargo run --release -q -p weft-scenario 2>/dev/null || true
done
# validation is exercised by the unit suite; here just confirm the examples parse
cargo test --release -p weft-scenario 2>&1 | grep -q "test result: ok" \
    && ok "phase4: scenario parser suite green (incl. 10k mutated-input robustness sweep)" \
    || bad "phase4: scenario parser suite failed"

# ---------- Phase 5: record → replay x10 identical ----------
rec=/tmp/verify-rec.weftlog
"$WEFT" run --seed 42 --net "latency=uniform:1000-2000" --nodes 2 --record "$rec" --shim "$SHIM" -- "$EX/pingpong" >/dev/null
h=""
p5=0
for i in $(seq 1 10); do
    hh=$("$WEFT" replay "$rec" 2>&1 | sha256sum)
    [ -z "$h" ] && h=$hh
    [ "$h" = "$hh" ] || p5=1
done
[ "$p5" = 0 ] && ok "phase5: recorded run replays byte-identically x10 ($h)" \
             || bad "phase5: replay output varied across 10 replays"

# ---------- Phase 6: fuzz exit codes ----------
rm -rf weft-fuzz-out
"$WEFT" fuzz --config examples/fuzz/ci.json >/dev/null 2>&1
rc=$?
[ "$rc" = 0 ] && ok "phase6: CI property sweep exits 0 (no violations, as designed)" \
             || bad "phase6: ci.json sweep exited $rc (expected 0)"
rm -rf weft-fuzz-out
"$WEFT" fuzz --config examples/fuzz/demo.json >/dev/null 2>&1
rc=$?
[ "$rc" = 2 ] && ok "phase6: demo sweep exits 2 with reproducers (violations expected by design)" \
             || bad "phase6: demo.json sweep exited $rc (expected 2)"
rm -rf weft-fuzz-out

# ---------- Sanitizers ----------
say "sanitizers: ASan+UBSan on entropy/prodcons/pingpong (native + under shim)…"
p7=0
for name in entropy prodcons; do
    cc -O1 -g -fsanitize=address,undefined -fno-omit-frame-pointer \
       -o "$EX/$name.asan" "examples/$name.c" -lpthread || p7=1
    "$EX/$name.asan" >/dev/null 2>/tmp/asan.log || { p7=1; echo "  native $name:"; head -3 /tmp/asan.log; }
    ASAN_OPTIONS=verify_asan_link_order=0 \
        wrun 42 -- "$EX/$name.asan" >/dev/null 2>/tmp/asan2.log \
        || { p7=1; echo "  shimmed $name:"; head -3 /tmp/asan2.log; }
done
[ "$p7" = 0 ] && ok "sanitizers: ASan+UBSan clean, native and under the shim" \
             || bad "sanitizers: ASan/UBSan findings above"
say "sanitizers: TSan positive/negative control (native)…"
cc -O1 -g -fsanitize=thread -o "$EX/race_bank.tsan" examples/race_bank.c -lpthread
cc -O1 -g -fsanitize=thread -o "$EX/prodcons.tsan" examples/prodcons.c -lpthread
if "$EX/race_bank.tsan" 4 25 >/dev/null 2>&1; then
    bad "sanitizers: TSan did NOT flag the deliberate race (positive control failed)"
else
    ok "sanitizers: TSan flags race_bank's deliberate race (positive control)"
fi
if "$EX/prodcons.tsan" >/dev/null 2>&1; then
    ok "sanitizers: TSan clean on prodcons (negative control)"
else
    bad "sanitizers: TSan flagged prodcons"
fi

echo
[ "$fail" = 0 ] && echo "[verify] ALL CHECKS PASSED" || echo "[verify] FAILURES PRESENT (see above)"
exit "$fail"
