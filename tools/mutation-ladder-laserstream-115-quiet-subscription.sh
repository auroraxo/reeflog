#!/usr/bin/env bash
# Mutation ladder for the LaserStream #115 quiet-subscription pin
# (tests/laserstream_115_quiet_subscription.rs).
#
# Falsification-first: each mutation weakens ONE claim the pin rests on and
# asserts RED. If the test stays green under the mutation, the test is
# vacuous and the ladder fails instead of reporting a CAUGHT.
#
#   1. DESTROYER: pristine bytes are copied aside BEFORE the first mutation
#      and restored by copy on any exit (git never restores a working-tree
#      file mid-ladder; a trap restores on any exit).
#   2. VACUOUS CATCHES: every test filter is PROVEN to match at least one
#      real test BEFORE mutating, or the ladder fails loudly.
#   3. APPLY GUARD: every mutation PRINTS the value it changed and is applied
#      BY KEY (rule + window + options), never by blind text replacement —
#      a key that does not exist must fail loudly, never silently.
#   4. The restore check compares against the PRE-LADDER snapshot, never
#      git HEAD (a dirty tree makes HEAD the wrong reference).
#
#   ./tools/mutation-ladder-laserstream-115-quiet-subscription.sh
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO"

TEST="laserstream_115_quiet_subscription"
SUMMARY="research/laserstream-115-quiet-subscription-matrix-2026-11-20.json"

echo "=== laserstream 115 quiet-subscription mutation ladder ==="

echo -n "unmutated tree ... "
if cargo test --release --test "$TEST" > /dev/null 2>&1; then
    echo "OK (green)"
else
    echo "FAILED: unmutated tree does not pass" >&2
    exit 1
fi

LIST="$(cargo test --release --test "$TEST" -- --list 2>/dev/null | awk -F': ' '/: test$/{print $1}')"

assert_filter_matches() {
    local filter="$1"
    if ! printf '%s\n' "$LIST" | grep -q "^${filter}$"; then
        echo "" >&2
        echo "LADDER BUG: filter \`$filter\` matches no test in tests/$TEST.rs — a CAUGHT verdict would be vacuous" >&2
        printf '%s\n' "$LIST" | sed 's/^/    available: /' >&2
        exit 1
    fi
}

PRISTINE="$(mktemp -d /tmp/ladder-pristine-quiet.XXXXXX)"
cp "$SUMMARY" "$PRISTINE/"
restore_all() {
    if [ -f "$PRISTINE/$(basename "$SUMMARY")" ]; then
        cp "$PRISTINE/$(basename "$SUMMARY")" "$SUMMARY"
        touch "$SUMMARY"
    fi
}
trap restore_all EXIT

# mutate_cell <name> <test-filter> <rule> <window> <options> <key> <new-json-value>
mutate_cell() {
    local name="$1"
    local test_fn="$2"
    local rule="$3"
    local window="$4"
    local options="$5"
    local key="$6"
    local value="$7"

    assert_filter_matches "$test_fn"

    echo -n "mutation: $name ... "
    local printed
    printed="$(python3 - "$SUMMARY" "$rule" "$window" "$options" "$key" "$value" <<'PY'
import json, sys
path, rule, window, options, key, value = sys.argv[1:7]
with open(path) as f:
    doc = json.load(f)
hits = [c for c in doc["cells"] if c["rule"] == rule and c["window"] == window and c["consumer_options"] == options]
if len(hits) != 1:
    print(f"ERROR: expected exactly one cell for ({rule},{window},{options}), found {len(hits)}", file=sys.stderr)
    sys.exit(2)
cell = hits[0]
if key not in cell:
    print(f"ERROR: cell ({rule},{window},{options}) has no key {key}", file=sys.stderr)
    sys.exit(2)
before = cell[key]
cell[key] = json.loads(value)
with open(path, "w") as f:
    json.dump(doc, f, indent=1)
print(f"mutated {rule}/{window}/{options}.{key}: {before!r} -> {cell[key]!r}")
PY
)" || {
        echo "" >&2
        echo "LADDER: mutation did not apply (cell or key not found)" >&2
        exit 1
    }
    echo "$printed"

    # APPLY GUARD: the mutated value must be readable back from the file.
    if ! python3 - "$SUMMARY" "$rule" "$window" "$options" "$key" "$value" <<'PY'
import json, sys
path, rule, window, options, key, value = sys.argv[1:7]
with open(path) as f:
    doc = json.load(f)
cell = [c for c in doc["cells"] if c["rule"] == rule and c["window"] == window and c["consumer_options"] == options][0]
sys.exit(0 if cell[key] == json.loads(value) else 1)
PY
    then
        echo "" >&2
        echo "LADDER: mutated value not visible after mutation — refusing to call this CAUGHT" >&2
        exit 1
    fi

    if cargo test --release --test "$TEST" -- "$test_fn" > /dev/null 2>&1; then
        echo "VACUOUS! (test passed under mutation)" >&2
        restore_all
        exit 1
    else
        echo "CAUGHT (went red, as expected)"
    fi
    restore_all
}

# ---------------------------------------------------------------------------
# 1. The headline claim: main's quiet cursor rides the tracker, not payloads.
#    Making main look payload-only (cursor 91) must go red.
# ---------------------------------------------------------------------------
mutate_cell "main quiet cursor looks payload-only (122 -> 91)" \
    "main_rides_the_internal_tracker_not_the_payloads" \
    "main" "aged" "default-options" "reconnect_from_slot" "91"

# ---------------------------------------------------------------------------
# 2. The inverse: PR #114's cursor stops at 91. Letting it ride to 122 must
#    go red (that would erase the actual tradeoff).
# ---------------------------------------------------------------------------
mutate_cell "pr114 quiet cursor rides the tracker (91 -> 122)" \
    "pr114_payload_only_cursor_stops_at_the_last_delivered_payload" \
    "pr114" "aged" "default-options" "reconnect_from_slot" "122"

# ---------------------------------------------------------------------------
# 3. The age-out refusal is claimed for BOTH rules. Erasing the refusal from
#    the pr114 aged cell must go red (that is the open question's answer).
# ---------------------------------------------------------------------------
mutate_cell "pr114 aged cell loses its OUT_OF_RANGE refusal (2 -> 0)" \
    "age_out_refuses_both_rules_and_the_stall_is_invisible_by_default" \
    "pr114" "aged" "default-options" "out_of_range_replies" "0"

# ---------------------------------------------------------------------------
# 4. The silent-stall claim: DEFAULT options surface NO error. Making the
#    default cell visible (errors 0 -> 1) must go red.
# ---------------------------------------------------------------------------
mutate_cell "main default-options aged cell becomes visible (errors 0 -> 1)" \
    "age_out_refuses_both_rules_and_the_stall_is_invisible_by_default" \
    "main" "aged" "default-options" "consumer_errors_count" "1"

# ---------------------------------------------------------------------------
# 5. The loud-when-opted-in claim: intra_slot_updates surfaces the error.
#    Silencing it (errors 1 -> 0) must go red.
# ---------------------------------------------------------------------------
mutate_cell "pr114 intra-slot-updates aged cell silenced (errors 1 -> 0)" \
    "age_out_refuses_both_rules_and_the_stall_is_invisible_by_default" \
    "pr114" "aged" "intra-slot-updates" "consumer_errors_count" "0"

# ---------------------------------------------------------------------------
# 6. The held-window guard: main recovers only 122 (1 payload). Claiming a
#    full recovery must go red — the #115 skip in quiet form must stay true.
# ---------------------------------------------------------------------------
mutate_cell "main held cell claims full recovery (1 -> 5)" \
    "the_held_window_shows_the_real_tradeoff" \
    "main" "held" "default-options" "payloads_delivered_after_reconnect" "5"

# ---------------------------------------------------------------------------
# 7. Durability: a cell that names a nonexistent artifact must go red.
# ---------------------------------------------------------------------------
mutate_cell "pr114 held cell points at a missing artifact" \
    "every_cell_names_its_raw_artifact" \
    "pr114" "held" "default-options" "artifact" '"research/laserstream-115-quiet-resume-rule-pr114-NEVER-MEASURED-2026-11-20.json"'

# ---------------------------------------------------------------------------
# byte-identical restore check vs the PRE-LADDER snapshot (never git HEAD).
# ---------------------------------------------------------------------------
echo -n "restore check (byte-identical vs pre-ladder snapshot) ... "
if cmp -s "$SUMMARY" "$PRISTINE/$(basename "$SUMMARY")"; then
    echo "OK"
else
    echo "FAILED: tree is not byte-identical after restore" >&2
    exit 1
fi

echo "=== 7/7 CAUGHT, restore byte-identical — ladder passed ==="
