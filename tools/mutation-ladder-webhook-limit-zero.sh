#!/usr/bin/env bash
# Mutation ladder for the webhook `--limit 0` / page-domain fix (PULS AI).
#
# A test that cannot go red proves nothing. Each mutation breaks ONE rule
# the fix introduced; the ladder must catch EVERY one of them, and the tree
# must come back byte-identical afterwards.
#
# The mutations:
#   M1  deliver_events rewrites a zero budget into one delivery (max(1) back)
#   M2  deliver_reorgs rewrites a zero budget into one retraction
#   M3  the DLQ listing rewrites a zero budget into one letter (journal)
#   M4  the push report stops naming the rows behind the page
#   M5  the push report stops calling a short page truncated
#
# Usage: tools/mutation-ladder-webhook-limit-zero.sh
# Exit:  0 all mutations caught, 1 a mutation survived, 2 setup failed.
set -uo pipefail

cd "$(dirname "$0")/.."
TEST="webhook_limit_zero"

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

# M1: the event channel rewrites the operator's zero into one delivery.
mutate "deliver_events rewrites --limit 0 into one delivery" \
    "push_with_limit_zero_sends_no_bytes_and_moves_no_cursor" \
    "src/webhook.rs" \
    "    let rows = if limit <= 0 {
        Vec::new()
    } else {
        j.events_after(rep.next_seq, limit)?
    };" \
    "    let rows = j.events_after(rep.next_seq, limit.max(1))?;"

# M2: the retraction channel rewrites the zero into one retraction.
mutate "deliver_reorgs rewrites --limit 0 into one retraction" \
    "push_with_limit_zero_sends_no_bytes_and_moves_no_cursor" \
    "src/webhook.rs" \
    "    let rows = if limit <= 0 {
        Vec::new()
    } else {
        j.reorgs_after(next_reorg_id, limit)?
    };" \
    "    let rows = j.reorgs_after(next_reorg_id, limit.max(1))?;"

# M3: the DLQ listing rewrites the zero into one letter again.
mutate "the DLQ listing rewrites --limit 0 into one letter" \
    "replay_with_limit_zero_moves_no_letter" \
    "src/journal.rs" \
    "            .query_map(params![target, limit.max(0)], |r| {" \
    "            .query_map(params![target, limit.max(1)], |r| {"

# M4: the report stops naming the rows behind the page.
mutate "the push report stops naming the rows behind the page" \
    "a_truncated_push_names_the_rows_behind_the_page" \
    "src/main.rs" \
    "                \"remaining_behind_cursor\": queue_truth[i].0," \
    "                \"remaining_behind_cursor\": 0,"

# M5: `truncated` stops telling the page from the queue.
mutate "the push report calls a short page untruncated" \
    "a_truncated_push_names_the_rows_behind_the_page" \
    "src/main.rs" \
    "                \"truncated\": queue_truth[i].0 + queue_truth[i].1 > 0 && !r.blocked()," \
    "                \"truncated\": false,"

echo "=== ladder complete: all 5 mutations caught ==="

after=$(find src tests -type f -name '*.rs' -exec sha256sum {} + | sort | sha256sum)
if [ "$before" != "$after" ]; then
    echo "LADDER BUG: the tree was not restored byte-identically" >&2
    exit 1
fi
echo "tree restored byte-identically"
