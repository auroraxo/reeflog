#!/usr/bin/env bash
# Mutation ladder for the seal-pass backlog drain.
#
# THE QUESTION: does the measurement actually CATCH a broken fix? A green
# measurement on a tree whose fix was reverted proves nothing. This ladder
# takes the fixed tree, breaks the fix in a scratch copy in several ways,
# and asserts that the measurement tool reports the defect each time.
#
# Each mutation must be CAUGHT (the tool must report a defect face on the
# mutated tree) and the tree must be restored byte-identically afterwards.
set -uo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
TOOL="$ROOT/tools/measure-seal-pass-backlog-drain.mjs"
WORK="$(mktemp -d /tmp/reeflog-ladder-XXXXXX)"
BASE="$WORK/base"
MUT="$WORK/mut"
FAILED=0

cleanup() { rm -rf "$WORK"; }
trap cleanup EXIT

say() { printf '%s\n' "$*"; }

# A pristine copy of the fixed tree.
cp -a "$ROOT" "$BASE"
rm -rf "$BASE/target" "$BASE/.git"
( cd "$BASE" && cargo build --release >/dev/null 2>&1 ) || { say "baseline build failed"; exit 2; }

BASELINE_BIN="${BASELINE_BIN:-/tmp/rl-baseline/target/release/reeflog}"
[ -x "$BASELINE_BIN" ] || { say "missing baseline binary: $BASELINE_BIN"; exit 2; }

# run_case <name> <expected: caught|green> — measures MUT against BASE.
# A mutation is CAUGHT when either the tool reports a defect face OR the
# mutated tree fails a guard: guards are properties the fix must keep
# (e.g. "a dead endpoint never looks like a pass"), so a mutation that
# breaks one of them is exactly what the measurement exists to notice.
run_case() {
  local name="$1" expect="$2"
  local out="$WORK/out-$name.json"
  node "$TOOL" --baseline "$BASE/target/release/reeflog" \
                --fixed "$MUT/target/release/reeflog" \
                --out "$out" >/dev/null 2>&1
  local rc=$?
  local defects guards_failed
  defects="$(node -e 'try{console.log(require(process.argv[1]).fixedDefectFaces)}catch(e){console.log("err")}' "$out")"
  guards_failed="$(node -e 'try{const a=require(process.argv[1]);const g=a.trees.fixed.guards;console.log(Object.values(g).filter(x=>!x.pass).length)}catch(e){console.log("err")}' "$out")"
  if [ "$expect" = "caught" ]; then
    if [ "${defects:-0}" -ge 1 ] 2>/dev/null || [ "${guards_failed:-0}" -ge 1 ] 2>/dev/null; then
      say "CAUGHT  $name (fixedDefectFaces=$defects, guardFailures=$guards_failed, rc=$rc)"
    else
      say "MISSED  $name — the measurement did NOT catch this mutation (fixedDefectFaces=$defects, guardFailures=$guards_failed)"
      FAILED=$((FAILED+1))
    fi
  else
    if [ "${defects:-0}" = "0" ] && [ "${guards_failed:-0}" = "0" ]; then
      say "GREEN   $name (fixedDefectFaces=$defects, guards clean)"
    else
      say "BROKE  $name — fixedDefectFaces=$defects guardFailures=$guards_failed"
      FAILED=$((FAILED+1))
    fi
  fi
}

restore() { rm -rf "$MUT"; cp -a "$BASE" "$MUT"; }

# --- M0: the unmutated tree must be green ---------------------------------
restore
run_case m0_unmutated green

# --- M1: the cursor becomes a window again (the original defect) -----------
restore
python3 - "$MUT/src/main.rs" <<'PY'
import re, sys
p = sys.argv[1]
s = open(p).read()
# Ask for the OLDEST pending rows again: the cursor is defeated by always
# starting from the head.
s = s.replace("let pending = j.unsealed_after(after_seq, 150)?;",
              "let pending = j.unsealed_after(0, 150)?;")
open(p,'w').write(s)
PY
( cd "$MUT" && cargo build --release >/dev/null 2>&1 ) && run_case m1_window_again caught || { say "M1 build failed"; FAILED=$((FAILED+1)); }

# --- M2: the walk stops after one batch ------------------------------------
restore
python3 - "$MUT/src/main.rs" <<'PY'
import sys
p = sys.argv[1]
s = open(p).read()
s = s.replace("fn seal_walk<F: backfill::Fetcher>(\n    j: &Journal,\n    f: &mut F,\n    max_batches: i64,\n) -> Result<Value, String> {",
              "fn seal_walk<F: backfill::Fetcher>(\n    j: &Journal,\n    f: &mut F,\n    max_batches: i64,\n) -> Result<Value, String> {\n    let max_batches = 1;")
open(p,'w').write(s)
PY
( cd "$MUT" && cargo build --release >/dev/null 2>&1 ) && run_case m2_one_batch_only caught || { say "M2 build failed"; FAILED=$((FAILED+1)); }

# --- M3: the pass always claims to be complete -----------------------------
restore
python3 - "$MUT/src/main.rs" <<'PY'
import sys
p = sys.argv[1]
s = open(p).read()
s = s.replace('total["complete"] = Value::Bool(complete);',
              'total["complete"] = Value::Bool(true);')
open(p,'w').write(s)
PY
( cd "$MUT" && cargo build --release >/dev/null 2>&1 ) && run_case m3_always_complete caught || { say "M3 build failed"; FAILED=$((FAILED+1)); }

# --- M4: the two-source rule is weakened (a trap becomes 'missing') --------
# The real weakening: a null from the status source alone is written as
# 'missing' without ever consulting getTransaction — the row the second
# source still serves would be declared unknown to the chain.
restore
python3 - "$MUT/src/main.rs" <<'PY'
import sys
p = sys.argv[1]
s = open(p).read()
old = '''        if st.is_null() {
            // Second source: only a refusal from BOTH proves the chain does
            // not know the transaction.
            let tx = f.call(
                "getTransaction",
                json!([sig, {"encoding": "json", "maxSupportedTransactionVersion": 1, "commitment": "finalized"}]),
            );
            match tx {'''
new = '''        if st.is_null() {
            // MUTATION: one source is enough — never ask the second one.
            let tx: Result<Value, String> = Ok(Value::Null);
            match tx {'''
if old not in s:
    raise SystemExit("M4 anchor not found")
s = s.replace(old, new, 1)
open(p,'w').write(s)
PY
( cd "$MUT" && cargo build --release >/dev/null 2>&1 ) && run_case m4_two_source_weakened caught || { say "M4 build failed"; FAILED=$((FAILED+1)); }

# --- the tree must be restored byte-identically ----------------------------
restore
if diff -r -q "$BASE" "$MUT" >/dev/null 2>&1; then
  say "RESTORED the tree byte-identically"
else
  say "RESTORE FAILED — the tree differs"
  FAILED=$((FAILED+1))
fi

if [ "$FAILED" -eq 0 ]; then
  say "LADDER: all cases behaved as expected"
  exit 0
fi
say "LADDER: $FAILED case(s) misbehaved"
exit 1
