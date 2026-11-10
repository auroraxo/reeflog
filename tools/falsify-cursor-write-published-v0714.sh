#!/usr/bin/env bash
# Falsifies the published-v0.7.14 pin (tests/cursor_write_durability_published_v0714.rs)
# three ways. A pin that cannot be made red certifies nothing.
#
#   F1  the headline count is falsified (baselineShowsDefect 4 -> 3):
#       the pin must go RED, because the published v0.7.13 asset showed
#       all four faces;
#   F2  a machine path is smuggled into the artifact: the pin must go RED;
#   F3  the artifact claims the fixed side was "this tree" instead of the
#       published asset: the pin must go RED — a measurement of the build
#       is not a measurement of the release a stranger installs.
#
# The tree is restored BYTE-IDENTICALLY from a copy taken BEFORE the first
# mutation (never `git checkout` — that is how an earlier ladder destroyed
# the very fix it was supposed to prove). After every falsification the pin
# must be green again on the restored tree, else this script exits non-zero.
set -euo pipefail
cd "$(dirname "$0")/.."

PIN="tests/cursor_write_durability_published_v0714.rs"
ART="research/cursor-write-durability-published-v0.7.14-2026-11-10.json"
FILTER="the_"

backup="$(mktemp -d /tmp/reeflog-falsify-cw14.XXXXXX)"
cp "$PIN" "$backup/"
cp "$ART" "$backup/"
trap 'rm -rf "$backup"' EXIT

fail() { echo "FALSIFICATION HARNESS FAILED: $*" >&2; exit 1; }

run_pin() { # $1 = label; fails the script if cargo itself errors
  local label="$1" log
  log="$(mktemp /tmp/reeflog-falsify-pin.XXXXXX)"
  if ! cargo test --release --locked --test cursor_write_durability_published_v0714 >"$log" 2>&1; then
    if grep -q "test result: FAILED" "$log"; then return 1; fi
    cat "$log" >&2
    fail "$label: the pin did not RUN (compile error or missing artifact is not a falsification)"
  fi
  return 0
}

require_red() { # $1 = label
  run_pin "$1" && fail "$1: pin stayed GREEN — the falsification was vacuous"
  echo "$1: pin RED as required"
}

require_green() { # $1 = label
  run_pin "$1" || fail "$1: pin is RED on the restored tree"
  echo "$1: pin green"
}

require_green "0 sanity (unmutated tree)"

# F1 — the count lies: the baseline showed 4 defect faces, not 3.
sed -i 's/"baselineShowsDefect": 4/"baselineShowsDefect": 3/' "$ART"
require_red "F1 falsified count (4 -> 3)"
cp "$backup/$(basename "$ART")" "$ART"
require_green "F1 restore"

# F2 — a machine path smuggled into the committed artifact.
sed -i 's|"tag": "v0.7.14"|"tag": "v0.7.14 measured on /home/quiet-sandbox"|' "$ART"
require_red "F2 host path smuggled"
cp "$backup/$(basename "$ART")" "$ART"
require_green "F2 restore"

# F3 — the fixed seat claims the build, not the published asset.
python3 - "$ART" <<'PY'
import sys, json
p = sys.argv[1]
a = json.load(open(p))
a["fixed"] = {"binary": "this tree, target/release/reeflog"}
json.dump(a, open(p, "w"), indent=2)
open(p, "a").write("\n")
PY
require_red "F3 fixed seat repointed at the branch build"
cp "$backup/$(basename "$ART")" "$ART"
require_green "F3 restore"

# byte-identical close-out
cmp "$PIN" "$backup/$(basename "$PIN")" || fail "pin test not restored byte-identically"
cmp "$ART" "$backup/$(basename "$ART")" || fail "artifact not restored byte-identically"
echo "all 3 falsifications caught; tree byte-identical; pin green"
