#!/usr/bin/env bash
# Mutation ladder for the webhook PASS-BUDGET contract (PULS BF).
#
# THE QUESTION: does the new test layer actually CATCH a broken fix? A green
# suite on a tree whose fix was reverted proves nothing. This ladder takes
# the fixed tree, breaks the fix in several ways in a scratch copy, runs the
# webhook unit tests (in `src/webhook.rs`) AND the live-daemon regression
# (`tests/webhook_pass_budget.rs`) on the mutated tree, and asserts the
# defect faces fire each time.
#
# Every step PRINTS its mutated line (a silent exec proves nothing) and the
# tree is restored byte-identically against a PRE-LADDER snapshot (not git).
set -uo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
WORK="$(mktemp -d /tmp/reeflog-ladder-webhook-budget-XXXXXX)"
BASE="$WORK/base"
SNAP="$WORK/snap"
MUT="$WORK/mut"
FAILED=0

cleanup() { rm -rf "$WORK"; }
trap cleanup EXIT

say() { printf '%s\n' "$*"; }

# A pristine copy of the fixed tree, built once. The copy EXCLUDES the
# build dirs: `cp -a` of a full cargo target/ blew past the disk (measured
# 2026-11-21 — the host runs at ~96% full), and a scratch tree does not
# need it: the baseline build below creates a fresh one.
mkdir -p "$BASE"
tar -C "$ROOT" --exclude=./target --exclude=./.git -cf - . | tar -C "$BASE" -xf -
( cd "$BASE" && cargo build --release >/dev/null 2>&1 ) || { say "baseline build failed"; exit 2; }

# Pre-ladder snapshot of the LIVE tree: the restore check below compares
# against THIS, never against git HEAD.
rm -rf "$SNAP"
cp -a "$ROOT/src" "$SNAP"

run_case() {
  local name="$1" expect="$2"
  local out
  out="$( cd "$MUT" && cargo test --release --locked --bin reeflog webhook:: 2>&1; cargo test --release --locked --test webhook_pass_budget 2>&1 )"
  local rc=$?
  local failed_tests red green
  failed_tests="$(printf '%s\n' "$out" | grep -c '^test .* FAILED$' || true)"
  red="$(printf '%s\n' "$out" | grep -c 'test result: FAILED' || true)"
  green="$(printf '%s\n' "$out" | grep -c 'test result: ok' || true)"
  if [ "$expect" = "caught" ]; then
    if [ "${red:-0}" -ge 1 ] && [ "${failed_tests:-0}" -ge 1 ]; then
      say "CAUGHT  $name (failing_tests=$failed_tests, red_runs=$red)"
    else
      say "MISSED  $name — the suite did NOT catch this mutation (failing_tests=$failed_tests, red_runs=$red, rc=$rc)"
      FAILED=$((FAILED+1))
    fi
  else
    if [ "${red:-0}" = "0" ] && [ "${green:-0}" -ge 1 ]; then
      say "GREEN   $name (red_runs=$red, green_runs=$green)"
    else
      say "BROKE   $name — red_runs=$red green_runs=$green"
      FAILED=$((FAILED+1))
    fi
  fi
}

restore() { rm -rf "$MUT"; cp -a "$BASE" "$MUT"; }

# --- M0: the unmutated tree must be green ----------------------------------
restore
run_case m0_unmutated green

# --- M1: the budget check is deleted (the original defect) ------------------
restore
python3 - "$MUT/src/webhook.rs" <<'PY'
import sys
p = sys.argv[1]
s = open(p).read()
old = """        if budget.is_some_and(|b| Instant::now() >= b) {
            budget_stopped = true;
            break;
        }
        // The payload debt rule"""
new = """        // The payload debt rule"""
if old not in s:
    raise SystemExit("M1 anchor not found")
s = s.replace(old, new, 1)
open(p, 'w').write(s)
head, sep, tail = s.partition("// The payload debt rule")
shown = (head + sep)[-240:]
print("M1 mutated (tail before the debt rule):")
print(shown)
if s.count("budget_stopped = true") != 3:
    raise SystemExit("M1 mutation DID NOT APPLY (check count)")
PY
( cd "$MUT" && cargo build --release >/dev/null 2>&1 ) && run_case m1_event_budget_check_deleted caught || { say "M1 build failed"; FAILED=$((FAILED+1)); }

# --- M2: a time-slice is mislabeled as a BLOCK (exit-voice defect) ----------
restore
python3 - "$MUT/src/webhook.rs" <<'PY'
import sys
p = sys.argv[1]
s = open(p).read()
old = """                if budget.is_some_and(|b| Instant::now() >= b) {
                    budget_stopped = true;
                    break;
                }
                Attempt::Transient(e)"""
new = """                if budget.is_some_and(|b| Instant::now() >= b) {
                    rep.blocked_seq = Some(r.seq);
                    rep.blocked_error = Some("pass budget spent".into());
                    break;
                }
                Attempt::Transient(e)"""
if old not in s:
    raise SystemExit("M2 anchor not found")
s = s.replace(old, new, 1)  # first occurrence = the EVENT pass
open(p, 'w').write(s)
line = next(l for l in s.splitlines() if '"pass budget spent"' in l)
print("M2 mutated line:")
print(line.strip())
if s.count('"pass budget spent"') != 1:
    raise SystemExit("M2 mutation DID NOT APPLY")
PY
( cd "$MUT" && cargo build --release >/dev/null 2>&1 ) && run_case m2_slice_mislabeled_as_block caught || { say "M2 build failed"; FAILED=$((FAILED+1)); }

# --- M3: the owed rows are not counted (the slice goes silent) --------------
restore
python3 - "$MUT/src/webhook.rs" <<'PY'
import sys
p = sys.argv[1]
s = open(p).read()
old = "        rep.events_owed = (rows.len() as i64 - examined).max(0) + rows_behind;"
new = "        rep.events_owed = 0; let _ = (examined, rows_behind);"
if old not in s:
    raise SystemExit("M3 anchor not found")
s = s.replace(old, new, 1)
open(p, 'w').write(s)
line = next(l for l in s.splitlines() if "let _ = (examined, rows_behind)" in l)
print("M3 mutated line:")
print(line.strip())
if "rep.events_owed = 0;" not in s:
    raise SystemExit("M3 mutation DID NOT APPLY")
PY
( cd "$MUT" && cargo build --release >/dev/null 2>&1 ) && run_case m3_owed_rows_not_counted caught || { say "M3 build failed"; FAILED=$((FAILED+1)); }

# --- M4: the in-flight slice lie is deleted (budget stop lost) --------------
restore
python3 - "$MUT/src/webhook.rs" <<'PY'
import sys
p = sys.argv[1]
s = open(p).read()
old = """                if budget.is_some_and(|b| Instant::now() >= b) {
                    budget_stopped = true;
                    break;
                }
                Attempt::Transient(e)"""
new = """                Attempt::Transient(e)"""
if old not in s:
    raise SystemExit("M4 anchor not found")
s = s.replace(old, new, 1)  # first occurrence = the EVENT pass
open(p, 'w').write(s)
print("M4 mutated: the event pass's in-flight interception is gone")
print("(the in-flight POST now lands as a transient failure and blocks the queue)")
if s.count("budget_stopped = true") != 3:
    raise SystemExit("M4 mutation DID NOT APPLY (check count)")
PY
( cd "$MUT" && cargo build --release >/dev/null 2>&1 ) && run_case m4_inflight_interception_deleted caught || { say "M4 build failed"; FAILED=$((FAILED+1)); }

# --- M5: the summary mutes the slice (budget_stops/rows_owed lie) -----------
restore
python3 - "$MUT/src/main.rs" <<'PY'
import sys
p = sys.argv[1]
s = open(p).read()
old = """            "budget_stops": webhook_budget_stops,
            "rows_owed": webhook_rows_owed,"""
new = """            "budget_stops": 0,
            "rows_owed": 0,"""
if old not in s:
    raise SystemExit("M5 anchor not found")
s = s.replace(old, new, 1)
open(p, 'w').write(s)
line = next(l for l in s.splitlines() if '"budget_stops": 0' in l)
print("M5 mutated line:")
print(line.strip())
if '"rows_owed": 0' not in s:
    raise SystemExit("M5 mutation DID NOT APPLY")
PY
( cd "$MUT" && cargo build --release >/dev/null 2>&1 ) && run_case m5_summary_mutes_the_slice caught || { say "M5 build failed"; FAILED=$((FAILED+1)); }

# --- the LIVE tree must be restored byte-identically ------------------------
restore
rm -rf "$MUT"
cp -a "$SNAP" "$MUT"
if diff -r -q "$SNAP" "$ROOT/src" >/dev/null 2>&1; then
  say "RESTORED the live tree byte-identically (vs pre-ladder snapshot)"
else
  say "RESTORE FAILED — the live tree differs from the pre-ladder snapshot"
  FAILED=$((FAILED+1))
fi

if [ "$FAILED" -eq 0 ]; then
  say "LADDER: all cases behaved as expected"
  exit 0
fi
say "LADDER: $FAILED case(s) misbehaved"
exit 1
