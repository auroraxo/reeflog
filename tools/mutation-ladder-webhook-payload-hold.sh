#!/usr/bin/env bash
# Mutation ladder for the WEBHOOK-PAYLOAD-HOLD fix (PULS AF, 2026-11-13).
#
# The rule under test: `deliver_events` must NOT post a row whose payload
# the journal still owes (`logs IS NULL AND payload_state IS NULL`). The
# queue stops at the row: the body is never sent, the cursor stays behind
# it, no receipt is written, the target is reported blocked (exit 3), and
# the paid debt reaches the SAME target on the next pass.
#
# Measured on the PUBLISHED v0.7.22 asset: the defect was visible
# (research/webhook-payload-hold-baseline-v0722-2026-11-13.json, verdict
# webhook-payload-hold-defect-present). A gate that cannot be made red
# certifies nothing, so every mutation below must turn its exact test RED,
# must COMPILE, and the tree must come back BYTE-IDENTICAL afterwards
# (file-copying backup — never `git checkout`).
#   1. DESTROYER: restore is a COPY.
#   2. VACUOUS CATCHES: every filter selects a real test BEFORE mutating;
#      a compile error is not a behavioural catch.
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO"

TEST="webhook_payload_hold"

echo "=== webhook-payload-hold mutation ladder ==="

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

PRISTINE="$(mktemp -d /tmp/ladder-wph-pristine.XXXXXX)"
mkdir -p "$PRISTINE/src"
cp src/webhook.rs "$PRISTINE/src/webhook.rs"
restore_all() {
    if [ -f "$PRISTINE/src/webhook.rs" ]; then
        cp "$PRISTINE/src/webhook.rs" src/webhook.rs
        touch src/webhook.rs
    fi
}
trap restore_all EXIT

mutate() {
    local name="$1"
    local test_fn="$2"
    local search="$3"
    local replace="$4"

    assert_filter_matches "$test_fn"

    echo -n "mutation: $name ... "
    python3 -c '
import sys
path, search, repl = sys.argv[1:4]
with open(path, "r") as f:
    text = f.read()
n = text.count(search)
if n != 1:
    print(f"LADDER BUG: pattern matched {n} times (must be exactly 1): {search[:70]!r}", file=sys.stderr)
    sys.exit(2)
with open(path, "w") as f:
    f.write(text.replace(search, repl, 1))
' src/webhook.rs "$search" "$replace" || {
        echo "" >&2
        echo "LADDER: pattern missing or ambiguous in src/webhook.rs: $search" >&2
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

# M1 — the measured defect class: the webhook path stops holding an owed
#     payload row (this is the v0.7.22 behaviour).
mutate "the webhook path stops holding an owed payload row" \
    "an_owed_payload_is_not_delivered_to_a_webhook_target" \
    "        if r.logs.is_none() && r.payload_state.is_none() {
            let reason = \"the payload is still owed — run \`hydrate\`\".to_string();" \
    "        if false && r.logs.is_none() && r.payload_state.is_none() {
            let reason = \"the payload is still owed — run \`hydrate\`\".to_string();"

# M2 — a hold that lies about being a hold: the queue stops, but the
#     target is not reported blocked (no blocked_seq, exit 0).
mutate "the hold stops reporting the target as blocked" \
    "an_owed_payload_is_not_delivered_to_a_webhook_target" \
    "            rep.blocked_seq = Some(r.seq);
            rep.blocked_error = Some(reason);
            break;" \
    "            let _ = reason;
            break;"

# M3 — the hold forgets the recorded-outcome exception: a row that can
#     NEVER be filled (unavailable) would hold the queue forever.
mutate "the hold swallows recorded outcomes too" \
    "a_recorded_outcome_is_delivered_with_its_marker" \
    "        if r.logs.is_none() && r.payload_state.is_none() {" \
    "        if r.logs.is_none() {"

# M4 — the hold overshoots the other way: every row is held, full payload
#     included (the channel that never delivers).
mutate "every row is held, not only owed ones" \
    "a_full_payload_is_delivered_as_before" \
    "        if r.logs.is_none() && r.payload_state.is_none() {" \
    "        if true {"

# M5 — the loss itself: the hold fires, but the cursor walks past the
#     held row anyway (the paid debt can then never reach this target).
mutate "the cursor advances past the held row" \
    "a_paid_payload_reaches_the_same_webhook_target" \
    "            j.webhook_set_error(&target.id, &target.url, Some(&reason))?;
            rep.blocked_seq = Some(r.seq);
            rep.blocked_error = Some(reason);
            break;" \
    "            j.webhook_set_state(&target.id, &target.url, r.seq, rep.next_reorg_id)?;
            break;"

cmp -s src/webhook.rs "$PRISTINE/src/webhook.rs" || {
    echo "LADDER BUG: tree not byte-identical after restore" >&2
    exit 1
}
echo "tree byte-identical after restore"

echo -n "control: full gate green on the restored tree ... "
cargo test --release --test "$TEST" > /dev/null 2>&1 && echo "OK" || { echo "FAILED" >&2; exit 1; }

echo "=== 5/5 mutations caught, control green ==="
