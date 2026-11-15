#!/usr/bin/env bash
# Mutation ladder for the `webhook status` orphan-dead-letter fix (PULS AL).
#
# A test that cannot go red proves nothing. Each mutation breaks ONE rule
# the fix introduced; the ladder must catch EVERY one of them, and the tree
# must come back byte-identical afterwards.
#
# The mutations:
#   M1  the status report drops the orphan count entirely (the v0.7.28
#       behaviour: the field does not exist)
#   M2  the report always says 0 for orphaned letters — a number that can
#       never be wrong and can never be right either
#   M3  the orphan count is the CONFIG's share instead of the journal's:
#       the letters of a departed target are rounded away
#   M4  the report never exits non-zero: the debt is named but the monitor
#       still reads "all clear"
#   M5  the report names the orphans but the target LIST is dropped, so a
#       reader learns the amount and not where it sits
#
# Usage: tools/mutation-ladder-status-orphan-dead-letters.sh
# Exit:  0 all mutations caught, 1 a mutation survived, 2 setup failed.
set -uo pipefail

cd "$(dirname "$0")/.."
TEST="webhook_status_orphan_dead_letters"

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

# M1: the report stops carrying the field — the v0.7.28 behaviour.
mutate "the status report drops the orphan count" \
    "an_orphaned_dead_letter_is_visible_in_the_status_report" \
    "src/main.rs" \
    '            "orphaned_dead_letters": orphaned_pending,' \
    '            // "orphaned_dead_letters": orphaned_pending,'

# M2: the count is a constant zero.
mutate "the report always says zero orphaned letters" \
    "an_orphaned_dead_letter_is_visible_in_the_status_report" \
    "src/main.rs" \
    '    let orphaned_pending =
        (j.webhook_total_pending_dead_letters().unwrap_or_else(|e| die(e)) - config_pending).max(0);' \
    '    let orphaned_pending: i64 = 0;'

# M3: the count is the CONFIG's share — the letters of a departed target are
# rounded away.
mutate "the orphan count is the config share, not the journal truth" \
    "an_orphaned_dead_letter_is_visible_in_the_status_report" \
    "src/main.rs" \
    '    let orphaned_pending =
        (j.webhook_total_pending_dead_letters().unwrap_or_else(|e| die(e)) - config_pending).max(0);' \
    '    let orphaned_pending: i64 = config_pending;'

# M4: the report never exits non-zero — the debt is named but the monitor
# still reads "all clear". The search is scoped to the status command's own
# refusal line: `std::process::exit(3);` appears in many commands and a
# bare match would mutate the WRONG SITE (that is exactly how this mutation
# first came back VACUOUS).
mutate "the report exits zero even with stranded letters" \
    "an_orphaned_dead_letter_is_visible_in_the_status_report" \
    "src/main.rs" \
    '        eprintln!(
            "reeflog: stranded — target(s): {}",
            orphaned_targets.join(", ")
        );
        std::process::exit(3);' \
    '        eprintln!(
            "reeflog: stranded — target(s): {}",
            orphaned_targets.join(", ")
        );'

# M5: the target list is dropped — a reader learns the amount, not where it
# sits.
mutate "the report names the amount but not the target" \
    "an_orphaned_dead_letter_is_visible_in_the_status_report" \
    "src/main.rs" \
    '            "orphaned_dead_letter_targets": orphaned_targets.iter().map(|id| json!({
                "target": id,
                "in_config": false,
            })).collect::<Vec<_>>(),' \
    '            "orphaned_dead_letter_targets": Vec::<serde_json::Value>::new(),'

echo "=== ladder complete: all 5 mutations caught ==="

after=$(find src tests -type f -name '*.rs' -exec sha256sum {} + | sort | sha256sum)
if [ "$before" != "$after" ]; then
  echo "LADDER BUG: the tree was not restored byte-identically" >&2
  exit 1
fi
echo "tree restored byte-identically"
