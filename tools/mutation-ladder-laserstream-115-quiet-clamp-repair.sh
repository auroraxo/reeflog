#!/usr/bin/env bash
# Mutation ladder for the LaserStream #115 consumer-side clamp-repair pin
# (tests/laserstream_115_quiet_clamp_repair.rs).
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
#      BY KEY (cell name + key), never by blind text replacement — a key that
#      does not exist must fail loudly, never silently.
#   4. The restore check compares against the PRE-LADDER snapshot, never
#      git HEAD (a dirty tree makes HEAD the wrong reference).
#
#   ./tools/mutation-ladder-laserstream-115-quiet-clamp-repair.sh
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO"

TEST="laserstream_115_quiet_clamp_repair"
SUMMARY="research/laserstream-115-quiet-clamp-summary-2026-11-20.json"

echo "=== laserstream 115 quiet-clamp-repair mutation ladder ==="

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

PRISTINE="$(mktemp -d /tmp/ladder-pristine-clamp.XXXXXX)"
cp "$SUMMARY" "$PRISTINE/"
restore_all() {
    if [ -f "$PRISTINE/$(basename "$SUMMARY")" ]; then
        cp "$PRISTINE/$(basename "$SUMMARY")" "$SUMMARY"
        touch "$SUMMARY"
    fi
}
trap restore_all EXIT

# mutate_cell <name> <test-filter> <cell> <key> <new-json-value>
mutate_cell() {
    local name="$1"
    local test_fn="$2"
    local cell_name="$3"
    local key="$4"
    local value="$5"

    assert_filter_matches "$test_fn"

    echo -n "mutation: $name ... "
    local printed
    printed="$(python3 - "$SUMMARY" "$cell_name" "$key" "$value" <<'PY'
import json, sys
path, cell_name, key, value = sys.argv[1:5]
with open(path) as f:
    doc = json.load(f)
cells = doc["cells"]
if not isinstance(cells, dict):
    print(f"ERROR: the summary must carry cells as an object", file=sys.stderr)
    sys.exit(2)
if cell_name not in cells:
    print(f"ERROR: no cell named {cell_name}", file=sys.stderr)
    sys.exit(2)
cell = cells[cell_name]
if key not in cell:
    print(f"ERROR: cell {cell_name} has no key {key}", file=sys.stderr)
    sys.exit(2)
before = cell[key]
cell[key] = json.loads(value)
with open(path, "w") as f:
    json.dump(doc, f, indent=1)
print(f"mutated {cell_name}.{key}: {before!r} -> {cell[key]!r}")
PY
)" || {
        echo "" >&2
        echo "LADDER: mutation did not apply (cell or key not found)" >&2
        exit 1
    }
    echo "$printed"

    # APPLY GUARD: the mutated value must be readable back from the file.
    if ! python3 - "$SUMMARY" "$cell_name" "$key" "$value" <<'PY'
import json, sys
path, cell_name, key, value = sys.argv[1:5]
with open(path) as f:
    doc = json.load(f)
cell = doc["cells"][cell_name]
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
# 1. The silent-stall claim: DEFAULT options surface NO error. Making the
#    silent cell visible (errors 0 -> 1) must go red — the invisible stall
#    is what makes the repair unreachable, so it must stay observed.
# ---------------------------------------------------------------------------
mutate_cell "silent cell becomes visible (errors 0 -> 1)" \
    "the_silent_cell_keeps_the_stall_invisible" \
    "silent" "consumer_error_count" "1"

# ---------------------------------------------------------------------------
# 2. The silent cell must actually have been refused by the server — a
#    scenario that never happened would describe nothing (errors 0, but no
#    refusal). Erasing the refusal must go red.
# ---------------------------------------------------------------------------
mutate_cell "silent cell never got refused (oor 2 -> 0)" \
    "the_silent_cell_keeps_the_stall_invisible" \
    "silent" "out_of_range_replies" "0"

# ---------------------------------------------------------------------------
# 3. The intra cell: the consumer MUST see the terminal error. Silencing it
#    (errors 1 -> 0) must go red.
# ---------------------------------------------------------------------------
mutate_cell "intra cell silenced (errors 1 -> 0)" \
    "the_intra_cell_keeps_the_error_surfaced" \
    "intra" "consumer_error_count" "0"

# ---------------------------------------------------------------------------
# 4. The repair's clamp: the consumer must clamp to the server's floor
#    (130), not to the stale cursor (122). Claiming the stale cursor must
#    go red.
# ---------------------------------------------------------------------------
mutate_cell "repair clamps to the stale cursor (130 -> 122)" \
    "the_repair_cell_clamps_to_first_available_and_declares_the_gap" \
    "repair" "clamp_from_slot" "122"

# ---------------------------------------------------------------------------
# 5. first_available is what the clamp is derived from. Reporting the
#    stale cursor as the server's floor must go red.
# ---------------------------------------------------------------------------
mutate_cell "first_available misreported (130 -> 122)" \
    "the_repair_cell_clamps_to_first_available_and_declares_the_gap" \
    "repair" "first_available_reported" "122"

# ---------------------------------------------------------------------------
# 6. The declared gap: 38 slots (92..129). Shrinking it to 0 would erase
#    the cost of the repair — the whole point of the measurement. Must go
#    red.
# ---------------------------------------------------------------------------
mutate_cell "declared gap erased (38 -> 0)" \
    "the_repair_cell_clamps_to_first_available_and_declares_the_gap" \
    "repair" "gap_declared_range_size" "0"

# ---------------------------------------------------------------------------
# 7. The repair queries the server exactly once. Claiming a different
#    count must go red.
# ---------------------------------------------------------------------------
mutate_cell "replay_info query count wrong (1 -> 2)" \
    "the_repair_cell_clamps_to_first_available_and_declares_the_gap" \
    "repair" "replay_info_queries" "2"

# ---------------------------------------------------------------------------
# 8. The control cell: the unclamped re-subscribe delivers NOTHING and is
#    refused again. Making the control deliver must go red — that is the
#    false-positive guard that makes the clamp the operative rule.
# ---------------------------------------------------------------------------
mutate_cell "unclamped control delivers (0 slots -> 3 slots)" \
    "the_control_cell_proves_the_clamp_is_the_operative_rule" \
    "control" "delivered_after_clamp" "[130, 131, 132]"

# ---------------------------------------------------------------------------
# 9. The control cell must still be refused. Erasing the refusal must go
#    red.
# ---------------------------------------------------------------------------
mutate_cell "unclamped control not refused (oor 3 -> 0)" \
    "the_control_cell_proves_the_clamp_is_the_operative_rule" \
    "control" "out_of_range_replies" "0"

# ---------------------------------------------------------------------------
# 10. Durability: a cell that names a nonexistent artifact must go red.
# ---------------------------------------------------------------------------
mutate_cell "repair cell points at a missing artifact" \
    "every_cell_names_its_raw_artifact" \
    "repair" "artifact" '"research/laserstream-115-quiet-clamp-NEVER-MEASURED-2026-11-20.json"'

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

echo "=== 10/10 CAUGHT, restore byte-identical — ladder passed ==="
