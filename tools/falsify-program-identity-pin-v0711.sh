#!/usr/bin/env bash
# Falsify the v0.7.11 published-asset pin: tamper the artifact in two ways
# (a wrong number, a machine path), expect RED both times, restore the
# artifact byte-identically, expect GREEN again.
set -uo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."

ART="research/program-identity-published-v0.7.11-2026-11-10.json"
TEST="program_identity_published_v0711"
BACKUP="/tmp/pi-pin-backup-$$.json"

cp "$ART" "$BACKUP"

echo "=== baseline: green ==="
cargo test --release --locked --test "$TEST" 2>&1 | grep -E "test result"

echo "=== falsification 1: fixedViolations 0 -> 1 (must be RED) ==="
sed -i 's/"fixedViolations": 0/"fixedViolations": 1/' "$ART"
cargo test --release --locked --test "$TEST" 2>&1 | grep -E "^test .*(ok|FAILED)|test result" | head -6

echo "=== falsification 2: machine path injected (must be RED) ==="
cp "$BACKUP" "$ART"
python3 - "$ART" <<'PY'
import json,sys
p=sys.argv[1]
d=json.load(open(p))
d["method"]="produced at /home/aurora/reeflog: "+d["method"]
json.dump(d,open(p,"w"),indent=2)
PY
cargo test --release --locked --test "$TEST" 2>&1 | grep -E "^test .*(ok|FAILED)|test result" | head -6

echo "=== restore (byte-identical) and re-run (must be GREEN) ==="
cp "$BACKUP" "$ART"
cmp "$BACKUP" "$ART" && echo "artifact restored byte-identically"
cargo test --release --locked --test "$TEST" 2>&1 | grep -E "test result"
rm -f "$BACKUP"
