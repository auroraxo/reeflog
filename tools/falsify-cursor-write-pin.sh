#!/usr/bin/env bash
# Falsify the cursor-write-durability pin three ways, restoring the tree
# byte-identically after each run. The ladder's own rule: a "green" pin that
# cannot be made red is decoration, not evidence.
#
# Usage: bash tools/falsify-cursor-write-pin.sh
set -euo pipefail
cd "$(dirname "$0")/.."

ART="research/cursor-write-durability-2026-11-10.json"
PIN="tests/cursor_write_durability_measurement.rs"
TEST="cargo test --release --locked --test cursor_write_durability_measurement"

restore() { cp "$STASH/$1" "$1"; }
STASH=$(mktemp -d)
trap 'rm -rf "$STASH"' EXIT

expect_red() {
  if "$TEST" >/tmp/reeflog-falsify.log 2>&1; then
    echo "FALSIFICATION $1 FAILED: pin stayed GREEN against the corrupted artifact"; exit 1
  else
    echo "FALSIFICATION $1: pin RED as required"
  fi
}

for f in "$ART" "$PIN"; do cp "$f" "$STASH/$(basename "$f")"; done
cp src/main.rs "$STASH/main.rs"

# --- F1: a falsified summary number must turn the pin red
python3 - "$ART" <<'PY'
import json, sys
p = sys.argv[1]
a = json.load(open(p))
a["summary"]["baselineShowsDefect"] = 3
json.dump(a, open(p, "w"), indent=2)
PY
expect_red F1
restore "$ART"

# --- F2: a host path smuggled into the artifact must turn the pin red
python3 - "$ART" <<'PY'
import json, sys
p = sys.argv[1]
a = json.load(open(p))
a["fixed"]["binary"] = "/home/someone/else/reeflog"
json.dump(a, open(p, "w"), indent=2)
PY
expect_red F2
restore "$ART"

# --- F3: the pin losing its REAL digest must turn the pin red (source on a
# branch instead of the release digest = the pin-lag defect family)
sed -i 's/4a0355f9536e30217b3b0ab2cb1510df1ab1583eafe0e0e8fa0c1cc577399eb6/0000000000000000000000000000000000000000000000000000000000000000/' "$PIN"
expect_red F3
restore "$PIN"

# --- integrity: tree byte-identical, then pin green again
if [ -n "$(git status --porcelain -- "$ART" "$PIN" src/main.rs)" ]; then
  echo "RESTORE FAILED: tree differs from HEAD"; git status --porcelain; exit 1
fi
"$TEST" >/tmp/reeflog-falsify-final.log 2>&1 || { echo "FINAL PIN RUN NOT GREEN"; tail -20 /tmp/reeflog-falsify-final.log; exit 1; }
echo "all 3 falsifications caught; tree byte-identical; pin green"
