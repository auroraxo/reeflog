#!/usr/bin/env bash
# Mutation ladder for the bootstrap() hang fix (PULS U, 2026-11-11).
#
# Each mutation breaks the fix in ONE way; the pins must turn RED. A mutation
# the pins do NOT catch is a vacuous ladder — the ladder fails loudly then.
#
# Design rules learned the hard way:
#  - ANCHORED: every mutation compares sha256 before/after; a no-op sed that
#    leaves the tree unchanged would make a green gate prove nothing.
#  - RESTORE from a FILE COPY, never `git checkout` (that would revert the fix
#    itself and restore the defect instead).
#  - A HANG IS A CATCH. An early version ran the behavioral faces per
#    mutation: a mutation that disables the timeout makes those faces HANG,
#    which stalled the ladder forever instead of reporting CAUGHT. Here the
#    per-run is under `timeout` and any non-zero outcome (fail or hang-kill)
#    counts as a catch — the run must produce a PASS/FAIL verdict line,
#    otherwise it is treated as no-verdict = caught.
#  - The cheap pins (T4/T5 contract asserts on the exported constants and the
#    destroy calls) catch the constants/timer mutations deterministically and
#    fast; the ladder runs exactly those.

set -uo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT" || exit 1

CLIENT="index.js"
WORK="$(mktemp -d)"
BACKUP="$WORK/index.js.bak"
cp "$CLIENT" "$BACKUP"

CAUGHT=0
TOTAL=0

restore() { cp "$BACKUP" "$CLIENT"; }
trap restore EXIT

# mutate <name> <expect yes|no> <sed args...>
mutate() {
  local name="$1" expect="$2"; shift 2
  TOTAL=$((TOTAL + 1))
  local before after
  before="$(sha256sum "$CLIENT" | cut -d' ' -f1)"
  sed -i "$@" "$CLIENT" || { echo "MUTATION-SETUP-FAILED: $name"; exit 1; }
  after="$(sha256sum "$CLIENT" | cut -d' ' -f1)"
  if [ "$before" = "$after" ]; then
    echo "VACUUM: mutation '$name' did not change $CLIENT"
    exit 1
  fi

  # Contract faces only — each under a hard wall-clock cap.
  local out rc
  out="$(timeout 90 node --test \
    --test-name-pattern='T4|T5' \
    "$ROOT/test/bootstrap-timeout.test.mjs" 2>&1)"
  rc=$?
  restore

  local failed
  failed="$(printf '%s' "$out" | sed -n 's/.*ℹ fail \([0-9]*\).*/\1/p' | head -1)"
  if [ "$rc" -ne 0 ] || { [ -n "$failed" ] && [ "$failed" -gt 0 ]; }; then
    CAUGHT=$((CAUGHT + 1))
    local how="fail=$failed"
    [ "$rc" -eq 124 ] && how="HANG (runner killed at 90 s)"
    echo "CAUGHT: $name ($how)"
  else
    if [ "$expect" = "yes" ]; then
      echo "ESCAPED: $name — pins stayed GREEN on a broken fix"
      printf '%s\n' "$out" | grep -E "^ℹ (tests|pass|fail)" | head -3
      exit 1
    fi
    echo "ESCAPED (expected): $name"
  fi
}

# M1: the default budget disappears (the published-tree defect: no timer).
mutate "no-timeout-at-all" yes 's/const DEFAULT_GET_TIMEOUT_MS = 30_000;/const DEFAULT_GET_TIMEOUT_MS = 0;/'

# M2: the redirect bound is loosened beyond any sane chain.
mutate "redirect-bound-raised" yes 's/const MAX_REDIRECTS = 5;/const MAX_REDIRECTS = 100000;/'

# M3: headers arrive but the headers-phase timer is NOT disarmed — it races
# the byte timer and mislabels every byte-stall as a headers-stall.
mutate "headers-timer-not-disarmed" yes 's/req.setTimeout(0);/req.setTimeout(timeoutMs);/'

# M4: the byte-stall verdict stops destroying the socket — abandoned requests
# pin the process open past their own answer.
mutate "byte-stall-no-destroy" yes 's/^          res\.destroy();$/          \/\/ destroy removed (mutation)/'

# M5: the headers-stall verdict stops destroying the request socket.
mutate "headers-stall-no-destroy" yes 's/^      req\.destroy();$/      \/\/ destroy removed (mutation)/'

# M6 (GUARD, expect ESCAPED): a harmless-looking edit the contract must NOT
# over-react to — changing only a comment must keep the pins GREEN.
mutate "comment-only-edit" no 's/not a total one/not a total one at all/'

restore
if [ "$(sha256sum "$CLIENT" | cut -d' ' -f1)" != "$(sha256sum "$BACKUP" | cut -d' ' -f1)" ]; then
  echo "RESTORE-MISMATCH: tree not byte-identical after the ladder"
  exit 1
fi

EXPECTED_CAUGHT=5
echo "LADDER: $CAUGHT/$TOTAL caught (M1-M6; M6 is a guard expected to escape), tree byte-identical"
[ "$CAUGHT" -eq "$EXPECTED_CAUGHT" ] || exit 1
