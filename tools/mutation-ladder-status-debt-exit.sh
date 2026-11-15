#!/usr/bin/env bash
# Mutation ladder for the `webhook status` debt-exit fix (PULS AM).
#
# A test that cannot go red proves nothing. Each mutation breaks ONE rule
# the fix introduced; the ladder must catch EVERY one of them, and the tree
# must come back byte-identical afterwards.
#
# The mutations:
#   M1  the report drops `dead_letters_pending` — a debt counted on `push`
#       but never printed by the report that polls
#   M2  the report drops `failing_targets` — the standing verdict is read
#       from the report only inside stderr prose, never as a number
#   M3  the configured quarantine is dropped from the EXIT condition: the
#       orphan keeps exit 3 (v0.7.29) while a configured letter reads health
#   M4  the standing failure is dropped from the EXIT condition: the report
#       names the failing target and still calls it healthy
#   M5  `failing_targets` counts everything — a target with no error is
#       counted too, so a clean journal exits 3 (the "always alarm" mirror)
#
# Usage: tools/mutation-ladder-status-debt-exit.sh
# Exit:  0 all mutations caught, 1 a mutation survived, 2 setup failed.
set -uo pipefail

cd "$(dirname "$0")/.."
TEST="webhook_status_debt_exit"

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

# M1: the pending-quarantine total is dropped from the report.
mutate "the report drops dead_letters_pending" \
    "a_configured_targets_pending_dead_letter_holds_the_exit_code" \
    "src/main.rs" \
    '            // The same journal truth `webhook push` reports under the same
            // names, so one fact carries one number across the surfaces.
            "dead_letters_pending": config_pending + orphaned_pending,
            "failing_targets": failing_targets.len(),' \
    '            // "dead_letters_pending" dropped by mutation
            "failing_targets": failing_targets.len(),'

# M2: the failing-target count is dropped from the report.
mutate "the report drops failing_targets" \
    "a_standing_failure_verdict_holds_the_exit_code" \
    "src/main.rs" \
    '            // The same journal truth `webhook push` reports under the same
            // names, so one fact carries one number across the surfaces.
            "dead_letters_pending": config_pending + orphaned_pending,
            "failing_targets": failing_targets.len(),' \
    '            "dead_letters_pending": config_pending + orphaned_pending,'

# M3: the configured quarantine leaves the EXIT condition — the v0.7.29
# behaviour, where only the orphan (and nothing else) holds the code.
mutate "the configured quarantine leaves the exit condition" \
    "a_configured_targets_pending_dead_letter_holds_the_exit_code" \
    "src/main.rs" \
    '    if orphaned_pending > 0 || config_pending > 0 || !failing_targets.is_empty() {
        std::process::exit(3);
    }' \
    '    if orphaned_pending > 0 || !failing_targets.is_empty() {
        std::process::exit(3);
    }'

# M4: the standing failure leaves the EXIT condition — the report names the
# failing target in prose and still calls the target healthy.
mutate "the standing failure leaves the exit condition" \
    "a_standing_failure_verdict_holds_the_exit_code" \
    "src/main.rs" \
    '    if orphaned_pending > 0 || config_pending > 0 || !failing_targets.is_empty() {
        std::process::exit(3);
    }' \
    '    if orphaned_pending > 0 || config_pending > 0 {
        std::process::exit(3);
    }'

# M5: the failing-target count counts every configured target, clean ones
# included — the "always alarm" mirror of M2, caught by the clean guard.
mutate "the failing count includes clean targets" \
    "a_clean_journal_still_exits_zero" \
    "src/main.rs" \
    '    let failing_targets: Vec<(&str, String)> = out
        .iter()
        .filter_map(|row| {
            row.get("last_error")
                .and_then(|e| e.as_str())
                .filter(|e| !e.is_empty())
                .map(|e| (row["target"].as_str().unwrap_or("?"), e.to_string()))
        })
        .collect();' \
    '    let failing_targets: Vec<(&str, String)> = out
        .iter()
        .map(|row| (row["target"].as_str().unwrap_or("?"), String::new()))
        .collect();'

echo "=== ladder complete: all 5 mutations caught ==="

after=$(find src tests -type f -name '*.rs' -exec sha256sum {} + | sort | sha256sum)
if [ "$before" != "$after" ]; then
  echo "LADDER BUG: the tree was not restored byte-identically" >&2
  exit 1
fi
echo "tree restored byte-identically"
