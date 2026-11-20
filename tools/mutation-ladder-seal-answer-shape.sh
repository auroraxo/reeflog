#!/usr/bin/env bash
# Mutation ladder for the seal-pass answer-shape contract.
#
# THE QUESTION: does the new test file actually CATCH a broken fix? A green
# suite on a tree whose fix was reverted proves nothing. This ladder takes
# the fixed tree, breaks the fix in several ways in a scratch copy, runs
# tests/seal_answer_covers_request.rs on the mutated tree, and asserts the
# defect faces fire each time.
#
# Every step PRINTS its mutated line (a silent exec proves nothing) and the
# tree is restored byte-identically against a PRE-LADDER snapshot (not git).
set -uo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
TEST="seal_answer_covers_request"
WORK="$(mktemp -d /tmp/reeflog-ladder-seal-answer-XXXXXX)"
BASE="$WORK/base"
SNAP="$WORK/snap"
MUT="$WORK/mut"
FAILED=0

cleanup() { rm -rf "$WORK"; }
trap cleanup EXIT

say() { printf '%s\n' "$*"; }

# A pristine copy of the fixed tree, built once.
cp -a "$ROOT" "$BASE"
rm -rf "$BASE/target" "$BASE/.git"
( cd "$BASE" && cargo build --release >/dev/null 2>&1 ) || { say "baseline build failed"; exit 2; }

# Pre-ladder snapshot of the LIVE tree: the restore check below compares
# against THIS, never against git HEAD.
rm -rf "$SNAP"
cp -a "$ROOT/src" "$SNAP"

run_case() {
  local name="$1" expect="$2"
  local out
  out="$( cd "$MUT" && cargo test --release --locked --test "$TEST" 2>&1 )"
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

# --- M1: the length guard is deleted (the original defect) ------------------
restore
python3 - "$MUT/src/main.rs" <<'PY'
import re, sys
p = sys.argv[1]
s = open(p).read()
old = '''    if values.len() != pending.len() {
        return Err(format!(
            "getSignatureStatuses: asked {} signature(s), the answer carries {} — a truncated answer is a failed source (nothing sealed, nothing consumed; the rows stay pending for the next pass)",
            pending.len(),
            values.len()
        ));
    }
'''
if old not in s:
    raise SystemExit("M1 anchor not found")
s = s.replace(old, "", 1)
open(p, 'w').write(s)
# PRINT GUARD: the mutated file must show the zip directly followed by the
# loop — the guard is really gone.
head, sep, tail = s.partition("for ((_, sig), st) in pending.iter().zip(values.iter()) {")
shown = (head + sep)[-260:]
print("M1 mutated (tail of pre-loop region):")
print(shown)
if "values.len() != pending.len()" in s:
    raise SystemExit("M1 mutation DID NOT APPLY (guard still present)")
PY
( cd "$MUT" && cargo build --release >/dev/null 2>&1 ) && run_case m1_length_guard_deleted caught || { say "M1 build failed"; FAILED=$((FAILED+1)); }

# --- M2: the guard compares the WRONG count (never fires) -------------------
restore
python3 - "$MUT/src/main.rs" <<'PY'
import sys
p = sys.argv[1]
s = open(p).read()
old = "    if values.len() != pending.len() {"
new = "    if values.len() != usize::MAX {"
if old not in s:
    raise SystemExit("M2 anchor not found")
s = s.replace(old, new, 1)
open(p, 'w').write(s)
line = next(l for l in s.splitlines() if "usize::MAX" in l)
print("M2 mutated line:")
print(line.strip())
if "usize::MAX" not in s:
    raise SystemExit("M2 mutation DID NOT APPLY")
PY
( cd "$MUT" && cargo build --release >/dev/null 2>&1 ) && run_case m2_guard_never_fires caught || { say "M2 build failed"; FAILED=$((FAILED+1)); }

# --- M3: the walk lies about completeness anyway ----------------------------
restore
python3 - "$MUT/src/main.rs" <<'PY'
import sys
p = sys.argv[1]
s = open(p).read()
old = '    total["complete"] = Value::Bool(complete);'
new = '    total["complete"] = Value::Bool(true); let _ = complete;'
if old not in s:
    raise SystemExit("M3 anchor not found")
s = s.replace(old, new, 1)
open(p, 'w').write(s)
line = next(l for l in s.splitlines() if 'Value::Bool(true)' in l)
print("M3 mutated line:")
print(line.strip())
if 'Value::Bool(true); let _ = complete;' not in s:
    raise SystemExit("M3 mutation DID NOT APPLY")
PY
( cd "$MUT" && cargo build --release >/dev/null 2>&1 ) && run_case m3_always_complete caught || { say "M3 build failed"; FAILED=$((FAILED+1)); }

# --- M4: the tick's INCOMPLETE voice is muted -------------------------------
restore
python3 - "$MUT/src/main.rs" <<'PY'
import sys
p = sys.argv[1]
s = open(p).read()
old = '''            if promoted["complete"].as_bool() != Some(true) {
                eprintln!(
                    "reeflog: seal pass INCOMPLETE {promoted} — the rows stay pending; a truncated answer is not a verdict"
                );
            }'''
new = '''            if promoted["complete"].as_bool() != Some(true) {
                let _ = promoted; // MUTATION: the voice is muted
            }'''
if old not in s:
    raise SystemExit("M4 anchor not found")
s = s.replace(old, new, 1)
open(p, 'w').write(s)
line = next(l for l in s.splitlines() if "the voice is muted" in l)
print("M4 mutated line:")
print(line.strip())
if "the voice is muted" not in s:
    raise SystemExit("M4 mutation DID NOT APPLY")
PY
( cd "$MUT" && cargo build --release >/dev/null 2>&1 ) && run_case m4_tick_voice_muted caught || { say "M4 build failed"; FAILED=$((FAILED+1)); }

# --- M5: the CLI drops its own exit-3 half of the same contract -------------
restore
python3 - "$MUT/src/main.rs" <<'PY'
import sys
p = sys.argv[1]
s = open(p).read()
old = '''    if !complete {
        // The pass did not reach the end of the pending set: rows are still
        // UNASKED, and a pass that did not ask everything cannot report
        // itself as done. Same contract as `verify` and `hydrate`: exit 3
        // means "the work is unfinished", not "the command failed".
        eprintln!(
            "reeflog: seal pass INCOMPLETE — the journal still holds rows this pass did not ask"
        );
        std::process::exit(3);
    }'''
new = '''    if !complete {
        let _ = complete; // MUTATION: exit 0 over an unfinished pass
    }'''
if old not in s:
    raise SystemExit("M5 anchor not found")
s = s.replace(old, new, 1)
open(p, 'w').write(s)
line = next(l for l in s.splitlines() if "exit 0 over an unfinished pass" in l)
print("M5 mutated line:")
print(line.strip())
if "exit 0 over an unfinished pass" not in s:
    raise SystemExit("M5 mutation DID NOT APPLY")
PY
( cd "$MUT" && cargo build --release >/dev/null 2>&1 ) && run_case m5_cli_exit3_dropped caught || { say "M5 build failed"; FAILED=$((FAILED+1)); }

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
