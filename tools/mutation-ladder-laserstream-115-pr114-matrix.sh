#!/usr/bin/env bash
# Mutation ladder for the LaserStream #115 / PR #114 two-rule matrix pin
# (tests/laserstream_115_pr114_matrix.rs).
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
#   3. APPLY GUARD: every mutation PRINTS the value it changed, and the
#      mutation is applied BY KEY (rule + script), not by text replacement —
#      a text pattern that no longer matches must fail loudly, never
#      silently mutate nothing and still be called CAUGHT.
#   4. The restore check compares against the PRE-LADDER snapshot, never
#      git HEAD (a dirty tree makes HEAD the wrong reference).
#
#   node tools/mutation-ladder-laserstream-115-pr114-matrix.sh
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO"

TEST="laserstream_115_pr114_matrix"
MATRIX="research/laserstream-115-pr114-two-rule-matrix-2026-11-19.json"

echo "=== laserstream 115 / PR114 matrix mutation ladder ==="

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

PRISTINE="$(mktemp -d /tmp/ladder-pristine.XXXXXX)"
cp "$MATRIX" "$PRISTINE/"
restore_all() {
    if [ -f "$PRISTINE/$(basename "$MATRIX")" ]; then
        cp "$PRISTINE/$(basename "$MATRIX")" "$MATRIX"
        touch "$MATRIX"
    fi
}
trap restore_all EXIT

# mutate_cell <name> <test-filter> <rule> <script> <key> <new-value>
mutate_cell() {
    local name="$1"
    local test_fn="$2"
    local rule="$3"
    local script="$4"
    local key="$5"
    local value="$6"

    assert_filter_matches "$test_fn"

    echo -n "mutation: $name ... "
    local printed
    printed="$(python3 - "$MATRIX" "$rule" "$script" "$key" "$value" <<'PY'
import json, sys
path, rule, script, key, value = sys.argv[1:6]
with open(path) as f:
    doc = json.load(f)
hits = [c for c in doc["matrix"] if c["rule"] == rule and c["script"] == script]
if len(hits) != 1:
    print(f"ERROR: expected exactly one cell for ({rule},{script}), found {len(hits)}", file=sys.stderr)
    sys.exit(2)
cell = hits[0]
if key not in cell:
    print(f"ERROR: cell ({rule},{script}) has no key {key}", file=sys.stderr)
    sys.exit(2)
before = cell[key]
cell[key] = json.loads(value)
with open(path, "w") as f:
    json.dump(doc, f, indent=1)
print(f"mutated {rule}/{script}.{key}: {before!r} -> {cell[key]!r}")
PY
)" || {
        echo "" >&2
        echo "LADDER: mutation did not apply (cell or key not found)" >&2
        exit 1
    }
    echo "$printed"

    # APPLY GUARD: the mutated value must be readable back from the file.
    if ! python3 - "$MATRIX" "$rule" "$script" "$key" "$value" <<'PY'
import json, sys
path, rule, script, key, value = sys.argv[1:6]
with open(path) as f:
    doc = json.load(f)
cell = [c for c in doc["matrix"] if c["rule"] == rule and c["script"] == script][0]
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
# 1. The headline claim: PR #114's rule is declared to have fixed the #115
#    face A — the wrong claim the whole pin exists to prevent.
# ---------------------------------------------------------------------------
mutate_cell "PR114 variant-a cell says the fix reached the #115 case-1 face" \
    "the_pr114_rule_still_fails_the_faces_115_reports" \
    "pr114" "variant-a" "verdict" '"ok-duplicate-1"'

# ---------------------------------------------------------------------------
# 2. The B face (the DOCUMENTED correct usage, filter_by_commitment) is
#    declared fixed under PR #114 while its cursor still jumps.
# ---------------------------------------------------------------------------
mutate_cell "PR114 variant-b cell loses its cursor jump" \
    "the_pr114_rule_still_fails_the_faces_115_reports" \
    "pr114" "variant-b" "reconnect_from_slot" "91"

# ---------------------------------------------------------------------------
# 3. The PR #114 C face (its own claim) is declared broken — the false
#    negative that would discredit a correct fix.
# ---------------------------------------------------------------------------
mutate_cell "PR114 variant-c cell says the internal-tick fix is lost" \
    "pr114_fixes_the_internal_tick_face_but_not_by_swapping_cells" \
    "pr114" "variant-c" "verdict" '"loss-30-slots"'

# ---------------------------------------------------------------------------
# 4. The side-effect fix on D is declared broken — the false negative that
#    would hide that PR #114 also stops the block-footer advance.
# ---------------------------------------------------------------------------
mutate_cell "PR114 variant-d cell says the footer advance still wins" \
    "pr114_fixes_the_internal_tick_face_but_not_by_swapping_cells" \
    "pr114" "variant-d" "verdict" '"loss-30-slots"'

# ---------------------------------------------------------------------------
# 5. The main rule's E cell is rounded to "fixed": the footer-only loss on
#    main (90..121) would disappear from the record.
# ---------------------------------------------------------------------------
mutate_cell "main variant-e cell hides the footer-only loss" \
    "the_footer_only_face_keeps_its_honest_two_sided_verdict" \
    "main" "variant-e" "verdict" '"ok-duplicate-1"'

# ---------------------------------------------------------------------------
# 6. PR #114's E cell is rounded up to "fixed": the duplicate-forever
#    verdict (the cursor never advances) would read as a clean fix.
# ---------------------------------------------------------------------------
mutate_cell "PR114 variant-e cell hides the duplicate-forever verdict" \
    "the_footer_only_face_keeps_its_honest_two_sided_verdict" \
    "pr114" "variant-e" "verdict" '"ok-duplicate-1"'

# ---------------------------------------------------------------------------
# 7. The control of the main rule is turned into a loss — a control reported
#    as broken would mean the measurement invents defects.
# ---------------------------------------------------------------------------
mutate_cell "main control cell reports a loss" \
    "the_controls_stay_ok_on_both_rules" \
    "main" "control" "verdict" '"loss-30-slots"'

# ---------------------------------------------------------------------------
# 8. Host path injection: an artifact that carries the machine that made it
#    would go red in CI on someone else's host (the sememe lesson).
# ---------------------------------------------------------------------------
echo -n "mutation: matrix carries a host path ... "
printf '%s' "$(python3 - <<'PY'
import json
path = "research/laserstream-115-pr114-two-rule-matrix-2026-11-19.json"
with open(path) as f:
    doc = json.load(f)
doc["path_leak"] = "/home/aurora/github_repos/laserstream-cursor-measure"
with open(path, "w") as f:
    json.dump(doc, f, indent=1)
print("mutated matrix.path_leak: /home/aurora/github_repos/laserstream-cursor-measure")
PY
)"
if ! grep -qF "/home/aurora" "$MATRIX"; then
    echo "" >&2
    echo "LADDER: host path not visible after mutation — refusing to call this CAUGHT" >&2
    exit 1
fi
assert_filter_matches "every_cell_carries_its_own_explanation_and_no_host_path"
if cargo test --release --test "$TEST" -- "every_cell_carries_its_own_explanation_and_no_host_path" > /dev/null 2>&1; then
    echo "VACUOUS! (test passed under mutation)" >&2
    restore_all
    exit 1
else
    echo "CAUGHT (went red, as expected)"
fi
restore_all

# ---------------------------------------------------------------------------
# 9. BYTE-IDENTICAL RESTORE: the tree must be exactly the pre-ladder
#    snapshot, not git HEAD.
# ---------------------------------------------------------------------------
echo -n "restore check ... "
if cmp -s "$MATRIX" "$PRISTINE/$(basename "$MATRIX")"; then
    echo "OK (byte-identical to the pre-ladder snapshot)"
else
    echo "FAILED: the matrix differs from the pre-ladder snapshot" >&2
    exit 1
fi

echo "=== laserstream 115 / PR114 matrix mutation ladder: all mutations CAUGHT ==="
