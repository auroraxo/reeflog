#!/usr/bin/env bash
# Mutation ladder for the captive-portal verdict fix (PULS V, 2026-11-11).
#
# Each mutation breaks the fix in ONE way; the pins must turn RED. A mutation
# the pins do NOT catch is a vacuous ladder — the ladder fails loudly then.
#
# Rules learned the hard way (PULS L/U/T):
#  - ANCHORED: sha256 before/after every mutation; a no-op sed leaves the
#    tree unchanged and would make a green gate prove nothing -> VACUUM.
#  - RESTORE from a FILE COPY, never `git checkout` (that would revert the
#    fix itself and reinstate the defect).
#  - CONTROL: the unmutated tree must PASS first, otherwise the ladder
#    proves nothing at all.
#  - Every mutation is run through the cheap deterministic pins (the portal
#    domain tests + the artifact pins); non-zero (incl. timeout kill) counts
#    as caught.

set -uo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT" || exit 1

CLIENT="index.js"
WORK="$(mktemp -d)"
BACKUP="$WORK/index.js.bak"
cp "$CLIENT" "$BACKUP"

PINS=(test/bootstrap-portal.test.mjs test/captive-portal-measurement.test.mjs)

CAUGHT=0
TOTAL=0

restore() { cp "$BACKUP" "$CLIENT"; }
trap restore EXIT

run_pins() {
  timeout 120 node --test "${PINS[@]}" >/dev/null 2>&1
}

mutate() {
  local name="$1"; shift
  TOTAL=$((TOTAL + 1))
  local before after
  before="$(sha256sum "$CLIENT" | cut -d' ' -f1)"
  sed -i "$@" "$CLIENT" || { echo "MUTATION-SETUP-FAILED: $name"; exit 1; }
  after="$(sha256sum "$CLIENT" | cut -d' ' -f1)"
  if [ "$before" = "$after" ]; then
    echo "VACUUM: mutation '$name' did not change $CLIENT"
    exit 1
  fi
  local rc=0
  run_pins || rc=$?
  if [ "$rc" -ne 0 ]; then
    echo "CAUGHT: $name"
    CAUGHT=$((CAUGHT + 1))
  else
    echo "ESCAPED: $name (pins stayed green with the fix broken)"
  fi
  restore
}

# --- CONTROL: unmutated tree passes ---------------------------------------
if ! run_pins; then
  echo "CONTROL-FAILED: pins are red on the unmutated tree — ladder proves nothing"
  exit 1
fi
echo "CONTROL: green"

# M1: the HTML class name is dropped — the verdict degenerates into a
#     generic parse failure (the captive portal becomes invisible).
mutate "M1 html-class-name-removed" \
  -e "s/it answered with HTML (a captive portal or an intercepting proxy fits this shape)/it answered something that is not an object/"

# M2: the URL is dropped — a verdict a stranger cannot act on.
mutate "M2 url-dropped" \
  -e 's|`the release API at ${releaseUrl} did not answer JSON — `|`the release API did not answer JSON — `|'

# M3: the shape guard's assets check is disabled — the rate-limit face
#     regresses to "release has no asset", the baseline defect verbatim.
mutate "M3 shape-guard-assets-check-disabled" \
  -e 's/|| !Array\.isArray(release\.assets))/|| false)/'

# M4: the endpoint's own words are dropped from the shape verdict —
#     "rate limit exceeded" no longer reaches the stranger.
mutate "M4 api-words-dropped" \
  -e 's/its own message: ${release\.message}/its own message: (undisclosed)/'

# M5: over-tightening — the shape guard now also fires on a well-formed
#     release whose assets array is EMPTY, which must keep the honest
#     "release has no asset named" message (G2). The pins must catch this
#     false-positive turn.
mutate "M5 shape-guard-eats-empty-assets" \
  -e 's/|| !Array\.isArray(release\.assets))/|| (release.assets || []).length === 0)/'

echo "LADDER: ${CAUGHT}/${TOTAL} caught (all 5 must be caught)"
[ "$CAUGHT" -eq "$TOTAL" ] || exit 1

restore
trap - EXIT
if ! cmp -s "$CLIENT" "$BACKUP"; then
  echo "RESTORE-FAILED: $CLIENT is not byte-identical after the ladder"
  exit 1
fi
echo "TREE byte-identical"
rm -rf "$WORK"
