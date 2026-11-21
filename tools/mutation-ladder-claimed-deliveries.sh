#!/usr/bin/env bash
# Mutation ladder for test/adoption-claimed-deliveries.test.mjs.
#
# Every pin must go RED when its artifact claim is flipped; the artifact
# must be restored BYTE-IDENTICAL against a PRE-LADDER SNAPSHOT (never git
# HEAD — the artifact may be dirty at ladder time).
#
# Two hard guards against the documented silent failures:
#   (a) a no-op mutation still counted as CAUGHT — every step PRINTS the
#       mutated value and FAILS if the pattern did not apply;
#   (b) the byte-identical check compares against a pre-ladder snapshot.
set -uo pipefail
cd "$(dirname "$0")/.."
ART=research/adoption-claimed-deliveries-2026-11-21.json
SNAP=$(mktemp)
cp "$ART" "$SNAP"
FAILED=0

restore() {
  cp "$SNAP" "$ART"
}

step() { # step <label> <python-mutation-expression> <printed-expression>
  local label="$1" mutation="$2" printed="$3"
  restore
  python3 - "$ART" "$mutation" <<'PY'
import json, sys
path, expr = sys.argv[1], sys.argv[2]
with open(path) as f:
    doc = json.load(f)
exec(expr, {"doc": doc})
with open(path, "w") as f:
    json.dump(doc, f, indent=2, ensure_ascii=False)
    f.write("\n")
PY
  # (a) the mutation must have actually changed a visible value
  if diff -q "$SNAP" "$ART" >/dev/null; then
    echo "FAIL $label: mutation did not apply (no-op) — ladder is void"
    FAILED=1
    return
  fi
  echo "  mutated: $(python3 -c "
import json;d=json.load(open('$ART'))
print(eval('''$printed''', {'d': d}))")"
  if node --test test/adoption-claimed-deliveries.test.mjs >/dev/null 2>&1; then
    echo "FAIL $label: test PASSED against the mutated claim — pin is not falsifiable"
    FAILED=1
  else
    echo "CAUGHT $label"
  fi
  restore
}

# M1: the dead agave delivery is re-forged into a reeflog signal.
step M1 'doc["distributionSurfaces"]["verdict"]["class"] = "foreign-reeflog-reaction"' \
  'd["distributionSurfaces"]["verdict"]["class"]'
# M2: the dead URL is removed from the dead list (the finding disappears).
step M2 'doc["distributionSurfaces"]["claimedDeliveries"]["dead"] = []' \
  'len(d["distributionSurfaces"]["claimedDeliveries"]["dead"])'
# M3: the dead comment is flipped to exists:true (the rot is hidden).
step M3 'exec("[x.__setitem__(\"exists\", True) for x in doc[\"distributionSurfaces\"][\"claimedDeliveries\"][\"deliveries\"] if x[\"repo\"] == \"anza-xyz/agave\"]")' \
  '[x["exists"] for x in d["distributionSurfaces"]["claimedDeliveries"]["deliveries"] if x["repo"] == "anza-xyz/agave"]'
# M4: the alive count is inflated to a perfect census.
step M4 'doc["distributionSurfaces"]["claimedDeliveries"]["alive"] = 6' \
  'd["distributionSurfaces"]["claimedDeliveries"]["alive"]'
# M5: an alive laserstream delivery is flipped dead.
step M5 'doc["distributionSurfaces"]["claimedDeliveries"]["deliveries"][0]["exists"] = False' \
  'd["distributionSurfaces"]["claimedDeliveries"]["deliveries"][0]["exists"]'
# M6: an alive delivery is forged onto a foreign author.
step M6 'doc["distributionSurfaces"]["claimedDeliveries"]["deliveries"][0]["author"] = "someoneelse"' \
  'd["distributionSurfaces"]["claimedDeliveries"]["deliveries"][0]["author"]'
# M7: a row is marked unverifiable while the block still claims ok.
step M7 'doc["distributionSurfaces"]["claimedDeliveries"]["deliveries"][0]["state"] = "unverifiable"' \
  'd["distributionSurfaces"]["claimedDeliveries"]["deliveries"][0]["state"]'
# M8: the measurement is marked incomplete (a verdict on partial evidence).
step M8 'doc["complete"] = False' \
  'd["complete"]'
# M9: the survival rule is replaced by trust-in-memory.
step M9 'doc["distributionSurfaces"]["claimedDeliveries"]["rule"] = "trust previous proofs"' \
  'd["distributionSurfaces"]["claimedDeliveries"]["rule"]'

# (b) byte-identical restore against the PRE-LADDER snapshot
if cmp -s "$SNAP" "$ART"; then
  echo "RESTORE byte-identical"
else
  echo "FAIL: artifact not restored byte-identical"
  FAILED=1
fi
# and the control must still pass on the clean artifact
if node --test test/adoption-claimed-deliveries.test.mjs >/dev/null 2>&1; then
  echo "CONTROL green on the restored artifact"
else
  echo "FAIL: control red on the restored artifact"
  FAILED=1
fi
rm -f "$SNAP"
echo "LADDER RESULT: $([ "$FAILED" -eq 0 ] && echo 9/9 CAUGHT || echo FAILED)"
exit "$FAILED"
