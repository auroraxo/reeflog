#!/usr/bin/env bash
# Mutation ladder for the REORG/VERDICT-SURVIVAL fix (PULS AW, 2026-11-19).
#
# The rule under test: a delivery channel's standing verdict is a fact
# about THAT channel's most recent phase. The event phase clears only
# `last_error`, the retraction phase only `last_reorg_error`, each clear
# runs only after that phase actually read its queue (a `--limit 0` pass
# reads no rows and may not judge), and a pass that GETS THROUGH still
# clears its column.
#
# Measured on this tree BEFORE the fix (research/
# reorg-verdict-survival-prefix-2026-11-19.json, verdict
# `reorg-verdict-erased-by-idle-pass`, 5/6 faces red): a zero-byte pass
# erased both columns. A gate that cannot be made red certifies nothing,
# so every mutation below must turn its exact test RED, must COMPILE, and
# the tree must come back BYTE-IDENTICAL afterwards (file-copying backup —
# never `git checkout`).
#   1. DESTROYER: restore is a COPY.
#   2. VACUOUS CATCHES: every filter selects a real test BEFORE mutating;
#      a compile error is not a behavioural catch; every mutation prints
#      the mutated line so a no-op mutation fails loudly.
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO"

TEST="webhook_verdict_survival"

echo "=== webhook-verdict-survival mutation ladder ==="

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

PRISTINE="$(mktemp -d /tmp/ladder-rvs-pristine.XXXXXX)"
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

    # Print the mutated region: a no-op mutation must fail loudly, never
    # count as a catch (the lesson of 19.11: sed-quoted no-ops reading as
    # VACUUM are not evidence).
    mutated_line="$(grep -Fn "$(printf '%s' "$replace" | head -c 60)" "$file" || true)"
    if [ -z "$mutated_line" ]; then
        echo "" >&2
        echo "LADDER BUG: mutation did not apply to $file (replacement not found)" >&2
        restore_all
        exit 1
    fi
    echo "applied -> $(echo "$mutated_line" | head -1 | sed 's/^[[:space:]]*//' | cut -c1-100)"

    if ! cargo build --release > /dev/null 2>&1; then
        echo "COMPILE ERROR (not a behavioural catch) — mutation must compile" >&2
        restore_all
        exit 1
    fi

    if cargo test --release --test "$TEST" -- "$test_fn" > /dev/null 2>&1; then
        echo "  VACUOUS! (test $test_fn passed under mutation)" >&2
        restore_all
        exit 1
    else
        echo "  CAUGHT (test $test_fn went red, as expected)"
    fi
    restore_all
}

# M1 — the event channel stops judging: its stale verdict is never
#     forgotten, and the recovered target reports the old failure forever.
mutate "the event phase stops clearing its own verdict" \
    "a_recovered_event_queue_stops_reporting_the_old_failure" \
    "src/webhook.rs" \
    "        // The queue was read: this phase's pass may now judge its own
        // channel (and only its channel) — see the two rules above.
        j.webhook_clear_event_error(&target.id)?;" \
    "        // The queue was read: this phase's pass may now judge its own
        // channel (and only its channel) — see the two rules above.
        let _ = &target.id;" 

# M2 — the retraction channel stops judging: same failure on the other
#     column (the mirror of M1).
mutate "the retraction phase stops clearing its own verdict" \
    "a_recovered_retraction_queue_stops_reporting_the_old_failure" \
    "src/webhook.rs" \
    "        j.webhook_clear_reorg_error(&target.id)?;" \
    "        let _ = &target.id;"

# M3 — the event clear is gated on the WRONG budget: a `--limit 0` pass
#     (which reads no row) erases the verdict, and real passes never do —
#     the pre-fix defect's mechanism, re-aimed at one channel.
mutate "the event clear runs on a zero-byte pass instead of a real one" \
    "an_idle_pass_keeps_the_event_verdict_in_the_journal" \
    "src/webhook.rs" \
    "    if limit > 0 {
        // The queue was read: this phase's pass may now judge its own" \
    "    if limit <= 0 {
        // The queue was read: this phase's pass may now judge its own"

# M4 — the retraction clear runs only on the zero-byte pass (which never
#     reaches it): real passes never clear, so a recovered retraction
#     queue reports its old failure forever — the verdict freezes.
mutate "the retraction clear runs on a zero-byte pass instead of a real one" \
    "a_recovered_retraction_queue_stops_reporting_the_old_failure" \
    "src/webhook.rs" \
    "        j.webhook_clear_reorg_error(&target.id)?;" \
    "        if limit <= 0 { j.webhook_clear_reorg_error(&target.id)?; } let _ = &target.id;"

# M5 — the event clear loses its budget gate: a zero-byte pass (which
#     reads no row) erases the event verdict — the pre-fix defect, on the
#     event channel.
mutate "the event clear runs unguarded, on zero-byte passes too" \
    "an_idle_pass_keeps_the_event_verdict_in_the_journal" \
    "src/webhook.rs" \
    "    if limit > 0 {
        // The queue was read: this phase's pass may now judge its own
        // channel (and only its channel) — see the two rules above.
        j.webhook_clear_event_error(&target.id)?;
    }" \
    "    {
        // The queue was read: this phase's pass may now judge its own
        // channel (and only its channel) — see the two rules above.
        j.webhook_clear_event_error(&target.id)?;
    }"

# M6 — the retraction clear leaks onto the zero-byte path (the budget
#     branch clears instead of staying silent): the idle-pass faces on the
#     retraction channel flip back to the measured pre-fix behaviour.
mutate "the retraction clear also runs on a zero-byte pass" \
    "an_idle_pass_keeps_the_retraction_verdict_in_the_journal" \
    "src/webhook.rs" \
    "    if limit <= 0 {
        // The budget forbids reading the queue: cursors are reported for
        // the report's sake, no verdict is touched." \
    "    if limit <= 0 {
        j.webhook_clear_reorg_error(&target.id)?;
        // The budget forbids reading the queue: cursors are reported for
        // the report's sake, no verdict is touched."

# M7 — the retraction channel's clear erases the EVENT column: the
#     channel swap. The mirrored mutation (event clear touching the
#     retraction column) is behaviorally masked in every reachable path —
#     main.rs always runs the retraction phase right after, which clears
#     its own column — so the ladder does not pretend to catch it.
mutate "the retraction channel's clear touches the event column" \
    "a_blocked_pass_records_both_verdicts_on_their_own_channels" \
    "src/journal.rs" \
    "\"UPDATE webhook_state SET last_reorg_error = NULL WHERE target = ?1\"," \
    "\"UPDATE webhook_state SET last_error = NULL WHERE target = ?1\","

# M8 (DROPPED, HONESTLY): the EVENT channel's clear touching the
#     retraction column is NOT catchable on this tree. The retraction
#     phase now clears its OWN column right after the event phase, so the
#     residual cross-channel leak is unobservable in every reachable path
#     (main.rs and the daemon always run both phases). The ladder found
#     this mutation VACUOUS and the tree was left alone rather than
#     counting a catch that does not exist. The pre-fix shared clear WAS
#     observable — precisely because the retraction phase had no clear of
#     its own.
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

echo "=== 7/7 mutations caught, control green ==="
