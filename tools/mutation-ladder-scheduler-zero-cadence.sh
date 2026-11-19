#!/usr/bin/env bash
# Mutation ladder for the zero-cadence scheduler fix (src/main.rs).
#
# Falsification-first: each mutation weakens ONE rule the fix rests on,
# runs the exact test that pins it, and asserts RED. If the test passes
# under the mutation, the test is vacuous and the ladder fails.
#
# Hardened like tools/mutation-ladder-status-two-queue-reason.sh:
#
#   1. DESTROYER: pristine bytes are copied aside BEFORE the first mutation
#      and restored by copy (git never restores a working-tree file; a trap
#      restores on any exit).
#   2. VACUOUS CATCHES: every test filter is PROVEN to match at least one
#      real test BEFORE mutating (`cargo test -- --list` names them), or the
#      ladder fails loudly instead of reporting a CAUGHT for a no-op.
#   3. COMPILE: a mutation that does not compile is an error, not a catch.
#   4. The RESTORE comparison is against the PRISTINE SNAPSHOT, never
#      `git HEAD` — a dirty tree or a mid-run commit makes HEAD the wrong
#      reference.
#
#   node tools/mutation-ladder-scheduler-zero-cadence.sh
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO"

TEST="scheduler_zero_cadence"

echo "=== zero-cadence scheduler mutation ladder ==="

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

PRISTINE="$(mktemp -d /tmp/ladder-pristine-zero-cad.XXXXXX)"
for f in src/main.rs; do
    mkdir -p "$PRISTINE/$(dirname "$f")"
    cp "$f" "$PRISTINE/$f"
done
restore_all() {
    for f in src/main.rs; do
        if [ -f "$PRISTINE/$f" ]; then
            cp "$PRISTINE/$f" "$f"
            touch "$f"
        fi
    done
}
trap restore_all EXIT

# mutate <name> <test-filter> <file> <search-marker> <replace-marker>
mutate() {
    local name="$1"
    local test_fn="$2"
    local file="$3"
    local search="$4"
    local replace="$5"

    assert_filter_matches "$test_fn"

    echo -n "mutation: $name ... "
    python3 - "$file" "$search" "$replace" <<'PY' >/dev/null 2>/tmp/ladder-err.txt
import sys
path, search, repl = sys.argv[1:4]
with open(path, "r") as f:
    text = f.read()
if search not in text:
    print(f"pattern not found: {search}", file=sys.stderr)
    sys.exit(2)
text = text.replace(search, repl, 1)
with open(path, "w") as f:
    f.write(text)
PY
    local py_rc=$?
    if [ "$py_rc" -ne 0 ]; then
        echo "" >&2
        echo "LADDER: mutation did not apply in $file: $search" >&2
        cat /tmp/ladder-err.txt >&2
        exit 1
    fi

    if ! cargo build --release > /dev/null 2>&1; then
        echo "COMPILE ERROR (not a behavioural catch) — mutation must compile" >&2
        restore_all
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
# 1. The measured defect, restored: the scheduler collapses a cadence of 0
#    to ONE SECOND again (`max(1)`), so the seal pass runs at 1 Hz while the
#    shutdown gate still reads the zero as "never".
# ---------------------------------------------------------------------------
mutate "the scheduler collapses a cadence of 0 to one second again" \
    "a_zero_seal_cadence_runs_the_seal_pass" \
    "src/main.rs" \
    "    let next_due = |cadence: u64| -> Option<Instant> {
        if cadence == 0 {
            None
        } else {
            Some(Instant::now() + Duration::from_secs(cadence.max(1)))
        }
    };" \
    "    let next_due = |cadence: u64| -> Option<Instant> {
        Some(Instant::now() + Duration::from_secs(cadence.max(1)))
    };"

# ---------------------------------------------------------------------------
# 2. The zero is honoured by the SCHEDULE but not by the SHUTDOWN GATE:
#    the final reorg check runs again although the same value means "never".
# ---------------------------------------------------------------------------
mutate "the shutdown gate runs the final reorg check at a cadence of 0" \
    "a_zero_reorg_cadence_runs_the_reorg_pass_but_skips_the_final_check" \
    "src/main.rs" \
    "    let final_reorg = if reorg_every_s > 0 {" \
    "    let final_reorg = if true {"

# ---------------------------------------------------------------------------
# 3. The same for the repair side.
# ---------------------------------------------------------------------------
mutate "the shutdown gate runs the final repair pass at a cadence of 0" \
    "a_zero_repair_cadence_runs_the_repair_pass_but_skips_the_final_pass" \
    "src/main.rs" \
    "    let final_repair = if repair_every_s > 0 {" \
    "    let final_repair = if true {"

# ---------------------------------------------------------------------------
# 4. The webhook schedule keeps delivering at a cadence of 0 — the one
#    face where a signed body LEAVES the machine.
# ---------------------------------------------------------------------------
mutate "the webhook schedule still fires at a cadence of 0" \
    "a_zero_webhook_cadence_does_not_deliver" \
    "src/main.rs" \
    "            if next_webhook.is_some_and(|due| Instant::now() >= due) {" \
    "            if next_webhook.map(|due| Instant::now() >= due).unwrap_or(true) {"

# ---------------------------------------------------------------------------
# 5. The schedule is OFF, but the summary still COUNTS the passes — the
#    report would claim work that never happened.
# ---------------------------------------------------------------------------
mutate "the summary counts scheduled passes that never ran" \
    "the_summary_shows_the_two_meanings_of_zero" \
    "src/main.rs" \
    "            if next_reorg.is_some_and(|due| Instant::now() >= due) {
                reorg_tick_now!();
            }" \
    "            if true {
                reorg_tick_now!();
            }"

# ---------------------------------------------------------------------------
# 6. A POSITIVE cadence stops scheduling at all — the fix must not silence
#    the schedules everybody else actually asked for.
# ---------------------------------------------------------------------------
mutate "a positive cadence schedules nothing (the schedules are silenced)" \
    "a_tight_positive_cadence_still_runs" \
    "src/main.rs" \
    "            if next_seal.is_some_and(|due| Instant::now() >= due) {" \
    "            if next_seal.is_some_and(|due| Instant::now() >= due) && false {"

# ---------------------------------------------------------------------------
# The RESTORE check: the tree must be byte-identical to the PRISTINE
# snapshot after the ladder (NOT to `git HEAD` — the tree is dirty by
# design here).
# ---------------------------------------------------------------------------
restore_all
if cmp -s "$PRISTINE/src/main.rs" "src/main.rs"; then
    echo "restore check: src/main.rs is byte-identical to the pre-ladder snapshot"
else
    echo "RESTORE FAILED: src/main.rs differs from the pre-ladder snapshot" >&2
    exit 1
fi

echo "=== zero-cadence scheduler mutation ladder: all mutations CAUGHT ==="
