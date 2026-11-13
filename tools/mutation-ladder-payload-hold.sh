#!/usr/bin/env bash
# Mutation ladder for the PAYLOAD-HOLD fix (PULS AE, 2026-11-13).
#
# The rule under test: the payload-bearing default `tail` must NOT consume a
# row the payload queue still owes (`logs IS NULL AND payload_state IS
# NULL`). It HOLDS: rows before it are delivered, the cursor stays behind
# it, the exit code is 3, and the filling command is named. The same cursor
# reads the row once `hydrate` paid the debt.
#
# Measured on the PUBLISHED v0.7.21 tree: the defect was visible
# (research/payload-hold-baseline-v0721-2026-11-13.json, verdict
# payload-hold-defect-present). A gate that cannot be made red certifies
# nothing, so every mutation below must turn the exact test that pins it
# RED, and the tree must come back BYTE-IDENTICAL afterwards (file-copying
# backup — never `git checkout`, which would wipe the uncommitted fix).
#
#   1. DESTROYER: the restore is a COPY, never `git checkout`.
#   2. VACUOUS CATCHES: every filter must select at least one real test
#      BEFORE mutating, and a COMPILE ERROR is not a behavioural catch.
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO"

TEST="tail_payload_hold"

echo "=== payload-hold mutation ladder ==="

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

PRISTINE="$(mktemp -d /tmp/ladder-ph-pristine.XXXXXX)"
mkdir -p "$PRISTINE/src"
cp src/main.rs "$PRISTINE/src/main.rs"
restore_all() {
    if [ -f "$PRISTINE/src/main.rs" ]; then
        cp "$PRISTINE/src/main.rs" src/main.rs
        touch src/main.rs
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
' src/main.rs "$search" "$replace" || {
        echo "" >&2
        echo "LADDER: pattern missing in src/main.rs: $search" >&2
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

# 1 — the measured defect class: the plain path stops holding on an owed
#     payload row (this is the v0.7.21 behaviour).
mutate "the plain path stops holding an owed payload row" \
    "plain_tail_holds_at_a_pending_payload_and_delivers_it_once_paid" \
    "        } else if with_logs && r.logs.is_none() && r.payload_state.is_none() {" \
    "        } else if false && with_logs && r.logs.is_none() && r.payload_state.is_none() {"

# 2 — the hold is named but the exit code goes back to 0 (a hold that lies
#     about being a hold).
mutate "a held delivery exits 0" \
    "plain_tail_holds_at_a_pending_payload_and_delivers_it_once_paid" \
    "    if held.is_some() {
        std::process::exit(3);
    }" \
    "    if held.is_some() {
        std::process::exit(0);
    }"

# 3 — the cursor advances past the held row anyway (the loss itself).
mutate "the cursor advances past the held row" \
    "plain_tail_holds_at_a_pending_payload_and_delivers_it_once_paid" \
    "    let advance_to = last_consumed_seq;" \
    "    let advance_to = last_consumed_seq.max(rows.iter().map(|x| x.seq).max().unwrap_or(after));"

# 4 — the hold stops naming the filling command (the consumer is told
#     "something is owed" but not what to run).
mutate "the hold stops naming hydrate" \
    "plain_tail_holds_at_a_pending_payload_and_delivers_it_once_paid" \
    "                \"the payload is still owed — run \`hydrate\`\"," \
    "                \"the payload is still owed\"," 

# 5 — the hold line loses the shape the Node client parses
#     ("HELD at seq <n> <sig>: ...").
mutate "the hold line loses its parseable shape" \
    "a_hold_reports_the_row_the_client_can_parse" \
    "                \" (HELD at seq {} {}: {}; nothing at or after this row is consumed)\"," \
    "                \" (held at {} {}: {})\"," 

# 6 — the guard: `--no-logs` callers must never be held. If the rule leaks
#     into the identity shape, this guard goes red.
mutate "the hold leaks into the --no-logs shape" \
    "no_logs_callers_are_never_held" \
    "        } else if with_logs && r.logs.is_none() && r.payload_state.is_none() {" \
    "        } else if r.logs.is_none() && r.payload_state.is_none() {"

# 7 — the guard: a recorded outcome (unavailable/invalid) must still be
#     DELIVERED with its marker. If the rule holds on decided rows, the
#     consumer wedges forever.
mutate "the hold swallows decided rows too" \
    "recorded_outcomes_are_delivered_and_consumed_never_held" \
    "        } else if with_logs && r.logs.is_none() && r.payload_state.is_none() {" \
    "        } else if with_logs && r.logs.is_none() && (r.payload_state.is_none() || r.payload_state.as_deref() == Some(\"unavailable\") || r.payload_state.as_deref() == Some(\"invalid\")) {"

echo "=== ladder complete: all 7 mutations caught ==="

if ! cmp -s "$PRISTINE/src/main.rs" src/main.rs; then
    echo "LADDER BUG: src/main.rs was not restored byte-identically" >&2
    exit 1
fi
echo "tree restored byte-identically"
