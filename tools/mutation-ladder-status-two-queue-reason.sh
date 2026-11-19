#!/usr/bin/env bash
# Mutation ladder for the two-queue standing-reason fix
# (src/journal.rs + src/webhook.rs + src/main.rs).
#
# Falsification-first: each mutation weakens ONE rule the fix rests on,
# runs the exact test that pins it, and asserts RED. If the test passes
# under the mutation, the test is vacuous and the ladder fails.
#
# Hardened like tools/mutation-ladder-webhook-redirect.sh:
#
#   1. DESTROYER: pristine bytes are copied aside BEFORE the first mutation
#      and restored by copy (git never restores a working-tree file; a trap
#      restores on any exit).
#   2. VACUOUS CATCHES: every test filter is PROVEN to match at least one
#      real test BEFORE mutating (`cargo test -- --list` names them), or the
#      ladder fails loudly instead of reporting a CAUGHT for a no-op.
#   3. COMPILE: a mutation that does not compile is an error, not a catch.
#
#   node tools/mutation-ladder-status-two-queue-reason.sh
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO"

TEST="webhook_two_queue_reason"

echo "=== two-queue reason mutation ladder ==="

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
for f in src/journal.rs src/webhook.rs src/main.rs; do
    mkdir -p "$PRISTINE/$(dirname "$f")"
    cp "$f" "$PRISTINE/$f"
done
restore_all() {
    for f in src/journal.rs src/webhook.rs src/main.rs; do
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
# 1. The worst regression: the retraction channel writes the EVENT channel's
#    column again — one reason column for two queues, the measured defect.
# ---------------------------------------------------------------------------
mutate "the retraction channel writes last_error again (one column for two queues)" \
    "both_queues_blocked_keeps_the_event_channel_reason" \
    "src/webhook.rs" \
    "                j.webhook_set_reorg_error(&target.id, &target.url, err.as_deref())?;" \
    "                j.webhook_set_error(&target.id, &target.url, err.as_deref())?;"

# ---------------------------------------------------------------------------
# 2. The retraction channel writes to a column NOTHING reads: the journal
#    keeps two columns but the summary never carries the retraction's.
# ---------------------------------------------------------------------------
mutate "the summary drops the retraction channel's reason" \
    "the_retraction_reason_is_attributable_to_its_channel" \
    "src/journal.rs" \
    "            last_error,
            last_reorg_error,
            delivered," \
    "            last_error,
            last_reorg_error: None,
            delivered,"

# ---------------------------------------------------------------------------
# 3. The status row prints the retraction's reason under the EVENT channel's
#    name again: one field, two queues — the report cannot say which queue
#    is stopped.
# ---------------------------------------------------------------------------
mutate "the status row prints the retraction reason as last_error" \
    "the_retraction_reason_is_attributable_to_its_channel" \
    "src/main.rs" \
    "            \"reorg_error\": s.last_reorg_error,
        }));" \
    "            \"reorg_error\": Value::Null,
        }));"

# ---------------------------------------------------------------------------
# 4. Clearing one channel only: a pass that gets through leaves the OTHER
#    channel's ended failure standing (the v0.7.23 stale-error defect, now
#    reachable through the new column).
# ---------------------------------------------------------------------------
mutate "clear only the event channel (a finished retraction failure stays)" \
    "a_recovered_target_clears_both_reasons" \
    "src/journal.rs" \
    "                \"UPDATE webhook_state SET last_error = NULL, last_reorg_error = NULL WHERE target = ?1\"," \
    "                \"UPDATE webhook_state SET last_error = NULL WHERE target = ?1\","

# ---------------------------------------------------------------------------
# 5. The exit code: a target stopped ONLY on the retraction queue no longer
#    counts as failing — the report prints its reason and calls the target
#    healthy (the v0.7.29 debt-exit class, now reachable through the new
#    field).
# ---------------------------------------------------------------------------
mutate "the exit code counts only the event channel" \
    "only_the_retraction_queue_blocked_is_unchanged" \
    "src/main.rs" \
    "            if let Some(e) = row
                .get(\"reorg_error\")
                .and_then(|e| e.as_str())
                .filter(|e| !e.is_empty())
            {
                reasons.push((id, e.to_string(), \"retractions\"));
            }" \
    "            let _ = row.get(\"reorg_error\");"

echo "=== two-queue reason mutation ladder: all mutations CAUGHT ==="
