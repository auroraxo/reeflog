#!/usr/bin/env bash
# Mutation ladder for the WEBHOOK-STALE-ERROR fix (PULS AG, 2026-11-13).
#
# The rule under test: `webhook_state.last_error` is the reason the
# PREVIOUS delivery pass stopped. Each new pass for a target clears it
# first (after the endpoint binding) and any block in THIS pass re-sets
# it — so a standing failure is reported on every pass it still happens,
# and a pass that gets through leaves `webhook status` with no error.
#
# Measured on the PUBLISHED v0.7.23 asset: the defect was visible
# (research/webhook-stale-error-published-v0723-2026-11-13.json, verdict
# stale-error-verdict-present). A gate that cannot be made red certifies
# nothing, so every mutation below must turn its exact test RED, must
# COMPILE, and the tree must come back BYTE-IDENTICAL afterwards
# (file-copying backup — never `git checkout`).
#   1. DESTROYER: restore is a COPY.
#   2. VACUOUS CATCHES: every filter selects a real test BEFORE mutating;
#      a compile error is not a behavioural catch.
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO"

TEST="webhook_stale_error"

echo "=== webhook-stale-error mutation ladder ==="

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
    if ! printf '%s\n' "$LIST" | grep -q "^${filter}"; then
        echo "" >&2
        echo "LADDER BUG: filter \`$filter\` matches no test in tests/$TEST.rs — a CAUGHT verdict would be vacuous" >&2
        printf '%s\n' "$LIST" | sed 's/^/    available: /' >&2
        exit 1
    fi
}

PRISTINE="$(mktemp -d /tmp/ladder-wse-pristine.XXXXXX)"
mkdir -p "$PRISTINE/src"
cp src/webhook.rs "$PRISTINE/src/webhook.rs"
cp src/journal.rs "$PRISTINE/src/journal.rs"
restore_all() {
    if [ -f "$PRISTINE/src/webhook.rs" ]; then
        cp "$PRISTINE/src/webhook.rs" src/webhook.rs
        cp "$PRISTINE/src/journal.rs" src/journal.rs
        touch src/webhook.rs src/journal.rs
    fi
}
trap restore_all EXIT

mutate() {
    local name="$1"
    local test_fn="$2"
    local file="$3"
    local search="$4"
    local replace="$5"

    assert_filter_matches "$test_fn"

    echo -n "mutation: $name ... "
    python3 -c '
import sys
path, search, repl = sys.argv[1:5]
with open(path, "r") as f:
    text = f.read()
n = text.count(search)
if n != 1:
    print(f"LADDER BUG: pattern matched {n} times (must be exactly 1): {search[:70]!r}", file=sys.stderr)
    sys.exit(2)
with open(path, "w") as f:
    f.write(text.replace(search, repl, 1))
' "$file" "$search" "$replace" || {
        echo "" >&2
        echo "LADDER: pattern missing or ambiguous in $file: $search" >&2
        exit 1
    }

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

# M1 — the measured defect class returns: the event channel stops clearing
#     the previous pass's verdict, so a recovered target reports the old
#     failure again.
mutate "the event channel keeps the previous pass's error" \
    "a_recovered_target_stops_reporting_the_old_failure" \
    "src/webhook.rs" \
    "    j.webhook_clear_error(&target.id)?;
    let (next_seq, next_reorg_id) = j.webhook_state(&target.id)?.unwrap_or((0, 0));" \
    "    let (next_seq, next_reorg_id) = j.webhook_state(&target.id)?.unwrap_or((0, 0));"

# M2 — a pass that got through now reports a FAKE failure: the clear also
#     writes a verdict, so a clean pass lies about being stuck.
mutate "a clean pass invents a failure" \
    "a_clean_target_reports_no_error" \
    "src/webhook.rs" \
    "    j.webhook_clear_error(&target.id)?;" \
    "    j.webhook_set_error(&target.id, &target.url, Some(\"cleared\"))?;"

# M3 — a standing failure is forgotten BEFORE it can be reported: the hold
#     block clears instead of records, so `webhook status` never carries the
#     hold reason the operator needs (the guard in the hold test goes red).
mutate "the hold block stops reporting its own reason" \
    "a_paid_payload_clears_the_hold_reason_status_reports" \
    "src/webhook.rs" \
    "        if r.logs.is_none() && r.payload_state.is_none() {
            let reason = \"the payload is still owed — run \`hydrate\`\".to_string();
            j.webhook_set_error(&target.id, &target.url, Some(&reason))?;
            rep.blocked_seq = Some(r.seq);
            rep.blocked_error = Some(reason);
            break;" \
    "        if r.logs.is_none() && r.payload_state.is_none() {
            let reason = \"the payload is still owed — run \`hydrate\`\".to_string();
            j.webhook_clear_error(&target.id)?;
            rep.blocked_seq = Some(r.seq);
            rep.blocked_error = Some(reason);
            break;"

# M4 — the clear does nothing (journal no-op): the verdict is permanent
#     again — the exact published v0.7.23 behaviour, byte for byte.
mutate "the clear becomes a no-op" \
    "a_recovered_target_stops_reporting_the_old_failure" \
    "src/journal.rs" \
    "                \"UPDATE webhook_state SET last_error = NULL WHERE target = ?1\",
                params![target]," \
    "                \"SELECT 1 WHERE 0\",
                params![target],"

cmp -s src/webhook.rs "$PRISTINE/src/webhook.rs" || {
    echo "LADDER BUG: webhook.rs not byte-identical after restore" >&2
    exit 1
}
cmp -s src/journal.rs "$PRISTINE/src/journal.rs" || {
    echo "LADDER BUG: journal.rs not byte-identical after restore" >&2
    exit 1
}
echo "tree byte-identical after restore"

echo -n "control: full gate green on the restored tree ... "
cargo test --release --test "$TEST" > /dev/null 2>&1 && echo "OK" || { echo "FAILED" >&2; exit 1; }

echo "=== 4/4 mutations caught, control green ==="
