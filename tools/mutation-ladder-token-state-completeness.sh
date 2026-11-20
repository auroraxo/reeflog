#!/usr/bin/env bash
# Mutation ladder for the TOKEN-STATE completeness fix (PULS AY, 2026-11-20).
#
# The rule under test: the envelope completeness predicates must know
# `token_state` — the event-time token authority the extractor has written
# since commit 3ed0536 (2026-11-02). An envelope without it is HALF: the
# authority of every token account the transaction touched lives in that
# field, and 103/125 touched token accounts over 10 live Raydium AMM v4
# transactions carry an UNCHANGED amount and live ONLY there.
#
# Measured on the PUBLISHED v0.7.40 tree: the defect is visible
# (research/token-state-completeness-published-v0740-2026-11-20.json,
# verdict token-state-completeness-blind). A gate that cannot be made red
# certifies nothing, so every mutation below must turn the exact test that
# pins it RED, and the tree must come back BYTE-IDENTICAL afterwards
# (file-copying backup — never `git checkout`, which would wipe the
# uncommitted fix).
#
#   1. DESTROYER: the restore is a COPY, never `git checkout`.
#   2. VACUOUS CATCHES: every filter must select at least one real test
#      BEFORE mutating, and a COMPILE ERROR is not a behavioural catch.
#   3. The write guard and the debt queue are two halves of ONE contract.
#      A mutation that breaks only one of them must go red — that
#      asymmetry is the whole point (a queue that refetches forever, or a
#      guard that stores a half envelope over a whole one).
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO"

INTEGRATION="token_state_completeness"
UNIT="payload::tests"

echo "=== token-state completeness mutation ladder ==="

echo -n "unmutated tree ... "
if cargo test --release --test "$INTEGRATION" > /dev/null 2>&1 && cargo test --release --bin reeflog > /dev/null 2>&1; then
    echo "OK (green)"
else
    echo "FAILED: unmutated tree does not pass" >&2
    exit 1
fi

INTEGRATION_LIST="$(cargo test --release --test "$INTEGRATION" -- --list 2>/dev/null | awk -F': ' '/: test$/{print $1}')"
UNIT_LIST="$(cargo test --release --bin reeflog -- --list 2>/dev/null | awk -F': ' '/: test$/{print $1}')"

assert_filter_matches() {
    local list="$1" filter="$2"
    if ! printf '%s\n' "$list" | grep -qx "$filter"; then
        echo "" >&2
        echo "LADDER BUG: filter \`$filter\` matches no test — a CAUGHT verdict would be vacuous" >&2
        printf '%s\n' "$list" | sed 's/^/    available: /' >&2
        exit 1
    fi
}

PRISTINE="$(mktemp -d /tmp/ladder-tokstate-pristine.XXXXXX)"
mkdir -p "$PRISTINE/src"
cp src/journal.rs "$PRISTINE/src/journal.rs"
cp src/main.rs "$PRISTINE/src/main.rs"
restore_all() {
    for f in journal.rs main.rs; do
        if [ -f "$PRISTINE/src/$f" ]; then
            cp "$PRISTINE/src/$f" "src/$f"
            touch "src/$f"
        fi
    done
}
trap restore_all EXIT

mutate() {
    local name="$1"
    local scope="$2"       # integration | unit
    local test_fn="$3"
    local file="$4"
    local search="$5"
    local replace="$6"

    if [ "$scope" = integration ]; then
        assert_filter_matches "$INTEGRATION_LIST" "$test_fn"
    else
        assert_filter_matches "$UNIT_LIST" "$test_fn"
    fi

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
' "$file" "$search" "$replace" || {
        echo "" >&2
        echo "LADDER: pattern missing in $file: $search" >&2
        exit 1
    }

    if ! cargo build --release > /dev/null 2>&1; then
        echo "COMPILE ERROR (not a behavioural catch) — mutation must compile" >&2
        restore_all
        exit 1
    fi

    if [ "$scope" = integration ]; then
        if cargo test --release --test "$INTEGRATION" -- "$test_fn" > /tmp/ladder-tokstate.out 2>&1; then
            echo "VACUOUS! (test passed under mutation)" >&2
            restore_all
            exit 1
        fi
    else
        if cargo test --release --bin reeflog -- "$test_fn" > /tmp/ladder-tokstate.out 2>&1; then
            echo "VACUOUS! (test passed under mutation)" >&2
            restore_all
            exit 1
        fi
    fi
    echo "CAUGHT (went red, as expected)"
    restore_all
}

J=src/journal.rs
M=src/main.rs

# 1 — THE MEASURED DEFECT CLASS, restated in the code: the debt QUEUE does
#     not know `token_state`. This is the published v0.7.40 behaviour: the
#     pass considers 0 rows and never spends an RPC on the debt.
mutate "the debt queue does not know token_state" \
    unit \
    "payload::tests::an_envelope_from_an_older_extractor_is_owed_again_and_converges" \
    "$J" \
    "                                 OR json_extract(accounts, '\$.token_state') IS NULL)))
                 ORDER BY seq LIMIT ?1" \
    "                                )))
                 ORDER BY seq LIMIT ?1"

# 2 — the debt COUNTER's twin: the same blindness in the count the health
#     report prints. A queue that knows the field but a counter that does
#     not still reports "all clear" over the very rows it is refetching.
mutate "the debt counter does not know token_state" \
    integration \
    "the_health_report_counts_a_half_envelope_as_partial" \
    "$J" \
    "                                 OR json_extract(accounts, '\$.token_state') IS NULL)))\",
                [],
                |r| r.get(0)," \
    "                                 OR 0)))\",
                [],
                |r| r.get(0),"

# 3 — the PARTIAL counter: the third copy of the predicate (the count the
#     report prints as accounts_partial). Blind here, a half envelope is
#     counted whole even while the queue is refetching it.
mutate "the partial counter does not know token_state" \
    integration \
    "the_health_report_counts_a_half_envelope_as_partial" \
    "$J" \
    "                        OR json_extract(accounts, '\$.token_state') IS NULL)\",
                [],
                |r| r.get(0)," \
    "                        OR 0)\",
                [],
                |r| r.get(0),"

# 4 — the write GUARD accepts a NEW envelope without the event-time
#     authority: a future extractor regression would be stored over a whole
#     one and the journal silently goes backwards.
mutate "the write guard accepts an envelope without token_state" \
    unit \
    "payload::tests::an_envelope_without_the_token_state_marker_is_never_written" \
    "$J" \
    "AND json_extract(?2, '\$.token_state') IS NOT NULL\"," \
    "AND 1=1\","

# 5 — the queue loses its 'held' NARROWING: a row with a recorded outcome
#     over a half envelope becomes selectable again — a debt the chain has
#     already answered is re-paid forever (the paid-never-finished class).
mutate "the queue loses its held narrowing and refetches final rows" \
    unit \
    "payload::tests::a_final_row_with_a_half_envelope_is_never_refetched" \
    "$J" \
    "                        OR (account_state='held'
                            AND accounts IS NOT NULL
                            AND (json_extract(accounts, '\$.instructions') IS NULL
                                 OR json_extract(accounts, '\$.accounts_flags_source') IS NULL
                                 OR json_extract(accounts, '\$.token_state') IS NULL)))
                 ORDER BY seq LIMIT ?1" \
    "                        OR (json_extract(accounts, '\$.instructions') IS NULL
                                 OR json_extract(accounts, '\$.accounts_flags_source') IS NULL
                                 OR json_extract(accounts, '\$.token_state') IS NULL))
                 ORDER BY seq LIMIT ?1"

# 6 — the ESCAPE HATCH disappears: mark_account_outcome narrows back to
#     'no state at all', so a half envelope whose transaction the chain no
#     longer knows can never record that outcome — the row sits in the
#     debt queue forever, costing one getTransaction per pass.
mutate "a half envelope can no longer record a final outcome" \
    unit \
    "payload::tests::a_half_envelope_can_record_that_the_chain_forgot_the_transaction" \
    "$J" \
    "                \"UPDATE events SET account_state=?2
                 WHERE signature=?1 AND logs IS NOT NULL
                   AND (account_state IS NULL
                        OR (account_state='held'
                            AND accounts IS NOT NULL
                            AND (json_extract(accounts, '\$.instructions') IS NULL
                                 OR json_extract(accounts, '\$.accounts_flags_source') IS NULL
                                 OR json_extract(accounts, '\$.token_state') IS NULL)))\",
                params![signature, state]," \
    "                \"UPDATE events SET account_state=?2
                 WHERE signature=?1 AND logs IS NOT NULL
                   AND account_state IS NULL\",
                params![signature, state],"

# 7 — the delivery filter does not know `token_state`: a half envelope is
#     DELIVERED and CONSUMED, which is the loss itself.
mutate "the delivery filter does not know token_state" \
    integration \
    "the_accounts_filter_holds_a_half_envelope_instead_of_delivering_it" \
    "$M" \
    "                && v.get(\"token_state\").is_some_and(|x| !x.is_null())" \
    "                && true"

# 8 — the GUARD: envelope completeness must never leak into the plain
#     delivery path. If it does, consumers that never asked for accounts
#     are held by a queue they cannot see.
mutate "the accounts rule leaks into the plain tail shape" \
    integration \
    "plain_tail_is_not_held_by_the_accounts_queue" \
    "$M" \
    "        } else if with_logs && r.logs.is_none() && r.payload_state.is_none() {" \
    "        } else if r.accounts.as_deref().is_some_and(|a| !envelope_markers_present(a)) || (with_logs && r.logs.is_none() && r.payload_state.is_none()) {"

for f in journal.rs main.rs; do
    if ! cmp -s "$PRISTINE/src/$f" "src/$f"; then
        echo "LADDER BUG: src/$f was not restored byte-identically" >&2
        exit 1
    fi
done
echo "tree restored byte-identically"
