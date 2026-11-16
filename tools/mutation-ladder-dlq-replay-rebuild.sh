#!/usr/bin/env bash
# Mutation ladder for the `dlq replay` rebuild fix (PULS AT).
#
# A test that cannot go red proves nothing. Each mutation breaks ONE rule
# the fix introduced; the ladder must catch EVERY one of them, and the tree
# must come back byte-identical afterwards.
#
# The mutations:
#   M1  the replay sends the FROZEN body again — the defect itself, back
#   M2  the rebuild ignores the seal and only the payload — a body that is
#       current in one field and stale in another is not current at all
#   M3  `rebuilt` is dropped from the report — the divergence becomes a
#       surprise at the receiver instead of a fact the operator sees
#   M4  the rebuild is UNCONDITIONAL — a letter with no journal row is
#       dropped instead of replaying its stored bytes
#   M5  the rebuild happens AFTER the receipt was written — the receipt
#       then names bytes the consumer never saw
#
# Usage: tools/mutation-ladder-dlq-replay-rebuild.sh
# Exit:  0 all mutations caught, 1 a mutation survived, 2 setup failed.
set -uo pipefail

cd "$(dirname "$0")/.."
TEST="dlq_replay_rebuild"

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

# M1: the replay goes back to the frozen snapshot — the defect itself.
mutate "the replay sends the frozen body again" \
    "d1_the_replayed_body_carries_the_seals_the_journal_holds" \
    "src/main.rs" \
    '            let body = if r.kind == "event" {
                match j.event_by_seq(r.seq).unwrap_or_else(|e| die(e)) {
                    Some(row) => {
                        let current = webhook::event_body(&row);
                        if current != r.body {
                            target_rebuilt += 1;
                        }
                        current
                    }
                    None => r.body.clone(),
                }
            } else {
                r.body.clone()
            };' \
    '            let body = r.body.clone();'

# M2: the rebuild keeps the frozen seal and only refreshes the payload — a
# body that is current in one field and stale in another is not current.
mutate "the rebuild keeps the frozen seal" \
    "d1_the_replayed_body_carries_the_seals_the_journal_holds" \
    "src/main.rs" \
    '                        let current = webhook::event_body(&row);' \
    '                        let mut current = webhook::event_body(&row);
                        current = current.replace("\"seal\":\"reverted\"", "\"seal\":\"finalized\"");'

# M3: `rebuilt` is dropped from the report.
mutate "the report drops rebuilt" \
    "d3_the_report_names_the_rebuilt_letters" \
    "src/main.rs" \
    '            "rebuilt": rebuilt,
            "pending_before": pending_before,' \
    '            "pending_before": pending_before,'

# M4: the rebuild is unconditional — a letter with no journal row is
# dropped instead of replaying its stored bytes.
mutate "the rebuild is unconditional" \
    "guard_g2_a_letter_without_a_journal_row_replays_its_stored_bytes" \
    "src/main.rs" \
    '                    None => r.body.clone(),' \
    '                    None => String::new(),'

# M5: the receipt names the FROZEN bytes while the consumer saw the
# rebuilt ones — the audit then proves a body that never left the machine.
mutate "the receipt names the frozen bytes" \
    "d1_the_replayed_body_carries_the_seals_the_journal_holds" \
    "src/main.rs" \
    '                    j.webhook_record_delivered(&t.id, &t.url, &r.kind, r.seq, &body)' \
    '                    j.webhook_record_delivered(&t.id, &t.url, &r.kind, r.seq, &r.body)'

echo "=== ladder complete: all 5 mutations caught ==="

after=$(find src tests -type f -name '*.rs' -exec sha256sum {} + | sort | sha256sum)
if [ "$before" != "$after" ]; then
  echo "LADDER BUG: the tree was not restored byte-identically" >&2
  exit 1
fi
echo "tree restored byte-identically"
