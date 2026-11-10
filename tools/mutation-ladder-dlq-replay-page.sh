#!/usr/bin/env bash
# Mutation ladder for the dlq-replay page/queue fix (src/main.rs:
# run_webhook_dlq_replay — the pending_before / remaining_pending /
# truncated report fields and the exit-3 on an undrained queue).
#
# Falsification-first: each mutation weakens ONE rule, runs the exact test
# that pins it, and asserts RED. If the test passes under the mutation, the
# test is vacuous and the ladder fails.
#
# HARDENED the same way as tools/mutation-ladder-program-identity.sh:
#
#   1. DESTROYER: the restore is a COPY, never `git checkout` — git would
#      reset to HEAD and WIPE the uncommitted fix.
#   2. VACUOUS CATCHES: every filter must select at least one real test
#      BEFORE mutating, and a COMPILE ERROR is not a behavioural catch.
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO"

TEST="dlq_replay_page_domain"

echo "=== dlq replay page/queue mutation ladder ==="

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

PRISTINE="$(mktemp -d /tmp/ladder-dlq-replay-pristine.XXXXXX)"
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
if search not in text:
    print(f"pattern not found: {search}", file=sys.stderr)
    sys.exit(2)
text = text.replace(search, repl, 1)
with open(path, "w") as f:
    f.write(text)
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

# 1. The pass exits 0 again while letters it never reached stay pending.
mutate "let a truncated replay exit 0 again" \
    "d1_a_truncated_replay_does_not_read_as_a_drained_queue" \
    "    if never_attempted > 0 {" \
    "    if false {"

# 2. The report stops naming the letters the pass did not reach.
mutate "stop reporting the remaining queue" \
    "d2_the_report_names_the_queue_before_the_page" \
    "            \"remaining_pending\": after," \
    "            \"remaining_pending\": 0,"

# 3. The per-target entry stops naming the queue it started from.
mutate "stop reporting the queue before the pass" \
    "d2_the_report_names_the_queue_before_the_page" \
    "            \"pending_before\": before," \
    "            \"pending_before\": 0,"

# 4. `truncated` stops telling the page from the queue.
mutate "stop reporting the truncation" \
    "d2_the_report_names_the_queue_before_the_page" \
    "            \"truncated\": before > attempted," \
    "            \"truncated\": false,"

# 5. The refusal voice stops naming the remaining count.
mutate "stop naming the remaining count" \
    "d4_the_truncation_voice_names_the_count_and_the_repair" \
    "            \"reeflog: {never_attempted} pending dead letter(s) were not reached by this pass — the queue still holds them; run \`reeflog webhook dlq replay\` again (or raise --limit) to drain it\"" \
    "            \"reeflog: the queue still holds pending dead letters; run \`reeflog webhook dlq replay\` again to drain it\""

echo "=== ladder complete: all 5 mutations caught ==="

if ! cmp -s "$PRISTINE/src/main.rs" src/main.rs; then
    echo "LADDER BUG: src/main.rs was not restored byte-identically" >&2
    exit 1
fi
echo "tree restored byte-identically"
