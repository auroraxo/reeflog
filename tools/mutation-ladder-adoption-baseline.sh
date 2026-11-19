#!/usr/bin/env bash
# Mutation ladder for test/adoption-npm-attribution.test.mjs.
# Every pin must go RED when its artifact claim is flipped; the artifact
# must be restored BYTE-IDENTICAL against a PRE-LADDER SNAPSHOT (never git
# HEAD — the artifact may be dirty at ladder time).
#
# Two hard guards against the documented silent failures:
#   (a) a no-op mutation still counted as CAUGHT — every step PRINTS the
#       mutated value and FAILS if the pattern did not apply;
#   (b) the byte-identical check compares against a pre-ladder snapshot,
#       not git HEAD.
set -uo pipefail
cd "$(dirname "$0")/.."
ART=research/adoption-baseline-2026-11-19.json
SNAP=$(mktemp)
cp "$ART" "$SNAP"
FAILED=0

restore() {
  cp "$SNAP" "$ART"
}

step() { # step <label> <python-mutation-expression>
  local label="$1" mutation="$2"
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
print({'M1':d['checks']['npmDownloads']['countsAsAdoption'],
       'M2':d['verdict']['class'],
       'M3':len(d['checks']['npmDownloads']['ownerCiRunsSameWeek']),
       'M4':d['checks']['npmDownloads']['downloadsBeforeFirstPublish'],
       'M5':d['checks']['npmDownloads']['lastWeek']['downloads'],
       'M6':d['complete']}['$label'])")"
  if node --test test/adoption-npm-attribution.test.mjs >/dev/null 2>&1; then
    echo "FAIL $label: test PASSED against the mutated claim — pin is not falsifiable"
    FAILED=1
  else
    echo "CAUGHT $label"
  fi
  restore
}

step M1 'doc["checks"]["npmDownloads"]["countsAsAdoption"] = True'
step M2 'doc["verdict"]["class"] = "user-one-evidence"'
step M3 'doc["checks"]["npmDownloads"]["ownerCiRunsSameWeek"] = []'
step M4 'doc["checks"]["npmDownloads"]["downloadsBeforeFirstPublish"] = 5'
step M5 'doc["checks"]["npmDownloads"]["lastWeek"]["downloads"] = 999999'
step M6 'doc["complete"] = False'

# (b) byte-identical restore against the PRE-LADDER snapshot
if cmp -s "$SNAP" "$ART"; then
  echo "RESTORE byte-identical"
else
  echo "FAIL: artifact not restored byte-identical"
  FAILED=1
fi
# and the control must still pass on the clean artifact
if node --test test/adoption-npm-attribution.test.mjs >/dev/null 2>&1; then
  echo "CONTROL green on the restored artifact"
else
  echo "FAIL: control red on the restored artifact"
  FAILED=1
fi
rm -f "$SNAP"
echo "LADDER RESULT: $([ "$FAILED" -eq 0 ] && echo 6/6 CAUGHT || echo FAILED)"
exit "$FAILED"
