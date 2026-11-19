#!/usr/bin/env bash
# Mutation ladder for test/adoption-distribution-surfaces.test.mjs.
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
ART=research/adoption-distribution-surfaces-2026-11-19.json
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
  if node --test test/adoption-distribution-surfaces.test.mjs >/dev/null 2>&1; then
    echo "FAIL $label: test PASSED against the mutated claim — pin is not falsifiable"
    FAILED=1
  else
    echo "CAUGHT $label"
  fi
  restore
}

# M1: the reply is reclassified as reeflog consumption (the exact mistake
# this pin exists to prevent).
step M1 'doc["distributionSurfaces"]["surfaces"][0]["classification"] = "foreign-reeflog-reaction"' \
  'd["distributionSurfaces"]["surfaces"][0]["classification"]'
# M2: the reply is deleted from the artifact.
step M2 'doc["distributionSurfaces"]["surfaces"][0]["nonOwnerRepliesAfter"] = []' \
  'len(d["distributionSurfaces"]["surfaces"][0]["nonOwnerRepliesAfter"])'
# M3: a reeflog mention is fabricated into the reply.
step M3 'doc["distributionSurfaces"]["surfaces"][0]["nonOwnerRepliesAfter"][0]["mentionsReeflog"] = True' \
  'd["distributionSurfaces"]["surfaces"][0]["nonOwnerRepliesAfter"][0]["mentionsReeflog"]'
# M4: the verdict claims reeflog user-one evidence.
step M4 'doc["distributionSurfaces"]["verdict"]["userOneEvidence"] = True' \
  'd["distributionSurfaces"]["verdict"]["userOneEvidence"]'
# M5: the verdict class is flipped to the old repo-scoped class.
step M5 'doc["distributionSurfaces"]["verdict"]["class"] = "not-distributed-not-consumed"' \
  'd["distributionSurfaces"]["verdict"]["class"]'
# M6: a foreign harness star is fabricated (harness adoption leaking in).
step M6 'doc["distributionSurfaces"]["harnessRepo"]["nonOwnerStars"] = ["someone"]' \
  'd["distributionSurfaces"]["harnessRepo"]["nonOwnerStars"]'
# M7: placement #93 is claimed as merged.
step M7 'doc["distributionSurfaces"]["placements"][0]["merged"] = True' \
  'd["distributionSurfaces"]["placements"][0]["merged"]'
# M8: the placement review count is inflated (a review that never happened).
step M8 'doc["distributionSurfaces"]["placements"][0]["reviews"] = 3' \
  'd["distributionSurfaces"]["placements"][0]["reviews"]'
# M9: the measurement is marked incomplete (a verdict on partial evidence).
step M9 'doc["complete"] = False' \
  'd["complete"]'
# M10: the classification reason that names the measured content is removed.
step M10 'doc["distributionSurfaces"]["surfaces"][0]["reasons"] = [r for r in doc["distributionSurfaces"]["surfaces"][0]["reasons"] if not r.startswith("content-engagement")]' \
  'len(d["distributionSurfaces"]["surfaces"][0]["reasons"])'

# (b) byte-identical restore against the PRE-LADDER snapshot
if cmp -s "$SNAP" "$ART"; then
  echo "RESTORE byte-identical"
else
  echo "FAIL: artifact not restored byte-identical"
  FAILED=1
fi
# and the control must still pass on the clean artifact
if node --test test/adoption-distribution-surfaces.test.mjs >/dev/null 2>&1; then
  echo "CONTROL green on the restored artifact"
else
  echo "FAIL: control red on the restored artifact"
  FAILED=1
fi
rm -f "$SNAP"
echo "LADDER RESULT: $([ "$FAILED" -eq 0 ] && echo 10/10 CAUGHT || echo FAILED)"
exit "$FAILED"
