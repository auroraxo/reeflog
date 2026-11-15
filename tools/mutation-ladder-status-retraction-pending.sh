#!/usr/bin/env bash
# Mutation ladder for the `webhook status` retraction-queue fix (PULS AK).
#
# A test that cannot go red proves nothing. Each mutation breaks ONE rule
# the fix introduced; the ladder must catch EVERY one of them, and the tree
# must come back byte-identical afterwards.
#
# The mutations:
#   M1  the status report drops the pending-retraction count entirely
#       (the v0.7.27 behaviour: the field does not exist)
#   M2  the report always says 0 for pending retractions — a number that can
#       never be wrong and can never be right either
#   M3  the retraction debt is read from the EVENT cursor: one number
#       standing in for the other is the defect this fix exists to prevent
#   M4  the report subtracts instead of counting, so a cursor past the tip
#       can report a debt that does not exist (and a real one as a negative)
#   M5  the two queue counts are swapped in the report
#
# Usage: tools/mutation-ladder-status-retraction-pending.sh
# Exit:  0 all mutations caught, 1 a mutation survived, 2 setup failed.
set -uo pipefail

cd "$(dirname "$0")/.."
TEST="webhook_status_retraction_pending"

SNAPSHOT=""
before=$(find src tests -type f -name '*.rs' -exec sha256sum {} + | sort | sha256sum)

snapshot_tree() {
  SNAPSHOT=$(mktemp -d)
  mkdir -p "$SNAPSHOT/src" "$SNAPSHOT/tests"
  cp -a src/. "$SNAPSHOT/src/" 2>/dev/null || true
  cp -a tests/. "$SNAPSHOT/tests/" 2>/dev/null || true
}

restore() {
  if [ -n "$SNAPSHOT" ] && [ -d "$SNAPSHOT/src" ]; then
    cp -a "$SNAPSHOT/src/." src/ 2>/dev/null || true
    cp -a "$SNAPSHOT/tests/." tests/ 2>/dev/null || true
    find src tests -type f -name '*.rs' -exec touch {} + 2>/dev/null || true
  else
    echo "restore: no snapshot — refusing to fall back to git checkout (it would destroy uncommitted work)"
    return 1
  fi
}

trap 'restore >/dev/null 2>&1 || true; [ -n "$SNAPSHOT" ] && rm -rf "$SNAPSHOT"' EXIT

snapshot_tree || exit 2

restore_all() {
  restore || exit 2
}

mutate() {
  local name="$1" test_fn="$2" path="$3" search="$4" replace="$5"

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
' "$path" "$search" "$replace" || {
        echo "" >&2
        echo "LADDER: pattern missing in $path: $search" >&2
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

# M1: the report stops carrying the field — the v0.7.27 behaviour.
mutate "the status report drops pending_retractions" \
    "a_target_with_pending_retractions_is_visible_in_the_status_report" \
    "src/main.rs" \
    "            \"pending_retractions\": pending_retractions," \
    "            // \"pending_retractions\": pending_retractions,"

# M2: the count is a constant zero.
mutate "the report always says zero pending retractions" \
    "a_target_with_pending_retractions_is_visible_in_the_status_report" \
    "src/main.rs" \
    "        let pending_retractions: i64 = j.reorgs_pending_count(s.next_reorg_id).unwrap_or(0);" \
    "        let pending_retractions: i64 = 0;"

# M3: the retraction debt is read from the EVENT cursor.
mutate "the retraction count is read from the event cursor" \
    "a_target_with_pending_retractions_is_visible_in_the_status_report" \
    "src/main.rs" \
    "        let pending_retractions: i64 = j.reorgs_pending_count(s.next_reorg_id).unwrap_or(0);" \
    "        let pending_retractions: i64 = j.reorgs_pending_count(s.next_seq).unwrap_or(0);"

# M4: the report subtracts instead of counting — a cursor past the tip then
# reports a debt that does not exist.
mutate "the report subtracts instead of counting" \
    "the_pending_retraction_count_is_never_negative" \
    "src/main.rs" \
    "        let pending_retractions: i64 = j.reorgs_pending_count(s.next_reorg_id).unwrap_or(0);" \
    "        let pending_retractions: i64 = (s.next_reorg_id - j.reorgs_pending_count(s.next_reorg_id).unwrap_or(0)).max(0);"

# M5: the two queue counts are swapped in the report.
mutate "the two queue counts are swapped" \
    "both_queues_are_reported_separately" \
    "src/main.rs" \
    "            \"pending_events\": pending," \
    "            \"pending_events\": pending_retractions,"

echo "=== ladder complete: all 5 mutations caught ==="

after=$(find src tests -type f -name '*.rs' -exec sha256sum {} + | sort | sha256sum)
if [ "$before" != "$after" ]; then
    echo "LADDER BUG: the tree was not restored byte-identically" >&2
    exit 1
fi
echo "tree restored byte-identically"
